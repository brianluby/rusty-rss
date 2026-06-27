# Rule Engine — Tagging and Gating

Status: Draft · Owner: Brian · Version: `rules-v1` · Last updated: 2026-06-26

## Summary

A rules layer that tags and scores saved posts so the pipeline can cheaply decide
what is worth fetching and worth handing to an agent for deep review. It is the
cheap, deterministic, re-runnable counterpart to LLM enrichment: it runs over post
metadata and (later) captured content, costs no tokens, is explainable, and can
re-tag the whole archive whenever the topic taxonomy changes.

The decision recorded here: implement rules as **data in a config file plus a small
evaluator over SQLite/FTS5**, materialized into a `post_tags` table by a re-runnable
`tag` command. Not a rules-engine crate, and not hand-written SQL `CASE` filters.

## Motivation

`rusty-rss` archives Reddit saved posts so they can feed a research agent ("Hermes")
that mines them for ideas to improve our work, homelab, and `rusty-brain` (a local,
networked-agent memory system). The expensive step in that loop is the agent review
(tokens). Fetching a source page is cheap (network). So the system needs a cheap
filter, applied first, that decides what is even worth fetching and reviewing.

Rules are that filter. They gate the expensive stages and improve over time: every
time Hermes judges an item "not worth it," that verdict becomes a new rule, so the
cheap layer prevents that class of expensive review in future.

## Goals

- Tag posts by topic (multi-label) and produce a per-topic score and pass/fail
  against a tunable threshold.
- Keep rules editable as **data** — add a keyword and re-run, no recompile, no SQL
  surgery.
- Record provenance (which rules fired) so results are debuggable and tunable.
- Be cheap, deterministic, and re-runnable; re-tag the full archive on every rule change.
- Reuse the same engine for Gate 2 (quality/relevance) once content is captured.

## Non-goals

