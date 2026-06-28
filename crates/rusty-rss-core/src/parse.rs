use crate::models::SavedPost;
use anyhow::{Context, Result, anyhow};
use scraper::{Html, Selector};
use std::io::Cursor;

#[derive(Debug, Default)]
pub struct ParsedFeed {
    pub posts: Vec<SavedPost>,
    pub errors: Vec<String>,
    pub entry_count: usize,
    pub last_entry_id: Option<String>,
}

pub fn parse_atom(body: &str) -> Result<ParsedFeed> {
    let feed = feed_rs::parser::parse(Cursor::new(body)).context("failed to parse Atom feed")?;

    let mut parsed = ParsedFeed::default();

    for (index, entry) in feed.entries.into_iter().enumerate() {
        let entry_id = entry.id.clone();
        parsed.entry_count += 1;
        if !entry_id.trim().is_empty() {
            parsed.last_entry_id = Some(entry_id.trim().to_string());
        }
        match parse_entry(entry) {
            Ok(post) => parsed.posts.push(post),
            Err(e) => parsed.errors.push(format!(
                "entry {} ({}): {}",
                index + 1,
                if entry_id.is_empty() {
                    "no id"
                } else {
                    &entry_id
                },
                e
            )),
        }
    }

    Ok(parsed)
}

fn parse_entry(entry: feed_rs::model::Entry) -> Result<SavedPost> {
    let fullname = non_empty(entry.id.clone(), "entry/id")?;
    let title = entry
        .title
        .as_ref()
        .map(|t| t.content.trim().to_string())
        .filter(|title| !title.is_empty())
        .ok_or_else(|| anyhow!("missing entry/title"))?;

    let permalink = entry
        .links
        .iter()
        .find(|link| link.rel.is_none() || link.rel.as_deref() == Some("alternate"))
        .or_else(|| entry.links.first())
        .map(|link| link.href.trim().to_string())
        .filter(|href| !href.is_empty())
        .ok_or_else(|| anyhow!("missing entry/link href"))?;

    let mut post = SavedPost::new(fullname, title, permalink, "atom".to_string());

    post.author = entry
        .authors
        .first()
        .map(|author| author.name.trim().to_string())
        .filter(|name| !name.is_empty());

    post.subreddit = entry
        .categories
        .first()
        .map(|category| category.term.trim().to_string())
        .filter(|term| !term.is_empty());

    post.published_at = entry.published;
    post.updated_at = entry.updated;

    post.content_markdown = entry
        .content
        .as_ref()
        .and_then(|content| content.body.as_deref())
        .map(html_to_markdown)
        .filter(|body| !body.trim().is_empty());

    post.thumbnail_url = extract_thumbnail(&entry);
    post.outbound_url = extract_outbound_url(&entry);

    Ok(post)
}

fn non_empty(value: String, field: &str) -> Result<String> {
    let value = value.trim().to_string();
    if value.is_empty() {
        Err(anyhow!("missing {field}"))
    } else {
        Ok(value)
    }
}

fn extract_thumbnail(entry: &feed_rs::model::Entry) -> Option<String> {
    entry
        .media
        .iter()
        .flat_map(|media| media.thumbnails.iter())
        .map(|thumbnail| thumbnail.image.uri.trim().to_string())
        .find(|uri| !uri.is_empty())
        .or_else(|| {
            entry
                .links
                .iter()
                .find(|link| link.rel.as_deref() == Some("enclosure"))
                .map(|link| link.href.trim().to_string())
                .filter(|href| !href.is_empty())
        })
}

fn html_to_markdown(html: &str) -> String {
    html2md::parse_html(html).trim().to_string()
}

fn extract_outbound_url(entry: &feed_rs::model::Entry) -> Option<String> {
    let html = entry
        .content
        .as_ref()
        .and_then(|content| content.body.as_deref())?;

    extract_first_link_url(html)
}

fn extract_first_link_url(html: &str) -> Option<String> {
    let selector = Selector::parse("a[href]").ok()?;
    let document = Html::parse_fragment(html);

    document
        .select(&selector)
        .filter_map(|element| element.value().attr("href"))
        .map(str::trim)
        .find(|url| is_valid_outbound(url))
        .map(str::to_string)
}

