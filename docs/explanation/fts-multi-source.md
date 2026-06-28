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
Reddit title. This document records how search covers all three sources.

Status (RSS-50): **wired and shipped**. `db::search` is the real cross-source
implementation and the `search` CLI command calls it. The post-only entry point
`db::search_posts` is preserved as a thin wrapper (pinned to `source = posts`)
for the MCP server and zero-regression callers. The earlier `search_multi_source`
prototype has been removed/superseded by `db::search`.

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

## Decision: flag, not a new default

Cross-source search is opt-in via `search --source posts|capture|enrichment|all`,
defaulting to `posts`. This keeps the shipped behavior byte-for-byte for existing
callers and agents (zero regression) while making the wider search one flag away.
Making `all` the default would silently change every existing query's result set
and ranking, which we explicitly avoid. `db::search` consults only the arms its
`SearchFilters::source` selects.

## Merge semantics

`db::search(conn, query, filters, limit)` uses `UNION ALL` across the selected
indexes inside a `MATERIALIZED` CTE and resolves every FTS `rowid` back to the
owning post:

- `posts_fts.rowid` → `saved_posts.rowid` → `reddit_fullname`
- `capture_fts.rowid` → `outbound_captures.rowid` → `reddit_fullname`
- `enrichment_fts.rowid` → `enrichment_runs.rowid` → `reddit_fullname`

Results are **de-duplicated by `reddit_fullname`**, keeping the single best
(lowest, source-penalized) rank, that match's snippet, and which `source` it came
from (surfaced on `SearchHit.source`). The CTE is `MATERIALIZED` because FTS5's
`bm25()`/`snippet()` are only valid in a SELECT that directly references the FTS
table with a `MATCH`; materializing evaluates each arm once in that context, and
the downstream `best`/snippet/source references then read plain columns.

### Cross-source rank normalization

Each FTS index weights its own columns, so raw BM25 magnitudes are not directly
comparable across tables. We normalize with a small **additive per-source
penalty** on the BM25 score (SQLite BM25 is negative; lower sorts first, so a
positive penalty demotes a source):

| source     | BM25 weights              | penalty |
|------------|---------------------------|---------|
| posts      | title 10×, body 1×        | +0.0    |
| capture    | all columns 1×            | +1.0    |
| enrichment | all columns 1×            | +2.0    |

Justification: the post's own title/body is the most authoritative relevance
signal (and the title is the strongest of those, hence the 10× column weight
carried over from `search_posts`); captured page text is secondary; model-
generated enrichment is the least authoritative. The penalties are small relative
to typical BM25 magnitudes, so a markedly stronger lower-tier match can still
outrank a weak post match — the bias only decides near-ties and the dedup winner.
For `source = posts` the penalty is 0, so the post-only path is numerically
identical to the previous `search_posts` (its ranking/snapshot tests are
unchanged).

### 1:many enrichment — latest successful run only

A post can have many enrichment runs, so `enrichment_fts` holds one document per
run. Rather than index every run and dedup at query time, the enrichment arm
filters to the **newest successful run per post** (`er.id = (SELECT MAX(id) …
WHERE status='success')`). This collapses the 1:many fan-out before the
cross-source dedup and ensures search reflects current enrichment, not a
superseded older run. We chose a **query-time `MAX(id)` filter over a partial
index or a new migration**: external-content FTS5 cannot express "newest row per
group" as an index predicate, and the filter keeps both the index and the schema
unchanged (no RSS-11 migration needed). Tradeoff: a term that existed only in an
older run is no longer findable, which is the intended "latest wins" behavior.

## Metadata filters

`SearchFilters` carries, beyond `subreddit`/`author`, the multi-source filters
`source`, `has_capture`, `has_enrichment`, `classification`, and `action`. The
boolean `has_*` flags are appended as static `EXISTS`/`NOT NULL` clauses (no
params, no injection surface); `classification`/`action` bind named params and
match against the post's latest successful enrichment run (the same `MAX(id)`
join used above), so a `NULL` filter is a no-op.

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
`posts_fts` and the post-only `search_posts` path alone, and isolate the merge
logic in one function (`db::search`).

## Known tradeoffs

- **Cross-table BM25 is normalized, not unified.** The additive per-source
  penalty (above) biases ordering by source authority and breaks near-ties; it is
  a deliberate heuristic, not a learned/global relevance model. Tuning the
  penalties or moving to per-source score normalization is a future refinement.
- **Latest-run-only enrichment.** Search reflects the newest successful
  enrichment run; text that existed only in a superseded run is not findable (by
  design — see above).
- **Re-evaluated snippet/source sub-selects.** The snippet and source columns are
  correlated sub-selects over the materialized match set. The `MATERIALIZED` CTE
  keeps the FTS aux functions valid and avoids re-running the underlying FTS
  match; for a personal-archive-sized database this is comfortably fast.