- Replacing FTS5 or SQL. FTS5 remains the matching substrate.
- Rule chaining, salience-based conflict resolution, or decision tables authored by
  non-developers. None are needed at this scale; see [Graduating to an engine](#graduating-to-an-engine).
- Semantic/topic classification that the LLM enrichment step already provides. Rules
  operate *on* cheap signals (and later on enrichment output), they do not replace it.

## Decision and rationale

| Option | Verdict | Why |
| --- | --- | --- |
| Pure SQL / FTS filters | Use as substrate | FTS5 already does stemmed keyword matching, `AND/OR/NOT/NEAR`, column-scoped queries, and `bm25()` per-column weighting. ~80% of Gate 1 matching is expressible today. But weighted multi-signal scoring, exclusions, multi-label, and provenance become `CASE` soup, and rules end up buried in SQL strings. |
| Rules-engine crate (RETE / decision tables) | Defer | Earns its keep with chained inference, conflict resolution, non-developer authoring, or thousands of rules. We have additive keyword scoring over ~870 rows. A heavy, opaque dependency solving a problem we do not have; worse for a small, auditable codebase. Performance is irrelevant at this scale, so it cannot be justified on speed. |
| **Rules-as-data + thin evaluator over FTS5** | **Chosen** | Rules live in a config file (editable as data). FTS5 does the matching; a small evaluator combines FTS hits with structured signals (subreddit prior, domain), applies exclusions, scores per topic, and writes tags with provenance. Re-runnable stage that writes its own table, SQLite as the boundary — same shape as `enrich` and `capture`. |

Division of labor: **FTS5** matches keywords (binary hit per rule, fast, stemmed).
The **evaluator** does additive scoring, subreddit priors, vetoes, thresholds, and
provenance. The **config** holds the rules.

## What the archive shows

Findings from the current archive (867 saved posts) that shape the design. `capture`
and `enrich` have not been run yet, so Gate 1 starts from sync-only signals (title,
subreddit, outbound URL, and the Reddit-side body/comments).

| Finding | Number | Design implication |
| --- | --- | --- |
| Link posts (outbound URL) vs self posts | 592 / 275 | For 68% the real content is behind an unfetched URL; Gate 1 runs on title + subreddit + comments, content comes at Gate 2. |
| Title-only memory gate candidates | ~43 (5%) | Cheap title gate is high-precision and tractable for hand-labeling. |
| Named memory tools (mem0, letta, zep, …) anywhere | 2 total | Entity matching adds ~nothing today. The archive's "memory" talk is coding-agent memory (CLAUDE.md, MCP memory servers, OpenCode persistence), not the academic toolkits. |
| `"memory"` in title vs in body/comments | 25 vs 106 (4x) | Body keyword gating is a precision blowout; require `min_hits` ≥ 2 for body signals. |
| Hardware-memory ("VRAM") titles among `"memory"` hits | 3 of 25 | Disambiguation/exclusion is mandatory, not optional. |
| Subreddit candidate density | opencodeCLI ~12%, mcp ~10%, ClaudeAI ~7%, LocalLLaMA ~3%, selfhosted ~1% | Subreddit is a meaningful prior, measured from our own data — not upvote reach. |
| GitHub share of link posts | 251 / 592 (42%) | Domain signals source *type* ("has code to check"), not topic. Reserve domain for Gate 2. |
| Topic overlap (agents/MCP 387, memory 190, …) | — | Topics overlap heavily; tagging must be multi-label scored, not exclusive buckets. |
| `outbound_url` extraction grabbing filenames | 13 of 43 candidates (`CLAUDE.md`, `SKILL.md`, `agents.md`, blanks) | Data-quality bug; pollutes the domain signal and breaks capture. Fix upstream; label off title + permalink meanwhile. |

## Pipeline position

Rules are Gate 1, and the same engine is reused at Gate 2 once content exists.

1. `sync` — free. Yields title, subreddit, outbound URL/domain, Reddit body/comments.
2. **Gate 1 (rules, no tokens)** — title + comments + subreddit + exclusions → per-topic
   score → ~870 down to a small candidate set. Threshold tuned loose (fetching is cheap).
3. `capture` survivors — network only, no tokens; domain-aware extraction of the source.
4. **Gate 2 (rules or a small local-LLM pass on captured content)** — "is it real / does it
   have working code / is it networked vs single-system?" Protects the expensive stage.
5. Hermes deep review (tokens) — only double-filtered items; cross-references `rusty-brain`
   and Obsidian; returns suggestions and a todo entry.
6. Feedback — every Hermes rejection and reason becomes a new Gate 1/2 rule.

## Data model

The materialized output of a `tag` command.

```sql
CREATE TABLE post_tags (
  reddit_fullname  TEXT NOT NULL REFERENCES saved_posts(reddit_fullname),
  topic            TEXT NOT NULL,        -- 'memory', 'rag', 'agents', ...
  score            REAL NOT NULL,        -- additive weighted score
  threshold        REAL NOT NULL,        -- threshold in effect at tag time
  passed           INTEGER NOT NULL,     -- 1 if score >= threshold
  matched_rules    TEXT NOT NULL,        -- JSON: ["title_concept","prior:opencodeCLI"]
  signals          TEXT,                 -- JSON: {"title":2.0,"prior":2.0,"body":0.5}
  ruleset_version  TEXT NOT NULL,        -- 'rules-v1' or a hash of the config
  tagged_at        TEXT NOT NULL,
  PRIMARY KEY (reddit_fullname, topic)
);
CREATE INDEX idx_post_tags_topic ON post_tags(topic, passed);
CREATE INDEX idx_post_tags_score ON post_tags(topic, score DESC);
```

Design choices:

- Primary key `(reddit_fullname, topic)` — a post can hold many topics (multi-label).
- **Upsert** on re-tag, same reasoning as `outbound_captures`: consumers want current
  tags, not a history of every run.
- Write a row whenever **any** signal fires (`score > 0`), with a `passed` flag, so
  near-misses stay queryable and the threshold can be tuned without re-running.
- `ruleset_version` mirrors `enrichment_runs.prompt_version`; it ties each tag to the
  rules that produced it and powers the feedback loop.

## Rules config

Rules live in `rules.toml`. The memory topic and its VRAM exclusion, as real data:

```toml
[meta]
version = "rules-v1"

[topics.memory]
threshold = 3.0
rules = [
  { id="title_concept", signal="title", kind="fts", weight=2.0,
    match='memor* OR "knowledge graph" OR "second brain" OR obsidian OR retriev* OR rag OR "context engineering" OR persisten*' },
  { id="body_concept",  signal="body",  kind="fts", weight=0.5, min_hits=2,
    match='memor* OR "knowledge graph" OR retriev* OR embedding*' },
  { id="named_tool",    signal="any",   kind="terms", weight=3.0,
    match=["mem0","letta","memgpt","zep","cognee","graphiti","langmem","supermemory"] },
]

# structured prior: subreddit -> weight, from measured candidate density
[topics.memory.subreddit_prior]
opencodeCLI   = 2.0
mcp           = 2.0
ClaudeAI      = 1.0
ChatGPTCoding = 1.0

# exclusion: a veto disqualifies the post for THIS topic
[[topics.memory.exclude]]
id     = "hardware_memory"
signal = "title"
kind   = "fts"
match  = 'vram OR "gpu memory" OR oom OR offload OR gddr OR "unified memory"'
unless = { signal="any", kind="terms", match=["mem0","letta","memgpt","zep","cognee"] }
veto   = true
```

Rule fields (the full design space; see the **rules-v1 subset** note below for
what the shipped parser accepts):

- `signal` — which field to match: `title`, `body` (`content_markdown`), `subreddit`,
  `domain`, or `any`. Maps to FTS5 column filters (`{title}:`, `{content_markdown}:`).
- `kind` — how to interpret `match`: `fts` (an FTS5 expression), `terms` (a list of
  exact tokens), `exact`, or `regex`.
- `weight` — points added on a hit. Negative weights penalize.
- `min_hits` — only score if at least N distinct terms match (guards noisy body text).
- `veto` (exclude rules) — if matched and the optional `unless` guard does not match,
  disqualify the post for this topic regardless of score.

**rules-v1 subset (what `rules.rs` actually loads).** The shipped parser accepts
`signal` ∈ {`title`, `body`, `any`} and `kind` ∈ {`fts`, `terms`}. `signal =
"subreddit"` (use `[topics.<t>.subreddit_prior]` instead), `signal = "domain"`
(a Gate 2 signal), and `kind` of `exact`/`regex` are rejected at config load with
a clear error. `min_hits` applies to the OR-alternatives of an `fts` expression or
the tokens of a `terms` list. See `docs/how-to/tag-posts.md`.

## Evaluation flow

For each post, for each topic:

1. Run each rule's `match` as an FTS5 query scoped to its column. A hit is binary;
   `min_hits` requires N distinct term matches. Add `weight` per hit.
2. Add the `subreddit_prior` weight for the post's subreddit (0 if unlisted).
3. If an `exclude` matches and its `unless` guard does not, **veto** — force `passed = 0`.
4. `passed = score >= threshold`. Upsert into `post_tags` with score, fired rules, the
   `signals` breakdown, `ruleset_version`, and timestamp.

### Worked examples

- *"We built a persistent memory plugin for OpenCode…"* (r/opencodeCLI): `title_concept`
  hits (+2.0), `opencodeCLI` prior (+2.0), no hardware terms → **4.0 ≥ 3.0, tagged
  `memory`**, `matched_rules=["title_concept","prior:opencodeCLI"]`.
- *"How much memory do I need, is 24GB VRAM enough for 70B?"*: `title_concept` hits on
  "memory" (+2.0), but `hardware_memory` matches (vram/GB) and the `unless` guard does
  not → **veto, passed = 0**. One of the three hardware-memory titles, correctly excluded.

## Design notes

- **Additive weights, not bm25, for the gate.** bm25 ranks well but its absolute values
  are unintuitive to threshold against. Integer-ish additive weights keep "why did this
  pass" legible (2 + 2 ≥ 3). Use bm25 only to order within a passed topic, if at all.
- **Exclusions are first-class.** memory-vs-VRAM here is the same machinery as future
  disambiguations (e.g., MTP vs RAG). Build vetoes and `unless` guards now.
- **Multi-label by design.** Topics overlap; a post can be `memory` + `agents` + `mcp`.
  One `post_tags` row per passing topic.
- **Domain is a Gate 2 signal.** It indicates source type, not topic.

## Dependencies and risks

- **`outbound_url` extraction bug.** ~30% of memory candidates have a filename or empty
  "domain." Fix the extractor or the domain signal is partly garbage; until then, do not
  weight domain in Gate 1.
- **Dedup.** GitHub repos get reposted. Deduplicate (by normalized source / repo path)
  before tuning thresholds or feeding Hermes, or precision numbers will lie.
- **Comment inflation.** Body text includes the full comment thread; cap its influence
  (`min_hits`, low weight) so a stray mention in a popular thread does not trigger a tag.
- **Taxonomy drift.** Terms in this space change weekly. Keep rules as data and rely on
  the Hermes feedback loop to add terms.

## Open questions and next steps

- **Threshold tuning.** Hand-label the ~43 candidates plus a sample of body-only
  near-misses to form a gold set; read precision/recall off each threshold directly. No
  ML needed at this scale. The labeled set doubles as Hermes' first answer key.
  (Starter file: `gate1-memory-candidates.csv`.)
- **`tag` CLI surface.** Spec flags to mirror `enrich`/`capture`: `--topic`, `--limit`,
  `--dry-run`, what it writes, candidate selection.
- **Seed `rules.toml`** from the actual trigger terms across all 43 candidates rather
  than a hand-picked list.
- **Gate 2 ruleset.** Define quality/relevance rules over captured content and repo
  signals (tests, last commit, single-system vs networked).

## Graduating to an engine

Revisit a real rules engine only when one of these becomes true: rule chaining (rule
outputs become facts for further rules), genuine priority/conflict resolution beyond
additive scoring, non-developer rule authors, or thousands of rules where hand
evaluation is too slow. Because the rules are already data, that swap is cheap — the
config outlives the evaluator.
