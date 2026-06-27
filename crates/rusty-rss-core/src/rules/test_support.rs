//! Shared fixtures for the rule-engine tests.

// TOML 1.0 forbids multi-line inline tables, so each rule sits on one line.
// The shipped seed uses the [[topics.<name>.rules]] form for readability.
pub(crate) const SEED_SNIPPET: &str = r#"
[meta]
version = "rules-v1"

[topics.memory]
threshold = 3.0
rules = [
  { id = "title_concept", signal = "title", kind = "fts", weight = 2.0, match = 'memor* OR "second brain" OR obsidian OR persisten*' },
  { id = "body_concept", signal = "body", kind = "fts", weight = 0.5, min_hits = 2, match = 'memor* OR "knowledge graph" OR retriev* OR embedding*' },
  { id = "named_tool", signal = "any", kind = "terms", weight = 3.0, match = ["mem0", "letta", "memgpt", "zep", "cognee"] },
]

[topics.memory.subreddit_prior]
opencodeCLI = 2.0
mcp = 2.0

[[topics.memory.exclude]]
id = "hardware_memory"
signal = "title"
kind = "fts"
match = 'vram OR "gpu memory" OR oom'
unless = { signal = "any", kind = "terms", match = ["mem0", "letta"] }
veto = true
"#;
