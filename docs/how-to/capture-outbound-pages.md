# Capture Outbound Pages

Use `capture` to fetch metadata and readable Markdown snapshots from saved posts that have an outbound URL.

## Run Capture

```bash
rusty-rss capture --limit 20
```

The command selects saved posts with `outbound_url` where there is no successful capture yet, or where the latest capture status is not `success`.

## What Capture Stores

For each successful outbound page, `capture` stores:

- original URL
- final URL reported by the HTTP client
- canonical URL when present
- title
- description
- site name
- preview image URL
- readable Markdown snapshot
- `sha256:` content hash
- HTTP status
- fetch timestamp
- attempt count

Failures are stored with status `error`, the error message, fetch timestamp, and updated attempt count.

## Network Defaults

Capture is intentionally conservative:

- Only `http` and `https` URLs are accepted.
- Redirects are not followed.
- `localhost`, private IP ranges, link-local hosts, documentation IPs, broadcast addresses, and unspecified addresses are blocked by default.
- Responses must be HTML.
- The response body is limited to 1 MiB.
- Each request has a 20-second timeout.
- The batch uses a default maximum concurrency of 4.
- Transient failures are retried up to 3 attempts.

The public CLI exposes `--limit`. Other capture options are core-library defaults.

## Inspect Captured Data

Use export to see captured metadata with saved posts:

```bash
rusty-rss export --format jsonl --limit 20
```

Each record includes `outbound_capture` when a capture row exists.

Use CSV for a compact metadata table:

```bash
rusty-rss export --format csv --limit 100
```
