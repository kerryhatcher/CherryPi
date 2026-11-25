//! Web search and page fetching for the agent's `web_search` and `web_fetch`
//! tools. Requests go through reqwest. Search hits are normalized to
//! [`SearchResult`], so the model sees the same shape whatever backend
//! produced them.

use std::iter::Peekable;
use std::str::Chars;
use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Browser-like User-Agent sent on every request.
pub const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";

/// Environment variable holding the Brave Search API key.
pub const BRAVE_API_KEY_ENV: &str = "BRAVE_SEARCH_API_KEY";

const BRAVE_SEARCH_URL: &str = "https://api.search.brave.com/res/v1/web/search";
const BRAVE_MAX_COUNT: u64 = 20;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const FETCH_MAX_CHARS: usize = 20_000;

/// One search hit, in the shape every search backend is normalized to.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Deserialize)]
struct BraveResponse {
    web: Option<BraveWeb>,
}

#[derive(Deserialize)]
struct BraveWeb {
    #[serde(default)]
    results: Vec<BraveResult>,
}

#[derive(Deserialize)]
struct BraveResult {
    title: Option<String>,
    url: Option<String>,
    description: Option<String>,
}

/// Build the shared HTTP client (browser User-Agent, request timeout).
pub fn client() -> Result<Client, String> {
    Client::builder()
        .user_agent(USER_AGENT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))
}

/// Run a Brave web search and return normalized results. `count` is clamped
/// to Brave's limit of 1..=20.
pub async fn brave_search(
    client: &Client,
    api_key: &str,
    query: &str,
    count: u64,
) -> Result<Vec<SearchResult>, String> {
    let count = count.clamp(1, BRAVE_MAX_COUNT).to_string();
    let resp = client
        .get(BRAVE_SEARCH_URL)
        .query(&[("q", query), ("count", count.as_str())])
        .header("X-Subscription-Token", api_key)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("Brave Search request failed: {e}"))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("Failed to read Brave Search response: {e}"))?;
    if !status.is_success() {
        return Err(format!(
            "Brave Search returned {status}: {}",
            truncate_chars(&body, 300)
        ));
    }
    normalize_brave(&body)
}

/// Convert a raw Brave Search JSON body into [`SearchResult`]s. Hits without a
/// URL are dropped. Titles and snippets have HTML markup removed, since Brave
/// wraps matched terms in tags.
pub fn normalize_brave(body: &str) -> Result<Vec<SearchResult>, String> {
    let parsed: BraveResponse = serde_json::from_str(body)
        .map_err(|e| format!("Failed to parse Brave Search response: {e}"))?;
    let results = parsed.web.map(|w| w.results).unwrap_or_default();
    Ok(results
        .into_iter()
        .filter_map(|r| {
            let url = r.url.filter(|u| !u.trim().is_empty())?;
            Some(SearchResult {
                title: html_to_text(&r.title.unwrap_or_default()),
                url,
                snippet: html_to_text(&r.description.unwrap_or_default()),
            })
        })
        .collect())
}

/// Fetch an http(s) URL and return its text. HTML is reduced to plain text and
/// the result is capped at a fixed size.
pub async fn fetch_text(client: &Client, url: &str) -> Result<String, String> {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("Only http:// and https:// URLs can be fetched".into());
    }

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Fetch failed: {e}"))?;
    let status = resp.status();
    let is_html = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.to_ascii_lowercase().contains("html"));
    let body = resp
        .text()
        .await
        .map_err(|e| format!("Failed to read response body: {e}"))?;
    if !status.is_success() {
        return Err(format!("Fetch returned {status}"));
    }

    let text = if is_html { html_to_text(&body) } else { body };
    Ok(truncate_chars(&text, FETCH_MAX_CHARS))
}

