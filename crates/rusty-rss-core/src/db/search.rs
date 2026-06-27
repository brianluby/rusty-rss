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

/// Unconditionally rebuild the `posts_fts` index from `saved_posts`.
///
/// `posts_fts` is an external-content FTS5 table over
/// `saved_posts(title, content_markdown)` with `content_rowid='rowid'`, so every
/// indexed document is keyed by its `saved_posts.rowid`. The `'rebuild'` command
/// discards the index and reconstructs it from that content table, repairing any
/// drift left by missed triggers or manual edits. Unlike `rebuild_stale_fts_index`
/// (which runs only inside `init_db` and skips when counts already match), this is
/// the maintenance entry point and always rebuilds.
pub fn rebuild_fts_index(conn: &Connection) -> Result<()> {
    conn.execute("INSERT INTO posts_fts(posts_fts) VALUES ('rebuild')", [])
        .context("failed to rebuild full-text search index")?;
    Ok(())
}

/// Verify the `posts_fts` index is internally consistent and matches its
/// `saved_posts` content table.
///
/// Runs the FTS5 `'integrity-check'` command. A structurally sound index returns
/// `Ok`; a corrupt or drifted one surfaces SQLite's `SQLITE_CORRUPT_VTAB`, which
/// is mapped to a clear error pointing at [`rebuild_fts_index`] for recovery. Any
/// other database failure is propagated with context rather than swallowed.
pub fn fts_integrity_check(conn: &Connection) -> Result<()> {
    match conn.execute(
        "INSERT INTO posts_fts(posts_fts) VALUES ('integrity-check')",
        [],
    ) {
        Ok(_) => Ok(()),
        Err(err) if is_fts_corruption(&err) => Err(anyhow!(
            "full-text search index failed its integrity check (corruption or drift \
             detected); rebuild it to recover: {err}"
        )),
        Err(err) => Err(err).context("failed to run full-text search integrity check"),
    }
}

/// Whether a rusqlite error is an FTS5 corruption signal (`SQLITE_CORRUPT` and its
/// `SQLITE_CORRUPT_VTAB` extension both map to the `DatabaseCorrupt` primary code).
fn is_fts_corruption(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(e, _) if e.code == rusqlite::ErrorCode::DatabaseCorrupt
    )
}

