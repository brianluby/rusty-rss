# Enrichment + FTS work — design spec (RSS-20, 24, 35, 36)

Branch: `enrichment` (off `main`). Edition 2024, workspace MSRV 1.88.

Verification gates (every task): `cargo test --workspace --all-features`,
`cargo clippy --workspace --all-targets --all-features -- -D warnings`,
`cargo fmt --all --check`. Run `cargo fmt --all` as the final step before
committing (the PostToolUse hook reorders imports differently from canonical
fmt). Conventional commits, **no AI-attribution trailers** (per global CLAUDE.md).
TDD: write the failing test first, watch it fail, then implement.

Key context from the codebase map:
- `EnrichmentOutput` (crates/rusty-rss-core/src/models.rs): `classification:
  Classification`, `tags: Vec<String>`, `summary: String`, `joy_value/work_value/
  confidence: f32` (each finite, `[0.0,1.0]`), `recommended_action:
  RecommendedAction`, `rationale: String`. `#[serde(deny_unknown_fields)]`.
- `Classification`: Article, Tool, Tutorial, Reference, Discussion, Question,
  News, Other (snake_case).
- `RecommendedAction`: ShouldTest, ShouldBuild, ReadingQueue, ReferenceOnly,
  Discard, Other (snake_case).
- FTS: `posts_fts` is external-content FTS5 over `saved_posts(title,
  content_markdown)`, `content_rowid='rowid'`, `tokenize='porter unicode61'`,
  maintained by triggers `saved_posts_ai/ad/au` (schema.rs). `search_posts`,
  `SearchFilters`, `SearchHit` live in db/search.rs. `rebuild_stale_fts_index`
  (schema.rs) runs only inside `init_db`.
- CLI: clap derive in src/cli.rs (`Command` enum), per-command modules in
  src/cli/*.rs returning `Result<()>`; `--json` is newline-delimited JSON.
- `RSS-34` (search CLI) is already shipped on main.

---

## RSS-35 — FTS integrity self-check + reindex (do first)

**Core (db):**
- `pub fn rebuild_fts_index(conn) -> Result<()>` — unconditional
  `INSERT INTO posts_fts(posts_fts) VALUES('rebuild')`.
- `pub fn fts_integrity_check(conn) -> Result<()>` — runs
  `INSERT INTO posts_fts(posts_fts) VALUES('integrity-check')`; map an FTS5
  corruption error to a clear `anyhow` error. (Lives in db/search.rs or
  db/schema.rs; export via db's re-export.)
- Keep `rebuild_stale_fts_index` for `init_db`.

**CLI:** new **hidden** `fts` subcommand (first nested clap `Subcommand` in the
repo) with `rebuild` and `check`. `#[command(hide = true)]` on the variant. New
module src/cli/fts.rs with `run_fts(db_path, FtsCommand)`. `check` prints OK or
returns a non-zero error; `rebuild` prints a short confirmation.

**Tests (TDD):**
1. Sequence/property test: apply a deterministic seeded sequence of random
   upserts + deletes to `saved_posts` (use a tiny in-test LCG; `Math.random`/
   `rand` not required), then assert the trigger-maintained index equals a
   freshly rebuilt one: a set of `search_posts` queries returns identical hits
   before vs. after `rebuild_fts_index`, and `fts_integrity_check` passes.
2. Drift/recovery test: deliberately desync the index (e.g. delete a
   `saved_posts` row's FTS entry via the special delete syntax, or insert a
   stray row), assert `fts_integrity_check` still structurally OK or the
   maintained!=rebuilt comparison detects drift, and `rebuild_fts_index`
   restores maintained == rebuilt.
Document the rowid dependency in a comment.

**Commit:** `feat(db): add FTS integrity-check + reindex with hidden `fts`
maintenance command (RSS-35)`

---

## RSS-24 — deterministic list sorter / "AI-sort" (pure, offline)

**New core module** crates/rusty-rss-core/src/sort.rs, `pub mod sort;` in lib.rs.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum List { ShouldTest, ShouldBuild, ReadingQueue, Reference, Discard }

#[derive(Debug, Clone, Copy)]
pub struct SortConfig {
    pub build_threshold: f32,        // default 0.7
    pub reading_threshold: f32,      // default 0.6
    pub test_threshold: f32,         // default 0.6
    pub min_discard_confidence: f32, // default 0.5
}
impl Default for SortConfig { /* the values above */ }

