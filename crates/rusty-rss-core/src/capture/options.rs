//! Configuration and result types for outbound capture.

const DEFAULT_MAX_CONCURRENCY: usize = 4;
const DEFAULT_MAX_RETRIES: usize = 3;

/// Tuning knobs for an outbound capture run.
#[derive(Debug, Clone, Copy)]
pub struct CaptureOptions {
    /// Maximum number of candidate posts to capture in this run.
    pub limit: usize,
    /// Allow capturing private/loopback hosts (intended for tests only).
    pub allow_private_hosts: bool,
    /// Maximum number of URLs fetched concurrently.
    pub max_concurrency: usize,
    /// Maximum fetch attempts per URL before recording a failure.
    pub max_retries: usize,
}

impl CaptureOptions {
    /// Create options for capturing up to `limit` posts using safe defaults
    /// (private hosts blocked, default concurrency and retry counts).
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            allow_private_hosts: false,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            max_retries: DEFAULT_MAX_RETRIES,
        }
    }
}

/// Outcome counts for a completed capture run.
#[derive(Debug, Clone, Default)]
pub struct CaptureSummary {
    /// Number of candidate posts selected for capture.
    pub selected_count: usize,
    /// Number of URLs captured successfully.
    pub captured_count: usize,
    /// Number of URLs that failed to capture.
    pub failed_count: usize,
}

/// Page metadata extracted from a captured URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedMetadata {
    /// The final URL after any client-side resolution (redirects are disabled).
    pub final_url: String,
    /// Canonical URL declared by the page (`<link rel="canonical">`), if any.
    pub canonical_url: Option<String>,
    /// Page title (OpenGraph `og:title`, falling back to `<title>`).
    pub title: Option<String>,
    /// Page description (`meta[name=description]` or `og:description`).
    pub description: Option<String>,
    /// Site name declared via `og:site_name`, if any.
    pub site_name: Option<String>,
    /// Preview image URL declared via `og:image`, if any.
    pub preview_image_url: Option<String>,
    /// Markdown rendering of the page body, if non-empty.
    pub content_markdown: Option<String>,
    /// `sha256:`-prefixed hash of `content_markdown`, if present.
    pub content_hash: Option<String>,
    /// HTTP status code of the successful response.
    pub http_status: u16,
}
