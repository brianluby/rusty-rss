# speckit brief — rusty-rss rule engine (v1, bespoke condition backend)

> Feed this entire file to speckit. It is self-contained. Before writing
> anything, **read every file referenced under "Inputs"** so the spec matches the
> real types, module layout, and conventions. Produce **two** artifacts (paths
> under "Deliverables"), not one.

## Objective

Spec and plan a local-first, user/agent-authored **action-rule engine** for
`rusty-rss`: declarative rules whose *condition* is a restricted boolean
expression evaluated over a flattened item+enrichment record, and whose *action*
is a member of a closed Rust enum that mutates a post's list membership / status
/ tags. This is the backlog epic "Rule engine (user-customizable triage)" and the
implementation of the decision in `docs/adr/rule-engine.md`.

## Inputs (READ THESE FIRST)

- `docs/adr/rule-engine.md` — **the governing decision.** v1 uses a bespoke
  parser behind a swappable `ConditionBackend` trait; `cel` is the escalation
  path only. Do NOT spec a CEL dependency.
- `crates/rusty-rss-core/src/models.rs` — `SavedPost`, `EnrichmentOutput`
  (classification/recommended_action are snake_case enums; joy/work/confidence
  are f32 in `[0.0, 1.0]` enforced by `validate()`), `EnrichmentRecord`.
- `crates/rusty-rss-core/src/sort.rs` — `lists_for()` is the documented "single
  seam a future rule engine is intended to replace"; the `List` enum
  (`ShouldTest | ShouldBuild | ReadingQueue | Reference | Discard`) and
  `SortConfig` live here. Rules target the same `List` enum.
- `crates/rusty-rss-core/src/rules/` (`types.rs`, `parse.rs`, `compile.rs`,
  `test_support.rs`) — the **existing** topic-scoring engine (FTS5-based, surfaced
  via the `tag` CLI). This is a *different* system; mirror its discipline (TOML
  `deny_unknown_fields`, structural `compile()` validation, dedup, precise
  bail-with-context errors, unit tests) but do **not** reuse its name. The new
  module must not collide; propose `crates/rusty-rss-core/src/rule_engine/`.
- `crates/rusty-rss-core/src/db/` (`posts.rs`, `enrichment.rs`, `tags.rs`,
  `migrations.rs`, `schema.rs`) — the persistence layer. Provenance rows go here.
- `src/cli.rs` and `src/cli/*.rs` — the clap `Command` enum. Add a new top-level
  `rules` subcommand with nested subcommands. Note the existing `tag`
  (topic-scoring) and `triage` (read-only views) commands to avoid collision.
- `docs/superpowers/specs/2026-06-27-install-script-design.md` and
  `docs/superpowers/plans/2026-06-27-install-script.md` — **format templates**.
  Match their structure and voice exactly.
- `Cargo.toml` — workspace: edition 2024, rust-version 1.88, members `.`,
  `crates/rusty-rss-core`, `crates/rusty-rss-mcp`. Existing deps: anyhow, clap
  (derive+env), serde/serde_json, tokio, chrono.

## Mandate (from the ADR — do not contradict)

1. **Bespoke parser for v1.** No new crates. Hard-scoped, non-Turing-complete
   grammar. Termination by construction + a parse-depth cap.
2. **Swappable backend via trait.** Condition stored as an opaque TOML string.
   Define `trait ConditionBackend { fn compile(&self, src: &str, fields: &FieldRegistry) -> anyhow::Result<Box<dyn Fn(&RuleRecord) -> bool + Send + Sync>> }`.
   Ship `BespokeBackend`. Document that a future `CelBackend` is a drop-in.
3. **Closed action enum in Rust.** Conditions are pure boolean; users never write
   actions as code.

## v1 grammar (specify precisely; this is the contract)

- **Literals:** `"strings"`, numbers (parse as f64, compare against f32 fields),
  `true`/`false`, `null`.
- **Comparisons:** `== != < <= > >=`, type-checked at compile (string-vs-number
  is a parse error).
- **Boolean composition:** `&& || !` and parentheses.
- **Field references:** only fields in the `FieldRegistry` allow-list (see
  Record). Unknown identifiers are a parse error (typo guard).
- **Fixed helpers:** `present(x)` (Option/empty), `contains(haystack, needle)`
  (string or Vec<String>), `days_since(published_at)` (age in days; evaluated
  against a clock injected at eval time so tests are deterministic).
- **Excluded (escalation triggers — call out explicitly):** assignment, loops,
  recursion, function definition, general arithmetic, method calls, regex.

## Record (the field allow-list)

