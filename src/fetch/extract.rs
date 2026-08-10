//! Static, bounded extraction for native HTTP responses.
//!
//! HTML is parsed as untrusted data. This module never executes scripts or renders markup.

use std::collections::{BTreeMap, HashSet};

use scraper::{ElementRef, Html, Selector, node::Node};
use url::Url;

const MAX_LINKS: usize = 1_000;
const MAX_LINK_CHARS: usize = 2_048;
const MAX_METADATA_ENTRIES: usize = 64;
const MAX_METADATA_CHARS: usize = 4_096;

/// Options controlling the size of extracted output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractionOptions {
    /// Maximum number of Unicode scalar values in the Markdown-like output.
    pub max_chars: usize,
}

impl Default for ExtractionOptions {
    fn default() -> Self {
        Self { max_chars: 30_000 }
    }
}

/// Bounded content and metadata extracted from a static response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedDocument {
    pub markdown: String,
    pub title: Option<String>,
    pub links: Vec<String>,
    pub metadata: BTreeMap<String, String>,
    pub truncated: bool,
}

/// Extract a static response without executing or rendering its contents.
///
/// `content_type` must be `text/html`, `application/xhtml+xml`, or `text/plain`.
/// Relative links are resolved against `base_url` when it is a valid URL.
///
/// # Errors
///
/// Returns an error for unsupported content types or a zero output bound.
pub fn extract(
    body: &[u8],
    content_type: &str,
    base_url: Option<&str>,
    options: ExtractionOptions,
) -> Result<ExtractedDocument, ExtractionError> {
    if options.max_chars == 0 {
        return Err(ExtractionError::OutputBoundZero);
    }
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match media_type.as_str() {
        "text/plain" => Ok(extract_plain_text(body, options)),
        "text/html" | "application/xhtml+xml" => Ok(extract_html(body, base_url, options)),
        other => Err(ExtractionError::UnsupportedContentType(other.to_owned())),
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExtractionError {
    #[error("unsupported extraction content type: {0}")]
    UnsupportedContentType(String),
    #[error("extraction output bound must be greater than zero")]
    OutputBoundZero,
}

fn extract_plain_text(body: &[u8], options: ExtractionOptions) -> ExtractedDocument {
    let text = String::from_utf8_lossy(body);
    let normalized = normalize_whitespace(&text);
    bounded_document(
        normalized,
        None,
        Vec::new(),
        BTreeMap::new(),
        options.max_chars,
    )
}

fn extract_html(
    body: &[u8],
    base_url: Option<&str>,
    options: ExtractionOptions,
) -> ExtractedDocument {
    let html = Html::parse_document(&String::from_utf8_lossy(body));
    let title = selector("title")
        .and_then(|selector| html.select(&selector).next())
        .map(|element| normalize_whitespace(&element.text().collect::<String>()))
        .filter(|value| !value.is_empty())
        .map(|value| bound_text(value, MAX_METADATA_CHARS));

    let mut metadata = BTreeMap::new();
    if let Some(value) = html.root_element().value().attr("lang") {
        metadata.insert("language".to_owned(), bound_text(value, MAX_METADATA_CHARS));
    }
    if let Some(selector) = selector("meta[name], meta[property]") {
        for element in html.select(&selector).take(MAX_METADATA_ENTRIES) {
            if metadata.len() >= MAX_METADATA_ENTRIES {
                break;
            }
            let key = element
                .value()
                .attr("name")
                .or_else(|| element.value().attr("property"));
            let value = element.value().attr("content");
            if let (Some(key), Some(value)) = (key, value) {
                let value = bound_text(normalize_whitespace(value), MAX_METADATA_CHARS);
                if !value.is_empty() {
                    metadata
                        .entry(bound_text(key, MAX_METADATA_CHARS))
                        .or_insert(value);
                }
            }
        }
    }
    if let Some(href) = selector("link[rel=canonical][href]")
        .and_then(|selector| html.select(&selector).next())
        .and_then(|element| element.value().attr("href"))
        && let Some(link) = resolve_link(href, base_url)
    {
        metadata.insert("canonical".to_owned(), link);
    }

    let links = selector("a[href]")
        .map(|selector| {
            let mut seen = HashSet::new();
            html.select(&selector)
                .filter_map(|element| resolve_link(element.value().attr("href")?, base_url))
                .filter_map(|link| {
                    let link = bound_text(link, MAX_LINK_CHARS);
                    (seen.insert(link.clone()) && seen.len() <= MAX_LINKS).then_some(link)
                })
                .collect()
        })
        .unwrap_or_default();

    let markdown = if let Some(selector) = selector("body")
        && let Some(body) = html.select(&selector).next()
    {
        render_element(body)
    } else {
        render_element(html.root_element())
    };
    bounded_document(markdown, title, links, metadata, options.max_chars)
}

fn render_element(element: ElementRef<'_>) -> String {
    let tag = element.value().name();
    if matches!(
        tag,
        "script"
            | "style"
            | "noscript"
            | "template"
            | "svg"
            | "nav"
            | "footer"
            | "header"
            | "aside"
            | "form"
    ) {
        return String::new();
    }
    if tag == "br" {
        return "\n".to_owned();
    }
    let inner = element
        .children()
        .filter_map(|child| match child.value() {
            Node::Text(text) => Some(text.to_string()),
            _ => ElementRef::wrap(child).map(render_element),
        })
        .collect::<String>();
    let text = inner;
    let text = normalize_whitespace(&text);
    if text.is_empty() {
        return String::new();
    }
    match tag {
        "h1" => format!("\n\n# {text}\n\n"),
        "h2" => format!("\n\n## {text}\n\n"),
        "h3" => format!("\n\n### {text}\n\n"),
        "h4" | "h5" | "h6" => format!("\n\n#### {text}\n\n"),
        "p" | "div" | "section" | "article" | "main" | "li" | "blockquote" | "pre" => {
            format!("\n\n{text}\n\n")
        }
        _ => text,
    }
}

fn bounded_document(
    markdown: impl AsRef<str>,
    title: Option<String>,
    links: Vec<String>,
    metadata: BTreeMap<String, String>,
    max_chars: usize,
) -> ExtractedDocument {
    let markdown = normalize_markdown(markdown.as_ref());
    let truncated = markdown.chars().count() > max_chars;
    let markdown = markdown.chars().take(max_chars).collect();
    ExtractedDocument {
        markdown,
        title,
        links,
        metadata,
        truncated,
    }
}

fn normalize_markdown(value: &str) -> String {
    value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn normalize_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn bound_text(value: impl AsRef<str>, max_chars: usize) -> String {
    value.as_ref().chars().take(max_chars).collect()
}

fn resolve_link(href: &str, base_url: Option<&str>) -> Option<String> {
    let href = href.trim();
    if href.is_empty()
        || href.starts_with('#')
        || href.starts_with("javascript:")
        || href.starts_with("mailto:")
    {
        return None;
    }
    let link = match base_url.and_then(|base| Url::parse(base).ok()) {
        Some(base) => base.join(href).ok()?,
        None => Url::parse(href).ok()?,
    };
    matches!(link.scheme(), "http" | "https").then(|| link.to_string())
}

fn selector(value: &str) -> Option<Selector> {
    Selector::parse(value).ok()
}

#[cfg(test)]
mod tests {
    use super::{ExtractionError, ExtractionOptions, extract};

    #[test]
    fn extracts_bounded_html_title_links_and_metadata_without_script_text() {
        let fixture = br#"<html lang="en"><head><title>Fixture page</title><meta name="description" content="A test page"><link rel="canonical" href="/docs"></head><body><nav>Ignore navigation</nav><main><h1>Hello</h1><p>Readable <strong>content</strong>.</p><script>document.write('evil')</script><a href="/next">Next</a><a href="/next">Duplicate</a></main></body></html>"#;
        let result = extract(
            fixture,
            "text/html; charset=utf-8",
            Some("https://example.com/start"),
            ExtractionOptions::default(),
        )
        .unwrap();
        assert_eq!(result.title.as_deref(), Some("Fixture page"));
        assert!(result.markdown.contains("# Hello"));
        assert!(result.markdown.contains("Readable content."));
        assert!(!result.markdown.contains("evil"));
        assert_eq!(result.links, ["https://example.com/next"]);
        assert_eq!(
            result.metadata.get("description").map(String::as_str),
            Some("A test page")
        );
        assert_eq!(
            result.metadata.get("canonical").map(String::as_str),
            Some("https://example.com/docs")
        );
    }

    #[test]
    fn extracts_plain_text_and_marks_truncation() {
        let result = extract(
            b"  one\n two  three ",
            "text/plain",
            None,
            ExtractionOptions { max_chars: 7 },
        )
        .unwrap();
        assert_eq!(result.markdown, "one two");
        assert!(result.truncated);
        assert!(result.title.is_none());
    }

    #[test]
    fn rejects_zero_bound_and_unsupported_type() {
        assert_eq!(
            extract(b"x", "text/plain", None, ExtractionOptions { max_chars: 0 }),
            Err(ExtractionError::OutputBoundZero)
        );
        assert_eq!(
            extract(b"x", "application/pdf", None, ExtractionOptions::default()),
            Err(ExtractionError::UnsupportedContentType(
                "application/pdf".to_owned()
            ))
        );
    }
}
