//! Deterministic, offline "AI-sort": map an [`EnrichmentOutput`] to the lists
//! it belongs to.
//!
//! This module is pure (no IO, no LLM, no clock) and fully deterministic: the
//! same input always yields the same ordered, de-duplicated list set. It is the
//! single seam a future CEL rule engine is intended to replace, so the policy
//! lives behind one function ([`lists_for`]) with an explicit [`SortConfig`].

use crate::models::{Classification, EnrichmentOutput, RecommendedAction};

/// A destination list a post can be sorted into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum List {
    ShouldTest,
    ShouldBuild,
    ReadingQueue,
    Reference,
    Discard,
}

/// Tunable thresholds for the [`lists_for`] policy.
#[derive(Debug, Clone, Copy)]
pub struct SortConfig {
    /// `work_value >= build_threshold` adds [`List::ShouldBuild`].
    pub build_threshold: f32,
    /// `joy_value >= reading_threshold` adds [`List::ReadingQueue`].
    pub reading_threshold: f32,
    /// A `Tool`/`Tutorial` with `work_value >= test_threshold` adds
    /// [`List::ShouldTest`].
    pub test_threshold: f32,
    /// A `Discard` with `confidence < min_discard_confidence` is treated as
    /// low-confidence and routed to [`List::Reference`] for manual review
    /// instead of being discarded.
    pub min_discard_confidence: f32,
}

impl Default for SortConfig {
    fn default() -> Self {
        Self {
            build_threshold: 0.7,
            reading_threshold: 0.6,
            test_threshold: 0.6,
            min_discard_confidence: 0.5,
        }
    }
}

/// Deterministic, offline policy mapping an [`EnrichmentOutput`] to the lists it
/// belongs to. The returned vector preserves policy order and is de-duplicated.
///
/// This is the single seam a future CEL rule engine replaces.
pub fn lists_for(output: &EnrichmentOutput, cfg: &SortConfig) -> Vec<List> {
    // Steps 1 & 2: a `Discard` recommendation is terminal. A low-confidence
    // discard is routed to manual review (Reference) instead of being dropped;
    // a confident discard is the only path to the Discard list.
    if output.recommended_action == RecommendedAction::Discard {
        if output.confidence < cfg.min_discard_confidence {
            return vec![List::Reference];
        }
        return vec![List::Discard];
    }

    let mut lists = Vec::new();

    // Step 3: base list from the recommended action. `Other` (and the already
    // handled `Discard`) contribute no base list.
    let base = match output.recommended_action {
        RecommendedAction::ShouldTest => Some(List::ShouldTest),
        RecommendedAction::ShouldBuild => Some(List::ShouldBuild),
        RecommendedAction::ReadingQueue => Some(List::ReadingQueue),
        RecommendedAction::ReferenceOnly => Some(List::Reference),
        RecommendedAction::Other | RecommendedAction::Discard => None,
    };
    if let Some(list) = base {
        lists.push(list);
    }

    // Step 4: threshold add-ons, in policy order (build, reading, test).
    if output.work_value >= cfg.build_threshold {
        push_unique(&mut lists, List::ShouldBuild);
    }
    if output.joy_value >= cfg.reading_threshold {
        push_unique(&mut lists, List::ReadingQueue);
    }
    if matches!(
        output.classification,
        Classification::Tool | Classification::Tutorial
    ) && output.work_value >= cfg.test_threshold
    {
        push_unique(&mut lists, List::ShouldTest);
    }

    // Step 5: fallback to manual review when nothing else matched.
    if lists.is_empty() {
        lists.push(List::Reference);
    }

    lists
}

