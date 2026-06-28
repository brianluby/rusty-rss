//! Batch enrichment orchestration: select candidate posts, call the configured
//! LLM provider with bounded concurrency, per-item timeouts, and retries, and
//! persist each result.

use crate::db;
use crate::llm::prompt::PROMPT_VERSION;
use crate::llm::{EnrichmentResult, LlmProvider};
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Default number of provider calls allowed in flight at once. Bounded so a
/// large batch never opens an unbounded number of concurrent LLM requests.
pub const DEFAULT_CONCURRENCY: usize = 4;
/// Default maximum attempts per item. Greater than one so a transient fault gets
/// one retry; non-transient failures still fail on the first attempt.
pub const DEFAULT_RETRY_ATTEMPTS: u32 = 3;
/// Default per-item provider timeout.
pub const DEFAULT_PER_ITEM_TIMEOUT: Duration = Duration::from_secs(60);

/// Tuning knobs for an enrichment batch.
#[derive(Debug, Clone, Copy)]
pub struct EnrichOptions {
    /// Maximum number of candidate posts to process this run.
    pub limit: usize,
    /// Select candidates and report counts, but write nothing.
    pub dry_run: bool,
    /// Maximum provider calls in flight at once (clamped to at least 1).
    pub concurrency: usize,
    /// Maximum attempts per item; retries apply only to transient errors.
    pub retry_attempts: u32,
    /// Timeout applied to each individual provider call.
    pub per_item_timeout: Duration,
    /// Re-enrich a post whose newest successful run is older than this window.
    /// `None` disables the age check (selection still re-runs on prompt change).
    pub stale_after: Option<Duration>,
}

impl EnrichOptions {
    /// Create options for a batch of up to `limit` posts using default
    /// concurrency, retry, and timeout settings and no staleness window.
    pub fn new(limit: usize, dry_run: bool) -> Self {
        Self {
            limit,
            dry_run,
            concurrency: DEFAULT_CONCURRENCY,
            retry_attempts: DEFAULT_RETRY_ATTEMPTS,
            per_item_timeout: DEFAULT_PER_ITEM_TIMEOUT,
            stale_after: None,
        }
    }
}

/// Outcome counts (and per-item failures) for a completed enrichment batch.
#[derive(Debug, Clone, Default)]
pub struct EnrichSummary {
    /// Number of candidate posts selected for enrichment.
    pub selected_count: usize,
    /// Number of posts enriched successfully.
    pub enriched_count: usize,
    /// Number of posts that failed to enrich.
    pub failed_count: usize,
    /// Details of each failed item.
    pub failures: Vec<EnrichFailure>,
}

/// A single failed enrichment, retained for reporting.
#[derive(Debug, Clone)]
pub struct EnrichFailure {
    /// Reddit fullname of the post that failed.
    pub reddit_fullname: String,
    /// Human-readable failure reason.
    pub error: String,
}

/// Select the posts a batch would enrich, applying the current prompt version
/// and the configured staleness window. Single source of truth so the CLI's
/// dry-run/gating count and [`run_enrichment_batch`] agree on what is selected.
pub fn select_candidates(
    conn: &Connection,
    options: EnrichOptions,
) -> Result<Vec<crate::models::SavedPost>> {
    db::list_enrichment_candidates(conn, options.limit, PROMPT_VERSION, options.stale_after)
}

