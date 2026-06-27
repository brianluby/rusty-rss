//! Read-only query commands: list, show, and search saved posts.

use anyhow::Result;
use rusty_rss_core::db::{self, SearchFilters};
use std::path::PathBuf;

pub(super) fn run_list(db_path: PathBuf, limit: usize, offset: usize) -> Result<()> {
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

pub(super) fn run_show(db_path: PathBuf, fullname: String) -> Result<()> {
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

pub(super) fn run_search(
    db_path: PathBuf,
    query: &str,
    filters: SearchFilters,
    limit: usize,
    json: bool,
) -> Result<()> {
    let conn = db::init_db(&db_path)?;
    // The unified multi-source search; `filters.source` (default `posts`) decides
    // which indexes are consulted, so the post-only default is a zero-regression.
    let hits = db::search(&conn, query, &filters, limit)?;

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
            "  {}. [{}] {} by u/{} in r/{} (matched: {})",
            index + 1,
            hit.reddit_fullname,
            hit.title,
            author,
            sub,
            hit.source
        );
        println!("     {}", hit.permalink);
        println!("     {}", hit.snippet.trim());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::test_support::{insert_post, test_db_path};

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
                ..SearchFilters::default()
            },
            10,
            true,
        )
        .expect("search should succeed");
    }
}
