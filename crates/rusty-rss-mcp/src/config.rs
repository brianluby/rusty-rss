//! Startup configuration for the MCP stdio server.
//!
//! Argument parsing is kept deliberately hand-rolled (rather than `clap`) so the
//! binary stays a thin stdio adapter with no help/version output competing for
//! the stdout JSON-RPC channel. Only `--db-path`/`-d` and `RUSTY_RSS_DB_PATH`
//! are honored, matching the previous server and the wider CLI convention.

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Default database path when neither flag nor env var is provided.
const DEFAULT_DB_PATH: &str = "./rusty-rss.sqlite3";

/// Usage banner printed for `--help`/`-h`. Callers must emit this to STDERR
/// (stdout is reserved for the JSON-RPC channel).
pub const USAGE: &str = "Usage: rusty-rss-mcp [--db-path PATH]";

/// Outcome of parsing the CLI arguments.
///
/// Distinguishing `HelpRequested` from a resolved path keeps [`parse_db_path`]
/// free of side effects (no printing, no `process::exit`), so it stays a pure,
/// testable library function. The binary entrypoint is responsible for printing
/// the [`USAGE`] banner and exiting on `HelpRequested`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbPathArgs {
    /// A database path was resolved from a flag, env var, or the default.
    Resolved(PathBuf),
    /// `--help`/`-h` was passed; the caller should print usage and exit 0.
    HelpRequested,
}

/// Resolve the database path from CLI args (`--db-path`/`-d`) falling back to the
/// `RUSTY_RSS_DB_PATH` environment variable and finally the default location.
///
/// This function has no side effects: `--help`/`-h` returns
/// [`DbPathArgs::HelpRequested`] rather than printing or exiting.
pub fn parse_db_path<I>(args: I) -> Result<DbPathArgs>
where
    I: IntoIterator<Item = String>,
{
    let mut db_path = std::env::var("RUSTY_RSS_DB_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_DB_PATH));

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--db-path" | "-d" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("{arg} requires a database path"))?;
                db_path = PathBuf::from(value);
            }
            "--help" | "-h" => return Ok(DbPathArgs::HelpRequested),
            other => return Err(anyhow!("unknown argument: {other}")),
        }
    }

    Ok(DbPathArgs::Resolved(db_path))
}

/// Fail fast when the database is missing instead of silently serving a freshly
/// created, empty archive because `--db-path` / `RUSTY_RSS_DB_PATH` is wrong.
///
/// Security posture: this is a fail-closed check. A missing path or a directory
/// both produce a clear error here rather than a cryptic SQLite open failure or,
/// worse, a brand-new empty database that masks misconfiguration.
pub fn ensure_db_exists(db_path: &Path) -> Result<()> {
    if !db_path.is_file() {
        return Err(anyhow!(
            "database file not found at {}; run `rusty-rss sync` first or pass a correct --db-path",
            db_path.display()
        ));
    }
    Ok(())
}

/// Open a strictly read-only connection to an existing archive.
///
/// Every tool call opens its own short-lived connection through this helper
/// (inside `spawn_blocking`). Using `SQLITE_OPEN_READ_ONLY` with no `CREATE`
/// flag makes this fail-closed in two ways at the SQLite layer itself:
///
/// 1. A missing file is refused atomically (no TOCTOU window, no silently
///    re-created empty database) — so no separate existence pre-check is needed.
/// 2. Every write is rejected by SQLite, which guarantees the read path never
///    runs the shared `db::init_db` migrations (FTS rebuild, `ALTER TABLE`,
///    content-HTML→Markdown `UPDATE`) on a user's archive during a tool call.
pub fn open_readonly_db(db_path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| {
        format!(
            "failed to open database read-only at {}; run `rusty-rss sync` first or pass a correct --db-path",
            db_path.display()
        )
    })?;

    // Wait on a contended lock instead of failing immediately with SQLITE_BUSY
    // when a concurrent writer (the CLI) holds the database.
    conn.busy_timeout(Duration::from_secs(5))
        .context("failed to configure busy timeout")?;

    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_db_path_reads_long_flag() {
        let parsed = parse_db_path(args(&["--db-path", "/tmp/archive.sqlite3"])).unwrap();
        assert_eq!(
            parsed,
            DbPathArgs::Resolved(PathBuf::from("/tmp/archive.sqlite3"))
        );
    }

    #[test]
    fn parse_db_path_reads_short_flag() {
        let parsed = parse_db_path(args(&["-d", "/tmp/short.sqlite3"])).unwrap();
        assert_eq!(
            parsed,
            DbPathArgs::Resolved(PathBuf::from("/tmp/short.sqlite3"))
        );
    }

    #[test]
    fn parse_db_path_help_is_side_effect_free() {
        // `--help`/`-h` must be reported as a request, never printed or exited
        // from inside this library function.
        assert_eq!(
            parse_db_path(args(&["--help"])).unwrap(),
            DbPathArgs::HelpRequested
        );
        assert_eq!(
            parse_db_path(args(&["-h"])).unwrap(),
            DbPathArgs::HelpRequested
        );
    }

    #[test]
    fn parse_db_path_rejects_unknown_argument() {
        let err = parse_db_path(args(&["--nope"])).unwrap_err();
        assert!(err.to_string().contains("unknown argument"), "{err}");
    }

    #[test]
    fn parse_db_path_requires_value_for_flag() {
        let err = parse_db_path(args(&["--db-path"])).unwrap_err();
        assert!(
            err.to_string().contains("requires a database path"),
            "{err}"
        );
    }

    #[test]
    fn ensure_db_exists_rejects_missing_file() {
        let missing = std::env::temp_dir().join(format!(
            "rusty_rss_mcp_missing_{}.sqlite3",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&missing);
        let err = ensure_db_exists(&missing).unwrap_err();
        assert!(err.to_string().contains("database file not found"), "{err}");
    }

    #[test]
    fn ensure_db_exists_rejects_directory() {
        let err = ensure_db_exists(&std::env::temp_dir()).unwrap_err();
        assert!(err.to_string().contains("database file not found"), "{err}");
    }

    fn unique_path(tag: &str) -> PathBuf {
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "rusty_rss_mcp_cfg_{tag}_{}_{id}.sqlite3",
            std::process::id()
        ))
    }

    #[test]
    fn open_readonly_db_refuses_missing_file_without_creating_it() {
        let missing = unique_path("missing");
        let _ = std::fs::remove_file(&missing);

        let err = open_readonly_db(&missing).unwrap_err();
        assert!(
            err.to_string().contains("read-only"),
            "error should be fail-closed and legible: {err}"
        );
        // Fail-closed contract: the server must NEVER create a database file when
        // pointed at a missing path (SQLITE_OPEN_READ_ONLY has no CREATE flag).
        assert!(
            !missing.exists(),
            "read-only open must not create {}",
            missing.display()
        );
    }

    #[test]
    fn open_readonly_db_rejects_writes() {
        use rusty_rss_core::db;

        let path = unique_path("readonly");
        let _ = std::fs::remove_file(&path);
        // Create a real archive through the write path, then close it.
        drop(db::init_db(&path).expect("init db"));

        let conn = open_readonly_db(&path).expect("open read-only");
        // Any write must be rejected by SQLite itself on a read-only connection.
        let write = conn.execute_batch("CREATE TABLE should_not_exist (id INTEGER);");
        assert!(
            write.is_err(),
            "read-only connection must reject writes, got {write:?}"
        );

        let _ = std::fs::remove_file(&path);
    }
}
