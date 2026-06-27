//! Command-line interface: argument parsing and the subcommand dispatcher.
//!
//! This root holds the clap [`Cli`]/[`Command`]/[`ExportFormat`] definitions and
//! the [`run`] dispatcher; each subcommand's handler lives in its own module
//! (read/sync/enrich/tag/triage/export).

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use rusty_rss_core::config::{Config, DEFAULT_MAX_PAGES, DEFAULT_SYNC_LIMIT};
use rusty_rss_core::db::SearchFilters;
use std::path::PathBuf;

mod enrich;
mod export;
mod fts;
mod read;
mod sync;
mod tag;
mod triage;

#[cfg(test)]
mod test_support;

use enrich::run_enrich;
use export::run_export;
use fts::{FtsCommand, run_fts};
use read::{run_list, run_search, run_show};
use sync::{run_capture, run_sync};
use tag::run_tag;
use triage::run_triage;

#[derive(Parser)]
#[command(
    name = "rusty-rss",
    about = "Sync Reddit saved posts from RSS/Atom feed to SQLite"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    #[arg(long, global = true, env = "RUSTY_RSS_FEED_URL")]
    pub feed_url: Option<String>,

    #[arg(
        long,
        global = true,
        short = 'd',
        env = "RUSTY_RSS_DB_PATH",
        default_value = "./rusty-rss.sqlite3"
    )]
    pub db_path: String,
}

#[derive(Subcommand)]
pub enum Command {
    /// Fetch the Atom feed and sync saved posts into the database
    Sync {
        /// Number of saved items to request per Reddit page
        #[arg(long, default_value_t = DEFAULT_SYNC_LIMIT)]
        limit: usize,

        /// Maximum number of Reddit pages to fetch
        #[arg(long, default_value_t = DEFAULT_MAX_PAGES)]
        max_pages: usize,
    },
    /// List saved posts
    List {
        /// Number of posts to show
        #[arg(short, long, default_value = "20")]
        limit: usize,

        /// Offset for pagination
        #[arg(short, long, default_value = "0")]
        offset: usize,
    },
    /// Show details of a specific post
    Show {
        /// Reddit fullname (e.g., t3_abc123)
        fullname: String,
    },
    /// Search saved posts by title and markdown content
    Search {
        /// Full-text search query
        query: String,

        /// Number of posts to show
        #[arg(short, long, default_value = "20")]
        limit: usize,

        /// Filter by subreddit name without r/
        #[arg(long)]
        subreddit: Option<String>,

        /// Filter by author name without u/
        #[arg(long)]
        author: Option<String>,

        /// Emit newline-delimited JSON records
        #[arg(long)]
        json: bool,
    },
    /// Enrich saved posts through the configured OpenAI-compatible LLM server
    Enrich {
        /// Maximum number of unenriched posts to process
        #[arg(short, long, default_value = "20")]
        limit: usize,

        /// Show how many posts would be enriched without calling the LLM or writing rows
        #[arg(long)]
        dry_run: bool,
    },
    /// List triage views from latest enrichment data
    Triage {
        /// View: all, unprocessed, high-value, should-test, should-build, reading-queue, reference-only, discard
        view: String,

        /// Number of items to show
        #[arg(short, long, default_value = "20")]
        limit: usize,

        /// Offset for pagination
        #[arg(short, long, default_value = "0")]
        offset: usize,

        /// Emit newline-delimited JSON records
        #[arg(long)]
        json: bool,
    },
    /// Export agent-ready records as JSONL, Markdown, or CSV
    Export {
        /// Output format
        #[arg(long, value_enum, default_value = "jsonl")]
        format: ExportFormat,

        /// Number of records to export
        #[arg(short, long, default_value = "100")]
        limit: usize,

        /// Offset for pagination
        #[arg(short, long, default_value = "0")]
        offset: usize,

        /// Filter by classification, e.g. article, tool, tutorial
        #[arg(long)]
        classification: Option<String>,

        /// Filter by recommended action, e.g. should_build, reading_queue
        #[arg(long)]
        action: Option<String>,

        /// Filter by minimum joy value
        #[arg(long)]
        min_joy: Option<f32>,

        /// Filter by minimum work value
        #[arg(long)]
        min_work: Option<f32>,
    },
    /// Capture outbound page metadata for saved posts
    Capture {
        /// Maximum number of uncaptured/failed outbound URLs to process
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },
    /// Tag saved posts by topic with the Gate 1 rule engine
    Tag {
        /// Tag only this topic (default: every topic in the rules file)
        #[arg(long)]
        topic: Option<String>,

        /// Path to the rules config
        #[arg(long, default_value = "./rules.toml")]
        rules: String,

        /// Maximum posts to process (default: the whole archive)
        #[arg(long)]
        limit: Option<usize>,

        /// Evaluate and report without writing any tags
        #[arg(long)]
        dry_run: bool,

        /// Emit newline-delimited JSON tag records
        #[arg(long)]
        json: bool,
    },
    /// Maintain all full-text search indexes: posts, captures, enrichment
    /// (rebuild / integrity check)
    #[command(hide = true)]
    Fts {
        #[command(subcommand)]
        command: FtsCommand,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ExportFormat {
    Jsonl,
    Markdown,
    Csv,
}

pub async fn run(cli: Cli) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init()
        .ok();

    match cli.command {
        Command::Sync { limit, max_pages } => {
            let config =
                Config::from_env_and_overrides(cli.feed_url, Some(cli.db_path), limit, max_pages)?;
            run_sync(config).await
        }
        Command::List { limit, offset } => run_list(PathBuf::from(cli.db_path), limit, offset),
        Command::Show { fullname } => run_show(PathBuf::from(cli.db_path), fullname),
        Command::Search {
            query,
            limit,
            subreddit,
            author,
            json,
        } => run_search(
            PathBuf::from(cli.db_path),
            &query,
            SearchFilters { subreddit, author },
            limit,
            json,
        ),
        Command::Enrich { limit, dry_run } => {
            run_enrich(PathBuf::from(cli.db_path), limit, dry_run).await
        }
        Command::Triage {
            view,
            limit,
            offset,
            json,
        } => run_triage(PathBuf::from(cli.db_path), &view, limit, offset, json),
        Command::Export {
            format,
            limit,
            offset,
            classification,
            action,
            min_joy,
            min_work,
        } => run_export(
            PathBuf::from(cli.db_path),
            format,
            limit,
            offset,
            classification,
            action,
            min_joy,
            min_work,
        ),
        Command::Capture { limit } => run_capture(PathBuf::from(cli.db_path), limit).await,
        Command::Tag {
            topic,
            rules,
            limit,
            dry_run,
            json,
        } => run_tag(
            PathBuf::from(cli.db_path),
            rules,
            topic,
            limit,
            dry_run,
            json,
        ),
        Command::Fts { command } => run_fts(PathBuf::from(cli.db_path), command),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::test_support::test_db_path;

    #[tokio::test]
    async fn run_list_command_does_not_require_feed_url() {
        let db_path = test_db_path();
        let cli = Cli {
            command: Command::List {
                limit: 10,
                offset: 0,
            },
            feed_url: None,
            db_path: db_path.to_string_lossy().to_string(),
        };

        run(cli).await.expect("list command should succeed");
    }
}
