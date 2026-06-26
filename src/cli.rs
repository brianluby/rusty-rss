use anyhow::{Result, anyhow};
use clap::{Parser, Subcommand};
use rusty_rss_core::config::{Config, DEFAULT_MAX_PAGES, DEFAULT_SYNC_LIMIT};
use rusty_rss_core::db::TriageView;
use rusty_rss_core::enrich::{self, EnrichOptions};
use rusty_rss_core::llm::{OpenAiConfig, OpenAiProvider};
use rusty_rss_core::{db, sync};
use std::path::PathBuf;

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
        Command::Enrich { limit, dry_run } => {
            run_enrich(PathBuf::from(cli.db_path), limit, dry_run).await
        }
        Command::Triage {
            view,
            limit,
            offset,
            json,
        } => run_triage(PathBuf::from(cli.db_path), &view, limit, offset, json),
    }
}

async fn run_sync(config: Config) -> Result<()> {
    let result = sync::run_sync(&config).await?;

    println!(
        "Sync complete: {} pages, {} fetched, {} inserted, {} updated, {} unchanged, {} errors",
        result.page_count,
        result.fetched_count,
        result.inserted_count,
        result.updated_count,
        result.unchanged_count,
        result.parse_errors.len()
    );

    for err in &result.parse_errors {
        eprintln!("  ERROR: {}", err);
    }

    Ok(())
}

fn run_list(db_path: PathBuf, limit: usize, offset: usize) -> Result<()> {
    let conn = db::init_db(&db_path)?;
    let posts = db::list_posts(&conn, limit, offset)?;

    if posts.is_empty() {
        println!("No saved posts found.");
        return Ok(());
    }

    let total = db::count_posts(&conn)?;
    println!("Showing {} of {} posts:\n", posts.len(), total);

    for (i, post) in posts.iter().enumerate() {
        let sub = post.subreddit.as_deref().unwrap_or("(no subreddit)");
        let author = post.author.as_deref().unwrap_or("(no author)");

        println!(
            "  {}. [{}] {} by u/{} in r/{}",
            i + 1 + offset,
            post.fullname,
            post.title,
            author,
            sub
        );
        println!("     {}", post.permalink);
        if let Some(dt) = &post.published_at {
            println!("     Published: {}", dt);
        }
        println!("     Last seen: {}", post.last_seen_at);
    }

    Ok(())
}

fn run_show(db_path: PathBuf, fullname: String) -> Result<()> {
    let conn = db::init_db(&db_path)?;
    let post = db::get_post(&conn, &fullname)?;

    match post {
        Some(post) => {
            println!("Title:    {}", post.title);
            println!("Fullname: {}", post.reddit_fullname);
            println!("Permalink: {}", post.permalink);
            if let Some(author) = &post.author {
                println!("Author:   u/{}", author);
            }
            if let Some(sub) = &post.subreddit {
                println!("Sub:      r/{}", sub);
            }
            if let Some(url) = &post.outbound_url {
                println!("URL:      {}", url);
            }
            if let Some(ts) = &post.published_at {
                println!("Published: {}", ts.to_rfc3339());
            }
            if let Some(markdown) = &post.content_markdown {
                println!("\nContent:\n{}", markdown.trim());
            }
        }
        None => {
            eprintln!("Post not found: {}", fullname);
            std::process::exit(1);
        }
    }

    Ok(())
}

async fn run_enrich(db_path: PathBuf, limit: usize, dry_run: bool) -> Result<()> {
    let conn = db::init_db(&db_path)?;

    if dry_run {
        let selected_count = db::list_enrichment_candidates(&conn, limit)?.len();
        println!("Would enrich {} posts", selected_count);
        return Ok(());
    }

    let provider = OpenAiProvider::new(OpenAiConfig::from_env()?);
    provider.preflight().await?;

    let summary = enrich::run_enrichment_batch(
        &conn,
        &provider,
        "openai-compatible",
        provider.model(),
        EnrichOptions::new(limit, dry_run),
    )
    .await?;

    println!(
        "Enrichment complete: {} selected, {} enriched, {} failed",
        summary.selected_count, summary.enriched_count, summary.failed_count
    );
    for failure in summary.failures {
        eprintln!("  ERROR [{}]: {}", failure.reddit_fullname, failure.error);
    }

    Ok(())
}

fn run_triage(db_path: PathBuf, view: &str, limit: usize, offset: usize, json: bool) -> Result<()> {
    let view = TriageView::parse(view).ok_or_else(|| anyhow!("unknown triage view: {view}"))?;
    let conn = db::init_db(&db_path)?;
    let items = db::list_triage_items(&conn, view, limit, offset)?;

    if json {
        for item in items {
            println!("{}", serde_json::to_string(&item)?);
        }
        return Ok(());
    }

    if items.is_empty() {
        println!("No matching items found.");
        return Ok(());
    }

    for (index, item) in items.iter().enumerate() {
        let sub = item.subreddit.as_deref().unwrap_or("(no subreddit)");
        let action = item
            .enrichment
            .as_ref()
            .and_then(|record| record.output.as_ref())
            .map(|output| output.recommended_action.as_str())
            .unwrap_or("unprocessed");
        println!(
            "  {}. [{}] {} in r/{} ({})",
            index + 1 + offset,
            item.reddit_fullname,
            item.title,
            sub,
            action
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_rss_core::db;
    use rusty_rss_core::models::SavedPost;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_db_path() -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "rusty_rss_cli_test_{}_{}.db",
            std::process::id(),
            id
        ))
    }

    fn insert_post(db_path: &std::path::Path) {
        let conn = db::init_db(db_path).expect("db should initialize");
        let mut post = SavedPost::new(
            "t3_cli123".to_string(),
            "CLI Test Post".to_string(),
            "https://www.reddit.com/r/rust/comments/cli123/test/".to_string(),
            "atom".to_string(),
        );
        post.author = Some("cli_user".to_string());
        post.subreddit = Some("rust".to_string());
        post.content_markdown = Some("content".to_string());
        db::upsert_post(&conn, &post).expect("post should insert");
    }

    fn insert_enriched_post(db_path: &std::path::Path) {
        insert_post(db_path);
        let conn = db::init_db(db_path).expect("db should initialize");
        db::record_enrichment_success(
            &conn,
            "t3_cli123",
            "test",
            "test-model",
            "test-prompt",
            "raw",
            &rusty_rss_core::models::EnrichmentOutput {
                classification: rusty_rss_core::models::Classification::Reference,
                tags: vec!["rust".to_string()],
                summary: "Useful".to_string(),
                joy_value: 0.2,
                work_value: 0.8,
                recommended_action: rusty_rss_core::models::RecommendedAction::ReferenceOnly,
                rationale: "Useful later".to_string(),
                confidence: 0.9,
            },
        )
        .expect("enrichment should insert");
    }

    #[test]
    fn list_empty_database_returns_ok_without_feed_url() {
        let db_path = test_db_path();

        run_list(db_path, 20, 0).expect("list should not require feed URL");
    }

    #[test]
    fn list_populated_database_returns_ok() {
        let db_path = test_db_path();
        insert_post(&db_path);

        run_list(db_path, 20, 0).expect("list should succeed");
    }

    #[test]
    fn show_existing_post_returns_ok() {
        let db_path = test_db_path();
        insert_post(&db_path);

        run_show(db_path, "t3_cli123".to_string()).expect("show should succeed");
    }

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

    #[test]
    fn triage_json_returns_enriched_rows() {
        let db_path = test_db_path();
        insert_enriched_post(&db_path);

        run_triage(db_path, "reference-only", 10, 0, true).expect("triage should succeed");
    }
}
