//! Versioned enrichment prompt construction and input budgeting.
//!
//! This is the single place that owns the enrichment prompt: the rubric the
//! model classifies against, the user-message layout, and the char-based input
//! budget. Keeping it separate from the transport code in `llm.rs` lets the
//! prompt be unit-tested in isolation (golden output) and versioned via
//! [`PROMPT_VERSION`] independently of how requests are sent.

use crate::models::SavedPost;
use serde::Serialize;

/// Version tag stored with every enrichment run. Bump whenever the rubric or
/// message layout changes in a way that should distinguish or invalidate
/// previously stored outputs.
pub(crate) const PROMPT_VERSION: &str = "enrich-v2";

/// Maximum number of post-content characters sent to the model. Char-based (not
/// token-based) on purpose: it avoids pulling in a tokenizer dependency while
/// still bounding the request to a size that fits typical local context windows
/// with headroom for the rubric and the response.
pub(crate) const MAX_CONTENT_CHARS: usize = 12_000;

/// Appended to post content that had to be cut to fit [`MAX_CONTENT_CHARS`], so
/// the model knows the input is incomplete. Callers should keep the budget well
/// above this marker's own length.
pub(crate) const TRUNCATION_MARKER: &str = "\n\n[content truncated to fit input budget]";

/// The system rubric. One line per [`crate::models::Classification`] and per
/// [`crate::models::RecommendedAction`] describing when to choose it, plus the
/// meaning and `[0.0, 1.0]` range of each score. It doubles as repair guidance:
/// a rejected response is re-sent with this same rubric in scope.
const SYSTEM_PROMPT: &str = concat!(
    "You classify saved Reddit items for a personal archive. Return only a single JSON object ",
    "matching the supplied schema: no prose, no markdown, and no code fences.\n",
    "\n",
    "Pick exactly one classification:\n",
    "- article: a written piece making an argument or telling a story.\n",
    "- tool: software, a library, a service, or a product that can be used.\n",
    "- tutorial: step-by-step instructions teaching how to do something.\n",
    "- reference: documentation, a spec, or material to look things up in.\n",
    "- discussion: an open-ended conversation, debate, or opinion thread.\n",
    "- question: a request for help or an answer to one.\n",
    "- news: a time-sensitive report on a recent event or release.\n",
    "- other: none of the classifications above fit.\n",
    "\n",
    "Pick exactly one recommended_action:\n",
    "- should_test: a tool or technique worth trying hands-on soon.\n",
    "- should_build: something to implement or build a project from.\n",
    "- reading_queue: worth reading in full later; no other action needed.\n",
    "- reference_only: keep for future lookup; no reading or action planned.\n",
    "- discard: low value for this archive; safe to drop.\n",
    "- other: none of the actions above fit.\n",
    "\n",
    "Each score is a number from 0.0 to 1.0 inclusive:\n",
    "- joy_value: personal interest, how much the reader would enjoy this for its own sake ",
    "(0.0 none, 1.0 high).\n",
    "- work_value: build/learn utility, how useful this is for building something or learning ",
    "a skill (0.0 none, 1.0 high).\n",
    "- confidence: model certainty, how sure you are of this classification ",
    "(0.0 guessing, 1.0 certain).\n",
    "\n",
    "Write a concise neutral summary and a one-sentence rationale for the recommended_action. ",
    "If an earlier response was rejected, re-read this rubric and return a corrected JSON object ",
    "that satisfies it.",
);

/// A single chat message in the OpenAI-compatible request shape. Owned by this
/// module so the prompt and the wire format stay defined together.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ChatMessage {
    pub(crate) role: &'static str,
    pub(crate) content: String,
}

/// Build the deterministic two-message enrichment prompt (system rubric + user
/// payload) for `post`, truncating the post content to `max_content_chars`.
pub(crate) fn build_enrichment_messages(
    post: &SavedPost,
    max_content_chars: usize,
) -> Vec<ChatMessage> {
    let (content, _truncated) = truncate_for_budget(
        post.content_markdown.as_deref().unwrap_or(""),
        max_content_chars,
    );

    vec![
        ChatMessage {
            role: "system",
            content: SYSTEM_PROMPT.to_string(),
        },
        ChatMessage {
            role: "user",
            content: format!(
                "Title: {}\nSubreddit: {}\nAuthor: {}\nPermalink: {}\nOutbound URL: {}\nContent:\n{}",
                post.title,
                post.subreddit.as_deref().unwrap_or(""),
                post.author.as_deref().unwrap_or(""),
                post.permalink,
                post.outbound_url.as_deref().unwrap_or(""),
                content,
            ),
        },
    ]
}

/// Build the repair prompt: the original enrichment messages followed by a
/// correction instruction that points back at the rubric already in scope.
pub(crate) fn build_repair_messages(post: &SavedPost, invalid_response: &str) -> Vec<ChatMessage> {
    let mut messages = build_enrichment_messages(post, MAX_CONTENT_CHARS);
    messages.push(ChatMessage {
        role: "user",
        content: format!(
            "The previous response was invalid JSON or failed validation. Re-read the rubric and \
             return a corrected JSON object only. Previous response:\n{invalid_response}"
        ),
    });
    messages
}

