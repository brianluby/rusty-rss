# ADR: Rule-engine condition language

- **Status:** Accepted (revised 2026-07-03; original spike 2026-07-02)
- **Supersedes / informs:** backlog epic *Rule engine (user-customizable triage)* (RSS-39 spike); extends the deterministic seam in `crates/rusty-rss-core/src/sort.rs`.
- **Task:** RSS-39 — Spike: evaluate regorus vs CEL vs rhai.

## Context

The deferred *Rule engine* epic wants user/agent-authored declarative rules that
auto-act on enriched posts, e.g.:

> `subreddit == "rust" && enrichment.work_value > 0.7 -> add_to_list("should_build")`

The existing `crates/rusty-rss-core/src/rules/` module is a **different** system:
a TOML-declared, FTS5-based *topic-scoring* ruleset (`threshold` + additive
`Rule`s + `subreddit_prior`) that assigns topic tags (surfaced via the `tag`
CLI). It does not express condition→action triage rules.

The deterministic counterpart those rules would extend is `sort::lists_for` — a
pure, offline `EnrichmentOutput -> Vec<List>` mapping whose own doc comment calls
it *"the single seam a future CEL rule engine is intended to replace."* Both that
seam and the future rule engine target the `sort::List` enum (`ShouldTest`,
`ShouldBuild`, `ReadingQueue`, `Reference`, `Discard`).

The rule condition references a flattened item+enrichment record (fields drawn
from `SavedPost` + `EnrichmentOutput` in `crates/rusty-rss-core/src/models.rs`).
Three example rules were prototyped in each candidate language, all producing
identical results:

1. `subreddit == "rust" && work_value >= 0.7` → `add_to_list("should_build")`
2. `classification == "news" && age_days > 30` → `discard`
3. `outbound_url != "" && joy_value > 0.5` → `add_to_list("reading_queue")`

## Decision

Build a small **bespoke** condition parser for a deliberately restricted v1
grammar, isolated behind a swappable `ConditionBackend` trait. Ship
`BespokeBackend` in v1. **Do not adopt CEL now** — keep `cel` (0.14.0, MIT,
`cel-rust/cel-rust`) as the documented **escalation path**. Regorus and rhai
remain rejected.

### Why bespoke for v1

- The v1 surface — typed comparisons joined by `&&`/`||`/`!` over a **fixed,
  known** field set — is small enough that a ~200-line Pratt parser + evaluator
  covers it, with zero new dependencies and zero binary-size cost.
- CEL's headline advantage is **guaranteed termination on a rich language**. A
  deliberately non-Turing-complete bespoke grammar (no recursion, no loops, no
  function definition) gives the same property *by construction* — we don't need
  CEL's machinery to be safe, because v1 rules can't express non-termination.
- CEL's cost buys a stdlib (durations, string ops, regex, macros) that v1 rules
  deliberately won't use. On a CLI shipped via `install.sh` (PR #20), +2.9 MB /
  +58 deps for an unused feature set is a poor trade.
- The project already has this idiom: `rules::compile` hand-rolls a
  TOML→FTS5 lowering with structural validation, `deny_unknown_fields`, dedup,
  and case-normalization. A condition DSL is the same pattern.
- Bespoke gives precise, parse-time errors against an allow-list of typed fields
  (catching typos like `subredit` at load time) and direct binding to typed
  structs — both things CEL only gets with extra wrapping.

### Why a swappable backend (the key design move)

The condition string is stored as an opaque TOML field and isolated behind:

```rust
trait ConditionBackend {
    fn compile(&self, src: &str, fields: &FieldRegistry)
        -> anyhow::Result<Box<dyn Fn(&RuleRecord) -> bool + Send + Sync>>;
}
```

`FieldRegistry` enumerates the allowed fields with their types, so `compile`
rejects unknown fields and type-mismatched comparisons. v1 ships
`BespokeBackend`; a future `CelBackend` implements the same trait. The TOML
schema, the closed `Action` enum, the evaluator loop, the provenance writer, and
the CLI are **backend-agnostic** — swapping bespoke→CEL is a one-line change at
engine construction, with no effect on stored rules. This makes "bespoke vs CEL"
a reversible leaf decision, not an architectural commitment.

### Escalation trigger

If a real rule needs anything in the **excluded** set below (assignment, loops,
recursion, general arithmetic, method calls, regex, or duration math), that is
the signal to flip the backend to CEL — not to grow the bespoke parser. The
swappable trait exists precisely so that flip is cheap.

## v1 grammar (hard-scoped, non-Turing-complete)

**Allowed:** string/number/bool/null literals; typed comparisons (`== != < <= >
>=`); boolean composition (`&& || !` and parentheses); field references from the
`FieldRegistry` allow-list; a **fixed** helper set — `present(x)` (non-null /
non-empty), `contains(haystack, needle)` (string/vec membership),
`days_since(published_at)` (age in days, evaluated against the post clock).

**Excluded (CEL escalation triggers):** assignment, loops, recursion, function
definition, general arithmetic, method calls, regex.

**Termination:** the grammar is a bounded expression tree with no recursion, so
evaluation always terminates. Parse depth is additionally capped to reject
pathologically nested input.

## Record (the fields rules reference)

A flattened `RuleRecord` built from `SavedPost` (+ derived fields) and the latest
`EnrichmentOutput`. `age_days` and `has_outbound` are derived at eval time.

