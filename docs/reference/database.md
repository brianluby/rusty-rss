# Database Reference

`rusty-rss` stores data in SQLite. The default database path is `./rusty-rss.sqlite3`, configurable with `--db-path` or `RUSTY_RSS_DB_PATH`.

The schema is initialized lazily whenever a command opens the database.

## `saved_posts`

Canonical saved Reddit items keyed by `reddit_fullname`.

| Column | Description |
| --- | --- |
| `reddit_fullname` | Stable Reddit fullname, such as `t3_abc123` or `t1_comment123`. Primary key. |
| `reddit_id` | Fullname without the known prefix when present. |
| `title` | Saved item title. |
| `author` | Author name without `u/`, when available. |
| `subreddit` | Subreddit name without `r/`, when available. |
| `permalink` | Reddit permalink from the feed entry. |
| `outbound_url` | First non-Reddit outbound link extracted from entry content, when available. |
| `content_markdown` | Feed entry content normalized to Markdown. |
| `thumbnail_url` | Media thumbnail or enclosure URL, when available. |
| `published_at` | Source publication timestamp, when available. |
| `updated_at` | Source update timestamp, when available. |
| `first_seen_at` | Time this item was first inserted. |
| `last_seen_at` | Time this item was most recently observed by sync. |
| `source` | Source label, currently `atom`. |
| `raw_entry` | Reserved raw source storage column. |

Indexes exist for subreddit, author, publication time, and last-seen time.

## `sync_runs`

History of sync attempts.

| Column | Description |
| --- | --- |
| `id` | Autoincrement primary key. |
| `started_at` | Sync start timestamp. |
| `finished_at` | Sync finish timestamp, when available. |
| `source_url` | Feed URL used for the run. |
| `status` | `running`, `success`, or `error`. |
| `fetched_count` | Feed entries fetched. |
| `inserted_count` | Saved posts inserted. |
| `updated_count` | Saved posts updated. |
| `error` | Run-level error, when the sync fails. |

## `enrichment_runs`

Append-only enrichment attempts for saved posts.

| Column | Description |
| --- | --- |
| `id` | Autoincrement primary key. |
| `reddit_fullname` | Saved post foreign key. |
| `provider` | Provider label, such as `openai-compatible`. |
| `model` | Model ID used for the attempt. |
| `prompt_version` | Prompt contract version, e.g. `enrich-v2`. |
| `status` | `success` or `error`. |
| `raw_response` | Raw model response content for successful attempts. |
| `classification` | Normalized classification. |
| `tags_json` | JSON array of tags. |
| `summary` | Model summary. |
| `joy_value` | Score from `0.0` to `1.0`. |
| `work_value` | Score from `0.0` to `1.0`. |
| `recommended_action` | Normalized action. |
| `rationale` | Model rationale. |
| `confidence` | Score from `0.0` to `1.0`. |
| `error` | Failure message for error attempts. |
| `created_at` | Attempt timestamp. |

The application treats the highest `id` for a post as the latest enrichment.

## `outbound_captures`

Latest outbound capture metadata per saved post.

| Column | Description |
| --- | --- |
| `reddit_fullname` | Saved post foreign key and primary key. |
| `original_url` | URL selected from the saved post. |
| `final_url` | URL reported by the HTTP client after the request. Redirects are disabled. |
| `canonical_url` | Canonical link from the HTML page, when present. |
| `title` | Open Graph title or document title. |
| `description` | Meta description or Open Graph description. |
| `site_name` | Open Graph site name. |
| `preview_image_url` | Open Graph image URL. |
| `content_markdown` | HTML converted to Markdown. |
| `content_hash` | `sha256:` hash of `content_markdown`. |
| `status` | `success` or `error`. |
| `http_status` | HTTP status for successful captures. |
| `error` | Failure message for failed captures. |
| `fetched_at` | Capture attempt timestamp. |
| `attempt_count` | Number of capture attempts for the post. |

Each new capture for a post upserts this table and increments `attempt_count`.

## `post_tags`

Materialized Gate 1 rule-engine tags. One row per `(post, topic)` that scored,
keyed by `(reddit_fullname, topic)` for multi-label tagging.

| Column | Description |
| --- | --- |
| `reddit_fullname` | Saved post foreign key. Part of the primary key. |
| `topic` | Topic name from `rules.toml`, such as `memory`. Part of the primary key. |
| `score` | Additive weighted score: fired rule weights plus subreddit prior. |
| `threshold` | Topic threshold in effect when the tag was written. |
| `passed` | `1` when `score >= threshold` and no veto fired, else `0`. |
| `matched_rules` | JSON array of provenance: fired rule ids, `prior:<subreddit>`, `veto:<id>`. |
| `signals` | JSON object: per-signal score breakdown keyed by rule id and `prior`. |
| `ruleset_version` | `[meta].version` from the rules file that produced the row. |
| `tagged_at` | Tagging run timestamp. |

A row exists only when at least one scoring rule fired (near-misses included); a
subreddit prior alone never creates a row. Indexes exist for `(topic, passed)`
and `(topic, score DESC)`. The `tag` command deletes the processed scope and
re-inserts within a transaction, so that scope reflects the current rules — a
full run covers the whole archive, while a `--limit` run refreshes only the
processed subset.

## `posts_fts`

SQLite FTS5 virtual table over saved post titles and Markdown content.

Search behavior:

- Tokenizer: `porter unicode61`.
- Content table: `saved_posts`.
- Title matches are weighted above Markdown body matches.
- Insert, update, and delete triggers keep the index synchronized.
- An empty FTS index is rebuilt automatically when saved posts already exist.

## Command Writes

| Command | Tables written |
| --- | --- |
| `sync` | `saved_posts`, `sync_runs`, `posts_fts` via triggers |
| `enrich` | `enrichment_runs` |
| `capture` | `outbound_captures` |
| `tag` | `post_tags` |
| `list` | none |
| `show` | none |
| `search` | none |
| `triage` | none |
| `export` | none |

Opening the database may initialize or migrate schema objects before the command-specific behavior runs.
