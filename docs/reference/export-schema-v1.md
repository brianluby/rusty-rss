# Export Schema v1

`rusty-rss.export.v1` is the stable JSONL envelope emitted by:

```bash
rusty-rss export --format jsonl
```

Each JSONL line is one complete record.

## Envelope

```json
{
  "schema_version": "rusty-rss.export.v1",
  "saved_post": {},
  "latest_enrichment": null,
  "outbound_capture": null
}
```

## Stability Contract

- `schema_version` is required and must equal `rusty-rss.export.v1`.
- `saved_post` is always present.
- `saved_post.reddit_fullname` is the stable item ID.
- `latest_enrichment` is present as an object or `null`.
- `outbound_capture` is present as an object or `null`.
- Timestamps are RFC3339 strings where present.
- Consumers should ignore unknown additive fields only after checking `schema_version`.
- Breaking field changes require a new schema version.

## `saved_post`

| Field | Type | Description |
| --- | --- | --- |
| `reddit_fullname` | string | Stable Reddit fullname. |
| `reddit_id` | string | Fullname without known prefix when present. |
| `title` | string | Saved item title. |
| `author` | string or null | Author name without `u/`. |
| `subreddit` | string or null | Subreddit name without `r/`. |
| `permalink` | string | Reddit permalink. |
| `outbound_url` | string or null | First non-Reddit outbound URL extracted from content. |
| `content_markdown` | string or null | Saved feed content converted to Markdown. |
| `thumbnail_url` | string or null | Thumbnail or enclosure URL. |
| `published_at` | string or null | Source publication timestamp. |
| `updated_at` | string or null | Source update timestamp. |
| `source` | string | Source label, currently `atom`. |

## `latest_enrichment`

`latest_enrichment` is `null` when no enrichment row exists for the post.

| Field | Type | Description |
| --- | --- | --- |
| `id` | integer | Enrichment row ID. |
| `reddit_fullname` | string | Saved post ID. |
| `provider` | string | Provider label. |
| `model` | string | Model ID. |
| `prompt_version` | string | Prompt contract version. |
| `status` | string | `success` or `error`. |
| `raw_response` | string or null | Raw model content for success rows. |
| `output` | object or null | Normalized model output for success rows. |
| `error` | string or null | Failure message for error rows. |
| `created_at` | string | Enrichment timestamp. |

### `latest_enrichment.output`

| Field | Type | Description |
| --- | --- | --- |
| `classification` | string | One of the accepted classifications. |
| `tags` | array of strings | Model-provided tags. |
| `summary` | string | Short summary. |
| `joy_value` | number | Finite score from `0.0` to `1.0`. |
| `work_value` | number | Finite score from `0.0` to `1.0`. |
| `recommended_action` | string | One of the accepted recommended actions. |
| `rationale` | string | Reason for the recommendation. |
| `confidence` | number | Finite score from `0.0` to `1.0`. |

Accepted classifications:

- `article`
- `tool`
- `tutorial`
- `reference`
- `discussion`
- `question`
- `news`
- `other`

Accepted recommended actions:

- `should_test`
- `should_build`
- `reading_queue`
- `reference_only`
- `discard`
- `other`

## `outbound_capture`

`outbound_capture` is `null` when no capture row exists for the post.

| Field | Type | Description |
| --- | --- | --- |
| `reddit_fullname` | string | Saved post ID. |
| `original_url` | string | URL selected from the saved post. |
| `final_url` | string or null | URL reported by the HTTP client. |
| `canonical_url` | string or null | Canonical URL from page metadata. |
| `title` | string or null | Captured page title. |
| `description` | string or null | Captured page description. |
| `site_name` | string or null | Captured site name. |
| `preview_image_url` | string or null | Captured preview image URL. |
| `content_markdown` | string or null | Captured HTML converted to Markdown. |
| `content_hash` | string or null | `sha256:` hash of captured Markdown. |
| `status` | string | `success` or `error`. |
| `http_status` | integer or null | HTTP status for successful captures. |
| `error` | string or null | Failure message for error captures. |
| `fetched_at` | string | Capture timestamp. |
| `attempt_count` | integer | Number of capture attempts for the post. |

## Projections

`rusty-rss export --format markdown` and `rusty-rss export --format csv` are projections of this envelope. Use JSONL when a consumer needs the stable machine-readable contract.

## Sample

See [docs/export-record-v1.sample.json](../export-record-v1.sample.json).
