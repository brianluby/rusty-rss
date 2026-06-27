# Rusty RSS Agent

Use this skill when an agent needs read-only access to a local `rusty-rss` saved-post archive.

## Preferred Surface

Prefer the MCP server for read-only access:

```sh
rusty-rss-mcp --db-path ./rusty-rss.sqlite3
```

Read-only MCP tools:

- `query_posts`: alias for `search_posts`; use this for agent query workflows.
- `search_posts`: search titles and Markdown content. Arguments: `query`, optional `limit`, `subreddit`, `author`.
- `list_posts`: list recent saved posts. Arguments: optional `limit`, `offset`.
- `show_post`: show one post by Reddit fullname. Arguments: `fullname`.

## CLI Fallback

Use CLI JSON output when MCP is unavailable:

```sh
rusty-rss --db-path ./rusty-rss.sqlite3 search "rust sqlite" --json --limit 10
rusty-rss --db-path ./rusty-rss.sqlite3 export --format jsonl --limit 50
```

## Boundaries

- Read-only: MCP tools, `list`, `show`, `search`, `triage`, `export`.
- Writes local SQLite: `sync`, `enrich`, `capture`, `tag`.
- Uses network: `sync`, `capture`, `enrich`.
- Uses an LLM endpoint: `enrich`.
- No network or tokens: `tag` (Gate 1 rule engine).

See `docs/agents/usage.md` for the full agent guide and `docs/agents/tool-manifest.v1.json` for a machine-readable command/tool summary.
