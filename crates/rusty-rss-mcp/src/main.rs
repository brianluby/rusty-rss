use anyhow::{Context, Result, anyhow};
use rusty_rss_core::db::{self, SearchFilters};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

fn main() -> Result<()> {
    let db_path = parse_db_path()?;
    ensure_db_exists(&db_path)?;
    run_server(std::io::stdin().lock(), std::io::stdout().lock(), db_path)
}

/// Fail fast when the database is missing instead of silently serving a freshly
/// created, empty archive because `--db-path` / `RUSTY_RSS_DB_PATH` is wrong.
fn ensure_db_exists(db_path: &Path) -> Result<()> {
    // Require an existing file specifically: a missing path or a directory both
    // produce a clear error here instead of a cryptic SQLite open failure later.
    if !db_path.is_file() {
        return Err(anyhow!(
            "database file not found at {}; run `rusty-rss sync` first or pass a correct --db-path",
            db_path.display()
        ));
    }
    Ok(())
}

fn parse_db_path() -> Result<PathBuf> {
    let mut args = std::env::args().skip(1);
    let mut db_path = std::env::var("RUSTY_RSS_DB_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./rusty-rss.sqlite3"));

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--db-path" | "-d" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("{arg} requires a database path"))?;
                db_path = PathBuf::from(value);
            }
            "--help" | "-h" => {
                eprintln!("Usage: rusty-rss-mcp [--db-path PATH]");
                std::process::exit(0);
            }
            _ => return Err(anyhow!("unknown argument: {arg}")),
        }
    }

    Ok(db_path)
}

fn run_server<R: Read, W: Write>(input: R, mut output: W, db_path: PathBuf) -> Result<()> {
    let mut reader = BufReader::new(input);
    while let Some(message) = read_message(&mut reader)? {
        match serde_json::from_slice::<Request>(&message) {
            Ok(request) => {
                if let Some(response) = handle_request(&db_path, request) {
                    write_message(&mut output, &response)?;
                }
            }
            Err(err) => {
                // A malformed request must not tear down the session: reply with
                // a JSON-RPC parse error (id null, per spec) and keep serving.
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32700, "message": format!("parse error: {err}") }
                });
                write_message(&mut output, &response)?;
            }
        }
    }
    Ok(())
}

fn read_message<R: BufRead>(reader: &mut R) -> Result<Option<Vec<u8>>> {
    let mut content_length = None;

    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return Ok(None);
        }

        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }

        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .context("invalid Content-Length header")?,
            );
        }
    }

    let content_length = content_length.ok_or_else(|| anyhow!("missing Content-Length header"))?;
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

fn write_message<W: Write>(writer: &mut W, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct Request {
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

fn handle_request(db_path: &Path, request: Request) -> Option<Value> {
    let id = request.id?;
    let result = match request.method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "rusty-rss-mcp", "version": env!("CARGO_PKG_VERSION") }
        })),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => call_tool(db_path, &request.params),
        _ => Err((-32601, format!("method not found: {}", request.method))),
    };

    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    })
}

fn tools() -> Value {
    json!([
        {
            "name": "search_posts",
            "description": "Search saved Reddit posts by title and markdown content.",
            "inputSchema": search_schema()
        },
        {
            "name": "query_posts",
            "description": "Alias for search_posts for agent query workflows.",
            "inputSchema": search_schema()
        },
        {
            "name": "list_posts",
            "description": "List saved Reddit posts ordered by last seen time.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "minimum": 0, "maximum": 100, "default": 20 },
                    "offset": { "type": "integer", "minimum": 0, "default": 0 }
                }
            }
        },
        {
            "name": "show_post",
            "description": "Show one saved Reddit post by reddit fullname.",
            "inputSchema": {
                "type": "object",
                "required": ["fullname"],
                "properties": { "fullname": { "type": "string" } }
            }
        }
    ])
}

fn search_schema() -> Value {
    json!({
        "type": "object",
        "required": ["query"],
        "properties": {
            "query": { "type": "string" },
            "limit": { "type": "integer", "minimum": 0, "maximum": 100, "default": 20 },
            "subreddit": { "type": "string" },
            "author": { "type": "string" }
        }
    })
}

