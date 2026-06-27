use anyhow::{Result, anyhow};
use clap::{Parser, Subcommand, ValueEnum};
use rusty_rss_core::capture::{self, CaptureOptions};
use rusty_rss_core::config::{Config, DEFAULT_MAX_PAGES, DEFAULT_SYNC_LIMIT};
use rusty_rss_core::db::{ExportFilters, SearchFilters, TriageView};
use rusty_rss_core::enrich::{self, EnrichOptions};
use rusty_rss_core::llm::{OpenAiConfig, OpenAiProvider};
use rusty_rss_core::models::{Classification, ExportRecord, RecommendedAction};
use rusty_rss_core::rules::RuleSet;
use rusty_rss_core::tag::{self, TagOptions};
use rusty_rss_core::{db, sync};
use std::path::PathBuf;
use std::str::FromStr;

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

fn run_search(
    db_path: PathBuf,
    query: &str,
    filters: SearchFilters,
    limit: usize,
    json: bool,
) -> Result<()> {
    let conn = db::init_db(&db_path)?;
    let hits = db::search_posts(&conn, query, &filters, limit)?;

    if json {
        for hit in hits {
            println!("{}", serde_json::to_string(&hit)?);
        }
        return Ok(());
    }

    if hits.is_empty() {
        println!("No matching posts found.");
        return Ok(());
    }

    for (index, hit) in hits.iter().enumerate() {
        let sub = hit.subreddit.as_deref().unwrap_or("(no subreddit)");
        let author = hit.author.as_deref().unwrap_or("(no author)");
        println!(
            "  {}. [{}] {} by u/{} in r/{}",
            index + 1,
            hit.reddit_fullname,
            hit.title,
            author,
            sub
        );
        println!("     {}", hit.permalink);
        println!("     {}", hit.snippet.trim());
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

#[expect(
    clippy::too_many_arguments,
    reason = "CLI flags map directly to export filters"
)]
fn run_export(
    db_path: PathBuf,
    format: ExportFormat,
    limit: usize,
    offset: usize,
    classification: Option<String>,
    action: Option<String>,
    min_joy: Option<f32>,
    min_work: Option<f32>,
) -> Result<()> {
    let conn = db::init_db(&db_path)?;
    let filters = ExportFilters {
        classification: classification
            .as_deref()
            .map(Classification::from_str)
            .transpose()
            .map_err(|err| anyhow!(err))?,
        recommended_action: action
            .as_deref()
            .map(RecommendedAction::from_str)
            .transpose()
            .map_err(|err| anyhow!(err))?,
        min_joy_value: min_joy,
        min_work_value: min_work,
    };
    let records = db::list_export_records(&conn, &filters, limit, offset)?;

    match format {
        ExportFormat::Jsonl => {
            for record in records {
                println!("{}", serde_json::to_string(&record)?);
            }
        }
        ExportFormat::Markdown => print_markdown_export(&records),
        ExportFormat::Csv => print_csv_export(&records),
    }

    Ok(())
}

async fn run_capture(db_path: PathBuf, limit: usize) -> Result<()> {
    let conn = db::init_db(&db_path)?;
    let summary = capture::capture_outbound_metadata(&conn, CaptureOptions::new(limit)).await?;

    println!(
        "Capture complete: {} selected, {} captured, {} failed",
        summary.selected_count, summary.captured_count, summary.failed_count
    );

    Ok(())
}

fn run_tag(
    db_path: PathBuf,
    rules_path: String,
    topic: Option<String>,
    limit: Option<usize>,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    let ruleset = RuleSet::load(std::path::Path::new(&rules_path))?;
    let conn = db::init_db(&db_path)?;
    let summary = tag::run_tagging_batch(
        &conn,
        &ruleset,
        &TagOptions {
            topic,
            limit,
            dry_run,
        },
    )?;

    if json {
        for tag in &summary.tags {
            println!("{}", serde_json::to_string(tag)?);
        }
        return Ok(());
    }

    println!(
        "{}: {} posts, {} topics, {} rows {}, {} passed, {} vetoed",
        if dry_run {
            "Tag dry-run"
        } else {
            "Tagging complete"
        },
        summary.selected_posts,
        summary.topics_evaluated,
        summary.rows_written,
        if dry_run {
            "would be written"
        } else {
            "written"
        },
        summary.passed_count,
        summary.vetoed_count,
    );

    Ok(())
}