/// Enrich a batch of candidate posts with bounded concurrency.
///
/// Provider calls run concurrently under a [`Semaphore`] capped at
/// `options.concurrency`, since the network round-trips dominate. The
/// `rusqlite::Connection` is `!Send`, so it never crosses into a spawned task:
/// the worker tasks only gather provider results, and every database write
/// (`record_enrichment_success`/`record_enrichment_failure`) is serialized back
/// on this task as results are joined. A single item's failure is recorded and
/// counted without aborting the rest of the batch; `dry_run` selects but writes
/// nothing.
pub async fn run_enrichment_batch<P: LlmProvider + Send + Sync + 'static>(
    conn: &Connection,
    provider: Arc<P>,
    provider_name: &str,
    model: &str,
    options: EnrichOptions,
) -> Result<EnrichSummary> {
    let candidates = select_candidates(conn, options)?;
    let mut summary = EnrichSummary {
        selected_count: candidates.len(),
        ..EnrichSummary::default()
    };

    if options.dry_run {
        return Ok(summary);
    }

    let concurrency = options.concurrency.max(1);
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let mut tasks: JoinSet<(
        crate::models::SavedPost,
        std::result::Result<EnrichmentResult, String>,
    )> = JoinSet::new();

    for post in candidates {
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .context("enrichment semaphore closed")?;
        let provider = Arc::clone(&provider);
        tasks.spawn(async move {
            let result = enrich_with_retries(provider.as_ref(), &post, options).await;
            drop(permit);
            (post, result)
        });
    }

    while let Some(joined) = tasks.join_next().await {
        let (post, result) = match joined {
            Ok(pair) => pair,
            Err(join_err) => {
                // A worker panicked or was cancelled. Count it once and keep
                // draining instead of aborting the whole batch.
                tracing::warn!(error = %join_err, "enrichment task failed to join; skipping candidate");
                summary.failed_count += 1;
                continue;
            }
        };

        match result {
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
            Err(error) => {
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

/// Run a single item's provider call with a per-attempt timeout, retrying only
/// transient faults ([`EnrichError::is_transient`]). `Parse`/`Validation`
/// failures are deterministic, so they return immediately without consuming the
/// remaining attempts. A timeout is treated as transient.
async fn enrich_with_retries<P: LlmProvider + ?Sized>(
    provider: &P,
    post: &crate::models::SavedPost,
    options: EnrichOptions,
) -> std::result::Result<EnrichmentResult, String> {
    let attempts = options.retry_attempts.max(1);
    let mut last_error = String::new();

    // Deterministic per-task jitter so concurrent workers don't retry a shared
    // transient fault in lockstep. Derived from the post's fullname (already in
    // scope) rather than a new `rand` dependency: stable within a run, but
    // staggered across tasks. Bounded to <100ms so it never dominates the linear
    // 100ms * attempt backoff below.
    let jitter_ms = u64::from(
        post.reddit_fullname
            .bytes()
            .fold(0u8, |acc, b| acc.wrapping_add(b)),
    ) % 100;

    for attempt in 1..=attempts {
        match tokio::time::timeout(options.per_item_timeout, provider.enrich(post)).await {
            Ok(Ok(result)) => return Ok(result),
            Ok(Err(err)) => {
                let transient = err.is_transient();
                last_error = err.to_string();
                // Fail fast on deterministic errors: a retry cannot change them.
                if !transient {
                    return Err(last_error);
                }
            }
            Err(_) => {
                // Timeouts are infrastructure faults, so they are retryable.
                last_error = format!("timed out after {}s", options.per_item_timeout.as_secs());
            }
        }

        if attempt < attempts {
            tokio::time::sleep(Duration::from_millis(100 * u64::from(attempt) + jitter_ms)).await;
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
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn ok_result(post: &SavedPost) -> EnrichmentResult {
        EnrichmentResult {
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
        }
    }

    /// Provider that fails for a fixed set of fullnames with a transient error.
    struct FakeProvider {
        failures: Arc<Mutex<HashSet<String>>>,
    }

    impl LlmProvider for FakeProvider {
        fn enrich<'a>(&'a self, post: &'a SavedPost) -> EnrichFuture<'a> {
            let fail = self
                .failures
                .lock()
                .expect("failures lock should not be poisoned")
                .contains(&post.reddit_fullname);
            let result = ok_result(post);
            Box::pin(async move {
                if fail {
                    return Err(EnrichError::Transport("boom".to_string()));
                }
                Ok(result)
            })
        }
    }

    /// Provider that tracks concurrent in-flight calls and records the peak, so a
    /// test can assert the concurrency cap is actually enforced.
    struct ConcurrencyProbeProvider {
        in_flight: Arc<AtomicUsize>,
        max_seen: Arc<AtomicUsize>,
    }

    impl LlmProvider for ConcurrencyProbeProvider {
        fn enrich<'a>(&'a self, post: &'a SavedPost) -> EnrichFuture<'a> {
            let in_flight = Arc::clone(&self.in_flight);
            let max_seen = Arc::clone(&self.max_seen);
            let result = ok_result(post);
            Box::pin(async move {
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                max_seen.fetch_max(now, Ordering::SeqCst);
                // Yield so siblings get a chance to ramp up in-flight before this
                // one finishes; on a current-thread runtime this interleaves the
                // tasks the JoinSet is driving.
                tokio::task::yield_now().await;
                tokio::time::sleep(Duration::from_millis(20)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                Ok(result)
            })
        }
    }

    /// Provider that fails the first `fail_attempts` calls with a configurable
    /// error, then succeeds, counting every call. Used to assert retry behavior.
    struct FlakyProvider {
        calls: Arc<AtomicUsize>,
        fail_attempts: usize,
        error: fn(String) -> EnrichError,
    }

    impl LlmProvider for FlakyProvider {
        fn enrich<'a>(&'a self, post: &'a SavedPost) -> EnrichFuture<'a> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let should_fail = call < self.fail_attempts;
            let error = (self.error)(format!("attempt {call}"));
            let result = ok_result(post);
            Box::pin(async move {
                if should_fail {
                    return Err(error);
                }
                Ok(result)
            })
        }
    }

    /// Provider that sleeps past any reasonable per-item timeout.
    struct SlowProvider;

    impl LlmProvider for SlowProvider {
        fn enrich<'a>(&'a self, post: &'a SavedPost) -> EnrichFuture<'a> {
            let result = ok_result(post);
            Box::pin(async move {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(result)
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

    fn options(limit: usize, dry_run: bool) -> EnrichOptions {
        EnrichOptions {
            dry_run,
            // Keep retries off by default in tests so failure counts are exact;
            // the retry tests opt back in explicitly.
            retry_attempts: 1,
            ..EnrichOptions::new(limit, dry_run)
        }
    }

    #[tokio::test]
    async fn dry_run_selects_without_writing() {
        let conn = test_db();
        insert_post(&conn, "t3_one");
        let provider = Arc::new(FakeProvider {
            failures: Arc::new(Mutex::new(HashSet::new())),
        });

        let summary =
            run_enrichment_batch(&conn, provider, "fake", "fake-model", options(10, true))
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
        let provider = Arc::new(FakeProvider {
            failures: Arc::new(Mutex::new(HashSet::from(["t3_fail".to_string()]))),
        });

        let summary =
            run_enrichment_batch(&conn, provider, "fake", "fake-model", options(10, false))
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

    #[tokio::test]
    async fn enforces_concurrency_cap() {
        let conn = test_db();
        for index in 0..8 {
            insert_post(&conn, &format!("t3_c{index}"));
        }
        let max_seen = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(ConcurrencyProbeProvider {
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_seen: Arc::clone(&max_seen),
        });

        let summary = run_enrichment_batch(
            &conn,
            provider,
            "fake",
            "fake-model",
            EnrichOptions {
                concurrency: 3,
                ..options(10, false)
            },
        )
        .await
        .expect("batch should succeed");

        assert_eq!(summary.enriched_count, 8);
        let peak = max_seen.load(Ordering::SeqCst);
        assert!(peak > 1, "calls should overlap, peak in-flight was {peak}");
        assert!(
            peak <= 3,
            "concurrency cap exceeded: peak in-flight was {peak}"
        );
    }

    #[tokio::test]
    async fn times_out_slow_items_and_records_failure() {
        let conn = test_db();
        insert_post(&conn, "t3_slow");
        let provider = Arc::new(SlowProvider);

        let summary = run_enrichment_batch(
            &conn,
            provider,
            "fake",
            "fake-model",
            EnrichOptions {
                per_item_timeout: Duration::from_millis(50),
                ..options(10, false)
            },
        )
        .await
        .expect("batch should succeed");

        assert_eq!(summary.failed_count, 1);
        assert!(summary.failures[0].error.contains("timed out"));
        let failed = db::latest_enrichment(&conn, "t3_slow")
            .expect("latest should query")
            .expect("failure should be recorded");
        assert_eq!(failed.status, "error");
    }

    #[tokio::test]
    async fn retries_transient_errors_until_success() {
        let conn = test_db();
        insert_post(&conn, "t3_flaky");
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(FlakyProvider {
            calls: Arc::clone(&calls),
            fail_attempts: 2,
            error: EnrichError::Transport,
        });

        let summary = run_enrichment_batch(
            &conn,
            provider,
            "fake",
            "fake-model",
            EnrichOptions {
                retry_attempts: 3,
                ..options(10, false)
            },
        )
        .await
        .expect("batch should succeed");

        assert_eq!(summary.enriched_count, 1);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "two transient failures then a success should be exactly three calls"
        );
    }

    #[tokio::test]
    async fn does_not_retry_parse_or_validation_errors() {
        for make_error in [EnrichError::Parse, EnrichError::Validation] {
            let conn = test_db();
            insert_post(&conn, "t3_deterministic");
            let calls = Arc::new(AtomicUsize::new(0));
            let provider = Arc::new(FlakyProvider {
                calls: Arc::clone(&calls),
                // Would fail many times, but a non-transient error must stop after one.
                fail_attempts: 99,
                error: make_error,
            });

            let summary = run_enrichment_batch(
                &conn,
                provider,
                "fake",
                "fake-model",
                EnrichOptions {
                    retry_attempts: 5,
                    ..options(10, false)
                },
            )
            .await
            .expect("batch should succeed");

            assert_eq!(summary.failed_count, 1);
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "deterministic errors must not be retried"
            );
        }
    }

    #[tokio::test]
    async fn resumes_without_reprocessing_completed_items() {
        let conn = test_db();
        insert_post(&conn, "t3_resume_a");
        insert_post(&conn, "t3_resume_b");

        // First pass enriches both posts.
        let provider = Arc::new(FakeProvider {
            failures: Arc::new(Mutex::new(HashSet::new())),
        });
        let first = run_enrichment_batch(&conn, provider, "fake", "fake-model", options(10, false))
            .await
            .expect("first batch should succeed");
        assert_eq!(first.enriched_count, 2);

        // Second pass: both are already enriched under the current prompt, so the
        // batch selects nothing and the provider is never called again.
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = Arc::new(FlakyProvider {
            calls: Arc::clone(&calls),
            fail_attempts: 0,
            error: EnrichError::Transport,
        });
        let second =
            run_enrichment_batch(&conn, provider, "fake", "fake-model", options(10, false))
                .await
                .expect("second batch should succeed");
        assert_eq!(second.selected_count, 0);
        assert_eq!(second.enriched_count, 0);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "resume must skip completed items"
        );
    }
}
