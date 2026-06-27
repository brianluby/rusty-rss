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

The server speaks JSON-RPC over stdio using MCP framing. It writes protocol frames to stdout.

## Available Tools

### `query_posts`

Alias for `search_posts`. Prefer this name for agent query workflows.

Arguments:

- `query`: required string.
- `limit`: optional integer, default `20`, maximum `100`.
- `subreddit`: optional string without `r/`.
- `author`: optional string without `u/`.

### `search_posts`

Search saved post titles and Markdown content.

Arguments are the same as `query_posts`.

### `list_posts`

List recent saved posts ordered by `last_seen_at` descending.

Arguments:

- `limit`: optional integer, default `20`, maximum `100`.
- `offset`: optional integer, default `0`.

### `show_post`

Show one saved post by Reddit fullname.

Arguments:

- `fullname`: required string, such as `t3_abc123`.

## Read-Only Boundary

The MCP server only exposes read operations. It does not sync feeds, call LLMs, capture outbound pages, or mutate the archive beyond opening and initializing the SQLite schema.

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