fn print_markdown_export(records: &[ExportRecord]) {
    for record in records {
        println!("## {}", record.saved_post.title);
        println!();
        println!("- ID: `{}`", record.saved_post.reddit_fullname);
        println!("- Permalink: {}", record.saved_post.permalink);
        if let Some(url) = &record.saved_post.outbound_url {
            println!("- Outbound URL: {url}");
        }
        if let Some(enrichment) = &record.latest_enrichment
            && let Some(output) = &enrichment.output
        {
            println!("- Classification: {}", output.classification.as_str());
            println!(
                "- Recommended action: {}",
                output.recommended_action.as_str()
            );
            println!("- Summary: {}", output.summary);
        }
        if let Some(capture) = &record.outbound_capture {
            if let Some(title) = &capture.title {
                println!("- Captured title: {title}");
            }
            if let Some(canonical) = &capture.canonical_url {
                println!("- Canonical URL: {canonical}");
            }
            if let Some(hash) = &capture.content_hash {
                println!("- Captured content hash: {hash}");
            }
        }
        if let Some(markdown) = &record.saved_post.content_markdown {
            println!();
            println!("{}", markdown.trim());
        }
        println!();
    }
}

fn print_csv_export(records: &[ExportRecord]) {
    println!(
        "schema_version,reddit_fullname,title,subreddit,author,permalink,outbound_url,classification,recommended_action,joy_value,work_value,capture_status,captured_title,canonical_url,content_hash"
    );
    for record in records {
        let output = record
            .latest_enrichment
            .as_ref()
            .and_then(|enrichment| enrichment.output.as_ref());
        let capture = record.outbound_capture.as_ref();
        println!(
            "{}",
            [
                record.schema_version.as_str(),
                record.saved_post.reddit_fullname.as_str(),
                record.saved_post.title.as_str(),
                record.saved_post.subreddit.as_deref().unwrap_or(""),
                record.saved_post.author.as_deref().unwrap_or(""),
                record.saved_post.permalink.as_str(),
                record.saved_post.outbound_url.as_deref().unwrap_or(""),
                output
                    .map(|output| output.classification.as_str())
                    .unwrap_or(""),
                output
                    .map(|output| output.recommended_action.as_str())
                    .unwrap_or(""),
                &output
                    .map(|output| output.joy_value.to_string())
                    .unwrap_or_default(),
                &output
                    .map(|output| output.work_value.to_string())
                    .unwrap_or_default(),
                capture.map(|capture| capture.status.as_str()).unwrap_or(""),
                capture
                    .and_then(|capture| capture.title.as_deref())
                    .unwrap_or(""),
                capture
                    .and_then(|capture| capture.canonical_url.as_deref())
                    .unwrap_or(""),
                capture
                    .and_then(|capture| capture.content_hash.as_deref())
                    .unwrap_or(""),
            ]
            .map(csv_escape)
            .join(",")
        );
    }
}

fn csv_escape(value: &str) -> String {
    // Neutralize spreadsheet formula injection: a cell beginning with one of
    // these is evaluated as a formula by Excel/Sheets, and titles/subreddits
    // come from untrusted Reddit content. Prefixing a single quote defuses it.
    let neutralized;
    let value = if matches!(
        value.as_bytes().first(),
        Some(b'=' | b'+' | b'-' | b'@' | b'\t' | b'\r' | b'\n')
    ) {
        neutralized = format!("'{value}");
        neutralized.as_str()
    } else {
        value
    };

    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
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

    #[test]
    fn search_existing_post_returns_ok() {
        let db_path = test_db_path();
        insert_post(&db_path);

        run_search(
            db_path,
            "content",
            SearchFilters {
                subreddit: Some("rust".to_string()),
                author: Some("cli_user".to_string()),
            },
            10,
            true,
        )
        .expect("search should succeed");
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

    #[test]
    fn csv_escape_neutralizes_formula_injection() {
        // Formula-leading values are prefixed with a single quote.
        assert_eq!(csv_escape("=SUM(A1:A2)"), "'=SUM(A1:A2)");
        assert_eq!(csv_escape("@cmd"), "'@cmd");
        // A leading newline must also be neutralized (some parsers trim it).
        assert_eq!(csv_escape("\n=evil"), "\"'\n=evil\"");
        // Neutralized values that also need quoting still get quoted.
        assert_eq!(csv_escape("=1,2"), "\"'=1,2\"");
        // Ordinary values are untouched.
        assert_eq!(csv_escape("normal"), "normal");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
    }
}
