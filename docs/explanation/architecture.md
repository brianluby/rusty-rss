# Architecture

`rusty-rss` is a local-first archive for Reddit saved posts. The application keeps the source feed, enrichment output, captured outbound metadata, and agent exports in one SQLite database.

## Components

The workspace has three packages:

- `rusty-rss`: the CLI binary.
- `rusty-rss-core`: shared sync, parse, storage, search, enrichment, capture, and export logic.
- `rusty-rss-mcp`: read-only stdio MCP server backed by the same SQLite database.

## Data Flow

1. `sync` fetches the saved-items RSS or Atom feed.
2. The parser normalizes feed entries into saved post records.
3. SQLite stores posts in `saved_posts` and updates the FTS5 index.
4. `search`, `list`, and `show` read directly from saved posts and FTS.
5. `enrich` selects posts without a successful enrichment and appends `enrichment_runs`.
6. `triage` reads saved posts joined to their latest enrichment.
7. `capture` selects posts with outbound URLs and stores latest page metadata in `outbound_captures`.
8. `export` joins saved posts, latest enrichment, and outbound capture into agent-ready records.
9. `rusty-rss-mcp` exposes read-only list, show, and search tools over stdio.

## Local SQLite as the Boundary

The SQLite database is the handoff point between workflows. Commands do not require a running service. A user can sync with the CLI, query later through MCP, and export records in another session as long as all commands point at the same database path.

## Read and Write Surfaces

Read-only archive surfaces:

- `list`
- `show`
- `search`
- `triage`
- `export`
- MCP `list_posts`
- MCP `show_post`
- MCP `search_posts`
- MCP `query_posts`

Write surfaces:

- `sync`: writes saved posts and sync history.
- `enrich`: writes enrichment attempts.
- `capture`: writes outbound capture attempts.
- `tag`: writes Gate 1 rule-engine tags (no network or tokens).

Network surfaces:

- `sync`: fetches the Reddit feed.
- `enrich`: calls an OpenAI-compatible LLM API.
- `capture`: fetches outbound URLs from saved posts.

## Why Latest Enrichment Is a Join

Enrichment attempts are append-only so failed and successful attempts remain auditable. Triage and export use the latest row by ID for each saved post. Candidate selection for `enrich` only skips posts that already have a successful enrichment; posts with only failed attempts remain eligible.

## Why Capture Is Upserted

Outbound capture represents the latest known metadata for a URL attached to a saved post. Unlike enrichment, repeated capture replaces the row and increments `attempt_count`, because consumers generally need the current page metadata rather than a history of every fetch.

## Agent Contract

Agents should prefer read-only MCP tools for interactive retrieval. For bulk or portable exchange, agents should use `export --format jsonl` and the `rusty-rss.export.v1` schema.
