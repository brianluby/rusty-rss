# rusty-rss

Download Reddit saved posts from an Atom feed into a SQLite database.

## Usage

```bash
RUSTY_RSS_FEED_URL="https://old.reddit.com/saved.rss?feed=...&user=..." \
  rusty-rss sync

rusty-rss list --limit 50
rusty-rss show t3_abc123
```

## Configuration

| Variable | Flag | Default | Description |
|---|---|---|---|
| `RUSTY_RSS_FEED_URL` | `--feed-url` | (required) | Atom feed URL |
| `RUSTY_RSS_DB_PATH` | `--db-path` / `-d` | `./rusty-rss.sqlite3` | SQLite database path |

The feed URL is required for `sync` via env var or `--feed-url`.
`list` and `show` only need the database path.

## Commands

- **sync** — Fetch the Atom feed and upsert saved posts into SQLite. Idempotent: safe to run repeatedly.
- **list** — List saved posts with `--limit` and `--offset`.
- **show** — Show full details of a post by Reddit fullname (e.g. `t3_abc123`).

## Database Schema

- `saved_posts` — Posts keyed by `reddit_fullname` with title, author, subreddit, permalink, content, timestamps.
- `sync_runs` — History of sync operations with status and counts.

## Build

```bash
cargo build --release
```

## Tests

```bash
cargo test
```

Tests use offline Atom fixtures in `test-fixtures/`.

## License

MIT
