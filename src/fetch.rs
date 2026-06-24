use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use std::time::Duration;

const TIMEOUT_SECS: u64 = 30;
const MAX_RETRIES: u32 = 3;

pub async fn fetch_feed(client: &Client, url: &str) -> Result<String> {
    let mut last_err = None;

    for attempt in 1..=MAX_RETRIES {
        match do_fetch(client, url).await {
            Ok(body) => return Ok(body),
            Err(e) => {
                tracing::warn!(attempt, error = %e, "fetch failed, retrying");
                last_err = Some(e);
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
        return Err(anyhow!("HTTP {} fetching {}", status, url));
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

    #[test]
    fn accepts_expected_content_types() {
        assert!(is_acceptable_content_type("application/atom+xml"));
        assert!(is_acceptable_content_type("application/rss+xml"));
        assert!(is_acceptable_content_type("application/json"));
        assert!(is_acceptable_content_type("text/plain"));
        assert!(!is_acceptable_content_type("text/html"));
    }
}
