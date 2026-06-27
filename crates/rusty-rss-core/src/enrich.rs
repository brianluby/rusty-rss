use crate::db;
use crate::llm::prompt::PROMPT_VERSION;
use crate::llm::{EnrichmentResult, LlmProvider};
use anyhow::Result;
use rusqlite::Connection;
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct EnrichOptions {
    pub limit: usize,
    pub dry_run: bool,
    pub retry_attempts: u32,
    pub per_item_timeout: Duration,
}

impl EnrichOptions {
    pub fn new(limit: usize, dry_run: bool) -> Self {
        Self {
            limit,
            dry_run,
            retry_attempts: 1,
            per_item_timeout: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct EnrichSummary {
    pub selected_count: usize,
    pub enriched_count: usize,
    pub failed_count: usize,
    pub failures: Vec<EnrichFailure>,
}

#[derive(Debug, Clone)]
pub struct EnrichFailure {
    pub reddit_fullname: String,
    pub error: String,
}

pub async fn run_enrichment_batch<P: LlmProvider + ?Sized>(
    conn: &Connection,
    provider: &P,
    provider_name: &str,
    model: &str,
    options: EnrichOptions,
) -> Result<EnrichSummary> {
    let candidates = db::list_enrichment_candidates(conn, options.limit)?;
    let mut summary = EnrichSummary {
        selected_count: candidates.len(),
        ..EnrichSummary::default()
    };

    if options.dry_run {
        return Ok(summary);
    }

    for post in candidates {
        match enrich_with_retries(provider, &post, options).await {
            Ok(result) => {
                db::record_enrichment_success(
                    conn,
                    &post.reddit_fullname,
                    provider_name,
                    model,
                    PROMPT_VERSION,
                    &result.raw_response,
                    &result.output,
                )?;
                summary.enriched_count += 1;
            }
            Err(err) => {
                let error = err.to_string();
                db::record_enrichment_failure(
                    conn,
                    &post.reddit_fullname,
                    provider_name,
                    model,
                    PROMPT_VERSION,
                    &error,
                )?;
                summary.failed_count += 1;
                summary.failures.push(EnrichFailure {
                    reddit_fullname: post.reddit_fullname,
                    error,
                });
            }
        }
    }

    Ok(summary)
}

async fn enrich_with_retries<P: LlmProvider + ?Sized>(
    provider: &P,
    post: &crate::models::SavedPost,
    options: EnrichOptions,
) -> std::result::Result<EnrichmentResult, String> {
    let mut last_error = String::new();

    for attempt in 1..=options.retry_attempts.max(1) {
        match tokio::time::timeout(options.per_item_timeout, provider.enrich(post)).await {
            Ok(Ok(result)) => return Ok(result),
            Ok(Err(err)) => last_error = err.to_string(),
            Err(_) => {
                last_error = format!("timed out after {}s", options.per_item_timeout.as_secs())
            }
        }

        if attempt < options.retry_attempts.max(1) {
            tokio::time::sleep(Duration::from_millis(100 * u64::from(attempt))).await;
        }
    }

    Err(last_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{EnrichError, EnrichFuture, EnrichmentResult};
    use crate::models::{Classification, EnrichmentOutput, RecommendedAction, SavedPost};
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    struct FakeProvider {
        failures: Arc<Mutex<HashSet<String>>>,
    }

    impl LlmProvider for FakeProvider {
        fn enrich<'a>(&'a self, post: &'a SavedPost) -> EnrichFuture<'a> {
            Box::pin(async move {
                if self
                    .failures
                    .lock()
                    .expect("failures lock should not be poisoned")
                    .contains(&post.reddit_fullname)
                {
                    return Err(EnrichError::Transport("boom".to_string()));
                }

                Ok(EnrichmentResult {
                    raw_response: "raw".to_string(),
                    output: EnrichmentOutput {
                        classification: Classification::Reference,
                        tags: vec!["rust".to_string()],
                        summary: format!("summary for {}", post.reddit_fullname),
                        joy_value: 0.2,
                        work_value: 0.8,
                        recommended_action: RecommendedAction::ReferenceOnly,
                        rationale: "useful later".to_string(),
                        confidence: 0.9,
                    },
                })
            })
        }
    }

    fn test_db() -> Connection {
        db::init_db(std::path::Path::new(":memory:")).expect("db should initialize")
    }

    fn insert_post(conn: &Connection, fullname: &str) {
        let post = SavedPost::new(
            fullname.to_string(),
            format!("Post {fullname}"),
            format!("https://reddit.com/r/rust/comments/{fullname}/post/"),
            "atom".to_string(),
        );
        db::upsert_post(conn, &post).expect("post should insert");
    }

    #[tokio::test]
    async fn dry_run_selects_without_writing() {
        let conn = test_db();
        insert_post(&conn, "t3_one");
        let provider = FakeProvider {
            failures: Arc::new(Mutex::new(HashSet::new())),
        };

        let summary = run_enrichment_batch(
            &conn,
            &provider,
            "fake",
            "fake-model",
            EnrichOptions {
                limit: 10,
                dry_run: true,
                retry_attempts: 1,
                per_item_timeout: Duration::from_secs(60),
            },
        )
        .await
        .expect("dry run should succeed");

        assert_eq!(summary.selected_count, 1);
        assert!(
            db::latest_enrichment(&conn, "t3_one")
                .expect("latest should query")
                .is_none()
        );
    }

    #[tokio::test]
    async fn records_successes_and_failures_without_aborting_batch() {
        let conn = test_db();
        insert_post(&conn, "t3_ok");
        insert_post(&conn, "t3_fail");
        let provider = FakeProvider {
            failures: Arc::new(Mutex::new(HashSet::from(["t3_fail".to_string()]))),
        };

        let summary = run_enrichment_batch(
            &conn,
            &provider,
            "fake",
            "fake-model",
            EnrichOptions {
                limit: 10,
                dry_run: false,
                retry_attempts: 1,
                per_item_timeout: Duration::from_secs(60),
            },
        )
        .await
        .expect("batch should succeed");

        assert_eq!(summary.enriched_count, 1);
        assert_eq!(summary.failed_count, 1);

        let ok = db::latest_enrichment(&conn, "t3_ok")
            .expect("latest should query")
            .expect("success should be recorded");
        assert_eq!(ok.status, "success");

        let failed = db::latest_enrichment(&conn, "t3_fail")
            .expect("latest should query")
            .expect("failure should be recorded");
        assert_eq!(failed.status, "error");
    }
}
