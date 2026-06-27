//! In-process integration tests for the rmcp-based stdio server.
//!
//! Each test builds a fixture SQLite archive, serves [`RustyRssServer`] over an
//! `rmcp` duplex transport, connects a bare client (`()`), and exercises the
//! tools end-to-end: schema listing, round-trips, and error cases.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use rmcp::RoleClient;
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::service::{RunningService, ServiceError};
use rusty_rss_core::db;
use rusty_rss_core::models::{Classification, EnrichmentOutput, RecommendedAction, SavedPost};
use serde_json::{Map, Value, json};

static COUNTER: AtomicU64 = AtomicU64::new(0);

type Client = RunningService<RoleClient, ()>;

fn unique_db_path() -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "rusty_rss_mcp_it_{}_{}.sqlite3",
        std::process::id(),
        id
    ))
}

/// Build a fixture archive with one enriched (high-value, should-build) post and
/// one unprocessed post, returning its path.
fn fixture_db() -> PathBuf {
    let path = unique_db_path();
    let _ = std::fs::remove_file(&path);
    let conn = db::init_db(&path).expect("init db");

    let mut enriched = SavedPost::new(
        "t3_enriched".to_string(),
        "Rust SQLite FTS Guide".to_string(),
        "https://reddit.com/r/rust/comments/enriched/".to_string(),
        "atom".to_string(),
    );
    enriched.subreddit = Some("rust".to_string());
    enriched.author = Some("ferris".to_string());
    enriched.content_markdown = Some("searchable agent content about sqlite".to_string());
    db::upsert_post(&conn, &enriched).expect("insert enriched");

    let mut plain = SavedPost::new(
        "t3_plain".to_string(),
        "Untouched Post".to_string(),
        "https://reddit.com/r/rust/comments/plain/".to_string(),
        "atom".to_string(),
    );
    plain.subreddit = Some("golang".to_string());
    db::upsert_post(&conn, &plain).expect("insert plain");

    let output = EnrichmentOutput {
        classification: Classification::Tool,
        tags: vec!["rust".to_string(), "sqlite".to_string()],
        summary: "A guide to FTS in SQLite".to_string(),
        joy_value: 0.8,
        work_value: 0.9,
        recommended_action: RecommendedAction::ShouldBuild,
        rationale: "Directly useful".to_string(),
        confidence: 0.95,
    };
    db::record_enrichment_success(
        &conn,
        "t3_enriched",
        "test-provider",
        "test-model",
        "v1",
        "{}",
        &output,
    )
    .expect("record enrichment");

    drop(conn);
    path
}

/// Spawn the server on a background task over a duplex transport and return a
/// connected client.
///
/// The server must run concurrently: its `serve()` blocks on the client's
/// `initialize` handshake, so awaiting it before the client exists would
/// deadlock. The spawned task ends naturally once the client closes the
/// transport (`waiting()` returns).
async fn connect(db_path: PathBuf) -> Client {
    let (server_transport, client_transport) = tokio::io::duplex(8 * 1024);
    tokio::spawn(async move {
        if let Ok(server) = rusty_rss_mcp::RustyRssServer::new(db_path)
            .serve(server_transport)
            .await
        {
            let _ = server.waiting().await;
        }
    });
    ().serve(client_transport)
        .await
        .expect("client should connect")
}

fn args(value: Value) -> Map<String, Value> {
    value.as_object().expect("object args").clone()
}

fn first_text(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.clone())
        .expect("text content")
}

#[tokio::test]
async fn lists_read_only_tools_with_valid_schemas() {
    let client = connect(fixture_db()).await;

    let tools = client.list_all_tools().await.expect("list tools");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert!(names.contains(&"search"), "got {names:?}");
    assert!(names.contains(&"list"), "got {names:?}");
    assert!(names.contains(&"show"), "got {names:?}");
    assert!(names.contains(&"triage"), "got {names:?}");
    // Read-only boundary: no mutation tools are exposed.
    for forbidden in ["enrich", "sync", "capture", "delete", "upsert"] {
        assert!(!names.contains(&forbidden), "unexpected tool {forbidden}");
    }

    // Every tool advertises a valid object input schema.
    for tool in &tools {
        let schema = &tool.input_schema;
        assert_eq!(
            schema.get("type").and_then(Value::as_str),
            Some("object"),
            "{} schema must be an object: {schema:?}",
            tool.name
        );
        assert!(
            schema.contains_key("properties"),
            "{} schema must declare properties",
            tool.name
        );
    }

    // The search schema must require the `query` parameter.
    let search = tools.iter().find(|t| t.name == "search").unwrap();
    let required = search
        .input_schema
        .get("required")
        .and_then(Value::as_array);
    let required: Vec<&str> = required
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    assert!(required.contains(&"query"), "search must require query");

    client.cancel().await.expect("client close");
}

