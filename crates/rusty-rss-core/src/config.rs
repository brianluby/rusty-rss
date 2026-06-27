use anyhow::{Context, Result};
use std::path::PathBuf;

/// Placeholder substituted for any value scrubbed out of a diagnostic string.
const REDACTED: &str = "[REDACTED]";

const DEFAULT_USER_AGENT: &str = "rusty-rss/0.1.0";
const DEFAULT_DB_PATH: &str = "./rusty-rss.sqlite3";
pub const DEFAULT_SYNC_LIMIT: usize = 100;
pub const DEFAULT_MAX_PAGES: usize = 50;

#[derive(Debug, Clone)]
pub struct Config {
    pub feed_url: String,
    pub db_path: PathBuf,
    pub user_agent: String,
    pub sync_limit: usize,
    pub max_pages: usize,
}

impl Config {
    pub fn from_env_and_overrides(
        feed_url: Option<String>,
        db_path: Option<String>,
        sync_limit: usize,
        max_pages: usize,
    ) -> Result<Self> {
        let feed_url = feed_url
            .or_else(|| std::env::var("RUSTY_RSS_FEED_URL").ok())
            .context("feed URL is required (pass --feed-url or set RUSTY_RSS_FEED_URL)")?;

        let db_path = PathBuf::from(
            db_path
                .or_else(|| std::env::var("RUSTY_RSS_DB_PATH").ok())
                .unwrap_or_else(|| DEFAULT_DB_PATH.to_string()),
        );

        let user_agent = std::env::var("RUSTY_RSS_USER_AGENT")
            .unwrap_or_else(|_| DEFAULT_USER_AGENT.to_string());

        url::Url::parse(&feed_url).context("feed URL is not a valid URL")?;

        Ok(Self {
            feed_url,
            db_path,
            user_agent,
            sync_limit: sync_limit.max(1),
            max_pages: max_pages.max(1),
        })
    }
}

/// Reduce a feed URL to `scheme://host[:port]/path`, dropping the query string
/// (which carries the Reddit `feed` token and `user`) and any fragment.
///
/// This is the only form that should ever be persisted to `sync_runs.source_url`
/// or shown to an operator/agent. Fails closed: if the URL cannot be parsed, the
/// portion from the first `?` onward is discarded so a query-embedded token
/// cannot leak.
pub fn redact_feed_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            parsed.set_query(None);
            parsed.set_fragment(None);
            parsed.to_string()
        }
        Err(_) => match url.split_once('?') {
            Some((head, _)) => head.to_string(),
            None => url.to_string(),
        },
    }
}

/// Scrub a diagnostic/error message so it cannot leak the feed token or user.
///
/// Fetch failures from reqwest embed the full request URL (including the Reddit
/// `feed` token and `user`), so both the persisted `sync_runs.error` and the
/// error returned to callers/agents must be redacted. Two passes, fail-closed:
/// (1) strip the query string from any URL in the text, then (2) belt-and-braces
/// replace each query-parameter value carried by the configured feed URL.
pub fn redact_error(message: &str, feed_url: &str) -> String {
    let mut redacted = strip_url_queries(message);
    for value in sensitive_values(feed_url) {
        if !value.is_empty() {
            redacted = redacted.replace(&value, REDACTED);
        }
    }
    redacted
}

