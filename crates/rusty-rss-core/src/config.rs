//! Runtime configuration: loading from CLI overrides and environment variables,
//! plus secret redaction helpers for the feed URL token and user.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Placeholder substituted for any value scrubbed out of a diagnostic string.
const REDACTED: &str = "[REDACTED]";

const DEFAULT_USER_AGENT: &str = "rusty-rss/0.1.0";
const DEFAULT_DB_PATH: &str = "./rusty-rss.sqlite3";
/// Default number of posts fetched per sync when not overridden.
pub const DEFAULT_SYNC_LIMIT: usize = 100;
/// Default maximum number of feed pages walked per sync when not overridden.
pub const DEFAULT_MAX_PAGES: usize = 50;

/// Resolved runtime configuration for a `rusty-rss` invocation.
#[derive(Debug, Clone)]
pub struct Config {
    /// Source feed URL (may carry a secret token/user in its query string).
    pub feed_url: String,
    /// Filesystem path to the SQLite database.
    pub db_path: PathBuf,
    /// `User-Agent` header sent with outbound requests.
    pub user_agent: String,
    /// Maximum number of posts to ingest per sync (clamped to at least 1).
    pub sync_limit: usize,
    /// Maximum number of feed pages to walk per sync (clamped to at least 1).
    pub max_pages: usize,
}

impl Config {
    /// Build a [`Config`] from explicit overrides, falling back to environment
    /// variables (`RUSTY_RSS_FEED_URL`, `RUSTY_RSS_DB_PATH`,
    /// `RUSTY_RSS_USER_AGENT`) and then to built-in defaults.
    ///
    /// The feed URL is required and must parse as a valid URL; pagination values
    /// are clamped to a minimum of 1. Returns an error if no feed URL is
    /// available or it is malformed.
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

/// Clear every secret-bearing component of a parsed URL in place: userinfo
/// (`user:pass@`), query string, and fragment. Leaves only
/// `scheme://host[:port]/path`.
fn clear_sensitive_url_parts(parsed: &mut url::Url) {
    // `set_username`/`set_password` return Err for URLs that cannot have
    // userinfo (e.g. `file:`); ignore that since there is nothing to clear.
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    parsed.set_query(None);
    parsed.set_fragment(None);
}

/// Reduce a feed URL to `scheme://host[:port]/path`, stripping userinfo
/// (`user:pass@`), the query string (where Reddit carries the `feed` token and
/// `user`), and any fragment.
///
/// This is the only form that should ever be persisted to `sync_runs.source_url`
/// or shown to an operator/agent. If the URL cannot be parsed, the portion from
/// the first `?` or `#` onward is still discarded. A secret embedded in the URL
/// *path* (rather than userinfo or query) is NOT removed.
pub fn redact_feed_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            clear_sensitive_url_parts(&mut parsed);
            parsed.to_string()
        }
        Err(_) => match url.split_once(['?', '#']) {
            Some((head, _)) => head.to_string(),
            None => url.to_string(),
        },
    }
}

/// Scrub a diagnostic/error message so it cannot leak the feed token or user.
///
/// Fetch failures from reqwest embed the full request URL (including any
/// `user:pass@` userinfo plus the Reddit `feed` token and `user` query params),
/// so both the persisted `sync_runs.error` and the error returned to
/// callers/agents must be redacted. Two passes, fail-closed: (1) strip userinfo,
/// query, and fragment from any URL in the text, then (2) belt-and-braces
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

/// Strip userinfo, query, and fragment from every `http(s)://…` URL in
/// `message`, leaving only `scheme://host[:port]/path`.
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
        // Parse to drop userinfo as well as query/fragment; fall back to a plain
        // `?`/`#` truncation when the substring is not a parseable URL.
        let cleaned = match url::Url::parse(url) {
            Ok(mut parsed) => {
                clear_sensitive_url_parts(&mut parsed);
                parsed.to_string()
            }
            Err(_) => url
                .split_once(['?', '#'])
                .map_or_else(|| url.to_string(), |(head, _)| head.to_string()),
        };
        out.push_str(&cleaned);
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
    fn redact_feed_url_strips_userinfo_and_query() {
        let redacted = redact_feed_url("https://user:pass@example.com/feed?feed=TOKEN&user=USER");
        assert_eq!(redacted, "https://example.com/feed");
        assert!(!redacted.contains("user:pass"));
        assert!(!redacted.contains("TOKEN"));
    }

    #[test]
    fn redact_feed_url_unparseable_fallback_drops_fragment() {
        let redacted = redact_feed_url("not a url#SECRETFRAG");
        assert!(
            !redacted.contains("SECRETFRAG"),
            "fragment leaked: {redacted}"
        );
        assert_eq!(redacted, "not a url");
    }

    #[test]
    fn redact_error_strips_userinfo_from_embedded_url() {
        let feed_url = "https://user:pass@host/p?feed=SECRETTOKEN";
        let message = "error sending request for url \
            (https://user:pass@host/p?feed=SECRETTOKEN): connection refused";

        let redacted = redact_error(message, feed_url);

        assert!(
            !redacted.contains("user:pass"),
            "userinfo leaked: {redacted}"
        );
        assert!(
            !redacted.contains("SECRETTOKEN"),
            "token leaked: {redacted}"
        );
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
