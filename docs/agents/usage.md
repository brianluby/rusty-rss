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

- `search`: full-text search over saved post titles and Markdown content.
- `list`: list recent saved posts.
- `show`: fetch one saved post by fullname (returns null when not found).
- `triage`: list enrichment-driven triage items for a view.

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

- MCP `search`
- MCP `list`
- MCP `show`
- MCP `triage`
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

MCP `search` arguments:

```json
{
  "query": "rust sqlite",
  "limit": 10,
  "subreddit": "rust"
}
```

MCP `list` arguments:

```json
{
  "limit": 20,
  "offset": 0
}
```

MCP `show` arguments:

```json
{
  "fullname": "t3_abc123"
}
```

MCP `triage` arguments:

```json
{
  "view": "should-build",
  "limit": 20
}
```

## Tool Selection

| Goal | Preferred tool | Fallback |
| --- | --- | --- |
| Search for saved posts | MCP `search` | `rusty-rss search --json` |
| Read one saved post | MCP `show` | `rusty-rss show` |
| Browse recent posts | MCP `list` | `rusty-rss list` |
| Get enriched queue | MCP `triage` or `rusty-rss triage --json` | `rusty-rss export --format jsonl --action ...` |
| Bulk handoff | `rusty-rss export --format jsonl` | none |

## Machine-Readable Manifest

See [tool-manifest.v1.json](tool-manifest.v1.json) for a compact summary of CLI commands, MCP tools, mutability, network usage, and output formats.
