# Dependency maintenance

Notes on dependency advisories tracked against `rusty-rss` and how they were
resolved. We gate the dependency tree with `cargo audit` (RUSTSEC advisory DB).

## RUSTSEC-2025-0057 — `fxhash` unmaintained (RESOLVED)

- **Advisory:** `fxhash` 0.2.1 is unmaintained
  (<https://rustsec.org/advisories/RUSTSEC-2025-0057>, 2025-09-05). It is an
  *unmaintained* warning, not a vulnerability, so `cargo audit` exited 0 even
  while it applied.
- **How it reached us:** transitively, `scraper 0.22 -> selectors 0.26 ->
  fxhash 0.2.1`. `scraper` is used for HTML parsing in `capture/fetch.rs` and
  `parse.rs`.
- **Resolution:** bumped `scraper` to `0.27` in
  `crates/rusty-rss-core/Cargo.toml`. `scraper 0.27 -> selectors 0.38` drops the
  `fxhash` dependency entirely (`selectors` now uses `rustc-hash`). After the
  bump `cargo tree -i fxhash` reports no match and `cargo audit` is clean.
- **Verification:** `cargo build`, `cargo test --workspace --all-features`
  (158 passed), and `cargo clippy --workspace --all-targets --all-features
  -- -D warnings` (clean) all pass on `scraper 0.27`; the HTML parse/capture
  tests confirm the parser API is unchanged for our usage.

No `cargo audit` ignore entry is needed — the advisory no longer applies.