fn call_tool(db_path: &Path, params: &Value) -> std::result::Result<Value, (i64, String)> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| (-32602, "tools/call requires a tool name".to_string()))?;
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let conn = db::init_db(db_path).map_err(internal_error)?;

    let result = match name {
        "search_posts" | "query_posts" => {
            let args: SearchArgs = serde_json::from_value(args).map_err(invalid_params)?;
            let hits = db::search_posts(&conn, &args.query, &args.filters(), args.limit())
                .map_err(invalid_params)?;
            serde_json::to_value(hits).map_err(internal_error)?
        }
        "list_posts" => {
            let args: ListArgs = serde_json::from_value(args).map_err(invalid_params)?;
            let posts =
                db::list_posts(&conn, args.limit(), args.offset()).map_err(internal_error)?;
            serde_json::to_value(posts).map_err(internal_error)?
        }
        "show_post" => {
            let args: ShowArgs = serde_json::from_value(args).map_err(invalid_params)?;
            // Propagate a serialization failure as an internal error instead of
            // collapsing it to null, which is indistinguishable from "not found".
            match db::get_post(&conn, &args.fullname).map_err(internal_error)? {
                Some(post) => serde_json::to_value(post).map_err(internal_error)?,
                None => Value::Null,
            }
        }
        _ => return Err((-32602, format!("unknown tool: {name}"))),
    };

    Ok(json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&result).map_err(internal_error)? }]
    }))
}

#[derive(Debug, Deserialize)]
struct SearchArgs {
    query: String,
    limit: Option<usize>,
    subreddit: Option<String>,
    author: Option<String>,
}

impl SearchArgs {
    fn limit(&self) -> usize {
        self.limit.unwrap_or(20).min(100)
    }

    fn filters(&self) -> SearchFilters {
        SearchFilters {
            subreddit: self.subreddit.clone(),
            author: self.author.clone(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ListArgs {
    limit: Option<usize>,
    offset: Option<usize>,
}

impl ListArgs {
    fn limit(&self) -> usize {
        self.limit.unwrap_or(20).min(100)
    }

    fn offset(&self) -> usize {
        self.offset.unwrap_or(0)
    }
}

#[derive(Debug, Deserialize)]
struct ShowArgs {
    fullname: String,
}

fn invalid_params(err: impl std::fmt::Display) -> (i64, String) {
    (-32602, err.to_string())
}

fn internal_error(err: impl std::fmt::Display) -> (i64, String) {
    (-32603, err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusty_rss_core::models::SavedPost;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_db_path() -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "rusty_rss_mcp_test_{}_{}.db",
            std::process::id(),
            id
        ))
    }

    #[test]
    fn tools_list_returns_read_only_tools() {
        let response = handle_request(
            &test_db_path(),
            Request {
                id: Some(json!(1)),
                method: "tools/list".to_string(),
                params: Value::Null,
            },
        )
        .expect("request should produce response");

        let tools = response["result"]["tools"].as_array().expect("tools array");
        let names = tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"search_posts"));
        assert!(names.contains(&"query_posts"));
        assert!(names.contains(&"list_posts"));
        assert!(names.contains(&"show_post"));
    }

    fn frame(body: &str) -> String {
        format!("Content-Length: {}\r\n\r\n{}", body.len(), body)
    }

