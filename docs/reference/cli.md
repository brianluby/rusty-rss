# CLI Reference

`rusty-rss` stores Reddit saved posts in a local SQLite database and exposes commands for syncing, reading, enrichment, capture, triage, and export.

```bash
rusty-rss [OPTIONS] <COMMAND>
```

## Global Options

| Option | Environment variable | Default | Description |
| --- | --- | --- | --- |
| `--feed-url <FEED_URL>` | `RUSTY_RSS_FEED_URL` | none | Reddit saved RSS or Atom feed URL. Required for `sync`. |
| `-d, --db-path <DB_PATH>` | `RUSTY_RSS_DB_PATH` | `./rusty-rss.sqlite3` | SQLite database path. |
| `-h, --help` | none | none | Print help. |

Additional environment variables:

| Environment variable | Default | Used by | Description |
| --- | --- | --- | --- |
| `RUSTY_RSS_USER_AGENT` | `rusty-rss/0.1.0` | `sync` | User agent for feed requests. |
| `RUSTY_RSS_OPENAI_BASE_URL` | `http://127.0.0.1:8080/v1` | `enrich` | OpenAI-compatible API base URL. |
| `RUSTY_RSS_OPENAI_MODEL` | `llama.cpp` | `enrich` | Model ID expected in `GET /models`. |
| `RUSTY_RSS_OPENAI_API_KEY` | none | `enrich` | Optional bearer token. |

## `sync`

Fetch the RSS or Atom feed and sync saved posts into the database.

```bash
rusty-rss sync [OPTIONS]
```

| Option | Default | Description |
| --- | --- | --- |
| `--limit <LIMIT>` | `100` | Number of saved items to request per Reddit page. |
| `--max-pages <MAX_PAGES>` | `50` | Maximum number of Reddit pages to fetch. |

Writes to:

- `saved_posts`
- `sync_runs`
- `posts_fts` through triggers

Uses network. Requires `--feed-url` or `RUSTY_RSS_FEED_URL`.

## `list`

List saved posts.

```bash
rusty-rss list [OPTIONS]
```

| Option | Default | Description |
| --- | --- | --- |
| `-l, --limit <LIMIT>` | `20` | Number of posts to show. |
| `-o, --offset <OFFSET>` | `0` | Offset for pagination. |

Read-only archive operation.

## `show`

Show details of a specific post.

```bash
rusty-rss show <FULLNAME>
```

Arguments:

| Argument | Description |
| --- | --- |
| `<FULLNAME>` | Reddit fullname, such as `t3_abc123`. |

Read-only archive operation. Exits non-zero if the post is not found.

## `search`

Search saved posts by title and Markdown content.

```bash
rusty-rss search [OPTIONS] <QUERY>
```

Arguments:

| Argument | Description |
| --- | --- |
| `<QUERY>` | Full-text search query. |

Options:

| Option | Default | Description |
| --- | --- | --- |
| `-l, --limit <LIMIT>` | `20` | Number of posts to show. |
| `--subreddit <SUBREDDIT>` | none | Filter by subreddit name without `r/`. |
| `--author <AUTHOR>` | none | Filter by author name without `u/`. |
| `--json` | false | Emit newline-delimited JSON records. |

Read-only archive operation.

## `enrich`

Enrich saved posts through the configured OpenAI-compatible LLM server.

```bash
rusty-rss enrich [OPTIONS]
```

Options:

| Option | Default | Description |
| --- | --- | --- |
| `-l, --limit <LIMIT>` | `20` | Maximum number of unenriched posts to process. |
| `--dry-run` | false | Show how many posts would be enriched without calling the LLM or writing rows. |

Writes to `enrichment_runs` unless `--dry-run` is set. Uses network and an LLM endpoint unless `--dry-run` is set.

## `triage`

List triage views from latest enrichment data.

```bash
rusty-rss triage [OPTIONS] <VIEW>
```

Arguments:

| View | Description |
| --- | --- |
| `all` | All saved posts, with latest enrichment when available. |
| `unprocessed` | Posts with no successful enrichment. |
| `high-value` | Posts with `joy_value >= 0.7` or `work_value >= 0.7`. |
| `should-test` | Latest successful enrichment recommends `should_test`. |
| `should-build` | Latest successful enrichment recommends `should_build`. |
| `reading-queue` | Latest successful enrichment recommends `reading_queue`. |
| `reference-only` | Latest successful enrichment recommends `reference_only`. |
| `discard` | Latest successful enrichment recommends `discard`. |

Underscore aliases are also accepted for multi-word views, such as `should_build` and `reference_only`.

Options:

| Option | Default | Description |
| --- | --- | --- |
| `-l, --limit <LIMIT>` | `20` | Number of items to show. |
| `-o, --offset <OFFSET>` | `0` | Offset for pagination. |
| `--json` | false | Emit newline-delimited JSON records. |

Read-only archive operation.

## `export`

Export agent-ready records as JSONL, Markdown, or CSV.

```bash
rusty-rss export [OPTIONS]
```

Options:

| Option | Default | Description |
| --- | --- | --- |
| `--format <FORMAT>` | `jsonl` | Output format: `jsonl`, `markdown`, or `csv`. |
| `-l, --limit <LIMIT>` | `100` | Number of records to export. |
| `-o, --offset <OFFSET>` | `0` | Offset for pagination. |
| `--classification <CLASSIFICATION>` | none | Filter by classification. |
| `--action <ACTION>` | none | Filter by recommended action. |
| `--min-joy <MIN_JOY>` | none | Filter by minimum joy value. |
| `--min-work <MIN_WORK>` | none | Filter by minimum work value. |

Accepted classifications:

- `article`
- `tool`
- `tutorial`
- `reference`
- `discussion`
- `question`
- `news`
- `other`

Accepted actions:

- `should_test`
- `should_build`
- `reading_queue`
- `reference_only`
- `discard`
- `other`

Read-only archive operation.

## `capture`

Capture outbound page metadata for saved posts.

```bash
rusty-rss capture [OPTIONS]
```

Options:

| Option | Default | Description |
| --- | --- | --- |
| `-l, --limit <LIMIT>` | `20` | Maximum number of uncaptured or failed outbound URLs to process. |

Writes to `outbound_captures`. Uses network.

## `tag`

Tag saved posts by topic with the Gate 1 rule engine. See
[Tag posts](../how-to/tag-posts.md) for the rules format.

```bash
rusty-rss tag [OPTIONS]
```

Options:

| Option | Default | Description |
| --- | --- | --- |
| `--topic <TOPIC>` | all topics | Tag only one topic; other topics' tags are preserved. |
| `--rules <RULES>` | `./rules.toml` | Path to the rules config. Errors if missing. |
| `--limit <LIMIT>` | whole archive | Debug cap on posts processed. |
| `--dry-run` | false | Evaluate and report without writing any rows. |
| `--json` | false | Emit newline-delimited JSON tag records. |

Writes to `post_tags` unless `--dry-run` is set. No network or tokens. The run
is authoritative for its scope: stale tags for processed posts and topics are
removed before fresh tags are written.

## MCP Server CLI

`rusty-rss-mcp` exposes read-only MCP tools over stdio.

```bash
rusty-rss-mcp [--db-path PATH]
```

Options:

| Option | Environment variable | Default | Description |
| --- | --- | --- | --- |
| `-d, --db-path <PATH>` | `RUSTY_RSS_DB_PATH` | `./rusty-rss.sqlite3` | SQLite database path. |
| `-h, --help` | none | none | Print usage. |

Available tools (built on the `rmcp` SDK, each with a JSON Schema for arguments):

- `search`
- `list`
- `show`
- `triage`
