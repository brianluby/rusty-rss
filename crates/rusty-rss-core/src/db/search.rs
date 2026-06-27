//! Full-text search over `posts_fts` (bm25 ranking, snippets, query normalization).

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, params};

#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    pub subreddit: Option<String>,
    pub author: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    pub reddit_fullname: String,
    pub title: String,
    pub author: Option<String>,
    pub subreddit: Option<String>,
    pub permalink: String,
    pub outbound_url: Option<String>,
    pub snippet: String,
    pub rank: f64,
    pub last_seen_at: String,
}

pub fn search_posts(
    conn: &Connection,
    query: &str,
    filters: &SearchFilters,
    limit: usize,
) -> Result<Vec<SearchHit>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let fts_query = normalize_fts_query(query)?;
    let mut stmt = conn
        .prepare(
            "SELECT p.reddit_fullname,
                    p.title,
                    p.author,
                    p.subreddit,
                    p.permalink,
                    p.outbound_url,
                    snippet(posts_fts, -1, '<mark>', '</mark>', '...', 32) AS snippet,
                    bm25(posts_fts, 10.0, 1.0) AS rank,
                    p.last_seen_at
             FROM posts_fts
             JOIN saved_posts p ON p.rowid = posts_fts.rowid
             WHERE posts_fts MATCH ?
               AND (? IS NULL OR p.subreddit = ? COLLATE NOCASE)
               AND (? IS NULL OR p.author = ? COLLATE NOCASE)
             ORDER BY rank ASC, p.last_seen_at DESC
             LIMIT ?",
        )
        .context("failed to prepare search query")?;

    let rows = stmt
        .query_map(
            params![
                fts_query,
                filters.subreddit.as_deref(),
                filters.subreddit.as_deref(),
                filters.author.as_deref(),
                filters.author.as_deref(),
                limit,
            ],
            |row| {
                Ok(SearchHit {
                    reddit_fullname: row.get(0)?,
                    title: row.get(1)?,
                    author: row.get(2)?,
                    subreddit: row.get(3)?,
                    permalink: row.get(4)?,
                    outbound_url: row.get(5)?,
                    snippet: row.get(6)?,
                    rank: row.get(7)?,
                    last_seen_at: row.get(8)?,
                })
            },
        )
        // The query syntax was already validated in normalize_fts_query, so an
        // error here is a database failure, not bad user input.
        .context("failed to execute search query")?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect search results")
}

fn normalize_fts_query(query: &str) -> Result<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;

    for ch in query.chars() {
        if ch == '"' {
            if in_quote {
                push_quoted_search_part(&mut parts, &current);
                current.clear();
                in_quote = false;
            } else {
                push_unquoted_search_parts(&mut parts, &current);
                current.clear();
                in_quote = true;
            }
        } else if in_quote || !ch.is_whitespace() {
            current.push(ch);
        } else {
            push_unquoted_search_parts(&mut parts, &current);
            current.clear();
        }
    }

    if in_quote {
        return Err(anyhow!("invalid search query: unterminated quoted phrase"));
    }
    push_unquoted_search_parts(&mut parts, &current);

    if parts.is_empty() {
        return Err(anyhow!(
            "invalid search query: query must contain searchable text"
        ));
    }

    Ok(parts.join(" AND "))
}

fn push_unquoted_search_parts(parts: &mut Vec<String>, value: &str) {
    for term in value.split_whitespace() {
        push_quoted_search_part(parts, term);
    }
}

