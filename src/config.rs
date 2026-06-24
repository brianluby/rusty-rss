use anyhow::{Context, Result};
use std::path::PathBuf;

const DEFAULT_USER_AGENT: &str = "rusty-rss/0.1.0";
const DEFAULT_DB_PATH: &str = "./rusty-rss.sqlite3";

#[derive(Debug, Clone)]
pub struct Config {
    pub feed_url: String,
    pub db_path: PathBuf,
    pub user_agent: String,
}

impl Config {
    pub fn from_env_and_overrides(
        feed_url: Option<String>,
        db_path: Option<String>,
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
        })
    }
}
