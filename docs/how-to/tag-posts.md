# Tag Posts (Gate 1 Rule Engine)

Use `tag` to apply the Gate 1 rule engine: a cheap, deterministic, token-free
filter that scores saved posts by topic and records which posts are worth the
more expensive `capture` and enrichment stages. Rules live in `rules.toml` as
data; edit them and re-run, with no recompile.

## Run Tagging

```bash
rusty-rss tag --rules ./rules.toml
```

This evaluates every topic in the rules file against the whole archive and
writes one `post_tags` row per `(post, topic)` that scored. The run is
**authoritative**: re-running after a rule change removes tags that no longer
fire, so consumers always see current tags.

```text
Tagging complete: 867 posts, 6 topics, 142 rows written, 58 passed, 9 vetoed
```

| Option | Default | Description |
| --- | --- | --- |
| `--rules <PATH>` | `./rules.toml` | Path to the rules config. |
| `--topic <TOPIC>` | all topics | Tag only one topic; preserves other topics' tags. |
| `--limit <N>` | whole archive | Debug cap on posts processed. |
| `--dry-run` | false | Evaluate and report without writing any rows. |
| `--json` | false | Emit newline-delimited JSON tag records. |

## Inspect Results

```bash
rusty-rss tag --rules ./rules.toml --topic memory --json | jq
```

```json
{
  "reddit_fullname": "t3_1sp0vrb",
  "topic": "memory",
  "score": 4.0,
  "threshold": 3.0,
  "passed": true,
  "matched_rules": ["title_concept", "prior:opencodeCLI"],
  "signals": { "prior": 2.0, "title_concept": 2.0 },
  "ruleset_version": "rules-v1",
  "tagged_at": "2026-06-26T12:00:00Z"
}
```

A row is written whenever at least one scoring rule fires, even below the
threshold, so near-misses stay queryable. `score` is stored, so a new threshold
can be applied at query time (`WHERE score >= ...`) without re-running. The
`threshold` and `passed` columns are materialized at tag time, so editing a
threshold only updates them after the next `tag` run. `passed` is true when
`score >= threshold` and no veto fired.

## How a Post Is Scored

For each topic, for each post:

1. Each rule compiles to column-scoped FTS5 queries. A rule fires when the post
   matches at least `min_hits` distinct alternatives; its `weight` is added once.
2. The subreddit prior for the post's subreddit is added (only when a rule fired).
3. If an `exclude` rule matches and its optional `unless` guard does not, the
   post is **vetoed** for that topic: `passed` is forced false.
4. `passed = score >= threshold AND not vetoed`.

`matched_rules` records the provenance: fired rule ids, `prior:<subreddit>`, and
`veto:<id>` markers.

## Writing Rules

`rules.toml` defines topics, each with a `threshold`, `rules`, an optional
`subreddit_prior` table, and optional `exclude` vetoes.

```toml
[meta]
version = "rules-v1"

[topics.memory]
threshold = 3.0

[[topics.memory.rules]]
id = "title_concept"
signal = "title"          # title | body | any
kind = "fts"              # fts | terms
weight = 2.0
match = 'memor* OR "second brain" OR obsidian'

[[topics.memory.rules]]
id = "named_tool"
signal = "any"
kind = "terms"
weight = 3.0
match = ["mem0", "letta", "memgpt"]

[topics.memory.subreddit_prior]
opencodeCLI = 2.0

[[topics.memory.exclude]]
id = "hardware_memory"
signal = "title"
kind = "fts"
match = 'vram OR "gpu memory"'
unless = { signal = "any", kind = "terms", match = ["mem0", "letta"] }
veto = true
```

Field reference:

- **`signal`** — which text to match: `title`, `body` (self-post Markdown), or
  `any`. `subreddit` and `domain` match rules are rejected in rules-v1; use
  `subreddit_prior` for subreddit weighting (domain is a Gate 2 signal).
- **`kind`** — `fts` (one FTS5 expression) or `terms` (a list of literal tokens;
  a trailing `*` means prefix match). `exact` and `regex` are not supported in
  rules-v1.
- **`weight`** — points added once when the rule fires. Default `1.0`.
- **`min_hits`** — a rule fires only when at least N distinct OR-alternatives
  (`fts`) or list tokens (`terms`) match. Default `1`. Useful to damp noisy body
  text: `min_hits = 2` requires two distinct concept hits.
- **`exclude` / `veto` / `unless`** — a matching veto disqualifies the post for
  the topic unless the `unless` guard also matches.

### Authoring Constraints

- TOML 1.0 forbids multi-line inline tables, so write each rule with the
  `[[topics.<name>.rules]]` array-of-tables form (as above), or keep an inline
  `{ ... }` table on a single line.
- Hyphenated or multi-word phrases in an `fts` expression must be quoted
  (`"tool use"`, `"knowledge graph"`), never bare. `terms` tokens are quoted and
  escaped automatically.
- Every compiled expression is smoke-tested against the FTS index before any
  rows are written; a malformed rule fails the whole run with its rule id, and
  nothing is persisted.

## Pipeline Position

Tagging is Gate 1: it runs on `sync`-only signals (title, self-text, subreddit)
to cut the archive down to a candidate set worth fetching (`capture`) and
reviewing. See [the rule-engine PRD](../prd/rule-engine.md) for the design and
worked examples.