| field               | type            | source / notes                                   |
| ------------------- | --------------- | ------------------------------------------------ |
| `subreddit`         | Option\<String\> | `SavedPost.subreddit`                            |
| `classification`    | String          | `EnrichmentOutput.classification` (snake_case)   |
| `recommended_action`| String          | `EnrichmentOutput.recommended_action`            |
| `joy_value`         | f32             | `EnrichmentOutput.joy_value`                     |
| `work_value`        | f32             | `EnrichmentOutput.work_value`                    |
| `confidence`        | f32             | `EnrichmentOutput.confidence`                    |
| `tags`              | Vec\<String\>   | `EnrichmentOutput.tags`                          |
| `summary`           | String          | `EnrichmentOutput.summary`                       |
| `title`             | String          | `SavedPost.title`                                |
| `outbound_url`      | Option\<String\> | `SavedPost.outbound_url`                         |
| `has_outbound`      | bool            | derived: `outbound_url.is_some_and(non-empty)`   |
| `age_days`          | f64             | derived: `now - SavedPost.published_at`          |
| `source`            | String          | `SavedPost.source`                               |

Out-of-range scores remain enforced by `EnrichmentOutput::validate()`
(`models.rs:184`) regardless of rule output.

## Evidence (measured 2026-07-02, Rust stable, macOS/arm64, release, stripped)

Each candidate was built as a standalone binary that parses the same JSON record
and evaluates the same 3 rules; the `baseline` column is `serde_json` + pure-Rust
matching only, so the deltas isolate each engine. These numbers now **motivate
the bespoke choice**: CEL's +2.9 MB / +58 deps buys a stdlib the v1 grammar
won't use.

| crate (version, license)              | stripped binary | Δ vs baseline | unique deps | one-line rule? |
| ------------------------------------- | --------------: | ------------: | ----------: | :------------- |
| baseline (`serde_json` only)          |         0.41 MB |          —    |          12 | n/a            |
| `cel` 0.14.0 (MIT) — escalation path  |         3.35 MB |       +2.94 MB|          70 | yes            |
| `rhai` 1.25.1 (MIT OR Apache-2.0)     |         3.21 MB |       +2.80 MB|          52 | yes            |
| `regorus` 0.10.1 (MIT AND Apache-2.0 AND BSD-3-Clause) | 7.32 MB | +6.91 MB | 152 | no (multi-line `if`) |

Notes on the numbers:

- These are **standalone** binaries. The real workspace already pulls `chrono`,
  `serde`, `serde_json`, and `regex`, so the *in-project marginal* new weight for
  `cel` is smaller than the +2.94 MB above (its heavy transitive deps are
  `antlr4rust` + `nom`; the rest is largely already present).
- `regorus`'s 152 deps include `jsonschema`, `icu_casemap`, `chrono-tz`,
  `globset`, and `regorus-mimalloc` — none of which the project otherwise needs.
- All three licenses are permissive and compatible with the project's MIT
  license; `regorus` additionally surfaces `BSD-3-Clause` transitively.

## Why the alternatives were rejected (unchanged)

- **rhai** — a close second (cleanest/smallest), but a full Turing-complete
  language. Safe use for *untrusted* rules needs recurring sandboxing (disable
  file/IO features, set `max_operations`, restrict `Scope`). The bespoke grammar
  gives the same one-line ergonomics with termination by construction.
- **regorus** — heaviest by every axis (7.32 MB, 152 deps, tri-license); Rego is
  policy/auth-decision shaped and a single condition is a 4-line rule plus a
  `default` — not a one-line rule.

## Consequences

- **No new dependencies for v1.** The bespoke parser lives in
  `crates/rusty-rss-core` alongside the existing `rules/` topic-scoring module
  (distinct module name to avoid collision — e.g. `rule_engine/`).
- **Closed action set in Rust** (`add_to_list`, `remove_from_list`,
  `set_status`/`discard`, `add_tag`); CEL/conditions evaluate only the *if*. Users
  never write actions as code, keeping the untrusted surface to pure boolean
  expressions.
- **Rule file shape:** TOML with `condition = '...'` + `action = { ... }`,
  `#[serde(deny_unknown_fields)]`, consistent with the existing `rules::types`
  loader.
- **Integration:** the evaluator extends/replaces the `sort::lists_for` seam;
  both target the `sort::List` enum. The CLI gets a new top-level `rules`
  subcommand (`run | test | validate | list`) in `src/cli.rs`, distinct from the
  existing `tag` (topic-scoring) and `triage` (read-only list views) commands.
- **Idempotent + safe by default:** re-runs re-stamp provenance, not duplicate
  actions; `--apply` is required to mutate, dry-run is the default, and no
  destructive action ships enabled.
- **Revisit / flip to CEL** when a rule needs the excluded grammar surface (see
  *Escalation trigger*). The swap is backend-only and does not touch stored
  rules, the schema, or the CLI.
- **No timing/throughput claims** were measured; this spike evaluated embed cost,
  dependency weight, authoring ergonomics, and safety only.

## Reproducibility

Throwaway prototype harness (not in-repo): four standalone crates (`baseline`,
`cel-spike`, `regorus-spike`, `rhai-spike`), each parsing the same 3 JSON records
and evaluating the 3 rules. All four produced identical output:

```
record 0: ["should_build"]
record 1: ["discard"]
record 2: ["reading_queue"]
```

Measurements taken with `cargo build --release`, `strip -x`, `wc -c`, and
`cargo metadata` unique-package counts:

```bash
cargo build --release
strip -x target/release/<bin> -o <bin>.stripped && wc -c <bin>.stripped
cargo metadata --format-version 1 | jq '[.packages[].id] | length'
```