/// Append `list` only if it is not already present, preserving insertion order.
fn push_unique(lists: &mut Vec<List>, list: List) {
    if !lists.contains(&list) {
        lists.push(list);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an [`EnrichmentOutput`] with only the fields the policy reads set
    /// to meaningful values; the rest are inert placeholders.
    fn output(
        classification: Classification,
        action: RecommendedAction,
        joy_value: f32,
        work_value: f32,
        confidence: f32,
    ) -> EnrichmentOutput {
        EnrichmentOutput {
            classification,
            tags: Vec::new(),
            summary: "summary".to_string(),
            joy_value,
            work_value,
            recommended_action: action,
            rationale: "rationale".to_string(),
            confidence,
        }
    }

    #[test]
    fn lists_for_matches_policy_table() {
        let cfg = SortConfig::default();

        // (name, output, expected lists)
        let cases: Vec<(&str, EnrichmentOutput, Vec<List>)> = vec![
            // --- base actions in isolation (low scores, no add-ons) ---
            (
                "base should_test",
                output(
                    Classification::Article,
                    RecommendedAction::ShouldTest,
                    0.0,
                    0.0,
                    1.0,
                ),
                vec![List::ShouldTest],
            ),
            (
                "base should_build",
                output(
                    Classification::Article,
                    RecommendedAction::ShouldBuild,
                    0.0,
                    0.0,
                    1.0,
                ),
                vec![List::ShouldBuild],
            ),
            (
                "base reading_queue",
                output(
                    Classification::Article,
                    RecommendedAction::ReadingQueue,
                    0.0,
                    0.0,
                    1.0,
                ),
                vec![List::ReadingQueue],
            ),
            (
                "base reference_only -> reference",
                output(
                    Classification::Article,
                    RecommendedAction::ReferenceOnly,
                    0.0,
                    0.0,
                    1.0,
                ),
                vec![List::Reference],
            ),
            (
                "base other -> fallback reference",
                output(
                    Classification::Article,
                    RecommendedAction::Other,
                    0.0,
                    0.0,
                    1.0,
                ),
                vec![List::Reference],
            ),
            // --- threshold add-ons in isolation (Other base, Article unless noted) ---
            (
                "work >= build_threshold adds should_build",
                output(
                    Classification::Article,
                    RecommendedAction::Other,
                    0.0,
                    0.7,
                    1.0,
                ),
                vec![List::ShouldBuild],
            ),
            (
                "joy >= reading_threshold adds reading_queue",
                output(
                    Classification::Article,
                    RecommendedAction::Other,
                    0.6,
                    0.0,
                    1.0,
                ),
                vec![List::ReadingQueue],
            ),
            (
                "tool + work >= test_threshold adds should_test",
                output(
                    Classification::Tool,
                    RecommendedAction::Other,
                    0.0,
                    0.6,
                    1.0,
                ),
                vec![List::ShouldTest],
            ),
            (
                "tutorial + work >= test_threshold adds should_test",
                output(
                    Classification::Tutorial,
                    RecommendedAction::Other,
                    0.0,
                    0.6,
                    1.0,
                ),
                vec![List::ShouldTest],
            ),
            (
                "non-tool does not get should_test from work alone",
                output(
                    Classification::Article,
                    RecommendedAction::Other,
                    0.0,
                    0.6,
                    1.0,
                ),
                vec![List::Reference],
            ),
            // --- threshold boundaries ---
            (
                "build boundary: exactly at threshold qualifies",
                output(
                    Classification::Article,
                    RecommendedAction::Other,
                    0.0,
                    0.7,
                    1.0,
                ),
                vec![List::ShouldBuild],
            ),
            (
                "build boundary: just below falls back",
                output(
                    Classification::Article,
                    RecommendedAction::Other,
                    0.0,
                    0.69,
                    1.0,
                ),
                vec![List::Reference],
            ),
            (
                "reading boundary: exactly at threshold qualifies",
                output(
                    Classification::Article,
                    RecommendedAction::Other,
                    0.6,
                    0.0,
                    1.0,
                ),
                vec![List::ReadingQueue],
            ),
            (
                "reading boundary: just below falls back",
                output(
                    Classification::Article,
                    RecommendedAction::Other,
                    0.59,
                    0.0,
                    1.0,
                ),
                vec![List::Reference],
            ),
            (
                "test boundary: exactly at threshold qualifies",
                output(
                    Classification::Tool,
                    RecommendedAction::Other,
                    0.0,
                    0.6,
                    1.0,
                ),
                vec![List::ShouldTest],
            ),
            (
                "test boundary: just below falls back",
                output(
                    Classification::Tool,
                    RecommendedAction::Other,
                    0.0,
                    0.59,
                    1.0,
                ),
                vec![List::Reference],
            ),
            // --- discard overrides ---
            (
                "low-confidence discard -> reference (manual review)",
                output(
                    Classification::Article,
                    RecommendedAction::Discard,
                    0.0,
                    0.0,
                    0.4,
                ),
                vec![List::Reference],
            ),
            (
                "low-confidence discard ignores high scores",
                output(
                    Classification::Tool,
                    RecommendedAction::Discard,
                    1.0,
                    1.0,
                    0.1,
                ),
                vec![List::Reference],
            ),
            (
                "discard confidence boundary: at threshold is confident -> discard",
                output(
                    Classification::Article,
                    RecommendedAction::Discard,
                    0.0,
                    0.0,
                    0.5,
                ),
                vec![List::Discard],
            ),
            (
                "confident discard -> discard (terminal)",
                output(
                    Classification::Article,
                    RecommendedAction::Discard,
                    0.0,
                    0.0,
                    0.9,
                ),
                vec![List::Discard],
            ),
            (
                "confident discard ignores high scores (terminal)",
                output(
                    Classification::Tool,
                    RecommendedAction::Discard,
                    1.0,
                    1.0,
                    1.0,
                ),
                vec![List::Discard],
            ),
            // --- multi-list: order preserved + de-duplicated ---
            (
                "should_build base + work add-on dedups to single should_build",
                output(
                    Classification::Article,
                    RecommendedAction::ShouldBuild,
                    0.6,
                    0.7,
                    1.0,
                ),
                vec![List::ShouldBuild, List::ReadingQueue],
            ),
            (
                "should_test base + tool work add-on dedups, build add-on appended",
                output(
                    Classification::Tool,
                    RecommendedAction::ShouldTest,
                    0.0,
                    0.7,
                    1.0,
                ),
                vec![List::ShouldTest, List::ShouldBuild],
            ),
            (
                "other base + all add-ons in policy order (build, reading, test)",
                output(
                    Classification::Tool,
                    RecommendedAction::Other,
                    0.6,
                    0.7,
                    1.0,
                ),
                vec![List::ShouldBuild, List::ReadingQueue, List::ShouldTest],
            ),
            (
                "reading_queue base + joy add-on dedups",
                output(
                    Classification::Article,
                    RecommendedAction::ReadingQueue,
                    0.6,
                    0.0,
                    1.0,
                ),
                vec![List::ReadingQueue],
            ),
        ];

        for (name, out, expected) in &cases {
            assert_eq!(lists_for(out, &cfg), *expected, "case: {name}");
        }
    }

    #[test]
    fn list_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&List::ShouldTest).unwrap(),
            "\"should_test\""
        );
        assert_eq!(
            serde_json::to_string(&List::ShouldBuild).unwrap(),
            "\"should_build\""
        );
        assert_eq!(
            serde_json::to_string(&List::ReadingQueue).unwrap(),
            "\"reading_queue\""
        );
        assert_eq!(
            serde_json::to_string(&List::Reference).unwrap(),
            "\"reference\""
        );
        assert_eq!(
            serde_json::to_string(&List::Discard).unwrap(),
            "\"discard\""
        );
    }

    #[test]
    fn default_config_has_documented_thresholds() {
        let cfg = SortConfig::default();
        assert_eq!(cfg.build_threshold, 0.7);
        assert_eq!(cfg.reading_threshold, 0.6);
        assert_eq!(cfg.test_threshold, 0.6);
        assert_eq!(cfg.min_discard_confidence, 0.5);
    }
}
