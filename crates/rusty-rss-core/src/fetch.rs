//! Feed HTTP fetching: retrying GET with timeouts, content-type validation, and
//! secret-redacting error handling.

use crate::config::{redact_error, redact_feed_url};
use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use std::time::Duration;

const TIMEOUT_SECS: u64 = 30;
const MAX_RETRIES: u32 = 3;

/// Fetch the feed at `url`, retrying transient failures with backoff.
///
/// Returns the response body as a string on success. Validates the response
/// status and content type, and scrubs the feed token/user from any error or log
/// output. Returns an error after all retries are exhausted.
pub async fn fetch_feed(client: &Client, url: &str) -> Result<String> {
    let mut last_err = None;

    for attempt in 1..=MAX_RETRIES {
        match do_fetch(client, url).await {
            Ok(body) => return Ok(body),
            Err(e) => {
                // Fail closed: reqwest's error Display embeds the full tokenized
                // request URL (the Reddit `feed` token + `user`), and so does the
                // non-2xx path below. Scrub both before they ever reach tracing
                // output; the raw error is only kept locally for the caller, which
                // redacts again before persisting/returning it.
                let redacted = redact_error(&e.to_string(), url);
                tracing::warn!(attempt, error = %redacted, "fetch failed, retrying");
                // Store the sanitized error, not the raw reqwest error: its
                // Display can embed the tokenized URL, and this is what callers
                // receive after retries are exhausted. Fail closed at our own
                // boundary.
                last_err = Some(anyhow!(redacted));
                if attempt < MAX_RETRIES {
                    tokio::time::sleep(Duration::from_secs(2u64.saturating_pow(attempt - 1))).await;
                }
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow!("all fetch retries exhausted")))
}

async fn do_fetch(client: &Client, url: &str) -> Result<String> {
    let resp = client
        .get(url)
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .send()
        .await
        .context("HTTP request failed")?;

    let status = resp.status();
    if !status.is_success() {
        // Never embed the raw query (feed token + user) in the error message.
        return Err(anyhow!("HTTP {} fetching {}", status, redact_feed_url(url)));
    }

    let ct = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let body = resp.text().await.context("failed to read response body")?;

    if !is_acceptable_content_type(&ct) {
        return Err(anyhow!(
            "unexpected Content-Type: {} (expected XML/Atom or JSON)",
            ct
        ));
    }

    Ok(body)
}

fn is_acceptable_content_type(ct: &str) -> bool {
    let lower = ct.to_lowercase();
    lower.contains("xml")
        || lower.contains("atom")
        || lower.contains("rss")
        || lower.contains("json")
        || lower.contains("text/plain")
}

/// Build the HTTP client used for feed fetching, configured with `user_agent`.
pub fn build_http_client(user_agent: &str) -> Client {
    Client::builder()
        .user_agent(user_agent)
        .build()
        .expect("reqwest client build should not fail")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use tokio::sync::Mutex;

    /// Serializes the tests that drive `fetch_feed`'s retry `warn!` callsite.
    ///
    /// tracing caches callsite interest globally; a test that hits the callsite
    /// with no thread-local subscriber installed can race-poison that cache to
    /// "never", silently dropping warnings the capture test asserts on. Holding
    /// this lock keeps those tests from overlapping. Async-aware so the guard can
    /// be held across the `fetch_feed` await without tripping `await_holding_lock`.
    static RETRY_WARN_TEST_LOCK: Mutex<()> = Mutex::const_new(());

    fn serve_once(
        status: &str,
        content_type: &str,
        body: &str,
    ) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("server should bind");
        let addr = listener.local_addr().expect("local address should exist");
        let status = status.to_string();
        let content_type = content_type.to_string();
        let body = body.to_string();
        let (tx, rx) = mpsc::channel();

        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("server should accept request");
            let mut request = [0u8; 4096];
            let read = stream
                .read(&mut request)
                .expect("request should be readable");
            tx.send(String::from_utf8_lossy(&request[..read]).to_string())
                .expect("request should be sent to test");

            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("response should be written");
        });

        (format!("http://{addr}/feed"), rx)
    }

    /// Serve a fixed sequence of `(status, content_type, body)` responses on one
    /// socket, one per accepted connection. Lets a test drive the retry loop:
    /// e.g. a 503 followed by a 200 to exercise "retry then succeed".
    fn serve_sequence(responses: Vec<(&str, &str, &str)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("server should bind");
        let addr = listener.local_addr().expect("local address should exist");
        let responses: Vec<(String, String, String)> = responses
            .into_iter()
            .map(|(s, c, b)| (s.to_string(), c.to_string(), b.to_string()))
            .collect();

        std::thread::spawn(move || {
            for (status, content_type, body) in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request);
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        format!("http://{addr}/feed")
    }

    #[tokio::test]
    async fn fetch_feed_retries_then_succeeds() {
        // First attempt returns a 503 (transient); the retry loop backs off and
        // the second attempt returns a valid feed. fetch_feed must return the
        // successful body rather than the earlier error.
        let url = serve_sequence(vec![
            ("503 Service Unavailable", "application/atom+xml", "down"),
            ("200 OK", "application/atom+xml", "<feed>ok</feed>"),
        ]);
        let client = build_http_client("rusty-rss-test/1.0");

        let body = fetch_feed(&client, &url)
            .await
            .expect("fetch should succeed after one retry");

        assert_eq!(body, "<feed>ok</feed>");
    }

    #[tokio::test]
    async fn fetch_feed_returns_last_error_after_exhausting_retries() {
        // Every attempt (MAX_RETRIES) returns a 503, so the loop exhausts its
        // budget and returns the last captured error rather than the
        // "all fetch retries exhausted" fallback.
        let responses = std::iter::repeat_n(
            ("503 Service Unavailable", "application/atom+xml", "down"),
            MAX_RETRIES as usize,
        )
        .collect();
        let url = serve_sequence(responses);
        let client = build_http_client("rusty-rss-test/1.0");

        let err = fetch_feed(&client, &url)
            .await
            .expect_err("exhausted retries should fail");

        assert!(err.to_string().contains("HTTP 503"), "got: {err}");
    }

    #[tokio::test]
    async fn fetch_feed_returns_body_and_sends_user_agent() {
        let (url, requests) = serve_once("200 OK", "application/atom+xml", "<feed />");
        let client = build_http_client("rusty-rss-test/1.0");

        let body = fetch_feed(&client, &url)
            .await
            .expect("fetch should succeed");

        assert_eq!(body, "<feed />");
        let request = requests.recv().expect("request should be captured");
        assert!(
            request
                .to_ascii_lowercase()
                .contains("user-agent: rusty-rss-test/1.0")
        );
    }

    #[tokio::test]
    async fn do_fetch_rejects_http_errors() {
        let (url, _requests) =
            serve_once("503 Service Unavailable", "application/atom+xml", "nope");
        let client = build_http_client("rusty-rss-test/1.0");

        let err = do_fetch(&client, &url)
            .await
            .expect_err("HTTP error should fail");

        assert!(err.to_string().contains("HTTP 503"));
    }

    #[tokio::test]
    async fn do_fetch_rejects_unexpected_content_type() {
        let (url, _requests) = serve_once("200 OK", "text/html", "<html>blocked</html>");
        let client = build_http_client("rusty-rss-test/1.0");

        let err = do_fetch(&client, &url)
            .await
            .expect_err("HTML response should fail");

        assert!(err.to_string().contains("unexpected Content-Type"));
    }

    /// In-memory `MakeWriter` so a test can capture everything the tracing
    /// subscriber emits and assert no secret ever reaches the log sink.
    #[derive(Clone, Default)]
    struct CapturingWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl CapturingWriter {
        fn contents(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().expect("log buffer lock")).into_owned()
        }
    }

    impl std::io::Write for CapturingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .expect("log buffer lock")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl tracing_subscriber::fmt::MakeWriter<'_> for CapturingWriter {
        type Writer = CapturingWriter;

        fn make_writer(&self) -> Self::Writer {
            self.clone()
        }
    }

    /// Process-wide capturing subscriber, installed exactly once.
    ///
    /// A thread-local `set_default` subscriber is not reliable here: other tests
    /// in this binary call `fetch_feed` with no subscriber active, and
    /// `NoSubscriber::register_callsite` caches the `warn!` callsite's interest as
    /// `Interest::never()`. `set_default` does not invalidate that cache, so a
    /// late thread-local subscriber can race and capture nothing. Installing the
    /// capturing subscriber as the *global* default runs through
    /// `register_dispatch`, which rebuilds the callsite-interest cache and
    /// recovers any callsite previously registered as `never`. Capturing
    /// process-wide is harmless for the assertions below: every fetch warning is
    /// redacted before it is emitted, so the secret tokens can never reach this
    /// buffer from any test.
    fn global_log_capture() -> &'static CapturingWriter {
        use std::sync::OnceLock;
        static CAPTURE: OnceLock<CapturingWriter> = OnceLock::new();
        CAPTURE.get_or_init(|| {
            let logs = CapturingWriter::default();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(logs.clone())
                .with_max_level(tracing::Level::WARN)
                .without_time()
                .finish();
            tracing::subscriber::set_global_default(subscriber)
                .expect("global default subscriber should be installable once");
            logs
        })
    }

    #[tokio::test]
    async fn fetch_failures_never_leak_feed_token_into_tracing() {
        // First attempt hits a live 503 (non-2xx error path); the one-shot server
        // then closes, so the remaining retries fail with reqwest network errors
        // whose Display embeds the full tokenized URL. Both paths must be scrubbed
        // before they reach the tracing sink.
        let _serialized = RETRY_WARN_TEST_LOCK.lock().await;
        let (base, _requests) = serve_once("503 Service Unavailable", "application/atom+xml", "no");
        let url = format!("{base}?feed=SECRETTOKEN&user=SECRETUSER");
        let client = build_http_client("rusty-rss-test/1.0");

        let logs = global_log_capture();
        let result = fetch_feed(&client, &url).await;

        assert!(
            result.is_err(),
            "fetch against a failing server should error"
        );

        let captured = logs.contents();
        assert!(
            captured.contains("fetch failed, retrying"),
            "expected retry warnings to be captured, got: {captured:?}"
        );
        assert!(
            !captured.contains("SECRETTOKEN"),
            "feed token leaked into tracing output: {captured}"
        );
        assert!(
            !captured.contains("SECRETUSER"),
            "feed user leaked into tracing output: {captured}"
        );
    }

    #[tokio::test]
    async fn fetch_feed_returned_error_never_leaks_token_on_network_failure() {
        // Bind then immediately drop the listener so the port is closed: every
        // connection attempt fails with a reqwest network error whose Display
        // embeds the full tokenized URL. The error RETURNED to the caller after
        // retries are exhausted must be sanitized.
        let _serialized = RETRY_WARN_TEST_LOCK.lock().await;
        let listener = TcpListener::bind("127.0.0.1:0").expect("server should bind");
        let addr = listener.local_addr().expect("local address should exist");
        drop(listener);
        let url = format!("http://{addr}/feed?feed=SECRETTOKEN&user=SECRETUSER");
        let client = build_http_client("rusty-rss-test/1.0");

        // Install a WARN subscriber for the duration so this test exercises the
        // retry `warn!` callsite with a live subscriber, keeping the shared
        // tracing interest cache from being poisoned to "never".
        let logs = CapturingWriter::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .without_time()
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        let err = fetch_feed(&client, &url)
            .await
            .expect_err("connection to a closed port should fail");
        drop(guard);

        // The captured warnings are redacted too (belt-and-braces on the log path).
        let captured = logs.contents();
        assert!(
            !captured.contains("SECRETTOKEN"),
            "token leaked into logs: {captured}"
        );
        assert!(
            !captured.contains("SECRETUSER"),
            "user leaked into logs: {captured}"
        );

        let rendered = format!("{err:#}");
        assert!(
            !rendered.contains("SECRETTOKEN"),
            "token leaked into returned error: {rendered}"
        );
        assert!(
            !rendered.contains("SECRETUSER"),
            "user leaked into returned error: {rendered}"
        );
    }

    #[test]
    fn accepts_expected_content_types() {
        assert!(is_acceptable_content_type("application/atom+xml"));
        assert!(is_acceptable_content_type("application/rss+xml"));
        assert!(is_acceptable_content_type("application/json"));
        assert!(is_acceptable_content_type("text/plain"));
        assert!(!is_acceptable_content_type("text/html"));
    }
}
