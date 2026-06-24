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
