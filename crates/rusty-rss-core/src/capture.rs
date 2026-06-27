//! Outbound link capture: fetch each saved post's outbound URL and extract page
//! metadata (title, description, canonical URL, a markdown snapshot, ...).
//!
//! Organized by concern: public [`options`] types, SSRF [`security`] (a
//! DNS-rebinding-safe client, URL validation, private-IP blocking), per-URL
//! [`fetch`]-and-parse, and the concurrent [`orchestrator`]. This root keeps no
//! logic of its own; it re-exports the public API so callers use `capture::*`.

mod fetch;
mod options;
mod orchestrator;
mod security;

#[cfg(test)]
mod test_support;

pub use fetch::capture_url;
pub use options::{CaptureOptions, CaptureSummary, CapturedMetadata};
pub use orchestrator::capture_outbound_metadata;
pub use security::{CaptureClient, build_capture_client};
