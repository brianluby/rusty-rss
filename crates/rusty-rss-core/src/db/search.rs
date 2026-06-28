//! Full-text search across posts, captures, and enrichment (bm25 ranking,
//! snippets, query normalization, cross-source merge).

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, named_params};

/// Which full-text indexes a search query consults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchSource {
    /// Saved-post title and body only (the historical default; zero regression).
    #[default]
    Posts,
    /// Captured outbound-page text only.
    Capture,
    /// Enrichment output (latest successful run) only.
    Enrichment,
    /// All three sources merged.
    All,
}

impl SearchSource {
    /// Parse a `--source` flag value. Returns `None` for unrecognized input so
    /// the caller can surface a clear error.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "posts" | "post" => Some(Self::Posts),
            "capture" | "captures" => Some(Self::Capture),
            "enrichment" | "enrich" => Some(Self::Enrichment),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    fn includes_posts(self) -> bool {
        matches!(self, Self::Posts | Self::All)
    }

    fn includes_capture(self) -> bool {
        matches!(self, Self::Capture | Self::All)
    }

    fn includes_enrichment(self) -> bool {
        matches!(self, Self::Enrichment | Self::All)
    }
}

#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    pub subreddit: Option<String>,
    pub author: Option<String>,
    /// Which index(es) to search. Defaults to [`SearchSource::Posts`].
    pub source: SearchSource,
    /// Keep only posts that have a successful outbound capture.
    pub has_capture: bool,
    /// Keep only posts that have a successful enrichment run.
    pub has_enrichment: bool,
    /// Keep only posts whose latest enrichment carries this classification.
    pub classification: Option<String>,
    /// Keep only posts whose latest enrichment recommends this action.
    pub action: Option<String>,
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
    /// Which source produced the best-ranked match: `posts`, `capture`, or
    /// `enrichment`.
    pub source: String,
    pub last_seen_at: String,
}

/// Post-only search: the historical entry point, preserved for the MCP server
/// and any caller that wants the zero-regression behavior regardless of the
/// `source` field on `filters`. Delegates to [`search`] with the source pinned
/// to [`SearchSource::Posts`].
pub fn search_posts(
    conn: &Connection,
    query: &str,
    filters: &SearchFilters,
    limit: usize,
) -> Result<Vec<SearchHit>> {
    let filters = SearchFilters {
        source: SearchSource::Posts,
        ..filters.clone()
    };
    search(conn, query, &filters, limit)
}

/// Every external-content FTS5 index in the schema, each keyed to its content
/// table by `rowid`: `posts_fts` over `saved_posts`, `capture_fts` over
/// `outbound_captures`, and `enrichment_fts` over `enrichment_runs`. The
/// maintenance helpers below operate on all of them so a `rebuild`/`check` keeps
/// the whole search subsystem (incl. the multi-source aux indexes) consistent,
/// not just post search. Names are compile-time constants — never user input —
/// so interpolating them into the FTS command statements is safe.
const FTS_INDEXES: [&str; 3] = ["posts_fts", "capture_fts", "enrichment_fts"];

/// Unconditionally rebuild every FTS index from its content table.
///
/// Each index is external-content (`content=...`, `content_rowid='rowid'`), so
/// the `'rebuild'` command discards the index and reconstructs it from the
/// content table, repairing any drift left by missed triggers or manual edits.
/// Unlike `rebuild_stale_fts_index` (which runs only inside `init_db` and skips
/// when counts already match), this is the maintenance entry point and always
/// rebuilds all of [`FTS_INDEXES`].
pub fn rebuild_fts_index(conn: &Connection) -> Result<()> {
    for index in FTS_INDEXES {
        conn.execute(
            &format!("INSERT INTO {index}({index}) VALUES ('rebuild')"),
            [],
        )
        .with_context(|| format!("failed to rebuild full-text search index {index}"))?;
    }
    Ok(())
}

