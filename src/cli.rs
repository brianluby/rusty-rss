use crate::config::Config;
use crate::db;
use crate::sync;
use anyhow::Result;
use clap::{Parser, Subcommand};
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
    Sync,
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
}

pub async fn run(cli: Cli) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    match cli.command {
        Command::Sync => {
            let config = Config::from_env_and_overrides(cli.feed_url, Some(cli.db_path))?;
            run_sync(config).await
        }
        Command::List { limit, offset } => run_list(PathBuf::from(cli.db_path), limit, offset),
        Command::Show { fullname } => run_show(PathBuf::from(cli.db_path), fullname),
    }
}

async fn run_sync(config: Config) -> Result<()> {
    let result = sync::run_sync(&config).await?;

    println!(
        "Sync complete: {} fetched, {} inserted, {} updated, {} unchanged, {} errors",
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
            if let Some(html) = &post.content_html {
                println!("\nContent:\n{}", html.trim());
            }
        }
        None => {
            eprintln!("Post not found: {}", fullname);
            std::process::exit(1);
        }
    }

    Ok(())
}
