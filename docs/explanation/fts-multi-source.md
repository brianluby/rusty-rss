# Multi-source full-text search

`rusty-rss` already ships full-text search over saved posts (`posts_fts`,
exercised by `search_posts`). A saved post, though, carries text in three places:

- the post itself (`saved_posts.title`, `content_markdown`),
- the captured outbound page (`outbound_captures.title`, `description`,
  `site_name`, `content_markdown`),
- the enrichment output (`enrichment_runs.classification`, `tags_json`,
  `summary`, `rationale`).

A query like "kubernetes operator" should find a post even when the phrase only
appears in the captured article body or in the model's summary, not in the
Reddit title. This document records how we extend search to cover all three
sources. The code is **prototype scaffolding** (RSS-36): the schema, triggers,
and a `search::search_multi_source` prototype exist and are tested, but the CLI
still calls the untouched `search_posts`.

## Decision: separate external-content aux FTS tables

We add one external-content FTS5 table per extra source, each mirroring the
existing `posts_fts` pattern, and merge them at query time:

- `capture_fts` — FTS5 over `outbound_captures(title, description, site_name,
  content_markdown)`, `content='outbound_captures'`, `content_rowid='rowid'`,
  `tokenize='porter unicode61'`, maintained by `outbound_captures_ai/ad/au`
  triggers.
- `enrichment_fts` — FTS5 over `enrichment_runs(classification, tags_json,
  summary, rationale)`, `content='enrichment_runs'`, `content_rowid='rowid'`,
  maintained by `enrichment_runs_ai/ad/au` triggers.

Both are *external-content* tables: they store only the inverted index and read
document text back from their content table by `rowid`. The INSERT/UPDATE/DELETE
triggers keep the index in sync exactly the way `saved_posts_ai/ad/au` keep
`posts_fts` in sync. `init_db` backfills any index that drifts from its content
table (`rebuild_index_if_stale`), so pre-existing rows get indexed on the next
open.

## Merge semantics

`search::search_multi_source(conn, query, limit)` uses `UNION ALL` across the
three indexes (de-duplicating later by `reddit_fullname`, not in SQL) and
resolves every FTS `rowid` back to the owning post:

- `posts_fts.rowid` → `saved_posts.rowid` → `reddit_fullname`
- `capture_fts.rowid` → `outbound_captures.rowid` → `reddit_fullname`
- `enrichment_fts.rowid` → `enrichment_runs.rowid` → `reddit_fullname`

Results are **de-duplicated by `reddit_fullname`**, keeping the single best
(lowest) BM25 rank and that match's snippet. The function reuses the existing
`SearchHit` shape and query normalizer, so a future CLI surface can adopt it
without new types.

### 1:many enrichment

A post can have many enrichment runs, so `enrichment_fts` holds one document per
run. The prototype indexes **all** runs and de-duplicates by `reddit_fullname`
at query time, keeping the best-ranked hit. A "latest run only" refinement
(filtering to the newest `enrichment_runs.id` per post, or a partial index) is
left as future work.

### NULL handling

The capture and enrichment columns are nullable. External-content FTS5 indexes a
NULL column as empty text, so posts with no capture or no enrichment simply never
appear in those aux indexes — no sentinel rows, no special-casing.

## Why not the alternatives

- **Denormalize onto `saved_posts`.** Copying capture/enrichment text into
  derived `saved_posts` columns would pollute the canonical table with stale,
  duplicated data and force cross-table triggers to fan in writes. It also risks
  regressing `posts_fts`/`search_posts`, which we explicitly keep untouched.
- **Materialized search view.** A combined materialized view (or a single FTS
  table fed from all three tables) adds the most new machinery: a synthetic
  rowid space, triggers on three tables writing one index, and bespoke rebuild
  logic. The aux-table approach reuses the proven `posts_fts` pattern three times
  instead.

The aux tables keep each index aligned with exactly one content table, leave
`posts_fts` and `search_posts` alone, and isolate the merge logic in one
prototype function.

## Known limitations (prototype)

- **Cross-table BM25 is approximate.** Each index weights its own columns
  independently (`posts_fts` boosts the title), so ranks are only roughly
  comparable across sources. A production merge would normalize per-source
  scores or apply source weights.
- **No filters yet.** `search_multi_source` does not take `SearchFilters`
  (subreddit/author); adding them means filtering the resolved `saved_posts`
  rows, mirroring `search_posts`.
- **Re-evaluated CTE.** The snippet sub-select re-runs the match CTE; correctness
  over efficiency for the prototype.
