//! Configuration and result types for outbound capture.

const DEFAULT_MAX_CONCURRENCY: usize = 4;
const DEFAULT_MAX_RETRIES: usize = 3;

#[derive(Debug, Clone, Copy)]
pub struct CaptureOptions {
    pub limit: usize,
    pub allow_private_hosts: bool,
    pub max_concurrency: usize,
    pub max_retries: usize,
}

impl CaptureOptions {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            allow_private_hosts: false,
            max_concurrency: DEFAULT_MAX_CONCURRENCY,
            max_retries: DEFAULT_MAX_RETRIES,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CaptureSummary {
    pub selected_count: usize,
    pub captured_count: usize,
    pub failed_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedMetadata {
    pub final_url: String,
    pub canonical_url: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub site_name: Option<String>,
    pub preview_image_url: Option<String>,
    pub content_markdown: Option<String>,
    pub content_hash: Option<String>,
    pub http_status: u16,
}
