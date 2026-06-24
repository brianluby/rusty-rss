use crate::models::SavedPost;
use anyhow::{Context, Result, anyhow};
use scraper::{Html, Selector};
use std::io::Cursor;

#[derive(Debug, Default)]
pub struct ParsedFeed {
    pub posts: Vec<SavedPost>,
    pub errors: Vec<String>,
}

pub fn parse_atom(body: &str) -> Result<ParsedFeed> {
    let feed = feed_rs::parser::parse(Cursor::new(body)).context("failed to parse Atom feed")?;

    let mut parsed = ParsedFeed::default();

    for (index, entry) in feed.entries.into_iter().enumerate() {
        let entry_id = entry.id.clone();
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

    post.content_html = entry
        .content
        .as_ref()
        .and_then(|content| content.body.clone())
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
        fs::read_to_string("test-fixtures/atom-feed.xml").expect("fixture file should exist")
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
