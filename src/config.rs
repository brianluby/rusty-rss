use anyhow::{Context, Result};
use std::path::PathBuf;

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

#[cfg(test)]
mod tests {
    use super::*;

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