/// Decoded query-parameter values from `feed_url`, treated as secrets to scrub.
fn sensitive_values(feed_url: &str) -> Vec<String> {
    match url::Url::parse(feed_url) {
        Ok(parsed) => parsed
            .query_pairs()
            .map(|(_, value)| value.into_owned())
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Truncate every `http(s)://…` URL in `message` at its query/fragment.
fn strip_url_queries(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;

    while let Some(idx) = next_scheme(rest) {
        out.push_str(&rest[..idx]);
        let from_url = &rest[idx..];

        // The URL runs until the first character that cannot belong to one.
        let end = from_url
            .find(|c: char| {
                c.is_whitespace()
                    || matches!(
                        c,
                        ')' | ']' | '}' | '"' | '\'' | '>' | '<' | '|' | '\\' | '`'
                    )
            })
            .unwrap_or(from_url.len());

        let url = &from_url[..end];
        let trimmed = url.split(['?', '#']).next().unwrap_or(url);
        out.push_str(trimmed);
        rest = &from_url[end..];
    }

    out.push_str(rest);
    out
}

/// Index of the earliest `http://` or `https://` marker in `s`, if any.
fn next_scheme(s: &str) -> Option<usize> {
    match (s.find("http://"), s.find("https://")) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_feed_url_strips_query_token_and_user() {
        let redacted =
            redact_feed_url("https://old.reddit.com/saved.rss?feed=SECRETTOKEN&user=SECRETUSER");
        assert_eq!(redacted, "https://old.reddit.com/saved.rss");
        assert!(!redacted.contains("SECRETTOKEN"));
        assert!(!redacted.contains("SECRETUSER"));
    }

    #[test]
    fn redact_feed_url_strips_fragment_and_preserves_port_and_path() {
        let redacted = redact_feed_url("http://127.0.0.1:8080/feed/path?feed=t&user=u#frag");
        assert_eq!(redacted, "http://127.0.0.1:8080/feed/path");
    }

    #[test]
    fn redact_feed_url_fails_closed_on_unparseable_input() {
        let redacted = redact_feed_url("not a url?feed=SECRETTOKEN&user=SECRETUSER");
        assert!(!redacted.contains("SECRETTOKEN"));
        assert!(!redacted.contains("SECRETUSER"));
    }

    #[test]
    fn redact_error_removes_token_and_user_from_fetch_error() {
        let feed_url = "https://old.reddit.com/saved.rss?feed=SECRETTOKEN&user=SECRETUSER";
        let message = "HTTP 503 fetching \
            https://old.reddit.com/saved.rss?feed=SECRETTOKEN&user=SECRETUSER&limit=2&after=t3_x";

        let redacted = redact_error(message, feed_url);

        assert!(
            !redacted.contains("SECRETTOKEN"),
            "token leaked: {redacted}"
        );
        assert!(!redacted.contains("SECRETUSER"), "user leaked: {redacted}");
        assert!(
            redacted.contains("https://old.reddit.com/saved.rss"),
            "host+path should survive: {redacted}"
        );
    }

    #[test]
    fn redact_error_scrubs_token_even_inside_parenthesized_reqwest_url() {
        let feed_url = "https://old.reddit.com/saved.rss?feed=SECRETTOKEN&user=SECRETUSER";
        let message = "error sending request for url \
            (https://old.reddit.com/saved.rss?feed=SECRETTOKEN&user=SECRETUSER&limit=2): \
            connection refused";

        let redacted = redact_error(message, feed_url);

        assert!(!redacted.contains("SECRETTOKEN"), "{redacted}");
        assert!(!redacted.contains("SECRETUSER"), "{redacted}");
    }

    #[test]
    fn builds_config_from_overrides() {
        let config = Config::from_env_and_overrides(
            Some("https://old.reddit.com/saved.rss?feed=token&user=user".to_string()),
            Some("./test.sqlite3".to_string()),
            100,
            50,
        )
        .expect("config should build");

        assert_eq!(
            config.feed_url,
            "https://old.reddit.com/saved.rss?feed=token&user=user"
        );
        assert_eq!(config.db_path, PathBuf::from("./test.sqlite3"));
        assert_eq!(config.user_agent, DEFAULT_USER_AGENT);
        assert_eq!(config.sync_limit, 100);
        assert_eq!(config.max_pages, 50);
    }

    #[test]
    fn uses_default_db_path_when_not_overridden() {
        let config = Config::from_env_and_overrides(
            Some("https://old.reddit.com/saved.rss?feed=token&user=user".to_string()),
            None,
            100,
            50,
        )
        .expect("config should build");

        assert_eq!(config.db_path, PathBuf::from(DEFAULT_DB_PATH));
    }

    #[test]
    fn rejects_invalid_feed_url() {
        let err = Config::from_env_and_overrides(Some("not a url".to_string()), None, 100, 50)
            .expect_err("invalid URL should fail");

        assert!(err.to_string().contains("feed URL is not a valid URL"));
    }

    #[test]
    fn clamps_zero_pagination_values() {
        let config = Config::from_env_and_overrides(
            Some("https://old.reddit.com/saved.rss?feed=token&user=user".to_string()),
            None,
            0,
            0,
        )
        .expect("config should build");

        assert_eq!(config.sync_limit, 1);
        assert_eq!(config.max_pages, 1);
    }
}