fn push_quoted_search_part(parts: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if !value.chars().any(|ch| ch.is_alphanumeric()) {
        return;
    }

    parts.push(format!("\"{}\"", value.replace('"', "\"\"")));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::{test_db, test_post};
    use crate::db::upsert_post;

    #[test]
    fn fts_triggers_keep_index_in_sync() {
        let conn = test_db();
        let mut post = test_post();
        post.title = "Alpha Title".to_string();
        post.content_markdown = Some("alpha body".to_string());
        upsert_post(&conn, &post).expect("post should insert");

        let hits = search_posts(&conn, "alpha", &SearchFilters::default(), 10)
            .expect("inserted post should search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].reddit_fullname, "t3_test123");

        post.title = "Beta Title".to_string();
        post.content_markdown = Some("beta body".to_string());
        upsert_post(&conn, &post).expect("post should update");

        let old_hits = search_posts(&conn, "alpha", &SearchFilters::default(), 10)
            .expect("old terms should search");
        assert!(old_hits.is_empty());
        let new_hits = search_posts(&conn, "beta", &SearchFilters::default(), 10)
            .expect("new terms should search");
        assert_eq!(new_hits.len(), 1);

        conn.execute(
            "DELETE FROM saved_posts WHERE reddit_fullname = ?",
            params![post.reddit_fullname],
        )
        .expect("post should delete");
        let deleted_hits = search_posts(&conn, "beta", &SearchFilters::default(), 10)
            .expect("deleted terms should search");
        assert!(deleted_hits.is_empty());
    }

    #[test]
    fn search_ranking_prefers_title_matches() {
        let conn = test_db();
        let mut title_match = test_post();
        title_match.reddit_fullname = "t3_title".to_string();
        title_match.reddit_id = "title".to_string();
        title_match.title = "Needle in title".to_string();
        title_match.content_markdown = Some("unrelated content".to_string());
        upsert_post(&conn, &title_match).expect("title match should insert");

        let mut body_match = test_post();
        body_match.reddit_fullname = "t3_body".to_string();
        body_match.reddit_id = "body".to_string();
        body_match.title = "Other post".to_string();
        body_match.content_markdown = Some("needle in body".to_string());
        upsert_post(&conn, &body_match).expect("body match should insert");

        let hits = search_posts(&conn, "needle", &SearchFilters::default(), 10)
            .expect("search should succeed");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].reddit_fullname, "t3_title");
        assert!(hits[0].snippet.contains("<mark>"));
    }

    #[test]
    fn search_filters_compose() {
        let conn = test_db();
        for (fullname, subreddit, author) in [
            ("t3_rust_alice", "rust", "alice"),
            ("t3_rust_bob", "rust", "bob"),
            ("t3_go_alice", "golang", "alice"),
        ] {
            let mut post = test_post();
            post.reddit_fullname = fullname.to_string();
            post.reddit_id = fullname.trim_start_matches("t3_").to_string();
            post.title = "Composed filter target".to_string();
            post.subreddit = Some(subreddit.to_string());
            post.author = Some(author.to_string());
            upsert_post(&conn, &post).expect("post should insert");
        }

        let hits = search_posts(
            &conn,
            "target",
            &SearchFilters {
                subreddit: Some("rust".to_string()),
                author: Some("alice".to_string()),
            },
            10,
        )
        .expect("filtered search should succeed");

        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].reddit_fullname, "t3_rust_alice");
    }

    #[test]
    fn search_filters_are_case_insensitive() {
        let conn = test_db();
        let mut post = test_post();
        post.title = "Case target".to_string();
        post.subreddit = Some("Rust".to_string());
        post.author = Some("Alice".to_string());
        upsert_post(&conn, &post).expect("post should insert");

        let hits = search_posts(
            &conn,
            "target",
            &SearchFilters {
                subreddit: Some("rust".to_string()),
                author: Some("alice".to_string()),
            },
            10,
        )
        .expect("filtered search should succeed");

        assert_eq!(hits.len(), 1, "filters should match regardless of case");
    }

    #[test]
    fn malformed_search_query_fails_cleanly() {
        let conn = test_db();
        let post = test_post();
        upsert_post(&conn, &post).expect("post should insert");

        let err = search_posts(&conn, "\"unterminated", &SearchFilters::default(), 10)
            .expect_err("malformed query should fail");
        assert!(err.to_string().contains("invalid search query"));
    }
}