/// Verify every FTS index is internally consistent and matches its content table.
///
/// Runs the FTS5 `'integrity-check'` command in its `rank = 1` form
/// (`VALUES ('integrity-check', 1)`) on each of [`FTS_INDEXES`]. The default
/// (rank 0) form only checks the index's internal structure; the `rank = 1` form
/// additionally verifies the index matches its external-content table, so it
/// catches drift between, e.g., `saved_posts` and `posts_fts` that the plain form
/// misses. A sound index returns `Ok`; a corrupt or drifted one surfaces SQLite's
/// `SQLITE_CORRUPT_VTAB`, mapped to a clear error pointing at
/// [`rebuild_fts_index`] for recovery. Any other database failure propagates with
/// context rather than being swallowed.
pub fn fts_integrity_check(conn: &Connection) -> Result<()> {
    for index in FTS_INDEXES {
        match conn.execute(
            &format!("INSERT INTO {index}({index}, rank) VALUES ('integrity-check', 1)"),
            [],
        ) {
            Ok(_) => {}
            Err(err) if is_fts_corruption(&err) => {
                return Err(anyhow!(
                    "full-text search index {index} failed its integrity check (corruption or \
                     drift detected); rebuild it to recover: {err}"
                ));
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("failed to run full-text search integrity check on {index}")
                });
            }
        }
    }
    Ok(())
}

/// Whether a rusqlite error is an FTS5 corruption signal (`SQLITE_CORRUPT` and its
/// `SQLITE_CORRUPT_VTAB` extension both map to the `DatabaseCorrupt` primary code).
fn is_fts_corruption(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(e, _) if e.code == rusqlite::ErrorCode::DatabaseCorrupt
    )
}

// Per-source BM25 penalty added to each match's rank (lower rank sorts first,
// and SQLite BM25 returns negative scores, so a positive penalty biases toward
// the more authoritative source). Posts (the user's own title/body) are the
// primary signal and get no penalty; captured page text is secondary; model-
// generated enrichment is the least authoritative. The penalties are small
// relative to typical BM25 magnitudes, so a markedly stronger lower-tier match
// can still outrank a weak post match — see docs/explanation/fts-multi-source.md.
const POSTS_ARM: &str = "
    SELECT sp.reddit_fullname AS reddit_fullname,
           bm25(posts_fts, 10.0, 1.0) AS rank,
           snippet(posts_fts, -1, '<mark>', '</mark>', '...', 32) AS snippet,
           'posts' AS source
    FROM posts_fts
    JOIN saved_posts sp ON sp.rowid = posts_fts.rowid
    WHERE posts_fts MATCH :q";

const CAPTURE_ARM: &str = "
    SELECT oc.reddit_fullname AS reddit_fullname,
           bm25(capture_fts) + 1.0 AS rank,
           snippet(capture_fts, -1, '<mark>', '</mark>', '...', 32) AS snippet,
           'capture' AS source
    FROM capture_fts
    JOIN outbound_captures oc ON oc.rowid = capture_fts.rowid
    WHERE capture_fts MATCH :q";

// Enrichment is 1:many per post. Index only the latest *successful* run so a
// stale older run never resurfaces in search; this also collapses the 1:many
// fan-out before the cross-source dedup. Chosen over a partial index / new
// migration: external-content FTS5 cannot express "newest row per group", and a
// query-time `MAX(id)` filter keeps the index plain and the schema unchanged.
const ENRICHMENT_ARM: &str = "
    SELECT er.reddit_fullname AS reddit_fullname,
           bm25(enrichment_fts) + 2.0 AS rank,
           snippet(enrichment_fts, -1, '<mark>', '</mark>', '...', 32) AS snippet,
           'enrichment' AS source
    FROM enrichment_fts
    JOIN enrichment_runs er ON er.rowid = enrichment_fts.rowid
    WHERE enrichment_fts MATCH :q
      AND er.status = 'success'
      AND er.id = (
          SELECT MAX(e2.id) FROM enrichment_runs e2
          WHERE e2.reddit_fullname = er.reddit_fullname AND e2.status = 'success'
      )";