/// Truncate `markdown` to at most `max_chars` characters, appending
/// [`TRUNCATION_MARKER`] when content is dropped. Counting is by `char` (Unicode
/// scalar values), never bytes, so multi-byte characters are never split. The
/// returned bool reports whether truncation occurred; the returned string stays
/// within `max_chars` as long as the budget exceeds the marker's length.
pub(crate) fn truncate_for_budget(markdown: &str, max_chars: usize) -> (String, bool) {
    if markdown.chars().count() <= max_chars {
        return (markdown.to_string(), false);
    }

    let head_budget = max_chars.saturating_sub(TRUNCATION_MARKER.chars().count());
    let head: String = markdown.chars().take(head_budget).collect();
    (format!("{head}{TRUNCATION_MARKER}"), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Classification, RecommendedAction, SavedPost};
    use std::path::{Path, PathBuf};

    fn golden_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/llm/testdata")
            .join(name)
    }

    /// Compare `actual` against a committed golden file. Set `UPDATE_GOLDEN=1`
    /// to (re)write the golden after an intentional prompt change.
    fn assert_golden(name: &str, actual: &str) {
        let path = golden_path(name);
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(path.parent().expect("golden path has a parent"))
                .expect("golden dir should be creatable");
            std::fs::write(&path, actual).expect("golden file should be writable");
            return;
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|err| {
            panic!(
                "missing golden {} ({err}); regenerate with UPDATE_GOLDEN=1",
                path.display()
            )
        });
        assert_eq!(
            actual,
            expected.as_str(),
            "golden mismatch for {} (regenerate with UPDATE_GOLDEN=1)",
            path.display()
        );
    }

    fn normal_post() -> SavedPost {
        let mut post = SavedPost::new(
            "t3_normal".to_string(),
            "A pragmatic guide to async Rust".to_string(),
            "https://reddit.com/r/rust/comments/normal/guide/".to_string(),
            "atom".to_string(),
        );
        post.author = Some("ferris".to_string());
        post.subreddit = Some("rust".to_string());
        post.outbound_url = Some("https://example.com/async-rust".to_string());
        post.content_markdown = Some(
            "Async Rust can be tricky. This guide covers tasks, futures, and pinning \
             with small, runnable examples."
                .to_string(),
        );
        post
    }

    fn oversized_post() -> SavedPost {
        let mut post = SavedPost::new(
            "t3_oversized".to_string(),
            "A very long write-up".to_string(),
            "https://reddit.com/r/rust/comments/oversized/longread/".to_string(),
            "atom".to_string(),
        );
        post.author = Some("longwinded".to_string());
        post.subreddit = Some("rust".to_string());
        post.outbound_url = Some("https://example.com/longread".to_string());
        post.content_markdown = Some("Distributed systems are hard. ".repeat(50));
        post
    }

    #[test]
    fn prompt_version_is_enrich_v2() {
        assert_eq!(PROMPT_VERSION, "enrich-v2");
    }

    #[test]
    fn system_prompt_documents_every_enum_variant() {
        let messages = build_enrichment_messages(&normal_post(), MAX_CONTENT_CHARS);
        let system = &messages[0].content;

        for classification in [
            Classification::Article,
            Classification::Tool,
            Classification::Tutorial,
            Classification::Reference,
            Classification::Discussion,
            Classification::Question,
            Classification::News,
            Classification::Other,
        ] {
            assert!(
                system.contains(classification.as_str()),
                "rubric is missing classification `{}`",
                classification.as_str()
            );
        }

        for action in [
            RecommendedAction::ShouldTest,
            RecommendedAction::ShouldBuild,
            RecommendedAction::ReadingQueue,
            RecommendedAction::ReferenceOnly,
            RecommendedAction::Discard,
            RecommendedAction::Other,
        ] {
            assert!(
                system.contains(action.as_str()),
                "rubric is missing recommended_action `{}`",
                action.as_str()
            );
        }

        for token in ["joy_value", "work_value", "confidence", "0.0", "1.0"] {
            assert!(system.contains(token), "rubric is missing `{token}`");
        }
    }

    #[test]
    fn truncate_returns_input_unchanged_when_within_budget() {
        let (out, truncated) = truncate_for_budget("short content", MAX_CONTENT_CHARS);
        assert_eq!(out, "short content");
        assert!(!truncated);
    }

    #[test]
    fn truncate_caps_oversized_input_and_marks_it() {
        let oversized = "x".repeat(MAX_CONTENT_CHARS + 5_000);
        let (out, truncated) = truncate_for_budget(&oversized, MAX_CONTENT_CHARS);

        assert!(truncated);
        assert!(
            out.chars().count() <= MAX_CONTENT_CHARS,
            "kept {} chars, budget is {MAX_CONTENT_CHARS}",
            out.chars().count()
        );
        assert!(out.ends_with(TRUNCATION_MARKER));
    }

    #[test]
    fn truncate_counts_chars_not_bytes_on_boundary() {
        // Multi-byte scalars must never be split, and the budget is measured in
        // chars rather than bytes so it stays meaningful for human-readable text.
        let content = "é".repeat(100);
        let (out, truncated) = truncate_for_budget(&content, 50);

        assert!(truncated);
        assert!(out.chars().count() <= 50);
        assert!(out.ends_with(TRUNCATION_MARKER));
    }

    #[test]
    fn golden_messages_for_normal_post() {
        let messages = build_enrichment_messages(&normal_post(), MAX_CONTENT_CHARS);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert!(
            messages[1]
                .content
                .contains("Title: A pragmatic guide to async Rust")
        );
        assert!(!messages[1].content.contains(TRUNCATION_MARKER));

        let rendered = serde_json::to_string_pretty(&messages).expect("messages should serialize");
        assert_golden("enrichment_messages_normal.json", &rendered);
    }

    #[test]
    fn golden_messages_for_oversized_post() {
        let budget = 120;
        let messages = build_enrichment_messages(&oversized_post(), budget);

        assert_eq!(messages.len(), 2);
        assert!(messages[1].content.contains(TRUNCATION_MARKER));

        let rendered = serde_json::to_string_pretty(&messages).expect("messages should serialize");
        assert_golden("enrichment_messages_oversized.json", &rendered);
    }
}