#[tokio::test]
async fn search_round_trips() {
    let client = connect(fixture_db()).await;

    let result = client
        .call_tool(
            CallToolRequestParams::new("search")
                .with_arguments(args(json!({ "query": "sqlite", "subreddit": "rust" }))),
        )
        .await
        .expect("search call");
    assert_ne!(result.is_error, Some(true));
    let text = first_text(&result);
    assert!(text.contains("t3_enriched"), "got {text}");
    assert!(
        !text.contains("t3_plain"),
        "subreddit filter failed: {text}"
    );

    client.cancel().await.ok();
}

#[tokio::test]
async fn list_round_trips() {
    let client = connect(fixture_db()).await;

    let result = client
        .call_tool(CallToolRequestParams::new("list").with_arguments(args(json!({ "limit": 10 }))))
        .await
        .expect("list call");
    let text = first_text(&result);
    assert!(text.contains("t3_enriched"), "got {text}");
    assert!(text.contains("t3_plain"), "got {text}");

    client.cancel().await.ok();
}

#[tokio::test]
async fn show_round_trips_and_returns_null_for_missing() {
    let client = connect(fixture_db()).await;

    let found = client
        .call_tool(
            CallToolRequestParams::new("show")
                .with_arguments(args(json!({ "fullname": "t3_enriched" }))),
        )
        .await
        .expect("show call");
    assert!(first_text(&found).contains("Rust SQLite FTS Guide"));

    let missing = client
        .call_tool(
            CallToolRequestParams::new("show")
                .with_arguments(args(json!({ "fullname": "t3_nope" }))),
        )
        .await
        .expect("show missing call");
    assert_eq!(first_text(&missing).trim(), "null");

    client.cancel().await.ok();
}

#[tokio::test]
async fn triage_round_trips() {
    let client = connect(fixture_db()).await;

    let high = client
        .call_tool(
            CallToolRequestParams::new("triage")
                .with_arguments(args(json!({ "view": "high-value" }))),
        )
        .await
        .expect("triage high-value");
    let high_text = first_text(&high);
    assert!(high_text.contains("t3_enriched"), "got {high_text}");
    assert!(!high_text.contains("t3_plain"), "got {high_text}");

    let unprocessed = client
        .call_tool(
            CallToolRequestParams::new("triage")
                .with_arguments(args(json!({ "view": "unprocessed" }))),
        )
        .await
        .expect("triage unprocessed");
    let unprocessed_text = first_text(&unprocessed);
    assert!(
        unprocessed_text.contains("t3_plain"),
        "got {unprocessed_text}"
    );
    assert!(
        !unprocessed_text.contains("t3_enriched"),
        "got {unprocessed_text}"
    );

    client.cancel().await.ok();
}

#[tokio::test]
async fn unknown_tool_is_an_error() {
    let client = connect(fixture_db()).await;

    let err = client
        .call_tool(CallToolRequestParams::new("no_such_tool"))
        .await
        .expect_err("unknown tool should error");
    assert!(matches!(err, ServiceError::McpError(_)), "got {err:?}");

    client.cancel().await.ok();
}

#[tokio::test]
async fn invalid_arguments_are_rejected() {
    let client = connect(fixture_db()).await;

    // Missing the required `query` field. rmcp's argument deserialization
    // surfaces this as a tool-level error result (is_error = true) rather than a
    // JSON-RPC protocol error, so the call still resolves to Ok.
    let result = client
        .call_tool(CallToolRequestParams::new("search").with_arguments(args(json!({}))))
        .await
        .expect("call resolves");
    assert_eq!(result.is_error, Some(true), "got {result:?}");
    assert!(
        first_text(&result).contains("query"),
        "error should mention the missing field: {result:?}"
    );

    // An unknown triage view is rejected by our explicit validation, which
    // returns ErrorData and therefore surfaces as a JSON-RPC protocol error.
    let bad_view = client
        .call_tool(
            CallToolRequestParams::new("triage")
                .with_arguments(args(json!({ "view": "not-a-view" }))),
        )
        .await
        .expect_err("bad view should error");
    assert!(
        matches!(bad_view, ServiceError::McpError(_)),
        "got {bad_view:?}"
    );

    client.cancel().await.ok();
}

#[tokio::test]
async fn missing_database_fails_tool_calls() {
    let missing = unique_db_path();
    let _ = std::fs::remove_file(&missing);
    let client = connect(missing).await;

    let err = client
        .call_tool(CallToolRequestParams::new("list").with_arguments(args(json!({}))))
        .await
        .expect_err("missing db should error");
    assert!(matches!(err, ServiceError::McpError(_)), "got {err:?}");

    client.cancel().await.ok();
}
