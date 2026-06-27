# rusty-rss

`rusty-rss` turns a Reddit saved-items RSS or Atom feed into a local SQLite archive. It can sync saved posts, search titles and Markdown content, enrich items with an OpenAI-compatible LLM, capture outbound page metadata, tag posts by topic with a token-free rule engine, export agent-ready records, and expose read-only MCP tools.

## Quick Start

```bash
cargo build --release
```

```bash
RUSTY_RSS_FEED_URL="https://old.reddit.com/saved.rss?feed=...&user=..." \
  cargo run -- sync
```

> **Security:** the feed URL embeds a private Reddit `feed` token and `user`.
> Prefer the `RUSTY_RSS_FEED_URL` environment variable over the `--feed-url`
> flag: a flag value leaks into shell history and the process list (`ps`), and
> would otherwise be the easiest way to expose your token. `rusty-rss` redacts
> the token and user from `sync_runs` (`source_url`/`error`) and from any sync
> error it returns or logs, storing host+path only.

```bash
cargo run -- list --limit 20
cargo run -- search "rust sqlite" --json
cargo run -- show t3_abc123
cargo run -- export --format jsonl --limit 50
```

After installation, replace `cargo run --` with `rusty-rss`.

## Core Workflows

- `sync`: fetch paginated saved-feed pages and upsert posts into SQLite.
- `list`: show recent saved posts from the local database.
- `show`: print one post by Reddit fullname, such as `t3_abc123`.
- `search`: query title and Markdown content with FTS5 snippets and optional JSON output.
- `enrich`: classify unenriched posts through an OpenAI-compatible chat completions endpoint.
- `triage`: list enrichment-driven views such as `should-build`, `reading-queue`, and `reference-only`.
- `capture`: fetch outbound page metadata with conservative network defaults.
- `tag`: apply the Gate 1 rule engine (`rules.toml`) to score and topic-tag posts, token-free.
- `export`: emit JSONL, Markdown, or CSV records for tools and agents.

## Configuration

| Setting | Flag | Environment variable | Default | Required for |
| --- | --- | --- | --- | --- |
| Feed URL | `--feed-url` | `RUSTY_RSS_FEED_URL` | none | `sync` |
| Database path | `--db-path`, `-d` | `RUSTY_RSS_DB_PATH` | `./rusty-rss.sqlite3` | all commands |
| User agent | none | `RUSTY_RSS_USER_AGENT` | `rusty-rss/0.1.0` | `sync` |
| OpenAI-compatible base URL | none | `RUSTY_RSS_OPENAI_BASE_URL` | `http://127.0.0.1:8080/v1` | `enrich` |
| OpenAI-compatible model | none | `RUSTY_RSS_OPENAI_MODEL` | `llama.cpp` | `enrich` |
| OpenAI API key | none | `RUSTY_RSS_OPENAI_API_KEY` | none | `enrich`, when required by provider |

Read-only commands such as `list`, `show`, `search`, `triage`, and `export` do not require the feed URL.

## Agent Access

The workspace includes a read-only stdio MCP server:

```bash
rusty-rss-mcp --db-path ./rusty-rss.sqlite3
```

Available MCP tools are `search_posts`, `query_posts`, `list_posts`, and `show_post`. Agents should prefer MCP for read-only archive access and use CLI JSON or JSONL output as the fallback.

See [Agent Usage](docs/agents/usage.md) and [Tool Manifest v1](docs/agents/tool-manifest.v1.json).

## Documentation

- [Documentation index](docs/index.md)
- [Getting started tutorial](docs/tutorials/getting-started.md)
- [CLI reference](docs/reference/cli.md)
- [Database reference](docs/reference/database.md)
- [Export schema v1](docs/reference/export-schema-v1.md)
- [Architecture explanation](docs/explanation/architecture.md)

## Build and Test

```bash
cargo fmt -- --check
cargo test
```

Tests use offline Atom fixtures in `test-fixtures/`.

## License

MIT
