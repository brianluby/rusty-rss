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
            ..SearchFilters::default()
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
}
