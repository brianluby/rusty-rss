//! Export command: agent-ready records as JSONL, Markdown, or CSV.

use super::ExportFormat;
use anyhow::{Result, anyhow};
use rusty_rss_core::db::{self, ExportFilters};
use rusty_rss_core::models::{Classification, ExportRecord, RecommendedAction};
use std::path::PathBuf;
use std::str::FromStr;

#[expect(
    clippy::too_many_arguments,
    reason = "CLI flags map directly to export filters"
)]
pub(super) fn run_export(
    db_path: PathBuf,
    format: ExportFormat,
    limit: usize,
    offset: usize,
    classification: Option<String>,
    action: Option<String>,
    min_joy: Option<f32>,
    min_work: Option<f32>,
) -> Result<()> {
    let conn = db::init_db(&db_path)?;
    let filters = ExportFilters {
        classification: classification
            .as_deref()
            .map(Classification::from_str)
            .transpose()
            .map_err(|err| anyhow!(err))?,
        recommended_action: action
            .as_deref()
            .map(RecommendedAction::from_str)
            .transpose()
            .map_err(|err| anyhow!(err))?,
        min_joy_value: min_joy,
        min_work_value: min_work,
    };
    let records = db::list_export_records(&conn, &filters, limit, offset)?;

    match format {
        ExportFormat::Jsonl => {
            for record in records {
                println!("{}", serde_json::to_string(&record)?);
            }
        }
        ExportFormat::Markdown => print_markdown_export(&records),
        ExportFormat::Csv => print_csv_export(&records),
    }

    Ok(())
}

fn print_markdown_export(records: &[ExportRecord]) {
    for record in records {
        println!("## {}", record.saved_post.title);
        println!();
        println!("- ID: `{}`", record.saved_post.reddit_fullname);
        println!("- Permalink: {}", record.saved_post.permalink);
        if let Some(url) = &record.saved_post.outbound_url {
            println!("- Outbound URL: {url}");
        }
        if let Some(enrichment) = &record.latest_enrichment
            && let Some(output) = &enrichment.output
        {
            println!("- Classification: {}", output.classification.as_str());
            println!(
                "- Recommended action: {}",
                output.recommended_action.as_str()
            );
            println!("- Summary: {}", output.summary);
        }
        if let Some(capture) = &record.outbound_capture {
            if let Some(title) = &capture.title {
                println!("- Captured title: {title}");
            }
            if let Some(canonical) = &capture.canonical_url {
                println!("- Canonical URL: {canonical}");
            }
            if let Some(hash) = &capture.content_hash {
                println!("- Captured content hash: {hash}");
            }
        }
        if let Some(markdown) = &record.saved_post.content_markdown {
            println!();
            println!("{}", markdown.trim());
        }
        println!();
    }
}

fn print_csv_export(records: &[ExportRecord]) {
    println!(
        "schema_version,reddit_fullname,title,subreddit,author,permalink,outbound_url,classification,recommended_action,joy_value,work_value,capture_status,captured_title,canonical_url,content_hash"
    );
    for record in records {
        let output = record
            .latest_enrichment
            .as_ref()
            .and_then(|enrichment| enrichment.output.as_ref());
        let capture = record.outbound_capture.as_ref();
        println!(
            "{}",
            [
                record.schema_version.as_str(),
                record.saved_post.reddit_fullname.as_str(),
                record.saved_post.title.as_str(),
                record.saved_post.subreddit.as_deref().unwrap_or(""),
                record.saved_post.author.as_deref().unwrap_or(""),
                record.saved_post.permalink.as_str(),
                record.saved_post.outbound_url.as_deref().unwrap_or(""),
                output
                    .map(|output| output.classification.as_str())
                    .unwrap_or(""),
                output
                    .map(|output| output.recommended_action.as_str())
                    .unwrap_or(""),
                &output
                    .map(|output| output.joy_value.to_string())
                    .unwrap_or_default(),
                &output
                    .map(|output| output.work_value.to_string())
                    .unwrap_or_default(),
                capture.map(|capture| capture.status.as_str()).unwrap_or(""),
                capture
                    .and_then(|capture| capture.title.as_deref())
                    .unwrap_or(""),
                capture
                    .and_then(|capture| capture.canonical_url.as_deref())
                    .unwrap_or(""),
                capture
                    .and_then(|capture| capture.content_hash.as_deref())
                    .unwrap_or(""),
            ]
            .map(csv_escape)
            .join(",")
        );
    }
}

fn csv_escape(value: &str) -> String {
    // Neutralize spreadsheet formula injection: a cell beginning with one of
    // these is evaluated as a formula by Excel/Sheets, and titles/subreddits
    // come from untrusted Reddit content. Prefixing a single quote defuses it.
    let neutralized;
    let value = if matches!(
        value.as_bytes().first(),
        Some(b'=' | b'+' | b'-' | b'@' | b'\t' | b'\r' | b'\n')
    ) {
        neutralized = format!("'{value}");
        neutralized.as_str()
    } else {
        value
    };

    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_escape_neutralizes_formula_injection() {
        // Formula-leading values are prefixed with a single quote.
        assert_eq!(csv_escape("=SUM(A1:A2)"), "'=SUM(A1:A2)");
        assert_eq!(csv_escape("@cmd"), "'@cmd");
        // A leading newline must also be neutralized (some parsers trim it).
        assert_eq!(csv_escape("\n=evil"), "\"'\n=evil\"");
        // Neutralized values that also need quoting still get quoted.
        assert_eq!(csv_escape("=1,2"), "\"'=1,2\"");
        // Ordinary values are untouched.
        assert_eq!(csv_escape("normal"), "normal");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
    }
}
