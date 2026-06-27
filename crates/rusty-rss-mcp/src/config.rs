//! Startup configuration for the MCP stdio server.
//!
//! Argument parsing is kept deliberately hand-rolled (rather than `clap`) so the
//! binary stays a thin stdio adapter with no help/version output competing for
//! the stdout JSON-RPC channel. Only `--db-path`/`-d` and `RUSTY_RSS_DB_PATH`
//! are honored, matching the previous server and the wider CLI convention.

use anyhow::{Result, anyhow};
use rusqlite::Connection;
use rusty_rss_core::db;
use std::path::{Path, PathBuf};

/// Default database path when neither flag nor env var is provided.
const DEFAULT_DB_PATH: &str = "./rusty-rss.sqlite3";

/// Resolve the database path from CLI args (`--db-path`/`-d`) falling back to the
/// `RUSTY_RSS_DB_PATH` environment variable and finally the default location.
pub fn parse_db_path<I>(args: I) -> Result<PathBuf>
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
            "--help" | "-h" => {
                // Diagnostics go to stderr; stdout is reserved for JSON-RPC.
                eprintln!("Usage: rusty-rss-mcp [--db-path PATH]");
                std::process::exit(0);
            }
            other => return Err(anyhow!("unknown argument: {other}")),
        }
    }

    Ok(db_path)
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

/// Open a connection to an existing archive, refusing to create a new one.
///
/// Every tool call opens its own short-lived connection through this helper
/// (inside `spawn_blocking`), keeping the re-validation of existence immediately
/// before use so a database deleted mid-session fails closed too.
pub fn open_existing_db(db_path: &Path) -> Result<Connection> {
    ensure_db_exists(db_path)?;
    db::init_db(db_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_db_path_reads_long_flag() {
        let path = parse_db_path(args(&["--db-path", "/tmp/archive.sqlite3"])).unwrap();
        assert_eq!(path, PathBuf::from("/tmp/archive.sqlite3"));
    }

    #[test]
    fn parse_db_path_reads_short_flag() {
        let path = parse_db_path(args(&["-d", "/tmp/short.sqlite3"])).unwrap();
        assert_eq!(path, PathBuf::from("/tmp/short.sqlite3"));
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
}
