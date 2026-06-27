//! HTTP fetch and HTML metadata extraction for a single captured URL.

use super::options::CapturedMetadata;
use super::security::{CaptureClient, validate_capture_url};
use anyhow::{Context, Result, anyhow};
use scraper::{Html, Selector};
use sha2::{Digest, Sha256};
use std::time::Duration;
use url::Url;

const TIMEOUT_SECS: u64 = 20;
const MAX_CAPTURE_BYTES: u64 = 1024 * 1024;

pub async fn capture_url(client: &CaptureClient, url: &str) -> Result<CapturedMetadata> {
    validate_capture_url(url, client.allow_private_hosts()).await?;
    do_capture_url(client, url).await
}

pub(super) async fn capture_url_with_retries(
    client: &CaptureClient,
    url: &str,
    max_retries: usize,
) -> Result<CapturedMetadata> {
    validate_capture_url(url, client.allow_private_hosts()).await?;
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

async fn do_capture_url(client: &CaptureClient, url: &str) -> Result<CapturedMetadata> {
    let mut response = client
        .inner()
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
    use crate::capture::build_capture_client;
    use crate::capture::test_support::{
        serve_html, serve_oversized_no_length, serve_status_sequence,
    };

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

        let metadata = capture_url(&client, &url)
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
    async fn capture_url_retries_transient_failures() {
        let url = serve_status_sequence(
            vec!["503 Service Unavailable", "200 OK"],
            "<html><head><title>Retried page</title></head></html>",
        );
        let client = build_capture_client("rusty-rss-test/1.0", true);

        let metadata = capture_url_with_retries(&client, &url, 2)
            .await
            .expect("retry should eventually succeed");

        assert_eq!(metadata.title.as_deref(), Some("Retried page"));
    }

    #[tokio::test]
    async fn capture_rejects_oversized_streamed_body() {
        let url = serve_oversized_no_length();
        let client = build_capture_client("rusty-rss-test/1.0", true);

        let err = capture_url(&client, &url)
            .await
            .expect_err("oversized streamed body should be rejected");

        assert!(err.to_string().contains("too large"), "got: {err}");
    }

    #[tokio::test]
    async fn capture_url_enforces_client_private_host_policy() {
        // The CaptureClient carries the allow_private_hosts policy, so capture_url
        // derives the validate pre-check from it. There is no separate per-call
        // flag a caller could set to disagree with the client's resolver, which
        // is what would otherwise re-open the DNS-rebinding window.
        let client = build_capture_client("rusty-rss-test/1.0", false);

        let err = capture_url(&client, "http://127.0.0.1:9/private")
            .await
            .expect_err("a strict client must block a loopback URL");

        assert!(
            err.to_string().contains("blocked private outbound host"),
            "got: {err}"
        );
    }
}
