//! Hidden full-text-search maintenance command: integrity check and reindex.
//!
//! These are recovery tools for a drifted or corrupt `posts_fts` index, not part
//! of the everyday workflow, so the parent `fts` command is `#[command(hide)]`.
//! This is the repo's first nested clap [`Subcommand`].

use anyhow::Result;
use clap::Subcommand;
use rusty_rss_core::db;
use std::path::PathBuf;

/// Maintenance operations for the full-text search index.
#[derive(Subcommand)]
pub enum FtsCommand {
    /// Rebuild the full-text search index from `saved_posts`
    Rebuild,
    /// Verify the full-text search index is consistent with its content table
    Check,
}

pub(super) fn run_fts(db_path: PathBuf, command: FtsCommand) -> Result<()> {
    let conn = db::init_db(&db_path)?;

    match command {
        FtsCommand::Rebuild => {
            db::rebuild_fts_index(&conn)?;
            println!("Full-text search index rebuilt.");
        }
        FtsCommand::Check => {
            // A failed integrity check returns an error here, which propagates to
            // a non-zero process exit via `main`.
            db::fts_integrity_check(&conn)?;
            println!("Full-text search index OK.");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::test_support::{insert_post, test_db_path};

    #[test]
    fn rebuild_succeeds_on_populated_db() {
        let db_path = test_db_path();
        insert_post(&db_path);

        run_fts(db_path, FtsCommand::Rebuild).expect("rebuild should succeed");
    }

    #[test]
    fn check_succeeds_on_populated_db() {
        let db_path = test_db_path();
        insert_post(&db_path);

        run_fts(db_path, FtsCommand::Check).expect("check should succeed");
    }

    #[test]
    fn rebuild_and_check_succeed_on_empty_db() {
        let db_path = test_db_path();

        run_fts(db_path.clone(), FtsCommand::Rebuild).expect("rebuild should succeed");
        run_fts(db_path, FtsCommand::Check).expect("check should succeed");
    }
}
