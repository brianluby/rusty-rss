# Use the MCP Server

Use `rusty-rss-mcp` when an agent or MCP client needs read-only access to the local archive.

## Start the Server

```bash
rusty-rss-mcp --db-path ./rusty-rss.sqlite3
```

You can also set the database path with an environment variable:

```bash
RUSTY_RSS_DB_PATH=./rusty-rss.sqlite3 rusty-rss-mcp
```

The server is built on the official [`rmcp`](https://crates.io/crates/rmcp) Rust SDK and speaks
MCP JSON-RPC over stdio. stdout is reserved for the protocol stream; all logs and diagnostics go
to stderr. Each tool publishes a JSON Schema for its arguments, so MCP clients can validate calls.

Logging verbosity follows the standard `RUST_LOG` environment variable (default `info`).

## Available Tools

### `search`

Full-text search over saved post titles and Markdown content.

Arguments:

- `query`: required string.
- `limit`: optional integer, default `20`, maximum `100`.
- `subreddit`: optional string without `r/`.
- `author`: optional string without `u/`.

### `list`

List recent saved posts ordered by `last_seen_at` descending.

Arguments:

- `limit`: optional integer, default `20`, maximum `100`.
- `offset`: optional integer, default `0`.

### `show`

Show one saved post by Reddit fullname. A missing post returns JSON `null`.

Arguments:

- `fullname`: required string, such as `t3_abc123`.

### `triage`

List enrichment-driven triage items for a view.

Arguments:

- `view`: optional string, default `unprocessed`. One of `all`, `unprocessed`, `high-value`,
  `should-test`, `should-build`, `reading-queue`, `reference-only`, `discard`.
- `limit`: optional integer, default `20`, maximum `100`.
- `offset`: optional integer, default `0`.

## Read-Only Boundary

The MCP server only exposes read operations. It does not sync feeds, call LLMs, capture outbound pages, or mutate the archive beyond opening and initializing the SQLite schema. The server fails closed: if the database file does not exist it refuses to start rather than creating an empty archive.

Use the CLI for write workflows:

- `rusty-rss sync`
- `rusty-rss enrich`
- `rusty-rss capture`

## CLI Fallback

When MCP is unavailable, use CLI JSON or JSONL output:

```bash
rusty-rss search "rust sqlite" --json --limit 10
rusty-rss export --format jsonl --limit 50
```
