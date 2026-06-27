# Export Agent Records

Use `export` to produce records for agents, scripts, readers, or spreadsheets.

## Export JSONL

```bash
rusty-rss export --format jsonl --limit 100
```

Each line is one `rusty-rss.export.v1` envelope:

```json
{"schema_version":"rusty-rss.export.v1","saved_post":{},"latest_enrichment":null,"outbound_capture":null}
```

Use JSONL when a consumer needs the stable machine-readable contract.

## Export Markdown

```bash
rusty-rss export --format markdown --limit 20
```

Markdown output is a readable projection. It includes the saved title, IDs, links, enrichment summary when present, capture metadata when present, and saved Markdown body.

## Export CSV

```bash
rusty-rss export --format csv --limit 100
```

CSV output includes one header row and fields useful for sorting or filtering in spreadsheets.

## Filter Exports

Filter by classification:

```bash
rusty-rss export --format jsonl --classification tutorial
```

Filter by recommended action:

```bash
rusty-rss export --format jsonl --action should_build
```

Filter by minimum scores:

```bash
rusty-rss export --format csv --min-joy 0.7
rusty-rss export --format csv --min-work 0.8
```

Combine filters:

```bash
rusty-rss export --format jsonl --action should_build --min-work 0.8 --limit 50
```

Filters use the latest enrichment row joined to each saved post.

## Paginate Exports

```bash
rusty-rss export --format jsonl --limit 100 --offset 100
```

Records are ordered by saved post `last_seen_at` descending.

## Use the Schema Reference

- [Export schema v1](../reference/export-schema-v1.md)
- [Validating sample](../export-record-v1.sample.json)
