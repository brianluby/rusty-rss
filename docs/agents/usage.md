# Agent Usage

This guide defines what an agent can safely use `rusty-rss` for and which surface to prefer.

## Use `rusty-rss` For

- Search a user's local Reddit saved-post archive.
- List recent saved posts.
- Inspect one saved post by Reddit fullname.
- Query enriched triage queues.
- Export stable records for downstream analysis.
- Read captured outbound page metadata when it exists.

## Preferred Read-Only Surface

Prefer MCP when available:

```bash
rusty-rss-mcp --db-path ./rusty-rss.sqlite3
```

Use these MCP tools:

- `query_posts`: preferred agent query tool; alias for `search_posts`.
- `search_posts`: search saved post titles and Markdown content.
- `list_posts`: list recent saved posts.
- `show_post`: fetch one saved post by fullname.

These tools are read-only archive operations.

## CLI Fallback

Use CLI JSON or JSONL output when MCP is unavailable.

Search:

```bash
rusty-rss --db-path ./rusty-rss.sqlite3 search "rust sqlite" --json --limit 10
```

Triage:

```bash
rusty-rss --db-path ./rusty-rss.sqlite3 triage should-build --json --limit 20
```

Export:

```bash
rusty-rss --db-path ./rusty-rss.sqlite3 export --format jsonl --limit 50
```

## Mutability and Consent

Treat these as read-only archive operations:

- MCP `query_posts`
- MCP `search_posts`
- MCP `list_posts`
- MCP `show_post`
- CLI `list`
- CLI `show`
- CLI `search`
- CLI `triage`
- CLI `export`

Treat these as write operations requiring clear user intent:

- `sync`: fetches the Reddit feed and writes `saved_posts` plus `sync_runs`.
- `enrich`: calls an LLM endpoint and writes `enrichment_runs`.
- `capture`: fetches outbound URLs and writes `outbound_captures`.

Treat these as network operations:

- `sync`
- `enrich`
- `capture`

Treat this as LLM usage:

- `enrich`

## Output Contracts

Use `search --json` for newline-delimited search hits. Each line includes `reddit_fullname`, title, author, subreddit, permalink, outbound URL, snippet, rank, and last-seen time.

Use `triage --json` for newline-delimited triage items with latest enrichment data when present.

Use `export --format jsonl` for the stable full-record envelope. The schema version is `rusty-rss.export.v1`; see [Export schema v1](../reference/export-schema-v1.md).

## Query Examples

MCP `query_posts` arguments:

```json
{
  "query": "rust sqlite",
  "limit": 10,
  "subreddit": "rust"
}
```

MCP `list_posts` arguments:

```json
{
  "limit": 20,
  "offset": 0
}
```

MCP `show_post` arguments:

```json
{
  "fullname": "t3_abc123"
}
```

## Tool Selection

| Goal | Preferred tool | Fallback |
| --- | --- | --- |
| Search for saved posts | MCP `query_posts` | `rusty-rss search --json` |
| Read one saved post | MCP `show_post` | `rusty-rss show` |
| Browse recent posts | MCP `list_posts` | `rusty-rss list` |
| Get enriched queue | `rusty-rss triage --json` | `rusty-rss export --format jsonl --action ...` |
| Bulk handoff | `rusty-rss export --format jsonl` | none |

## Machine-Readable Manifest

See [tool-manifest.v1.json](tool-manifest.v1.json) for a compact summary of CLI commands, MCP tools, mutability, network usage, and output formats.