    #[test]
    fn malformed_request_does_not_kill_session() {
        // A bad frame followed by a valid one: the bad frame must get a parse
        // error and the session must keep serving the valid request.
        let input = format!(
            "{}{}",
            frame("{ not valid json"),
            frame(r#"{"jsonrpc":"2.0","id":7,"method":"initialize"}"#)
        );
        let mut output = Vec::new();
        run_server(
            std::io::Cursor::new(input.into_bytes()),
            &mut output,
            test_db_path(),
        )
        .expect("server loop should survive a malformed request");

        let out = String::from_utf8(output).expect("utf8 output");
        assert!(
            out.contains("-32700"),
            "parse error should be reported: {out}"
        );
        assert!(
            out.contains("protocolVersion"),
            "session should continue and answer initialize: {out}"
        );
        assert!(
            out.contains("\"id\":7"),
            "the valid request id should be echoed: {out}"
        );
    }

    #[test]
    fn missing_database_path_is_rejected() {
        let missing = std::env::temp_dir().join(format!(
            "rusty_rss_mcp_missing_{}_{}.sqlite3",
            std::process::id(),
            TEST_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&missing);

        let err = ensure_db_exists(&missing).expect_err("missing db should error");
        assert!(
            err.to_string().contains("database file not found"),
            "got: {err}"
        );

        // A directory at the path is also rejected (not a usable database file).
        let dir = std::env::temp_dir();
        let dir_err = ensure_db_exists(&dir).expect_err("a directory should error");
        assert!(dir_err.to_string().contains("database file not found"));
    }

    #[test]
    fn search_tool_uses_core_search() {
        let db_path = test_db_path();
        let conn = db::init_db(&db_path).expect("db should init");
        let mut post = SavedPost::new(
            "t3_mcp".to_string(),
            "MCP Search Target".to_string(),
            "https://reddit.com/r/rust/comments/mcp/item/".to_string(),
            "atom".to_string(),
        );
        post.subreddit = Some("rust".to_string());
        post.content_markdown = Some("agent searchable content".to_string());
        db::upsert_post(&conn, &post).expect("post should insert");
        drop(conn);

        let response = handle_request(
            &db_path,
            Request {
                id: Some(json!(1)),
                method: "tools/call".to_string(),
                params: json!({
                    "name": "search_posts",
                    "arguments": { "query": "searchable", "subreddit": "rust" }
                }),
            },
        )
        .expect("request should produce response");

        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("text content");
        assert!(text.contains("t3_mcp"));
    }

    // ── read_message tests ────────────────────────────────────────────────────

    #[test]
    fn read_message_returns_none_on_empty_input() {
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(b"".to_vec()));
        let result = read_message(&mut reader).expect("should not error on EOF");
        assert!(result.is_none(), "empty input should return None");
    }

    #[test]
    fn read_message_parses_valid_frame() {
        let body = b"hello world";
        let input = format!("Content-Length: {}\r\n\r\n", body.len());
        let mut combined = input.into_bytes();
        combined.extend_from_slice(body);
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(combined));

        let result = read_message(&mut reader)
            .expect("should succeed")
            .expect("should have message");
        assert_eq!(result, b"hello world");
    }

    #[test]
    fn read_message_handles_lf_only_line_endings() {
        // Some clients may send LF-only headers instead of CRLF.
        let body = b"{}";
        let input = format!("Content-Length: {}\n\n", body.len());
        let mut combined = input.into_bytes();
        combined.extend_from_slice(body);
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(combined));

