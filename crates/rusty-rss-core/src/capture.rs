use crate::db::{self, OutboundCaptureUpsert};
use anyhow::{Context, Result, anyhow};
use reqwest::{Client, redirect::Policy};
use rusqlite::Connection;
use scraper::{Html, Selector};
use sha2::{Digest, Sha256};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use url::Url;

const TIMEOUT_SECS: u64 = 20;
const MAX_CAPTURE_BYTES: u64 = 1024 * 1024;
const DEFAULT_MAX_CONCURRENCY: usize = 4;
const DEFAULT_MAX_RETRIES: usize = 3;
const CAPTURE_USER_AGENT: &str = "rusty-rss/0.1";

#[derive(Debug, Clone, Copy)]
pub struct CaptureOptions {
    pub limit: usize,
    pub allow_private_hosts: bool,
    pub max_concurrency: usize,
    pub max_retries: usize,
}

impl CaptureOptions {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            allow_private_hosts: false,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            max_retries: DEFAULT_MAX_RETRIES,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CaptureSummary {
    pub selected_count: usize,
    pub captured_count: usize,
    pub failed_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedMetadata {
    pub final_url: String,
    pub canonical_url: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub site_name: Option<String>,
    pub preview_image_url: Option<String>,
    pub content_markdown: Option<String>,
    pub content_hash: Option<String>,
    pub http_status: u16,
}

pub fn build_capture_client(user_agent: &str, allow_private_hosts: bool) -> Client {
    Client::builder()
        .user_agent(user_agent)
        .redirect(Policy::none())
        .dns_resolver(Arc::new(GuardedResolver {
            allow_private_hosts,
        }))
        .build()
        .expect("reqwest client build should not fail")
}

/// DNS resolver that drops private/loopback addresses at resolution time, so the
/// address the client actually connects to is the one that passed the check.
///
/// The standalone [`validate_capture_url`] pre-check resolves the host once, but
/// reqwest resolves again when it connects; an attacker-controlled name can
/// return a public IP to the pre-check and a private IP to the connection (DNS
/// rebinding). Enforcing the policy inside the resolver the client connects with
/// closes that window. Literal-IP URLs bypass DNS entirely and are guarded by
/// [`validate_capture_url`] instead.
#[derive(Debug, Clone, Copy)]
struct GuardedResolver {
    allow_private_hosts: bool,
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let allow_private_hosts = self.allow_private_hosts;
        Box::pin(async move {
            // Port 0 is a placeholder; reqwest's connector applies the real port.
            let resolved = tokio::net::lookup_host((name.as_str(), 0)).await?;
            let allowed = allowed_resolved_addrs(resolved, allow_private_hosts);
            if allowed.is_empty() {
                return Err(Box::<dyn std::error::Error + Send + Sync>::from(
                    "blocked private outbound host",
                ));
            }
            let addrs: reqwest::dns::Addrs = Box::new(allowed.into_iter());
            Ok(addrs)
        })
    }
}

/// Keep only the addresses the client is allowed to connect to. This is the
/// policy the connection uses, so a name that resolves to a mix of public and
/// private IPs only ever connects to the public ones.
fn allowed_resolved_addrs(
    resolved: impl Iterator<Item = SocketAddr>,
    allow_private_hosts: bool,
) -> Vec<SocketAddr> {
    resolved
        .filter(|addr| allow_private_hosts || !is_blocked_ip(addr.ip()))
        .collect()
}

pub async fn capture_outbound_metadata(
    conn: &Connection,
    options: CaptureOptions,
) -> Result<CaptureSummary> {
    // Build the client here so the DNS-rebinding guard is always installed and
    // matches `options.allow_private_hosts`; callers cannot supply an unguarded
    // client.
    let client = build_capture_client(CAPTURE_USER_AGENT, options.allow_private_hosts);
    let candidates = db::list_outbound_capture_candidates(conn, options.limit)?;
    let mut summary = CaptureSummary {
        selected_count: candidates.len(),
        ..CaptureSummary::default()
    };
    let max_concurrency = options.max_concurrency.max(1);
    let max_retries = options.max_retries.max(1);
    let semaphore = Arc::new(Semaphore::new(max_concurrency));
    let mut tasks = JoinSet::new();

    for candidate in candidates {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .context("capture semaphore closed")?;
        let client = client.clone();
        let allow_private_hosts = options.allow_private_hosts;
        tasks.spawn(async move {
            let result = capture_url_with_retries(
                &client,
                &candidate.outbound_url,
                allow_private_hosts,
                max_retries,
            )
            .await;
            drop(permit);
            (candidate, result)
        });
    }

    while let Some(result) = tasks.join_next().await {
        let (candidate, capture_result) = result.context("capture task failed")?;
        match capture_result {
            Ok(metadata) => {
                db::upsert_outbound_capture(
                    conn,
                    &OutboundCaptureUpsert {
                        reddit_fullname: candidate.reddit_fullname,
                        original_url: candidate.outbound_url,
                        final_url: Some(metadata.final_url),
                        canonical_url: metadata.canonical_url,
                        title: metadata.title,
                        description: metadata.description,
                        site_name: metadata.site_name,
                        preview_image_url: metadata.preview_image_url,
                        content_markdown: metadata.content_markdown,
                        content_hash: metadata.content_hash,
                        status: "success".to_string(),
                        http_status: Some(i64::from(metadata.http_status)),
                        error: None,
                    },
                )?;
                summary.captured_count += 1;
            }
            Err(err) => {
                db::upsert_outbound_capture(
                    conn,
                    &OutboundCaptureUpsert {
                        reddit_fullname: candidate.reddit_fullname,
                        original_url: candidate.outbound_url,
                        final_url: None,
                        canonical_url: None,
                        title: None,
                        description: None,
                        site_name: None,
                        preview_image_url: None,
                        content_markdown: None,
                        content_hash: None,
                        status: "error".to_string(),
                        http_status: None,
                        error: Some(err.to_string()),
                    },
                )?;
                summary.failed_count += 1;
            }
        }
    }

    Ok(summary)
}

pub async fn capture_url(
    client: &Client,
    url: &str,
    allow_private_hosts: bool,
) -> Result<CapturedMetadata> {
    validate_capture_url(url, allow_private_hosts).await?;
    do_capture_url(client, url).await
}

async fn capture_url_with_retries(
    client: &Client,
    url: &str,
    allow_private_hosts: bool,
    max_retries: usize,
) -> Result<CapturedMetadata> {
    validate_capture_url(url, allow_private_hosts).await?;
    let mut last_err = None;

    for attempt in 1..=max_retries.max(1) {
        match do_capture_url(client, url).await {
            Ok(metadata) => return Ok(metadata),
            Err(err) => {
                last_err = Some(err);
                if attempt < max_retries {
                    tokio::time::sleep(Duration::from_millis(50 * attempt as u64)).await;
                }
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow!("capture retries exhausted")))
}

async fn do_capture_url(client: &Client, url: &str) -> Result<CapturedMetadata> {
    let mut response = client
        .get(url)
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .send()
        .await
        .context("HTTP request failed")?;
    let status = response.status();
    let final_url = response.url().to_string();

    if !status.is_success() {
        return Err(anyhow!("HTTP {} fetching {}", status, url));
    }
    if !is_html_response(&response) {
        return Err(anyhow!("unexpected Content-Type for capture"));
    }
    if response
        .content_length()
        .is_some_and(|len| len > MAX_CAPTURE_BYTES)
    {
        return Err(anyhow!("capture response is too large"));
    }

    // Read the body incrementally and stop as soon as the cap is exceeded, so a
    // server that omits or lies about Content-Length cannot stream an unbounded
    // body into memory before the size check runs.
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("failed to read response body")?
    {
        if body.len() + chunk.len() > MAX_CAPTURE_BYTES as usize {
            return Err(anyhow!("capture response is too large"));
        }
        body.extend_from_slice(&chunk);
    }

    let html = String::from_utf8_lossy(&body);
    let document = Html::parse_document(&html);
    let content_markdown = html2md::parse_html(&html).trim().to_string();
    let (content_markdown, content_hash) = if content_markdown.is_empty() {
        (None, None)
    } else {
        let content_hash = format!("sha256:{}", hex_sha256(content_markdown.as_bytes()));
        (Some(content_markdown), Some(content_hash))
    };
    Ok(CapturedMetadata {
        final_url,
        canonical_url: absolute_url(url, select_attr(&document, "link[rel='canonical']", "href")),
        title: select_attr(&document, "meta[property='og:title']", "content")
            .or_else(|| select_text(&document, "title")),
        description: select_attr(&document, "meta[name='description']", "content")
            .or_else(|| select_attr(&document, "meta[property='og:description']", "content")),
        site_name: select_attr(&document, "meta[property='og:site_name']", "content"),
        preview_image_url: absolute_url(
            url,
            select_attr(&document, "meta[property='og:image']", "content"),
        ),
        content_markdown,
        content_hash,
        http_status: status.as_u16(),
    })
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

async fn validate_capture_url(url: &str, allow_private_hosts: bool) -> Result<()> {
    let parsed = Url::parse(url).context("invalid outbound URL")?;
    match parsed.scheme() {
        "http" | "https" => {}
        scheme => return Err(anyhow!("unsupported outbound URL scheme: {scheme}")),
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("outbound URL is missing a host"))?;
    if host.eq_ignore_ascii_case("localhost") && !allow_private_hosts {
        return Err(anyhow!("blocked private outbound host"));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !allow_private_hosts && is_blocked_ip(ip) {
            return Err(anyhow!("blocked private outbound host"));
        }
        return Ok(());
    }
    if allow_private_hosts {
        return Ok(());
    }

    let port = parsed.port_or_known_default().unwrap_or(443);
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .context("failed to resolve outbound host")?;
    for addr in addrs {
        if is_blocked_ip(addr.ip()) {
            return Err(anyhow!("blocked private outbound host"));
        }
    }

    Ok(())
}

fn is_blocked_ip(ip: IpAddr) -> bool {
    // Normalize IPv4-mapped IPv6 (e.g. ::ffff:127.0.0.1) to IPv4 so an embedded
    // private/loopback address cannot slip through the IPv6 branch.
    let ip = match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        other => other,
    };
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                // CGNAT / shared address space, 100.64.0.0/10.
                || (a == 100 && (64..=127).contains(&b))
                // Benchmarking, 198.18.0.0/15.
                || (a == 198 && (b == 18 || b == 19))
                // Reserved (incl. future use), 240.0.0.0/4.
                || a >= 240
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            let first = segments[0];
            ip.is_loopback()
                || ip.is_unspecified()
                // Unique local addresses, fc00::/7.
                || (first & 0xfe00) == 0xfc00
                // Link-local unicast, fe80::/10 (the old 0xfe00 mask never matched).
                || (first & 0xffc0) == 0xfe80
                // Multicast, ff00::/8.
                || (first & 0xff00) == 0xff00
                // Documentation, 2001:db8::/32.
                || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        }
    }
}