/// Reduce HTML to plain text. Drops `<script>` and `<style>` bodies and tags,
/// decodes common entities, and collapses whitespace. This is a deliberately
/// simple extractor, not a full HTML parser.
pub fn html_to_text(input: &str) -> String {
    let without_blocks = strip_blocks(&strip_blocks(input, "script"), "style");

    let mut stripped = String::with_capacity(without_blocks.len());
    let mut in_tag = false;
    let mut chars: Peekable<Chars<'_>> = without_blocks.chars().peekable();
    while let Some(c) = chars.next() {
        if in_tag {
            if c == '>' {
                in_tag = false;
                stripped.push(' ');
            }
            continue;
        }
        // Only `<` that opens a tag starts one, so "a < b" keeps its text.
        if c == '<' && chars.peek().is_some_and(|n| n.is_ascii_alphabetic() || *n == '/' || *n == '!') {
            in_tag = true;
        } else {
            stripped.push(c);
        }
    }

    let decoded = stripped
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Remove every `<tag ...>...</tag>` block, case-insensitively.
fn strip_blocks(html: &str, tag: &str) -> String {
    // ASCII lowercasing keeps byte offsets identical to `html`.
    let lower = html.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(html.len());
    let mut pos = 0;
    while let Some(rel) = lower[pos..].find(&open) {
        let start = pos + rel;
        out.push_str(&html[pos..start]);
        out.push(' ');
        match lower[start..].find(&close) {
            Some(end_rel) => pos = start + end_rel + close.len(),
            None => {
                pos = html.len();
                break;
            }
        }
    }
    out.push_str(&html[pos..]);
    out
}

/// Keep at most `max` characters, marking the cut.
pub fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((idx, _)) => format!("{}\n[truncated]", &s[..idx]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_brave_results() {
        let body = r#"{
            "type": "search",
            "web": {"results": [
                {"title": "Rust <strong>lang</strong>", "url": "https://www.rust-lang.org/", "description": "A &quot;safe&quot; language."},
                {"title": "No URL", "description": "dropped"},
                {"title": "Blank URL", "url": "  ", "description": "dropped"},
                {"title": "Minimal", "url": "https://example.com/"}
            ]}
        }"#;
        let results = normalize_brave(body).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust lang");
        assert_eq!(results[0].url, "https://www.rust-lang.org/");
        assert_eq!(results[0].snippet, "A \"safe\" language.");
        assert_eq!(results[1].title, "Minimal");
        assert_eq!(results[1].url, "https://example.com/");
        assert_eq!(results[1].snippet, "");
    }

    #[test]
    fn brave_body_without_web_section_yields_no_results() {
        assert!(normalize_brave(r#"{"type":"search"}"#).unwrap().is_empty());
    }

    #[test]
    fn brave_invalid_json_is_an_error() {
        assert!(normalize_brave("<html>rate limited</html>").is_err());
    }

    #[test]
    fn html_to_text_drops_scripts_styles_and_tags() {
        let html = "<html><head><style>p{color:red}</style></head>\
                    <body><script>var x = '<b>';</script><p>Hello <b>big</b>&nbsp;world</p></body></html>";
        assert_eq!(html_to_text(html), "Hello big world");
    }

    #[test]
    fn html_to_text_keeps_literal_less_than() {
        assert_eq!(html_to_text("if a < b then &amp; done"), "if a < b then & done");
    }

    #[test]
    fn html_to_text_script_block_case_insensitive() {
        assert_eq!(html_to_text("a<SCRIPT>x()</Script>b"), "a b");
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate_chars("héllo", 10), "héllo");
        assert_eq!(truncate_chars("héllo", 2), "hé\n[truncated]");
    }

    /// Live check against the Brave API. Needs BRAVE_SEARCH_API_KEY in the
    /// environment, e.g. `sops exec-env ~/secrets/secrets.env 'cargo test ...'`.
    #[tokio::test]
    #[ignore]
    async fn live_brave_search() {
        let key = std::env::var(BRAVE_API_KEY_ENV).expect("BRAVE_SEARCH_API_KEY not set");
        let client = client().unwrap();
        let results = brave_search(&client, key.trim(), "rust programming language", 3)
            .await
            .unwrap();
        assert!(!results.is_empty());
        for r in &results {
            println!("{} | {}", r.title, r.url);
        }
    }

    #[tokio::test]
    async fn fetch_rejects_non_http_schemes() {
        let client = client().unwrap();
        let err = fetch_text(&client, "file:///etc/passwd").await.unwrap_err();
        assert!(err.contains("http"), "{err}");
    }
}
