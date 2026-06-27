# Enrich and Triage Saved Posts

Use `enrich` to classify saved posts with an OpenAI-compatible chat completions endpoint. Use `triage` to browse views derived from the latest successful enrichment.

## Configure an OpenAI-Compatible Endpoint

Defaults:

- `RUSTY_RSS_OPENAI_BASE_URL`: `http://127.0.0.1:8080/v1`
- `RUSTY_RSS_OPENAI_MODEL`: `llama.cpp`
- `RUSTY_RSS_OPENAI_API_KEY`: unset

Example:

```bash
export RUSTY_RSS_OPENAI_BASE_URL="http://127.0.0.1:8080/v1"
export RUSTY_RSS_OPENAI_MODEL="llama.cpp"
rusty-rss enrich --limit 20
```

If your endpoint requires authentication:

```bash
export RUSTY_RSS_OPENAI_API_KEY="..."
rusty-rss enrich --limit 20
```

Before writing enrichment rows, `enrich` checks `GET /models` and verifies that the configured model is present.

## Preview Work Without Calling the LLM

```bash
rusty-rss enrich --dry-run --limit 50
```

Dry run only counts unenriched candidates. It does not validate LLM configuration, call the endpoint, or write rows.

## Enrich Saved Posts

```bash
rusty-rss enrich --limit 20
```

The command selects saved posts that do not yet have a successful enrichment. For each item it records either:

- a `success` row with the raw response and normalized fields, or
- an `error` row with the failure message.

Failures do not abort the whole batch.

## Triage Items

List all items with latest enrichment data when available:

```bash
rusty-rss triage all
```

List unenriched items:

```bash
rusty-rss triage unprocessed
```

List high-value items:

```bash
rusty-rss triage high-value
```

List by recommended action:

```bash
rusty-rss triage should-build
rusty-rss triage should-test
rusty-rss triage reading-queue
rusty-rss triage reference-only
rusty-rss triage discard
```

Use JSON output for agents or scripts:

```bash
rusty-rss triage should-build --json --limit 50
```

## Accepted Enrichment Values

Classifications:

- `article`
- `tool`
- `tutorial`
- `reference`
- `discussion`
- `question`
- `news`
- `other`

Recommended actions:

- `should_test`
- `should_build`
- `reading_queue`
- `reference_only`
- `discard`
- `other`

Scores `joy_value`, `work_value`, and `confidence` must be finite numbers from `0.0` to `1.0`.

See [Enrichment and triage](../explanation/enrichment-and-triage.md) for the model and view behavior.