fn is_html_response(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase().contains("html"))
        .unwrap_or(false)
}

fn select_text(document: &Html, selector: &str) -> Option<String> {
    let selector = Selector::parse(selector).ok()?;
    document
        .select(&selector)
        .next()
        .map(|element| element.text().collect::<String>().trim().to_string())
        .filter(|value| !value.is_empty())
}

fn select_attr(document: &Html, selector: &str, attr: &str) -> Option<String> {
    let selector = Selector::parse(selector).ok()?;
    document
        .select(&selector)
        .find_map(|element| element.value().attr(attr))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn absolute_url(base: &str, value: Option<String>) -> Option<String> {
    let value = value?;
    Url::parse(&value)
        .or_else(|_| Url::parse(base)?.join(&value))
        .ok()
        .map(|url| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SavedPost;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn serve_html(body: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("server should bind");
        let addr = listener.local_addr().expect("local address should exist");
        let body = body.to_string();

        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("server should accept request");
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request).expect("request should read");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("response should write");
        });

        format!("http://{addr}/article")
    }

    fn serve_status_sequence(statuses: Vec<&'static str>, body: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("server should bind");
        let addr = listener.local_addr().expect("local address should exist");
        let body = body.to_string();

        std::thread::spawn(move || {
            for status in statuses {
                let (mut stream, _) = listener.accept().expect("server should accept request");
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request).expect("request should read");
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("response should write");
            }
        });

        format!("http://{addr}/article")
    }

    fn serve_concurrent_html(
        response_count: usize,
        current: Arc<AtomicUsize>,
        max_seen: Arc<AtomicUsize>,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("server should bind");
        let addr = listener.local_addr().expect("local address should exist");

        std::thread::spawn(move || {
            for _ in 0..response_count {
                let (mut stream, _) = listener.accept().expect("server should accept request");
                let current = Arc::clone(&current);
                let max_seen = Arc::clone(&max_seen);
                std::thread::spawn(move || {
                    let now = current.fetch_add(1, Ordering::SeqCst) + 1;
                    max_seen.fetch_max(now, Ordering::SeqCst);
                    let mut request = [0u8; 4096];
                    let _ = stream.read(&mut request).expect("request should read");
                    std::thread::sleep(std::time::Duration::from_millis(80));
                    let body = "<html><head><title>Concurrent page</title></head><body>Concurrent body</body></html>";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    stream
                        .write_all(response.as_bytes())
                        .expect("response should write");
                    current.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });

        format!("http://{addr}")
    }

    #[tokio::test]
    async fn capture_url_extracts_page_metadata() {
        let url = serve_html(
            r#"<html><head>
            <title>Fallback title</title>
            <link rel="canonical" href="/canonical">
            <meta property="og:title" content="OG title">
            <meta name="description" content="Description">
            <meta property="og:site_name" content="Example Site">
            <meta property="og:image" content="/image.png">
            </head><body><p>Visible article body</p></body></html>"#,
        );
        let client = build_capture_client("rusty-rss-test/1.0", true);

        let metadata = capture_url(&client, &url, true)
            .await
            .expect("capture should succeed");

        assert_eq!(metadata.title.as_deref(), Some("OG title"));
        assert_eq!(metadata.description.as_deref(), Some("Description"));
        assert_eq!(metadata.site_name.as_deref(), Some("Example Site"));
        assert!(
            metadata
                .content_markdown
                .as_deref()
                .is_some_and(|content| content.contains("Visible article body"))
        );
        assert!(
            metadata
                .content_hash
                .as_deref()
                .is_some_and(|hash| hash.starts_with("sha256:"))
        );
        assert!(
            metadata
                .canonical_url
                .as_deref()
                .is_some_and(|url| url.ends_with("/canonical"))
        );
        assert!(
            metadata
                .preview_image_url
                .as_deref()
                .is_some_and(|url| url.ends_with("/image.png"))
        );
    }

    #[tokio::test]
    async fn validate_capture_url_blocks_private_hosts_by_default() {
        let err = validate_capture_url("http://127.0.0.1:8080/private", false)
            .await
            .expect_err("private host should be blocked");

        assert!(err.to_string().contains("blocked private outbound host"));
    }

    #[tokio::test]
    async fn capture_outbound_metadata_records_success() {
        let db_path = std::env::temp_dir().join(format!(
            "rusty_rss_capture_test_{}_{}.db",
            std::process::id(),
            1
        ));
        let _ = std::fs::remove_file(&db_path);
        let conn = db::init_db(&db_path).expect("db should init");
        let url = serve_html("<html><head><title>Captured page</title></head></html>");
        let mut post = SavedPost::new(
            "t3_capture".to_string(),
            "Capture".to_string(),
            "https://reddit.com/r/rust/comments/capture/item/".to_string(),
            "atom".to_string(),
        );
        post.outbound_url = Some(url);
        db::upsert_post(&conn, &post).expect("post should insert");

        let summary = capture_outbound_metadata(
            &conn,
            CaptureOptions {
                limit: 10,
                allow_private_hosts: true,
                ..CaptureOptions::new(10)
            },
        )
        .await
        .expect("capture should run");

        assert_eq!(summary.selected_count, 1);
        assert_eq!(summary.captured_count, 1);
        let capture = db::latest_outbound_capture(&conn, "t3_capture")
            .expect("capture should query")
            .expect("capture should exist");
        assert_eq!(capture.title.as_deref(), Some("Captured page"));
        assert!(
            capture
                .content_markdown
                .as_deref()
                .is_some_and(|content| content.contains("Captured page"))
        );
        assert!(
            capture
                .content_hash
                .as_deref()
                .is_some_and(|hash| hash.starts_with("sha256:"))
        );
    }

    #[tokio::test]
    async fn capture_url_retries_transient_failures() {
        let url = serve_status_sequence(
            vec!["503 Service Unavailable", "200 OK"],
            "<html><head><title>Retried page</title></head></html>",
        );
        let client = build_capture_client("rusty-rss-test/1.0", true);

        let metadata = capture_url_with_retries(&client, &url, true, 2)
            .await
            .expect("retry should eventually succeed");

        assert_eq!(metadata.title.as_deref(), Some("Retried page"));
    }

    #[tokio::test]
    async fn capture_outbound_metadata_respects_concurrency_limit() {
        let db_path = std::env::temp_dir().join(format!(
            "rusty_rss_capture_concurrency_test_{}_{}.db",
            std::process::id(),
            1
        ));
        let _ = std::fs::remove_file(&db_path);
        let conn = db::init_db(&db_path).expect("db should init");
        let current = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let base_url = serve_concurrent_html(3, Arc::clone(&current), Arc::clone(&max_seen));

        for index in 0..3 {
            let mut post = SavedPost::new(
                format!("t3_capture_{index}"),
                format!("Capture {index}"),
                format!("https://reddit.com/r/rust/comments/capture/{index}/"),
                "atom".to_string(),
            );
            post.outbound_url = Some(format!("{base_url}/article-{index}"));
            db::upsert_post(&conn, &post).expect("post should insert");
        }

        let summary = capture_outbound_metadata(
            &conn,
            CaptureOptions {
                limit: 10,
                allow_private_hosts: true,
                max_concurrency: 2,
                max_retries: 1,
            },
        )
        .await
        .expect("capture should run");

        assert_eq!(summary.captured_count, 3);
        assert!(max_seen.load(Ordering::SeqCst) <= 2);
    }

    fn serve_oversized_no_length() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("server should bind");
        let addr = listener.local_addr().expect("local address should exist");

        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("server should accept request");
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            // No Content-Length: the body is close-delimited, so the size cap can
            // only be enforced by reading incrementally.
            let header = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n";
            if stream.write_all(header.as_bytes()).is_err() {
                return;
            }
            let chunk = vec![b'a'; 64 * 1024];
            // 2 MiB total, double the 1 MiB cap. Stop once the client disconnects.
            for _ in 0..32 {
                if stream.write_all(&chunk).is_err() {
                    break;
                }
            }
        });

        format!("http://{addr}/big")
    }

    #[tokio::test]
    async fn capture_rejects_oversized_streamed_body() {
        let url = serve_oversized_no_length();
        let client = build_capture_client("rusty-rss-test/1.0", true);

        let err = capture_url(&client, &url, true)
            .await
            .expect_err("oversized streamed body should be rejected");

        assert!(err.to_string().contains("too large"), "got: {err}");
    }

    #[test]
    fn allowed_resolved_addrs_drops_private_hosts() {
        let public: SocketAddr = "1.1.1.1:0".parse().expect("addr");
        let private: SocketAddr = "10.0.0.5:0".parse().expect("addr");
        let loopback: SocketAddr = "127.0.0.1:0".parse().expect("addr");

        // Default policy keeps only the public address, so the connection can
        // never reach the private/loopback ones even if DNS returns them.
        let allowed = allowed_resolved_addrs([public, private, loopback].into_iter(), false);
        assert_eq!(allowed, vec![public]);

        // No public address resolves -> empty -> the resolver fails closed.
        let none = allowed_resolved_addrs([private, loopback].into_iter(), false);
        assert!(none.is_empty());

        // Opt-in allows everything (used for the localhost test servers).
        let all = allowed_resolved_addrs([public, private, loopback].into_iter(), true);
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn is_blocked_ip_covers_ipv6_edge_cases() {
        let blocked = [
            "::ffff:127.0.0.1", // IPv4-mapped loopback
            "::ffff:10.0.0.1",  // IPv4-mapped private
            "::1",              // IPv6 loopback
            "fe80::1",          // link-local fe80::/10
            "fc00::1",          // unique local fc00::/7
            "ff02::1",          // IPv6 multicast ff00::/8
            "2001:db8::1",      // IPv6 documentation 2001:db8::/32
            "100.64.0.1",       // CGNAT 100.64.0.0/10
            "198.18.0.1",       // benchmarking 198.18.0.0/15
            "240.0.0.1",        // reserved 240.0.0.0/4
            "224.0.0.1",        // IPv4 multicast 224.0.0.0/4
        ];
        for addr in blocked {
            assert!(
                is_blocked_ip(addr.parse().expect("addr")),
                "{addr} should be blocked"
            );
        }

        let allowed = [
            "1.1.1.1",              // public IPv4
            "2606:4700:4700::1111", // public IPv6
            "::ffff:1.1.1.1",       // IPv4-mapped public
        ];
        for addr in allowed {
            assert!(
                !is_blocked_ip(addr.parse().expect("addr")),
                "{addr} should be allowed"
            );
        }
    }
}
