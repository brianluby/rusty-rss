# Search and Query the Archive

Use `search` for human CLI searches and JSON records. Use MCP `query_posts` or `search_posts` when an agent has MCP access.

## Search from the CLI

```bash
rusty-rss search "rust sqlite"
```

The search covers saved post titles and normalized Markdown content. Results are ranked by SQLite FTS5 and include highlighted snippets.

## Emit JSON Records

```bash
rusty-rss search "rust sqlite" --json --limit 10
```

Each line is one JSON record with:

- `reddit_fullname`
- `title`
- `author`
- `subreddit`
- `permalink`
- `outbound_url`
- `snippet`
- `rank`
- `last_seen_at`

## Filter Results

Filter by subreddit name without `r/`:

```bash
rusty-rss search "sqlite" --subreddit rust
```

Filter by author name without `u/`:

```bash
rusty-rss search "sqlite" --author example_user
```

Combine filters:

```bash
rusty-rss search "sqlite" --subreddit rust --author example_user --json
```

## Search Phrases

Quote a phrase to search for the phrase as one part of the FTS query:

```bash
rusty-rss search "\"full text search\" sqlite"
```

Unquoted terms are combined with `AND`. A query must contain searchable alphanumeric text. Unterminated quotes are rejected.

## Query Through MCP

Start the read-only MCP server:

```bash
rusty-rss-mcp --db-path ./rusty-rss.sqlite3
```

Use `query_posts` for agent query workflows:

```json
{
  "query": "rust sqlite",
  "limit": 10,
  "subreddit": "rust"
}
```

`query_posts` is an alias for `search_posts`. Both return the same search records as pretty-printed JSON in MCP text content.

## Pick the Right Surface

- Use MCP `query_posts` when an agent needs read-only archive access.
- Use CLI `search --json` when MCP is unavailable or a script needs newline-delimited JSON.
- Use `export --format jsonl` when a consumer needs full saved post records with latest enrichment and outbound capture data.