/// Deterministic, offline policy mapping an EnrichmentOutput to the lists it
/// belongs to. This is the single seam a future CEL rule engine replaces.
pub fn lists_for(output: &EnrichmentOutput, cfg: &SortConfig) -> Vec<List>;
```

**Default multi-list policy** (order preserved, de-duplicated):
1. A low-confidence Discard (`recommended_action == Discard &&
   confidence < min_discard_confidence`) → `[Reference]` (manual review), return.
2. A confident Discard → `[Discard]` (terminal), return.
3. Base list from `recommended_action`: ShouldTest→ShouldTest,
   ShouldBuild→ShouldBuild, ReadingQueue→ReadingQueue, ReferenceOnly→Reference,
   Other→(none yet).
4. Threshold add-ons: `work_value >= build_threshold` → +ShouldBuild;
   `joy_value >= reading_threshold` → +ReadingQueue;
   `matches!(classification, Tool|Tutorial) && work_value >= test_threshold`
   → +ShouldTest.
5. Fallback: if still empty → `[Reference]`.

**Tests (TDD):** table-driven fixtures — an array of `(EnrichmentOutput,
expected Vec<List>)` covering each base action, each threshold add-on, the
low-confidence-discard override, the confident-discard terminal case, the Other
fallback, and boundary values at each threshold. No LLM dependency. Make
`lists_for` `pub` in the core lib (building block; not dead code as public API).

**Commit:** `feat(sort): add deterministic EnrichmentOutput->list sorter
(RSS-24)`

---

## RSS-20 — versioned enrichment prompt + scoring rubric

**Prompt builder unit:** extract prompt construction out of llm.rs into a
focused unit (e.g. a `prompt` submodule under llm, or a `prompt.rs` in core)
with:
- `pub(crate) const PROMPT_VERSION: &str = "enrich-v2";` (single definition;
  update enrich.rs to use it — keep stored per run).
- A documented **rubric** embedded in the system prompt: one line per
  `Classification` and per `RecommendedAction` value describing when to choose
  it, and the meaning + `[0.0,1.0]` range of `joy_value` (personal interest),
  `work_value` (build/learn utility), `confidence` (model certainty). The rubric
  also serves as repair guidance.
- `build_enrichment_messages(post, budget) -> Vec<Message>` — deterministic;
  same shape the OpenAI provider already sends.
- `truncate_for_budget(markdown, max_chars) -> (String, bool)` — char-based (no
  tokenizer dep); keeps the head and appends a truncation marker when cut.
  Default budget a named const (e.g. `MAX_CONTENT_CHARS = 12_000`).

**Tests (TDD):** golden-output tests — fixtures (a normal post and an oversized
post) → assert the rendered messages match committed golden text (inline
expected strings or committed fixture files); assert truncation keeps the input
under budget and only fires when needed; assert `PROMPT_VERSION == "enrich-v2"`.
Keep existing llm tests green.

**Commit:** `feat(llm): versioned enrichment prompt with scoring rubric and
input budget (RSS-20)`

---

## RSS-36 — multi-source FTS: decision record + prototype + scaffold (backlog)

**Decision (record in docs/explanation/ + this spec):** extend search with
**separate external-content aux FTS tables**, mirroring the `posts_fts` pattern,
rather than denormalizing onto `saved_posts` or a materialized view:
- `capture_fts` — external-content FTS5 over `outbound_captures(title,
  description, site_name, content_markdown)`, `content_rowid='rowid'`, with
  `outbound_captures_ai/ad/au` triggers.
- `enrichment_fts` — external-content FTS5 over `enrichment_runs(classification,
  tags_json, summary, rationale)`, `content_rowid='rowid'`, with triggers.
- **Merge:** a multi-source search UNIONs `posts_fts` ∪ `capture_fts` ∪
  `enrichment_fts`, resolving each FTS `rowid` back to `reddit_fullname`
  (capture_fts.rowid→outbound_captures.rowid→reddit_fullname;
  enrichment_fts.rowid→enrichment_runs.rowid→reddit_fullname), de-duplicating by
  `reddit_fullname` and keeping the best BM25 rank.
- **1:many enrichment:** index all runs; dedup-by-fullname at query time keeps
  the best-ranked. (Documented tradeoff; a "latest only" refinement is future.)
- **NULL handling:** external-content FTS indexes empty strings for NULLs
  gracefully; posts with no capture/enrichment simply don't appear in those aux
  tables.
- **Why not:** denormalizing pollutes `saved_posts` with derived columns +
  cross-table triggers; a materialized view adds the most new machinery. The aux
  tables keep `posts_fts` and `search_posts` untouched (no regression).

**Scaffold:** add `capture_fts` + `enrichment_fts` tables + their triggers to
schema.rs (real + trigger-maintained), and `rebuild_stale_fts_index` /
init coverage as appropriate. **Do NOT modify `search_posts` / `SearchFilters` /
`SearchHit`.** Provide a documented, tested prototype interface (e.g.
`pub fn search_multi_source(conn, query, limit) -> Result<Vec<SearchHit>>` or a
clearly-named prototype fn) that performs the merged query, exercised by a
prototype test proving multi-source hits resolve to the right posts. Mark/name
it as prototype scaffolding.

**Decision doc:** docs/explanation/fts-multi-source.md (or similar) capturing the
above.

**Commit:** `feat(db): scaffold multi-source FTS (capture_fts + enrichment_fts)
with decision record (RSS-36)`

---

## Sequencing & PRs
Order: RSS-35 → RSS-24 → RSS-20 → RSS-36 (RSS-35 before RSS-36 since both touch
schema.rs/FTS). Each task: TDD → green gates → adversarial review → commit. At
the end: split into two PRs for focused review — enrichment (RSS-20 + RSS-24)
and FTS (RSS-35 + RSS-36) — or one combined PR.