fn is_valid_outbound(url: &str) -> bool {
    !url.is_empty()
        && !url.starts_with('#')
        && !url.starts_with("http://localhost")
        && !url.contains("reddit.com")
        && !url.contains("redd.it")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn load_fixture() -> String {
        fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../test-fixtures/atom-feed.xml"
        ))
        .expect("fixture file should exist")
    }

    fn parse_fixture_posts() -> Vec<SavedPost> {
        let body = load_fixture();
        let parsed = parse_atom(&body).expect("parse should succeed");
        assert!(
            parsed.errors.is_empty(),
            "unexpected parse errors: {:?}",
            parsed.errors
        );
        parsed.posts
    }

    #[test]
    fn parses_atom_fixture() {
        let posts = parse_fixture_posts();

        assert_eq!(posts.len(), 3);
    }

    #[test]
    fn parses_fullname_and_id() {
        let posts = parse_fixture_posts();

        let first = &posts[0];
        assert_eq!(first.reddit_fullname, "t3_abc123");
        assert_eq!(first.reddit_id, "abc123");
    }

    #[test]
    fn parses_saved_comment_id() {
        let feed = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Saved</title>
  <link href="https://example.com" rel="self"/>
  <id>tag:example,2026:saved</id>
  <updated>2026-01-01T00:00:00Z</updated>
  <entry>
    <id>t1_comment123</id>
    <title>Saved comment</title>
    <link href="https://www.reddit.com/r/rust/comments/post/comment/comment123/" rel="alternate"/>
    <updated>2026-01-01T00:00:00Z</updated>
  </entry>
</feed>"#;

        let parsed = parse_atom(feed).expect("feed should parse");
        assert!(parsed.errors.is_empty());
        assert_eq!(parsed.posts[0].reddit_fullname, "t1_comment123");
        assert_eq!(parsed.posts[0].reddit_id, "comment123");
    }

    #[test]
    fn parses_title() {
        let posts = parse_fixture_posts();

        assert_eq!(posts[0].title, "Rust 1.96 Released");
        assert_eq!(posts[1].title, "Effective Go");
    }

    #[test]
    fn parses_author() {
        let posts = parse_fixture_posts();

        assert_eq!(posts[0].author, Some("rust_lang".to_string()));
        assert_eq!(posts[1].author, Some("gopher_dev".to_string()));
    }

    #[test]
    fn parses_subreddit() {
        let posts = parse_fixture_posts();

        assert_eq!(posts[0].subreddit, Some("rust".to_string()));
        assert_eq!(posts[1].subreddit, Some("golang".to_string()));
    }

    #[test]
    fn parses_permalink() {
        let posts = parse_fixture_posts();

        assert!(posts[0].permalink.contains("abc123"));
        assert!(posts[1].permalink.contains("def456"));
    }

    #[test]
    fn parses_media_thumbnail() {
        let posts = parse_fixture_posts();

        assert!(posts[0].thumbnail_url.is_some());
        assert!(posts[1].thumbnail_url.is_none());
    }

    #[test]
    fn parses_timestamps() {
        let posts = parse_fixture_posts();

        assert!(posts[0].published_at.is_some());
        assert!(posts[0].updated_at.is_some());
    }

    #[test]
    fn extracts_outbound_url() {
        let posts = parse_fixture_posts();

        assert_eq!(
            posts[0].outbound_url,
            Some("https://blog.rust-lang.org/1.96.0/".to_string())
        );
        assert_eq!(
            posts[1].outbound_url,
            Some("https://go.dev/doc/effective_go".to_string())
        );
        assert!(posts[2].outbound_url.is_none());
    }

    #[test]
    fn converts_html_content_to_markdown() {
        let posts = parse_fixture_posts();

        assert_eq!(
            posts[0].content_markdown,
            Some("[https://blog.rust-lang.org/1.96.0/](https://blog.rust-lang.org/1.96.0/) Rust 1.96 is here with exciting new features.".to_string())
        );
        assert_eq!(
            posts[1].content_markdown,
            Some(
                "[https://go.dev/doc/effective\\_go](https://go.dev/doc/effective_go)".to_string()
            )
        );
        assert_eq!(
            posts[2].content_markdown,
            Some("This is a self-post with no external link.".to_string())
        );
    }

    #[test]
    fn converts_basic_html_formatting_to_markdown() {
        assert_eq!(
            html_to_markdown("<p>Hello <strong>Rust</strong></p>"),
            "Hello **Rust**"
        );
    }

    #[test]
    fn filters_reddit_urls_as_outbound() {
        assert!(!is_valid_outbound("https://www.reddit.com/r/rust"));
        assert!(!is_valid_outbound("https://old.reddit.com/r/rust"));
        assert!(!is_valid_outbound("https://redd.it/abc123"));
        assert!(is_valid_outbound("https://example.com/post"));
    }

    #[test]
    fn handles_empty_feed() {
        let empty_feed = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Empty</title>
  <link href="https://example.com" rel="self"/>
  <id>tag:example,2026:empty</id>
  <updated>2026-01-01T00:00:00Z</updated>
</feed>"#;

        let parsed = parse_atom(empty_feed).expect("should parse empty feed");
        assert!(parsed.posts.is_empty());
        assert!(parsed.errors.is_empty());
    }

    #[test]
    fn parse_atom_rejects_malformed_xml() {
        // feed-rs must reject input that is not a well-formed feed; the error is
        // surfaced (not swallowed) so the caller can record a failed sync.
        let err = parse_atom("<<<not a feed>>>").expect_err("malformed XML should fail");
        assert!(
            err.to_string().contains("failed to parse Atom feed"),
            "got: {err}"
        );
    }

    #[test]
    fn reports_entry_missing_link_href() {
        // An entry with id + title but no <link> exercises the missing-href branch
        // in parse_entry: the entry is rejected and reported, not panicked on.
        let feed = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>No Link</title>
  <link href="https://example.com" rel="self"/>
  <id>tag:example,2026:nolink</id>
  <updated>2026-01-01T00:00:00Z</updated>
  <entry>
    <id>t3_nolink</id>
    <title>Has title but no link</title>
    <updated>2026-01-01T00:00:00Z</updated>
  </entry>
</feed>"#;

        let parsed = parse_atom(feed).expect("feed parse should succeed");

        assert!(parsed.posts.is_empty());
        assert_eq!(parsed.errors.len(), 1);
        assert!(
            parsed.errors[0].contains("missing entry/link href"),
            "got: {:?}",
            parsed.errors
        );
    }

    #[test]
    fn non_empty_rejects_blank_id_and_trims_value() {
        // feed-rs synthesizes an id when <id> is absent, so the empty-id branch of
        // parse_entry is exercised directly against the non_empty guard: a
        // whitespace-only value is rejected with the field name, and a padded
        // value is trimmed.
        let err = non_empty("   ".to_string(), "entry/id").expect_err("blank id should fail");
        assert!(err.to_string().contains("missing entry/id"), "got: {err}");

        let trimmed =
            non_empty("  t3_padded  ".to_string(), "entry/id").expect("non-blank id should pass");
        assert_eq!(trimmed, "t3_padded");
    }

    #[test]
    fn extract_thumbnail_falls_back_to_enclosure_link() {
        // No media:thumbnail, but an enclosure <link> is present: the thumbnail
        // must fall back to the enclosure href.
        let feed = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Enclosure</title>
  <link href="https://example.com" rel="self"/>
  <id>tag:example,2026:enclosure</id>
  <updated>2026-01-01T00:00:00Z</updated>
  <entry>
    <id>t3_enclosure</id>
    <title>Has enclosure</title>
    <link href="https://www.reddit.com/r/rust/comments/enclosure/" rel="alternate"/>
    <link href="https://img.example.com/preview.jpg" rel="enclosure"/>
    <updated>2026-01-01T00:00:00Z</updated>
  </entry>
</feed>"#;

        let parsed = parse_atom(feed).expect("feed parse should succeed");

        assert!(parsed.errors.is_empty(), "errors: {:?}", parsed.errors);
        assert_eq!(
            parsed.posts[0].thumbnail_url,
            Some("https://img.example.com/preview.jpg".to_string())
        );
    }

    #[test]
    fn reports_invalid_entries() {
        let feed = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Invalid</title>
  <link href="https://example.com" rel="self"/>
  <id>tag:example,2026:invalid</id>
  <updated>2026-01-01T00:00:00Z</updated>
  <entry>
    <id>t3_missing_title</id>
    <link href="https://www.reddit.com/r/rust/comments/missing/" rel="alternate"/>
    <updated>2026-01-01T00:00:00Z</updated>
  </entry>
</feed>"#;

        let parsed = parse_atom(feed).expect("feed parse should succeed");

        assert!(parsed.posts.is_empty());
        assert_eq!(parsed.errors.len(), 1);
        assert!(parsed.errors[0].contains("missing entry/title"));
    }
}