/// Full-text search across the sources selected by `filters.source`.
///
/// Merges `posts_fts` / `capture_fts` / `enrichment_fts` with `UNION ALL`,
/// resolving every FTS `rowid` back to its owning `saved_posts.reddit_fullname`,
/// then de-duplicates by `reddit_fullname` keeping the single best (lowest,
/// source-penalized) BM25 rank, its snippet, and which source matched. The
/// subreddit/author and capture/enrichment/classification/action filters are
/// applied to the resolved post (the latter four against the post's latest
/// successful enrichment run). `search_posts` is the post-only entry point.
pub fn search(
    conn: &Connection,
    query: &str,
    filters: &SearchFilters,
    limit: usize,
) -> Result<Vec<SearchHit>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let fts_query = normalize_fts_query(query)?;

    let mut arms: Vec<&str> = Vec::new();
    if filters.source.includes_posts() {
        arms.push(POSTS_ARM);
    }
    if filters.source.includes_capture() {
        arms.push(CAPTURE_ARM);
    }
    if filters.source.includes_enrichment() {
        arms.push(ENRICHMENT_ARM);
    }
    let matches_cte = arms.join("\n    UNION ALL\n");

    // Boolean filters are appended as static clauses (no params, no injection
    // surface); the string filters bind named params so NULL disables them.
    let mut extra = String::new();
    if filters.has_capture {
        extra.push_str(
            " AND EXISTS (SELECT 1 FROM outbound_captures oc \
               WHERE oc.reddit_fullname = p.reddit_fullname AND oc.status = 'success')",
        );
    }
    if filters.has_enrichment {
        extra.push_str(" AND le.id IS NOT NULL");
    }

    // MATERIALIZED is load-bearing: the FTS5 `bm25()`/`snippet()` aux functions
    // are only valid in a SELECT that directly references the FTS table with a
    // MATCH. Forcing the CTE to materialize evaluates every arm exactly once in
    // that valid context; the `best`/snippet/source references downstream then
    // read plain columns. Without it SQLite may inline the CTE into the
    // aggregating/correlated parents, where bm25 fails with "unable to use
    // function bm25 in the requested context".
    let sql = format!(
        "WITH matches AS MATERIALIZED (
            {matches_cte}
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
                (SELECT m.source FROM matches m
                 WHERE m.reddit_fullname = b.reddit_fullname
                 ORDER BY m.rank ASC LIMIT 1) AS source,
                p.last_seen_at
         FROM best b
         JOIN saved_posts p ON p.reddit_fullname = b.reddit_fullname
         LEFT JOIN enrichment_runs le ON le.id = (
             SELECT MAX(e.id) FROM enrichment_runs e
             WHERE e.reddit_fullname = p.reddit_fullname AND e.status = 'success'
         )
         WHERE 1 = 1
           AND (:subreddit IS NULL OR p.subreddit = :subreddit COLLATE NOCASE)
           AND (:author IS NULL OR p.author = :author COLLATE NOCASE)
           AND (:classification IS NULL OR le.classification = :classification COLLATE NOCASE)
           AND (:action IS NULL OR le.recommended_action = :action COLLATE NOCASE)
           {extra}
         ORDER BY b.rank ASC, p.last_seen_at DESC
         LIMIT :limit"
    );

    let mut stmt = conn
        .prepare(&sql)
        .context("failed to prepare search query")?;

    let rows = stmt
        .query_map(
            named_params! {
                ":q": fts_query,
                ":subreddit": filters.subreddit.as_deref(),
                ":author": filters.author.as_deref(),
                ":classification": filters.classification.as_deref(),
                ":action": filters.action.as_deref(),
                ":limit": limit,
            },
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
                    source: row.get(8)?,
                    last_seen_at: row.get(9)?,
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
    use crate::db::test_support::{test_db, test_output, test_post};
    use crate::db::{
        OutboundCaptureUpsert, record_enrichment_success, upsert_outbound_capture, upsert_post,
    };
    use crate::models::RecommendedAction;
    use rusqlite::params;

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
                ..SearchFilters::default()
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
                ..SearchFilters::default()
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

        // The rank=1 integrity-check must catch the drift (the plain rank=0 form
        // does not, which is why fts_integrity_check uses 'integrity-check', 1).
        let err =
            fts_integrity_check(&conn).expect_err("integrity check should fail on a drifted index");
        assert!(err.to_string().contains("integrity check"), "got: {err}");

        // The unconditional rebuild restores maintained == rebuilt.
        rebuild_fts_index(&conn).expect("rebuild should succeed");
        let restored = search_posts(&conn, "zebra", &SearchFilters::default(), 10)
            .expect("restored search should succeed");
        assert_eq!(restored.len(), 1, "rebuild repairs the drifted index");
        assert_eq!(restored[0].reddit_fullname, "t3_drift");

        fts_integrity_check(&conn).expect("integrity check should pass after rebuild");
    }

    #[test]
    fn is_fts_corruption_classifies_only_corruption_errors() {
        // The corruption-mapping arm of fts_integrity_check is the function's
        // reason to exist; driving real index corruption is SQLite-version
        // dependent, so verify the classifier directly and deterministically.
        // SQLITE_CORRUPT_VTAB is the extended code FTS5 raises; rusqlite reports
        // its primary code as DatabaseCorrupt.
        let corrupt = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT_VTAB),
            Some("fts5: corruption detected".to_string()),
        );
        assert!(
            is_fts_corruption(&corrupt),
            "SQLITE_CORRUPT_VTAB is corruption"
        );

        let busy = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        );
        assert!(!is_fts_corruption(&busy), "SQLITE_BUSY is not corruption");

        assert!(
            !is_fts_corruption(&rusqlite::Error::QueryReturnedNoRows),
            "non-SqliteFailure errors are not corruption"
        );
    }

    #[test]
    fn fts_integrity_check_covers_aux_indexes() {
        // Maintenance must catch drift in the aux FTS tables, not just posts_fts —
        // otherwise `fts check` reports OK while capture/enrichment search is stale.
        let conn = test_db();
        let mut post = test_post();
        post.reddit_fullname = "t3_aux".to_string();
        post.reddit_id = "aux".to_string();
        upsert_post(&conn, &post).expect("post should insert");
        upsert_outbound_capture(&conn, &capture_with("t3_aux", "tungsten capture body"))
            .expect("capture should insert");

        fts_integrity_check(&conn).expect("freshly maintained aux indexes pass");

        // Desync capture_fts only (its rowid mirrors outbound_captures.rowid),
        // leaving posts_fts intact, so the failure must come from the aux index.
        conn.execute(
            "INSERT INTO capture_fts(capture_fts, rowid, title, description, site_name, content_markdown)
             SELECT 'delete', rowid, title, description, site_name, content_markdown
             FROM outbound_captures WHERE reddit_fullname = 't3_aux'",
            [],
        )
        .expect("capture_fts desync should succeed");

        let err =
            fts_integrity_check(&conn).expect_err("integrity check must catch drifted aux index");
        assert!(err.to_string().contains("capture_fts"), "got: {err}");

        rebuild_fts_index(&conn).expect("rebuild should succeed");
        fts_integrity_check(&conn).expect("rebuild repairs the aux index");
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

    fn all_sources() -> SearchFilters {
        SearchFilters {
            source: SearchSource::All,
            ..SearchFilters::default()
        }
    }

    /// Stage three posts, one matching only in each source, plus a term shared
    /// between a post and its capture, exercising the cross-source merge.
    fn seed_multi_source(conn: &Connection) {
        // Post A: a term unique to the post body (posts_fts path) plus a term
        // shared with its capture (cross-source dedup path).
        let mut post_a = test_post();
        post_a.reddit_fullname = "t3_postonly".to_string();
        post_a.reddit_id = "postonly".to_string();
        post_a.title = "Plain heading".to_string();
        post_a.content_markdown = Some("xenon body argon overlap".to_string());
        upsert_post(conn, &post_a).expect("post a should insert");
        upsert_outbound_capture(conn, &capture_with("t3_postonly", "argon mirrored"))
            .expect("capture a should insert");

        // Post B: the term lives only in the outbound capture (capture_fts path).
        let mut post_b = test_post();
        post_b.reddit_fullname = "t3_captureonly".to_string();
        post_b.reddit_id = "captureonly".to_string();
        post_b.title = "Unrelated heading".to_string();
        post_b.content_markdown = Some("nothing notable".to_string());
        upsert_post(conn, &post_b).expect("post b should insert");
        upsert_outbound_capture(conn, &capture_with("t3_captureonly", "krypton deep dive"))
            .expect("capture b should insert");

        // Post C: the term lives only in enrichment output (enrichment_fts path),
        // recorded twice to exercise the latest-run-only / 1:many dedup.
        let mut post_c = test_post();
        post_c.reddit_fullname = "t3_enrichonly".to_string();
        post_c.reddit_id = "enrichonly".to_string();
        post_c.title = "Another heading".to_string();
        post_c.content_markdown = Some("plain text".to_string());
        upsert_post(conn, &post_c).expect("post c should insert");
        for summary in ["radon first run", "radon second run"] {
            record_enrichment_success(
                conn,
                "t3_enrichonly",
                "provider",
                "model",
                "prompt",
                "raw",
                &test_output(RecommendedAction::ReadingQueue, summary),
            )
            .expect("enrichment should insert");
        }
    }

    /// The merged search resolves matches from each aux FTS table back to the
    /// owning post, reports which source matched, and de-duplicates by fullname.
    #[test]
    fn search_all_sources_resolves_dedupes_and_reports_source() {
        let conn = test_db();
        seed_multi_source(&conn);

        // Each source resolves to exactly its owning post with the right source.
        let post_hits = search(&conn, "xenon", &all_sources(), 10).expect("post search succeeds");
        assert_eq!(post_hits.len(), 1);
        assert_eq!(post_hits[0].reddit_fullname, "t3_postonly");
        assert_eq!(post_hits[0].source, "posts");

        let capture_hits =
            search(&conn, "krypton", &all_sources(), 10).expect("capture search succeeds");
        assert_eq!(capture_hits.len(), 1);
        assert_eq!(capture_hits[0].reddit_fullname, "t3_captureonly");
        assert_eq!(capture_hits[0].source, "capture");

        let enrich_hits =
            search(&conn, "radon", &all_sources(), 10).expect("enrichment search succeeds");
        assert_eq!(
            enrich_hits.len(),
            1,
            "1:many enrichment dedupes by fullname"
        );
        assert_eq!(enrich_hits[0].reddit_fullname, "t3_enrichonly");
        assert_eq!(enrich_hits[0].source, "enrichment");

        // A term present in both a post and its capture yields a single hit, and
        // the more authoritative post source wins the dedup.
        let shared_hits =
            search(&conn, "argon", &all_sources(), 10).expect("shared search succeeds");
        assert_eq!(shared_hits.len(), 1, "matches dedupe across sources");
        assert_eq!(shared_hits[0].reddit_fullname, "t3_postonly");
        assert_eq!(shared_hits[0].source, "posts");
    }

    /// `source` scopes which indexes are consulted: a capture-only term is found
    /// under `capture`/`all` but not under `posts` or `enrichment`.
    #[test]
    fn search_source_scopes_the_indexes() {
        let conn = test_db();
        seed_multi_source(&conn);

        let posts_only = SearchFilters {
            source: SearchSource::Posts,
            ..SearchFilters::default()
        };
        assert!(
            search(&conn, "krypton", &posts_only, 10)
                .expect("posts search succeeds")
                .is_empty(),
            "a capture-only term must not surface under source=posts"
        );

        let capture_only = SearchFilters {
            source: SearchSource::Capture,
            ..SearchFilters::default()
        };
        let hits = search(&conn, "krypton", &capture_only, 10).expect("capture search succeeds");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source, "capture");

        let enrichment_only = SearchFilters {
            source: SearchSource::Enrichment,
            ..SearchFilters::default()
        };
        assert!(
            search(&conn, "krypton", &enrichment_only, 10)
                .expect("enrichment search succeeds")
                .is_empty(),
            "a capture-only term must not surface under source=enrichment"
        );
    }

    /// The has_capture / has_enrichment / classification / action filters narrow
    /// the resolved post set, composing with the full-text match.
    #[test]
    fn search_metadata_filters_narrow_results() {
        let conn = test_db();

        // Two posts share the search term; only one has a capture and enrichment.
        let mut enriched = test_post();
        enriched.reddit_fullname = "t3_meta_enriched".to_string();
        enriched.reddit_id = "metaenriched".to_string();
        enriched.title = "tungsten alloy".to_string();
        upsert_post(&conn, &enriched).expect("post should insert");
        upsert_outbound_capture(
            &conn,
            &capture_with("t3_meta_enriched", "supporting capture"),
        )
        .expect("capture should insert");
        let mut output = test_output(RecommendedAction::ShouldBuild, "build it");
        output.classification = crate::models::Classification::Tool;
        record_enrichment_success(
            &conn,
            "t3_meta_enriched",
            "provider",
            "model",
            "prompt",
            "raw",
            &output,
        )
        .expect("enrichment should insert");

        let mut bare = test_post();
        bare.reddit_fullname = "t3_meta_bare".to_string();
        bare.reddit_id = "metabare".to_string();
        bare.title = "tungsten ingot".to_string();
        upsert_post(&conn, &bare).expect("post should insert");

        // Without filters, both posts match.
        let all = search(&conn, "tungsten", &all_sources(), 10).expect("search succeeds");
        assert_eq!(all.len(), 2);

        // has_capture keeps only the post with a capture.
        let with_capture = SearchFilters {
            source: SearchSource::All,
            has_capture: true,
            ..SearchFilters::default()
        };
        let hits = search(&conn, "tungsten", &with_capture, 10).expect("search succeeds");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].reddit_fullname, "t3_meta_enriched");

        // has_enrichment + classification + action all target the enriched post.
        let enriched_filter = SearchFilters {
            source: SearchSource::All,
            has_enrichment: true,
            classification: Some("tool".to_string()),
            action: Some("should_build".to_string()),
            ..SearchFilters::default()
        };
        let hits = search(&conn, "tungsten", &enriched_filter, 10).expect("search succeeds");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].reddit_fullname, "t3_meta_enriched");

        // A non-matching classification filters everything out.
        let wrong_class = SearchFilters {
            source: SearchSource::All,
            classification: Some("article".to_string()),
            ..SearchFilters::default()
        };
        assert!(
            search(&conn, "tungsten", &wrong_class, 10)
                .expect("search succeeds")
                .is_empty()
        );
    }

    /// Only the latest successful enrichment run is searchable: a term that lived
    /// solely in an older run no longer matches once a newer run supersedes it.
    #[test]
    fn search_enrichment_uses_latest_run_only() {
        let conn = test_db();
        let mut post = test_post();
        post.reddit_fullname = "t3_latest".to_string();
        post.reddit_id = "latest".to_string();
        post.title = "Neutral heading".to_string();
        post.content_markdown = Some("neutral body".to_string());
        upsert_post(&conn, &post).expect("post should insert");

        record_enrichment_success(
            &conn,
            "t3_latest",
            "provider",
            "model",
            "prompt",
            "raw",
            &test_output(RecommendedAction::ReadingQueue, "promethium summary"),
        )
        .expect("first run should insert");
        record_enrichment_success(
            &conn,
            "t3_latest",
            "provider",
            "model",
            "prompt",
            "raw",
            &test_output(RecommendedAction::ReadingQueue, "francium summary"),
        )
        .expect("second run should insert");

        let stale = search(&conn, "promethium", &all_sources(), 10).expect("search succeeds");
        assert!(stale.is_empty(), "older run's text must not be searchable");
        let current = search(&conn, "francium", &all_sources(), 10).expect("search succeeds");
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].reddit_fullname, "t3_latest");
    }

    #[test]
    fn search_respects_zero_limit() {
        let conn = test_db();
        let post = test_post();
        upsert_post(&conn, &post).expect("post should insert");
        let hits = search(&conn, "markdown", &all_sources(), 0).expect("zero limit should succeed");
        assert!(hits.is_empty());
    }

    #[test]
    fn post_only_search_reports_posts_source() {
        let conn = test_db();
        let mut post = test_post();
        post.title = "Cobalt heading".to_string();
        upsert_post(&conn, &post).expect("post should insert");
        let hits = search_posts(&conn, "cobalt", &SearchFilters::default(), 10)
            .expect("search should succeed");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source, "posts");
    }
}
