# Enrichment and Triage

Enrichment turns saved posts into structured decision records. Triage turns those records into focused queues.

## Enrichment Goal

The enrichment prompt asks an OpenAI-compatible model to classify a saved Reddit item for a personal archive. The model receives the saved post title, subreddit, author, permalink, outbound URL, and Markdown content.

The output must match a strict JSON schema and is validated before it is stored as a successful enrichment.

## Output Fields

`classification` identifies the kind of saved item:

- `article`
- `tool`
- `tutorial`
- `reference`
- `discussion`
- `question`
- `news`
- `other`

`recommended_action` identifies what to do with the item:

- `should_test`
- `should_build`
- `reading_queue`
- `reference_only`
- `discard`
- `other`

`joy_value`, `work_value`, and `confidence` are finite numbers from `0.0` to `1.0`.

`summary` and `rationale` must be non-empty.

## Attempt Model

Each enrichment attempt appends a row to `enrichment_runs`.

Success rows store:

- provider
- model
- prompt version
- raw response
- normalized output fields

Error rows store:

- provider
- model
- prompt version
- error message

The batch continues after individual item failures.

## Candidate Selection

`enrich` selects saved posts that do not have a successful enrichment row. A post with a failed enrichment remains eligible for a later run.

Use dry run to preview the candidate count:

```bash
rusty-rss enrich --dry-run --limit 100
```

## Triage Views

Triage views use each post's latest enrichment row.

| View | Selection |
| --- | --- |
| `all` | All saved posts. |
| `unprocessed` | No successful enrichment exists. |
| `high-value` | Latest success has `joy_value >= 0.7` or `work_value >= 0.7`. |
| `should-test` | Latest success recommends `should_test`. |
| `should-build` | Latest success recommends `should_build`. |
| `reading-queue` | Latest success recommends `reading_queue`. |
| `reference-only` | Latest success recommends `reference_only`. |
| `discard` | Latest success recommends `discard`. |

The multi-word views also accept underscore aliases, such as `should_build`.

## Export Filters

Export filters use the latest enrichment row:

```bash
rusty-rss export --format jsonl --classification tutorial
rusty-rss export --format jsonl --action should_build
rusty-rss export --format jsonl --min-work 0.8
```

Items without matching latest enrichment values are excluded by those filters.
