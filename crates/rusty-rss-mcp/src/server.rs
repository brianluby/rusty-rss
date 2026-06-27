//! Read-only MCP server built on the official `rmcp` SDK.
//!
//! The server exposes four read tools — `search`, `list`, `show`, and `triage` —
//! over an `rmcp` [`ToolRouter`]. There are deliberately no write/enrich/sync
//! tools: the stdio surface is read-only and the CLI owns all mutation.
//!
//! ## rusqlite under tokio
//!
//! `rusqlite::Connection` is `!Send + !Sync`, so it cannot be held across an
//! `.await` or stored in the (Send + Sync) handler. Each tool therefore performs
//! its database work inside [`tokio::task::spawn_blocking`], opening a fresh
//! short-lived connection per call via [`crate::config::open_readonly_db`]. This
//! keeps the async runtime unblocked and sidesteps the `Send` requirement
//! entirely — the connection never crosses an await point.

use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, Content, ErrorData, Implementation, ServerCapabilities, ServerInfo,
};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use rusqlite::Connection;
use rusty_rss_core::db::{self, SearchFilters, TriageView};
use schemars::JsonSchema;
use serde::Deserialize;

/// Largest page any tool will return, clamping caller-supplied limits.
const MAX_LIMIT: usize = 100;
/// Default page size when a caller omits `limit`.
const DEFAULT_LIMIT: usize = 20;

fn clamp_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT)
}

/// Parameters for [`RustyRssServer::search`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Full-text query over saved post titles and Markdown content.
    pub query: String,
    /// Maximum number of hits to return (1-100, default 20).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Restrict results to a subreddit (without the `r/` prefix).
    #[serde(default)]
    pub subreddit: Option<String>,
    /// Restrict results to an author (without the `u/` prefix).
    #[serde(default)]
    pub author: Option<String>,
}

/// Parameters for [`RustyRssServer::list`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListParams {
    /// Maximum number of posts to return (1-100, default 20).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Number of posts to skip for pagination (default 0).
    #[serde(default)]
    pub offset: Option<usize>,
}

/// Parameters for [`RustyRssServer::show`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShowParams {
    /// Reddit fullname of the post, such as `t3_abc123`.
    pub fullname: String,
}

/// Parameters for [`RustyRssServer::triage`].
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TriageParams {
    /// Triage view to list. One of: `all`, `unprocessed`, `high-value`,
    /// `should-test`, `should-build`, `reading-queue`, `reference-only`,
    /// `discard`. Defaults to `unprocessed`.
    #[serde(default)]
    pub view: Option<String>,
    /// Maximum number of items to return (1-100, default 20).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Number of items to skip for pagination (default 0).
    #[serde(default)]
    pub offset: Option<usize>,
}

/// Read-only MCP server over a local rusty-rss SQLite archive.
#[derive(Clone)]
pub struct RustyRssServer {
    db_path: Arc<PathBuf>,
    tool_router: ToolRouter<Self>,
}

impl RustyRssServer {
    /// Build a server bound to the archive at `db_path`.
    pub fn new(db_path: PathBuf) -> Self {
        Self {
            db_path: Arc::new(db_path),
            tool_router: Self::tool_router(),
        }
    }

    /// Run a blocking database closure on the blocking thread pool.
    ///
    /// The closure receives a fresh, short-lived connection that is opened and
    /// dropped entirely within the blocking task, so the `!Send` connection
    /// never crosses an await boundary.
    async fn with_db<T, F>(&self, f: F) -> Result<T, ErrorData>
    where
        F: FnOnce(&Connection) -> anyhow::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let db_path = Arc::clone(&self.db_path);
        let outcome = tokio::task::spawn_blocking(move || {
            let conn = crate::config::open_readonly_db(&db_path)?;
            f(&conn)
        })
        .await
        .map_err(|err| ErrorData::internal_error(format!("blocking task failed: {err}"), None))?;

        outcome.map_err(|err| ErrorData::internal_error(err.to_string(), None))
    }
}

/// Serialize a value to a pretty JSON text content block.
fn json_content<T: serde::Serialize>(value: &T) -> Result<CallToolResult, ErrorData> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|err| ErrorData::internal_error(format!("serialization failed: {err}"), None))?;
    Ok(CallToolResult::success(vec![Content::text(text)]))
}

#[tool_router]
impl RustyRssServer {
    #[tool(description = "Full-text search over saved Reddit posts by title and Markdown content.")]
    async fn search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        if params.query.trim().is_empty() {
            return Err(ErrorData::invalid_params(
                "query must be a non-empty string",
                None,
            ));
        }
        let limit = clamp_limit(params.limit);
        let filters = SearchFilters {
            subreddit: params.subreddit,
            author: params.author,
        };
        let query = params.query;
        let hits = self
            .with_db(move |conn| db::search_posts(conn, &query, &filters, limit))
            .await?;
        json_content(&hits)
    }

    #[tool(description = "List saved Reddit posts ordered by last-seen time (most recent first).")]
    async fn list(
        &self,
        Parameters(params): Parameters<ListParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = clamp_limit(params.limit);
        let offset = params.offset.unwrap_or(0);
        let posts = self
            .with_db(move |conn| db::list_posts(conn, limit, offset))
            .await?;
        json_content(&posts)
    }

    #[tool(description = "Show one saved Reddit post by its Reddit fullname (such as t3_abc123).")]
    async fn show(
        &self,
        Parameters(params): Parameters<ShowParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let fullname = params.fullname;
        if fullname.trim().is_empty() {
            return Err(ErrorData::invalid_params(
                "fullname must be a non-empty string",
                None,
            ));
        }
        let post = self
            .with_db(move |conn| db::get_post(conn, &fullname))
            .await?;
        // A missing post serializes to JSON `null`, which is cleanly
        // distinguishable from a serialization error (mapped to an internal
        // error above) by the caller.
        json_content(&post)
    }

    #[tool(
        description = "List enrichment-driven triage items for a view (all, unprocessed, \
            high-value, should-test, should-build, reading-queue, reference-only, discard)."
    )]
    async fn triage(
        &self,
        Parameters(params): Parameters<TriageParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let view_name = params.view.unwrap_or_else(|| "unprocessed".to_string());
        let view = TriageView::parse(&view_name).ok_or_else(|| {
            ErrorData::invalid_params(format!("unknown triage view: {view_name}"), None)
        })?;
        let limit = clamp_limit(params.limit);
        let offset = params.offset.unwrap_or(0);
        let items = self
            .with_db(move |conn| db::list_triage_items(conn, view, limit, offset))
            .await?;
        json_content(&items)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for RustyRssServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "rusty-rss-mcp",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Read-only access to a local rusty-rss Reddit archive. Tools: \
                 search (FTS over titles/content), list (recent posts), show (one post \
                 by fullname), triage (enrichment-driven views). No write operations are \
                 exposed; use the rusty-rss CLI for sync/enrich/capture.",
            )
    }
}
