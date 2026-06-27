# Error handling in core

`rusty-rss-core` mixes two error styles on purpose:

- **`anyhow::Result`** for the DB (`db::*`), search (`search::*`), and capture
  (`capture::*`) APIs.
- **Typed `thiserror` enums** at the one boundary where a caller actually
  branches on the failure class: `llm::EnrichError`
  (`crates/rusty-rss-core/src/llm.rs:20`).

This document records why we keep that split instead of converting the whole
core to typed errors, and when to revisit.

## Decision: keep `anyhow::Result` for DB / search / capture

We do **not** do a broad typed-error refactor of the DB, search, and capture
APIs right now. Reasons:

- **No consumer discriminates the variants.** Every current caller of these
  APIs propagates with `?` or reports the error; none matches on a specific
  failure class to decide retry vs. skip vs. abort. A typed enum would be pure
  churn with no behavioral payoff.
- **Context is already preserved.** These paths use `anyhow::Context`
  (`.with_context(...)`) to attach human-readable, source-chained messages, so
  diagnostics do not regress by staying on `anyhow`.
- **Cost vs. benefit.** Introducing enums (and `From` plumbing) across three
  modules touches a lot of signatures and tests for zero current benefit, and
  it risks regressing well-tested code.

## Where typed errors *are* warranted (precedent: `EnrichError`)

Reserve `thiserror` enums for boundaries where a caller needs to **branch on the
variant**. The enrichment path is the established precedent:
`EnrichError::{Transport, ModelUnavailable, Parse, Validation}`
(`crates/rusty-rss-core/src/llm.rs:20-28`). Callers distinguish a transient
`Transport`/`ModelUnavailable` failure (retry/skip the run) from a `Parse` or
`Validation` failure (a bad model response, surfaced differently). That
discrimination is the thing that justifies the typed enum; without it,
`anyhow` is the right default.

## Masking findings already resolved (RSS-48)

An earlier review (CodeRabbit) flagged two spots where the DB layer could
**mask** a real failure by silently coercing a bad value. Both are already fixed
on `main` under RSS-48 — this card folds in the confirmation, no new code:

- **`enrichment.rs`** now surfaces `rusqlite::Error::FromSqlConversionFailure`
  instead of swallowing an unparseable stored value
  (`crates/rusty-rss-core/src/db/enrichment.rs:296,307,316`; test at
  `enrichment.rs:455`).
- **`posts.rs::parse_optional_timestamp`** now fails on a malformed RFC3339
  timestamp rather than defaulting it away
  (`crates/rusty-rss-core/src/db/posts.rs:259-273`; test at `posts.rs:385`).

Fixed in commits `f36f5d4` and `6120ae1` (both `fix(core): … (RSS-48)`). These
were error-*masking* bugs, not an argument for a typed-error rewrite: the fix is
to propagate the underlying error, which `anyhow` already does well.

## Revisit when

Convert a specific DB / search / capture API to a typed `thiserror` enum the
first time a real caller needs to **branch on a failure class** from that API —
for example, distinguishing "row not found" or "unique-constraint conflict" from
a generic I/O failure to drive retry, upsert, or user-facing messaging. At that
point, introduce the enum scoped to that one API (mirroring `EnrichError`),
not a blanket refactor.
