# Sync Saved Posts

Use `sync` to fetch Reddit saved items from an RSS or Atom feed and write them into the local SQLite archive.

## Configure the Feed URL

Set the feed URL with an environment variable:

```bash
export RUSTY_RSS_FEED_URL="https://old.reddit.com/saved.rss?feed=...&user=..."
rusty-rss sync
```

Or pass it on one invocation:

```bash
rusty-rss --feed-url "https://old.reddit.com/saved.rss?feed=...&user=..." sync
```

`sync` fails when no feed URL is available. Other commands can read an existing database without the feed URL.

> **Security: prefer `RUSTY_RSS_FEED_URL` over `--feed-url`.**
> The feed URL embeds a private Reddit `feed` token and `user`. Passing it as a
> command-line flag exposes it in your shell history and in the process list
> (`ps`), where any local user can read it. Setting it as the
> `RUSTY_RSS_FEED_URL` environment variable avoids both leaks. `rusty-rss`
> additionally redacts the token and user before writing to `sync_runs`
> (`source_url` and `error` store host+path only) and before returning or
> logging any sync error, but keeping the raw URL out of your shell history and
> process list is your responsibility.

## Choose the Database

The default database path is `./rusty-rss.sqlite3`.

```bash
rusty-rss --db-path ./archive.sqlite3 sync
```

The same path must be used later when listing, searching, exporting, or serving MCP tools from that archive.

## Control Pagination

By default, `sync` requests `100` items per Reddit page and follows up to `50` pages.

```bash
rusty-rss sync --limit 100 --max-pages 50
```

Use a smaller page count for a quick refresh:

```bash
rusty-rss sync --max-pages 3
```

The command stops early when a page has fewer entries than the requested limit, when no new IDs appear, or when the feed does not provide a usable last entry ID for pagination.

## Run Sync Repeatedly

`sync` is safe to run repeatedly:

- New saved items are inserted.
- Changed items are updated.
- Unchanged items keep their original `first_seen_at` and receive a new `last_seen_at`.
- A row is written to `sync_runs` for each sync attempt.

## Customize the User Agent

Set `RUSTY_RSS_USER_AGENT` when you need a custom request identity:

```bash
RUSTY_RSS_USER_AGENT="my-archive/1.0" rusty-rss sync
```

## Troubleshoot Sync

If `sync` fails before fetching, check that the feed URL is valid and includes the Reddit `feed` and `user` parameters.

If the server responds but the command rejects the response, check the response content type. Feed fetch accepts XML, Atom, RSS, JSON, and `text/plain` content types.

If some entries fail to parse, `sync` still inserts parseable posts and reports entry-level errors in the command output.
