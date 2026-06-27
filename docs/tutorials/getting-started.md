# Getting Started

This tutorial walks through a first local archive: sync saved Reddit items, inspect them, search them, and export records for another tool.

## Prerequisites

- Rust toolchain compatible with the workspace `rust-version` of `1.88`.
- A Reddit saved-items feed URL, such as `https://old.reddit.com/saved.rss?feed=...&user=...`.

The feed URL is only needed for `sync`. Read-only commands can run against an existing database without it.

## Build the CLI

```bash
cargo build --release
```

During development, use `cargo run --` before the command:

```bash
cargo run -- --help
```

After installing or using the release binary, use `rusty-rss` directly.

## Sync Saved Items

Set the feed URL and run `sync`:

```bash
export RUSTY_RSS_FEED_URL="https://old.reddit.com/saved.rss?feed=...&user=..."
cargo run -- sync
```

By default, the command writes to `./rusty-rss.sqlite3`, requests up to `100` saved items per page, and follows up to `50` pages.

To choose a database path:

```bash
cargo run -- --db-path ./archive.sqlite3 sync
```

To tune pagination:

```bash
cargo run -- sync --limit 100 --max-pages 10
```

`sync` is idempotent. Running it again updates changed saved posts, records newly seen posts, and refreshes `last_seen_at` for unchanged posts.

## List Recent Posts

```bash
cargo run -- list --limit 20
```

Use `--offset` for pagination:

```bash
cargo run -- list --limit 20 --offset 20
```

## Show One Post

Use the Reddit fullname from `list` output:

```bash
cargo run -- show t3_abc123
```

The command prints the title, permalink, author, subreddit, outbound URL when available, publication time, and saved Markdown content.

## Search the Archive

Search title and Markdown content:

```bash
cargo run -- search "rust sqlite"
```

Emit newline-delimited JSON records for scripts or agents:

```bash
cargo run -- search "rust sqlite" --json --limit 10
```

Filter by subreddit or author:

```bash
cargo run -- search "async" --subreddit rust --author example_user --json
```

## Export Records

Export the stable agent envelope as JSONL:

```bash
cargo run -- export --format jsonl --limit 50
```

Export Markdown for reading:

```bash
cargo run -- export --format markdown --limit 10
```

Export CSV for spreadsheets:

```bash
cargo run -- export --format csv --limit 100
```

The JSONL envelope is documented in [Export schema v1](../reference/export-schema-v1.md).

## Next Steps

- Use [Search and query the archive](../how-to/search-and-query.md) for search syntax, filters, and MCP query tools.
- Use [Enrich and triage saved posts](../how-to/enrich-and-triage.md) to classify saved items.
- Use [Capture outbound pages](../how-to/capture-outbound-pages.md) to store metadata and readable snapshots from outbound links.