A `RuleRecord` flattened from `SavedPost` (+ derived) and the latest
`EnrichmentOutput`. Specify exact field names/types and how each is populated
(which join against `db::enrichment` for "latest successful run"). Include
derived `age_days: f64` (from `published_at`, eval-time), `has_outbound: bool`.
Note that score bounds stay enforced by `EnrichmentOutput::validate()`, not by
rules.

## Scope (what to design — one task per item in the plan)

- **Rule file schema + serde loader** — TOML `[meta]` (version) + list of rules,
  each `id`, `condition` (opaque string), `action` (tagged enum:
  `add_to_list`, `remove_from_list`, `set_status`/`discard`, `add_tag` with
  args), `enabled` (default true), optional `note`.
  `#[serde(deny_unknown_fields)]`. Reuse the `rules::compile` pattern: parse →
  structural `compile()` that validates every condition via the backend and
  rejects duplicates/empty ids/impossible rules.
- **Condition backend + bespoke parser** — the trait, `FieldRegistry`, a Pratt
  parser for the v1 grammar, type checking, depth cap, and precise errors with
  source spans.
- **Evaluation engine** — load ruleset once; for each post, build `RuleRecord`,
  evaluate firing rules, produce an ordered, de-duplicated action plan. Idempotent
  (re-runs re-stamp provenance, not duplicate effects). **Dry-run is the default**;
  `--apply` mutates. No destructive action (e.g. `discard`) enabled by default.
- **Provenance** — rows recording `rule_id`, `ruleset_version`, `action`, target,
  timestamp, per fired rule (mirror the existing `post_tags` provenance model in
  `db::tags.rs`).
- **CLI** — new top-level `rules` command with nested `run | test | validate |
  list`, JSON output (`--json`) for machine consumption. `run` defaults to
  dry-run; `test` evaluates a single record/condition for authoring feedback.
- **Starter rule library + docs** — a small `rules.toml` (the 3 spike examples
  disabled by default), a docs page, and an explicit statement that no
  destructive rule ships enabled.

## Integration points (be concrete)

- New core module: `crates/rusty-rss-core/src/rule_engine/` (mod re-exported from
  `lib.rs`). Distinct from `rules/`.
- CLI: add `Rules` variant to `Command` in `src/cli.rs`, handler under
  `src/cli/rules.rs`, mirroring `src/cli/fts.rs` (the repo's first nested
  subcommand — copy that pattern).
- Relationship to `sort::lists_for`: rules are an *extension* of that seam, not a
  silent override. Specify whether rules compose with or replace the deterministic
  mapping (recommend: compose — deterministic lists first, rules layer
  add/remove on top — and justify).
- DB: add/extend provenance + ruleset-version tracking via a migration in
  `db/migrations.rs`.

## Constraints & conventions (match the existing code)

- anyhow for errors; bail/with_context like `rules/compile.rs`.
- clap derive for the CLI; serde with `deny_unknown_fields`.
- Edition 2024, MSRV 1.88, no new dependencies.
- Module doc comments on every public item (the crate enables
  `#![warn(missing_docs)]`).
- Unit tests per module; include parser error-path tests (unknown field,
  type mismatch, depth cap, excluded grammar).
- Clock injection for `days_since` so tests are deterministic.

## Out of scope (state explicitly)

Workflow chaining, event triggers/scheduling, a GUI rule builder, regex, CEL
stdlib, and the CEL backend itself (escalation path only).

## Deliverables (match the install-script pair exactly)

1. `docs/superpowers/specs/<DATE>-rule-engine-design.md` — sections: Goal, Scope
   (by component), Grammar, Record, Rule file schema, Evaluation & provenance,
   CLI, Security/safety (termination, dry-run default, no destructive default),
   Integration with `sort`, Risks/open questions.
2. `docs/superpowers/plans/<DATE>-rule-engine.md` — Goal, Architecture, Tech
   stack, Global constraints, then Task N blocks each with **Files / Interfaces
   (Consumes + Produces) / checkbox Steps / verification commands / lint+commit**.
   Sequence: schema+loader → backend+parser → evaluator+provenance → CLI →
   starter library+docs. Each task must be independently verifiable.

## Acceptance for the spec/plan

- A reader can implement each task from the plan without further design
  decisions.
- The v1 grammar, the record field allow-list, and the `ConditionBackend` trait
  are specified exactly enough to code.
- Every file/path referenced exists or is explicitly "create:".
- The plan's verification steps are runnable (specific `cargo`/CLI commands), and
  the final task integrates with the `rules` CLI end-to-end against the DB.

Use `<DATE>` = today's date in the filenames. Do not begin implementation; this
brief asks only for the spec and the plan.