        let result = read_message(&mut reader)
            .expect("should succeed")
            .expect("should have message");
        assert_eq!(result, b"{}");
    }

    #[test]
    fn read_message_errors_on_missing_content_length() {
        // A line that is not Content-Length but not empty either.
        let input = b"X-Custom-Header: value\r\n\r\n";
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(input.to_vec()));
        let err = read_message(&mut reader).expect_err("should error without Content-Length");
        assert!(
            err.to_string().contains("missing Content-Length"),
            "got: {err}"
        );
    }

    #[test]
    fn read_message_errors_on_non_numeric_content_length() {
        let input = b"Content-Length: abc\r\n\r\n";
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(input.to_vec()));
        let err =
            read_message(&mut reader).expect_err("non-numeric Content-Length should error");
        assert!(
            err.to_string().contains("invalid Content-Length"),
            "got: {err}"
        );
    }

    #[test]
    fn read_message_reads_multiple_sequential_messages() {
        let first = b"first";
        let second = b"second body";
        let input = format!(
            "Content-Length: {}\r\n\r\n{}Content-Length: {}\r\n\r\n{}",
            first.len(),
            std::str::from_utf8(first).unwrap(),
            second.len(),
            std::str::from_utf8(second).unwrap(),
        );
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(input.into_bytes()));

        let msg1 = read_message(&mut reader)
            .expect("first read ok")
            .expect("first message present");
        assert_eq!(msg1, b"first");

        let msg2 = read_message(&mut reader)
            .expect("second read ok")
            .expect("second message present");
        assert_eq!(msg2, b"second body");

        let eof = read_message(&mut reader).expect("third read ok (EOF)");
        assert!(eof.is_none(), "EOF after two messages");
    }

    // ── write_message tests ───────────────────────────────────────────────────

    #[test]
    fn write_message_produces_valid_lsp_frame() {
        let mut buf = Vec::new();
        let value = json!({"hello": "world"});
        write_message(&mut buf, &value).expect("write should succeed");

        let output = String::from_utf8(buf).expect("utf8");
        assert!(
            output.starts_with("Content-Length:"),
            "must start with Content-Length header: {output}"
        );
        assert!(
            output.contains("\r\n\r\n"),
            "must have blank-line separator: {output}"
        );
        // Extract and parse the body to verify it round-trips.
        let sep = output.find("\r\n\r\n").unwrap();
        let body = &output[sep + 4..];
        let parsed: Value = serde_json::from_str(body).expect("body must be valid JSON");
        assert_eq!(parsed["hello"], "world");
    }

    #[test]
    fn write_message_content_length_matches_body() {
        let mut buf = Vec::new();
        let value = json!({"key": "value"});
        write_message(&mut buf, &value).expect("write should succeed");

        let output = String::from_utf8(buf).expect("utf8");
        let header_end = output.find("\r\n\r\n").unwrap();
        let header_line = &output[..header_end];
        let claimed_len: usize = header_line
            .trim_start_matches("Content-Length:")
            .trim()
            .parse()
            .expect("numeric length");
        let body = &output[header_end + 4..];
        assert_eq!(
            claimed_len,
            body.len(),
            "Content-Length must equal actual body byte count"
        );
    }

    // ── handle_request tests ──────────────────────────────────────────────────

    #[test]
    fn handle_request_notification_returns_none() {
        // JSON-RPC notifications have no id; the server must not send a reply.
        let response = handle_request(
            &test_db_path(),
            Request {
                id: None,
                method: "initialize".to_string(),
                params: Value::Null,
            },
        );
        assert!(
            response.is_none(),
            "notifications (no id) must not produce a response"
        );
    }

    #[test]
    fn handle_request_initialize_returns_protocol_version() {
        let response = handle_request(
            &test_db_path(),
            Request {
                id: Some(json!(42)),
                method: "initialize".to_string(),
                params: Value::Null,
            },
        )
        .expect("initialize must produce a response");

        assert_eq!(response["id"], json!(42));
        assert_eq!(response["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(response["result"]["serverInfo"]["name"], "rusty-rss-mcp");
    }

    #[test]
    fn handle_request_unknown_method_returns_method_not_found() {
        let response = handle_request(
            &test_db_path(),
            Request {
                id: Some(json!("req-1")),
                method: "nonexistent/method".to_string(),
                params: Value::Null,
            },
        )
        .expect("unknown method must still produce a response");

        assert_eq!(response["id"], json!("req-1"));
        assert_eq!(response["error"]["code"], -32601);
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("method not found"),
            "error message should describe the problem: {response}"
        );
    }

    #[test]
    fn handle_request_echoes_request_id() {
        // The id field supports strings, numbers, and null.
        for id in [json!(1), json!("abc"), json!(null)] {
            let response = handle_request(
                &test_db_path(),
                Request {
                    id: Some(id.clone()),
                    method: "initialize".to_string(),
                    params: Value::Null,
                },
            )
            .expect("should produce response");
            assert_eq!(
                response["id"], id,
                "request id must be echoed back unchanged"
            );
        }
    }

    // ── call_tool edge-case tests ─────────────────────────────────────────────

    #[test]
    fn call_tool_missing_name_returns_invalid_params() {
        let response = handle_request(
            &test_db_path(),
            Request {
                id: Some(json!(1)),
                method: "tools/call".to_string(),
                params: json!({ "arguments": {} }),
            },
        )
        .expect("should produce response");

        assert_eq!(response["error"]["code"], -32602);
    }

    #[test]
    fn call_tool_unknown_tool_name_returns_error() {
        let db_path = test_db_path();
        db::init_db(&db_path).expect("init db");

        let response = handle_request(
            &db_path,
            Request {
                id: Some(json!(2)),
                method: "tools/call".to_string(),
                params: json!({ "name": "delete_everything" }),
            },
        )
        .expect("should produce response");

        assert_eq!(response["error"]["code"], -32602);
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("unknown tool"),
            "got: {response}"
        );
    }

    #[test]
    fn query_posts_alias_works_like_search_posts() {
        let db_path = test_db_path();
        let conn = db::init_db(&db_path).expect("db should init");
        let mut post = SavedPost::new(
            "t3_alias".to_string(),
            "Alias Test Post".to_string(),
            "https://reddit.com/r/rust/comments/alias/item/".to_string(),
            "atom".to_string(),
        );
        post.content_markdown = Some("unique alias workflow content".to_string());
        db::upsert_post(&conn, &post).expect("post should insert");
        drop(conn);

        let response = handle_request(
            &db_path,
            Request {
                id: Some(json!(3)),
                method: "tools/call".to_string(),
                params: json!({
                    "name": "query_posts",
                    "arguments": { "query": "alias workflow" }
                }),
            },
        )
        .expect("query_posts should produce response");

        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("text content");
        assert!(text.contains("t3_alias"), "query_posts should find inserted post");
    }

    #[test]
    fn list_posts_tool_returns_content_array() {
        let db_path = test_db_path();
        let conn = db::init_db(&db_path).expect("db should init");
        let post = SavedPost::new(
            "t3_list1".to_string(),
            "List Test Post".to_string(),
            "https://reddit.com/r/test/comments/list1/".to_string(),
            "atom".to_string(),
        );
        db::upsert_post(&conn, &post).expect("post should insert");
        drop(conn);

        let response = handle_request(
            &db_path,
            Request {
                id: Some(json!(4)),
                method: "tools/call".to_string(),
                params: json!({
                    "name": "list_posts",
                    "arguments": { "limit": 10, "offset": 0 }
                }),
            },
        )
        .expect("list_posts should produce response");

        // Response must have content array with a text item.
        let content = response["result"]["content"]
            .as_array()
            .expect("content array");
        assert!(!content.is_empty(), "content should not be empty");
        assert_eq!(content[0]["type"], "text");
        let text = content[0]["text"].as_str().expect("text field");
        assert!(text.contains("t3_list1"), "should include inserted post");
    }

    #[test]
    fn list_posts_tool_uses_default_limit_when_omitted() {
        let db_path = test_db_path();
        db::init_db(&db_path).expect("db should init");

        // No arguments at all – the server should use sensible defaults.
        let response = handle_request(
            &db_path,
            Request {
                id: Some(json!(5)),
                method: "tools/call".to_string(),
                params: json!({ "name": "list_posts" }),
            },
        )
        .expect("list_posts with no args should produce response");

        // An empty db returns null/empty array but must not error.
        assert!(
            response.get("result").is_some(),
            "should have result, not error: {response}"
        );
    }

    #[test]
    fn show_post_returns_post_when_found() {
        let db_path = test_db_path();
        let conn = db::init_db(&db_path).expect("db should init");
        let post = SavedPost::new(
            "t3_showme".to_string(),
            "Show This Post".to_string(),
            "https://reddit.com/r/test/comments/showme/".to_string(),
            "atom".to_string(),
        );
        db::upsert_post(&conn, &post).expect("post should insert");
        drop(conn);

        let response = handle_request(
            &db_path,
            Request {
                id: Some(json!(6)),
                method: "tools/call".to_string(),
                params: json!({
                    "name": "show_post",
                    "arguments": { "fullname": "t3_showme" }
                }),
            },
        )
        .expect("show_post should produce response");

        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("text content");
        assert!(
            text.contains("t3_showme"),
            "response should contain the post fullname"
        );
        assert!(
            text.contains("Show This Post"),
            "response should contain the post title"
        );
    }

    #[test]
    fn show_post_returns_null_when_not_found() {
        let db_path = test_db_path();
        db::init_db(&db_path).expect("db should init");

        let response = handle_request(
            &db_path,
            Request {
                id: Some(json!(7)),
                method: "tools/call".to_string(),
                params: json!({
                    "name": "show_post",
                    "arguments": { "fullname": "t3_doesnotexist" }
                }),
            },
        )
        .expect("show_post for missing post should produce response");

        // The text content must be the JSON serialization of null.
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("text content");
        assert_eq!(text.trim(), "null", "missing post should serialize as null");
    }

    #[test]
    fn show_post_missing_fullname_arg_returns_invalid_params() {
        let db_path = test_db_path();
        db::init_db(&db_path).expect("db should init");

        let response = handle_request(
            &db_path,
            Request {
                id: Some(json!(8)),
                method: "tools/call".to_string(),
                params: json!({
                    "name": "show_post",
                    "arguments": {}
                }),
            },
        )
        .expect("should produce response");

        assert_eq!(
            response["error"]["code"], -32602,
            "missing required field should be invalid params"
        );
    }

    // ── SearchArgs / ListArgs helper tests ────────────────────────────────────

    #[test]
    fn search_args_limit_defaults_to_20() {
        let args = SearchArgs {
            query: "test".to_string(),
            limit: None,
            subreddit: None,
            author: None,
        };
        assert_eq!(args.limit(), 20);
    }

    #[test]
    fn search_args_limit_caps_at_100() {
        let args = SearchArgs {
            query: "test".to_string(),
            limit: Some(9999),
            subreddit: None,
            author: None,
        };
        assert_eq!(args.limit(), 100);
    }

    #[test]
    fn search_args_limit_respects_explicit_value() {
        let args = SearchArgs {
            query: "test".to_string(),
            limit: Some(42),
            subreddit: None,
            author: None,
        };
        assert_eq!(args.limit(), 42);
    }

    #[test]
    fn search_args_filters_maps_fields() {
        let args = SearchArgs {
            query: "test".to_string(),
            limit: None,
            subreddit: Some("programming".to_string()),
            author: Some("alice".to_string()),
        };
        let filters = args.filters();
        assert_eq!(filters.subreddit.as_deref(), Some("programming"));
        assert_eq!(filters.author.as_deref(), Some("alice"));
    }

    #[test]
    fn search_args_filters_none_when_not_set() {
        let args = SearchArgs {
            query: "test".to_string(),
            limit: None,
            subreddit: None,
            author: None,
        };
        let filters = args.filters();
        assert!(filters.subreddit.is_none());
        assert!(filters.author.is_none());
    }

    #[test]
    fn list_args_limit_defaults_to_20() {
        let args = ListArgs {
            limit: None,
            offset: None,
        };
        assert_eq!(args.limit(), 20);
    }

    #[test]
    fn list_args_limit_caps_at_100() {
        let args = ListArgs {
            limit: Some(200),
            offset: None,
        };
        assert_eq!(args.limit(), 100);
    }

    #[test]
    fn list_args_offset_defaults_to_0() {
        let args = ListArgs {
            limit: None,
            offset: None,
        };
        assert_eq!(args.offset(), 0);
    }

    #[test]
    fn list_args_offset_respects_explicit_value() {
        let args = ListArgs {
            limit: None,
            offset: Some(50),
        };
        assert_eq!(args.offset(), 50);
    }

    // ── ensure_db_exists success path ─────────────────────────────────────────

    #[test]
    fn ensure_db_exists_succeeds_for_real_file() {
        let db_path = test_db_path();
        // Create a real (empty) file at the path.
        std::fs::write(&db_path, b"").expect("should create temp file");
        let result = ensure_db_exists(&db_path);
        std::fs::remove_file(&db_path).ok();
        result.expect("existing file should pass ensure_db_exists");
    }

    // ── error helper tests ────────────────────────────────────────────────────

    #[test]
    fn invalid_params_produces_correct_error_code() {
        let (code, msg) = invalid_params("bad value");
        assert_eq!(code, -32602);
        assert_eq!(msg, "bad value");
    }

    #[test]
    fn internal_error_produces_correct_error_code() {
        let (code, msg) = internal_error("something broke");
        assert_eq!(code, -32603);
        assert_eq!(msg, "something broke");
    }

    // ── run_server integration tests ──────────────────────────────────────────

    #[test]
    fn run_server_does_not_respond_to_notification() {
        // A notification has no id; the server must produce no response frame.
        let input = frame(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        let mut output = Vec::new();
        run_server(
            std::io::Cursor::new(input.into_bytes()),
            &mut output,
            test_db_path(),
        )
        .expect("server should not error on notification");
        assert!(output.is_empty(), "notification must produce no output");
    }

    #[test]
    fn run_server_empty_input_completes_cleanly() {
        let mut output = Vec::new();
        run_server(
            std::io::Cursor::new(Vec::<u8>::new()),
            &mut output,
            test_db_path(),
        )
        .expect("empty input should complete without error");
        assert!(output.is_empty());
    }

    // ── tools / search_schema structural tests ────────────────────────────────

    #[test]
    fn tools_each_have_required_fields() {
        let tool_list = tools();
        let arr = tool_list.as_array().expect("tools() returns array");
        for tool in arr {
            assert!(
                tool.get("name").and_then(Value::as_str).is_some(),
                "tool must have a string name: {tool}"
            );
            assert!(
                tool.get("description").and_then(Value::as_str).is_some(),
                "tool must have a description: {tool}"
            );
            assert!(
                tool.get("inputSchema").is_some(),
                "tool must have an inputSchema: {tool}"
            );
        }
    }

    #[test]
    fn search_schema_requires_query_field() {
        let schema = search_schema();
        let required = schema["required"].as_array().expect("required array");
        let required_fields: Vec<&str> = required
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(
            required_fields.contains(&"query"),
            "search schema must require 'query': {schema}"
        );
    }

    #[test]
    fn search_schema_has_optional_filter_properties() {
        let schema = search_schema();
        let props = schema["properties"].as_object().expect("properties object");
        assert!(props.contains_key("subreddit"), "schema should have subreddit property");
        assert!(props.contains_key("author"), "schema should have author property");
        assert!(props.contains_key("limit"), "schema should have limit property");
    }
}