/// Prototype scaffolding (RSS-36): merged full-text search across all three
/// external-content FTS indexes.
///
/// UNIONs `posts_fts` ∪ `capture_fts` ∪ `enrichment_fts`, resolving every FTS
/// `rowid` back to its owning `saved_posts.reddit_fullname`
/// (`capture_fts.rowid` → `outbound_captures.rowid` → `reddit_fullname`;
/// `enrichment_fts.rowid` → `enrichment_runs.rowid` → `reddit_fullname`). A post
/// that matches in several sources — or several enrichment runs (1:many) — is
/// de-duplicated by `reddit_fullname`, keeping the single best (lowest) BM25
/// rank and that match's snippet.
///
/// This is **unwired prototype scaffolding**: the shipped CLI still calls
/// [`search_posts`], which is deliberately untouched. BM25 ranks are only
/// roughly comparable across tables (each index weights its own columns), so
/// ordering *between* sources is approximate. See
/// `docs/explanation/fts-multi-source.md` for the design and open tradeoffs.
pub fn search_multi_source(conn: &Connection, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let fts_query = normalize_fts_query(query)?;
    let mut stmt = conn
        .prepare(
            "WITH matches AS (
                 SELECT sp.reddit_fullname AS reddit_fullname,
                        bm25(posts_fts, 10.0, 1.0) AS rank,
                        snippet(posts_fts, -1, '<mark>', '</mark>', '...', 32) AS snippet
                 FROM posts_fts
                 JOIN saved_posts sp ON sp.rowid = posts_fts.rowid
                 WHERE posts_fts MATCH ?1
                 UNION ALL
                 SELECT oc.reddit_fullname AS reddit_fullname,
                        bm25(capture_fts) AS rank,
                        snippet(capture_fts, -1, '<mark>', '</mark>', '...', 32) AS snippet
                 FROM capture_fts
                 JOIN outbound_captures oc ON oc.rowid = capture_fts.rowid
                 WHERE capture_fts MATCH ?1
                 UNION ALL
                 SELECT er.reddit_fullname AS reddit_fullname,
                        bm25(enrichment_fts) AS rank,
                        snippet(enrichment_fts, -1, '<mark>', '</mark>', '...', 32) AS snippet
                 FROM enrichment_fts
                 JOIN enrichment_runs er ON er.rowid = enrichment_fts.rowid
                 WHERE enrichment_fts MATCH ?1
             ),
             best AS (
                 SELECT reddit_fullname, MIN(rank) AS rank
                 FROM matches
                 GROUP BY reddit_fullname
             )
             SELECT p.reddit_fullname,
                    p.title,
                    p.author,
                    p.subreddit,
                    p.permalink,
                    p.outbound_url,
                    (SELECT m.snippet FROM matches m
                     WHERE m.reddit_fullname = b.reddit_fullname
                     ORDER BY m.rank ASC LIMIT 1) AS snippet,
                    b.rank,
                    p.last_seen_at
             FROM best b
             JOIN saved_posts p ON p.reddit_fullname = b.reddit_fullname
             ORDER BY b.rank ASC, p.last_seen_at DESC
             LIMIT ?2",
        )
        .context("failed to prepare multi-source search query")?;

    let rows = stmt
        .query_map(params![fts_query, limit], |row| {
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
        })
        // The query syntax was already validated in normalize_fts_query, so an
        // error here is a database failure, not bad user input.
        .context("failed to execute multi-source search query")?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to collect multi-source search results")
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
    use crate::db::test_support::{test_db, test_output, test_post};
    use crate::db::{
        OutboundCaptureUpsert, record_enrichment_success, upsert_outbound_capture, upsert_post,
    };
    use crate::models::RecommendedAction;

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

    /// Battery of stable search terms exercised before and after a rebuild. Every
    /// term is present in the deterministic fixtures below regardless of the
    /// random token suffix, so the result set is comparable across rebuilds.
    const REBUILD_QUERIES: &[&str] = &[
        "seq", "title", "body", "alpha0", "alpha1", "alpha2", "alpha3", "alpha4", "alpha5",
        "beta0", "beta1", "beta2", "beta3", "beta4", "beta5",
    ];

    /// Snapshot the trigger-maintained search results across the query battery as
    /// comparable `(query, [fullname|rank])` rows. Rank is formatted to a fixed
    /// precision so the trigger-maintained and freshly rebuilt indexes compare by
    /// value even though `SearchHit` is not `PartialEq`.
    fn search_snapshot(conn: &Connection) -> Vec<(String, Vec<String>)> {
        REBUILD_QUERIES
            .iter()
            .map(|query| {
                let hits = search_posts(conn, query, &SearchFilters::default(), 100)
                    .expect("snapshot search should succeed");
                let rows = hits
                    .iter()
                    .map(|hit| format!("{}|{:.6}", hit.reddit_fullname, hit.rank))
                    .collect();
                ((*query).to_string(), rows)
            })
            .collect()
    }

    #[test]
    fn maintained_index_matches_rebuild_after_random_mutations() {
        let conn = test_db();

        // Tiny deterministic LCG (no `rand` dependency) so the upsert/delete
        // sequence is reproducible. Constants are the well-known PCG/MMIX values.
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u32
        };

        // Churn a small pool of rows so inserts, updates, and deletes interleave
        // and the FTS triggers (ai/ad/au) all fire repeatedly.
        const POOL: u32 = 6;
        for _ in 0..200 {
            let idx = next() % POOL;
            let fullname = format!("t3_seq_{idx}");
            if next() % 3 == 2 {
                conn.execute(
                    "DELETE FROM saved_posts WHERE reddit_fullname = ?",
                    params![fullname],
                )
                .expect("delete should succeed");
            } else {
                let token = next() % 1000;
                let mut post = test_post();
                post.reddit_fullname = fullname;
                post.reddit_id = format!("seq{idx}");
                post.title = format!("seq title alpha{idx} term{token}");
                post.content_markdown = Some(format!("seq body beta{idx} word{token}"));
                upsert_post(&conn, &post).expect("upsert should succeed");
            }
        }

        // The trigger-maintained index must already be internally consistent.
        fts_integrity_check(&conn).expect("maintained index should pass integrity check");

        // An unconditional rebuild reconstructs the index from `saved_posts`
        // alone; if the triggers kept it faithful, the result set is identical.
        let before = search_snapshot(&conn);
        rebuild_fts_index(&conn).expect("rebuild should succeed");
        let after = search_snapshot(&conn);

        assert_eq!(
            before, after,
            "trigger-maintained index must match a freshly rebuilt one"
        );
        fts_integrity_check(&conn).expect("rebuilt index should pass integrity check");
    }

    #[test]
    fn rebuild_restores_drifted_index() {
        let conn = test_db();
        let mut post = test_post();
        post.reddit_fullname = "t3_drift".to_string();
        post.reddit_id = "drift".to_string();
        post.title = "Zebra drift title".to_string();
        post.content_markdown = Some("zebra drift body".to_string());
        upsert_post(&conn, &post).expect("post should insert");

        let baseline = search_posts(&conn, "zebra", &SearchFilters::default(), 10)
            .expect("baseline search should succeed");
        assert_eq!(baseline.len(), 1, "precondition: post is indexed");

        // Deliberately desync the index: remove the post's FTS entry via the FTS5
        // special 'delete' syntax while leaving the `saved_posts` row in place, so
        // the index drifts (a document missing relative to the content table).
        // This depends on the FTS rowid mirroring `saved_posts.rowid`.
        conn.execute(
            "INSERT INTO posts_fts(posts_fts, rowid, title, content_markdown)
             SELECT 'delete', rowid, title, content_markdown
             FROM saved_posts WHERE reddit_fullname = 't3_drift'",
            [],
        )
        .expect("desync should succeed");

        let drifted = search_posts(&conn, "zebra", &SearchFilters::default(), 10)
            .expect("drifted search should succeed");
        assert!(
            drifted.is_empty(),
            "drift is observable: maintained index no longer matches the table"
        );

        // The unconditional rebuild restores maintained == rebuilt.
        rebuild_fts_index(&conn).expect("rebuild should succeed");
        let restored = search_posts(&conn, "zebra", &SearchFilters::default(), 10)
            .expect("restored search should succeed");
        assert_eq!(restored.len(), 1, "rebuild repairs the drifted index");
        assert_eq!(restored[0].reddit_fullname, "t3_drift");

        fts_integrity_check(&conn).expect("integrity check should pass after rebuild");
    }

    /// Build a successful capture upsert whose searchable text lives in
    /// `description`, so a term unique to the capture exercises `capture_fts`.
    fn capture_with(reddit_fullname: &str, description: &str) -> OutboundCaptureUpsert {
        OutboundCaptureUpsert {
            reddit_fullname: reddit_fullname.to_string(),
            original_url: "https://example.com/article".to_string(),
            final_url: None,
            canonical_url: None,
            title: Some("Captured heading".to_string()),
            description: Some(description.to_string()),
            site_name: Some("example.com".to_string()),
            preview_image_url: None,
            content_markdown: None,
            content_hash: None,
            status: "success".to_string(),
            http_status: Some(200),
            error: None,
        }
    }

    /// Prototype scaffolding (RSS-36): the merged multi-source search must resolve
    /// matches from each external-content aux FTS table back to the owning post
    /// and de-duplicate by `reddit_fullname`, keeping a single best-ranked hit.
    #[test]
    fn search_multi_source_resolves_and_dedupes_across_sources() {
        let conn = test_db();

        // Post A: a term unique to the post body (posts_fts path) plus a term
        // shared with its capture (cross-source dedup path).
        let mut post_a = test_post();
        post_a.reddit_fullname = "t3_postonly".to_string();
        post_a.reddit_id = "postonly".to_string();
        post_a.title = "Plain heading".to_string();
        post_a.content_markdown = Some("xenon body argon overlap".to_string());
        upsert_post(&conn, &post_a).expect("post a should insert");
        upsert_outbound_capture(&conn, &capture_with("t3_postonly", "argon mirrored"))
            .expect("capture a should insert");

        // Post B: the term lives only in the outbound capture (capture_fts path).
        let mut post_b = test_post();
        post_b.reddit_fullname = "t3_captureonly".to_string();
        post_b.reddit_id = "captureonly".to_string();
        post_b.title = "Unrelated heading".to_string();
        post_b.content_markdown = Some("nothing notable".to_string());
        upsert_post(&conn, &post_b).expect("post b should insert");
        upsert_outbound_capture(&conn, &capture_with("t3_captureonly", "krypton deep dive"))
            .expect("capture b should insert");

        // Post C: the term lives only in enrichment output (enrichment_fts path),
        // recorded twice to exercise the 1:many dedup-by-fullname.
        let mut post_c = test_post();
        post_c.reddit_fullname = "t3_enrichonly".to_string();
        post_c.reddit_id = "enrichonly".to_string();
        post_c.title = "Another heading".to_string();
        post_c.content_markdown = Some("plain text".to_string());
        upsert_post(&conn, &post_c).expect("post c should insert");
        for summary in ["radon first run", "radon second run"] {
            record_enrichment_success(
                &conn,
                "t3_enrichonly",
                "provider",
                "model",
                "prompt",
                "raw",
                &test_output(RecommendedAction::ReadingQueue, summary),
            )
            .expect("enrichment should insert");
        }

        // Each source resolves to exactly its owning post.
        let post_hits =
            search_multi_source(&conn, "xenon", 10).expect("post search should succeed");
        assert_eq!(post_hits.len(), 1);
        assert_eq!(post_hits[0].reddit_fullname, "t3_postonly");

        let capture_hits =
            search_multi_source(&conn, "krypton", 10).expect("capture search should succeed");
        assert_eq!(capture_hits.len(), 1);
        assert_eq!(capture_hits[0].reddit_fullname, "t3_captureonly");

        let enrich_hits =
            search_multi_source(&conn, "radon", 10).expect("enrichment search should succeed");
        assert_eq!(
            enrich_hits.len(),
            1,
            "1:many enrichment dedupes by fullname"
        );
        assert_eq!(enrich_hits[0].reddit_fullname, "t3_enrichonly");

        // A term present in both a post and its capture yields a single hit.
        let shared_hits =
            search_multi_source(&conn, "argon", 10).expect("shared search should succeed");
        assert_eq!(
            shared_hits.len(),
            1,
            "matches dedupe across sources by fullname"
        );
        assert_eq!(shared_hits[0].reddit_fullname, "t3_postonly");
    }

    #[test]
    fn search_multi_source_respects_zero_limit() {
        let conn = test_db();
        let post = test_post();
        upsert_post(&conn, &post).expect("post should insert");
        let hits = search_multi_source(&conn, "markdown", 0).expect("zero limit should succeed");
        assert!(hits.is_empty());
    }
}
