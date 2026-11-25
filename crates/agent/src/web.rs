//! Web search and page fetching for the agent's `web_search` and `web_fetch`
//! tools. Requests go through reqwest. Search hits are normalized to
//! [`SearchResult`], so the model sees the same shape whatever backend
//! produced them.

use std::iter::Peekable;
use std::str::Chars;
use std::time::Duration;

use encoding_rs::{Encoding, UTF_8};
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Browser-like User-Agent sent on every request.
pub const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";

/// Environment variable holding the Brave Search API key.
pub const BRAVE_API_KEY_ENV: &str = "BRAVE_SEARCH_API_KEY";

const BRAVE_SEARCH_URL: &str = "https://api.search.brave.com/res/v1/web/search";
const BRAVE_MAX_COUNT: u64 = 20;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Maximum characters of page text returned to the model.
pub const FETCH_MAX_CHARS: usize = 20_000;
/// Raw response bytes read before giving up, so a huge page cannot exhaust memory.
const FETCH_MAX_BYTES: usize = 5 * 1024 * 1024;

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
    fetch_text_limited(client, url, FETCH_MAX_BYTES).await
}

/// Accept only absolute http and https URLs. Returns the trimmed URL.
pub fn validate_http_url(url: &str) -> Result<&str, String> {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        Ok(url)
    } else {
        Err("Only http:// and https:// URLs can be fetched".into())
    }
}

async fn fetch_text_limited(client: &Client, url: &str, max_bytes: usize) -> Result<String, String> {
    let url = validate_http_url(url)?;

    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Fetch failed: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("Fetch returned {status}"));
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let is_html = content_type
        .as_deref()
        .is_some_and(|ct| ct.to_ascii_lowercase().contains("html"));

    let bytes = read_capped(&mut resp, max_bytes).await?;
    let body = decode_body(&bytes, content_type.as_deref(), is_html);

    let text = if is_html { html_to_text(&body) } else { body };
    Ok(truncate_chars(&text, FETCH_MAX_CHARS))
}

/// Read the response body chunk by chunk, failing once it exceeds `max_bytes`.
/// This does not depend on Content-Length, which a response may omit.
async fn read_capped(resp: &mut reqwest::Response, max_bytes: usize) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("Failed to read response body: {e}"))?
    {
        if buf.len() + chunk.len() > max_bytes {
            return Err(format!("Response body exceeds {max_bytes} bytes; not fetched"));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
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

    let decoded = decode_entities(&stripped);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decode named and numeric (decimal or hex) character references in one
/// pass. A decoded result is never decoded again, so `&amp;#39;` becomes
/// `&#39;`. Unknown or malformed references are left as written.
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        match parse_entity(after) {
            Some((ch, len)) => {
                out.push(ch);
                rest = &after[len..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Parse one reference at the start of `s` (which begins with `&`). Returns the
/// character and the number of bytes consumed.
fn parse_entity(s: &str) -> Option<(char, usize)> {
    // Entity names are short. Search only a small window, and scan bytes so a
    // multi-byte character at the window edge cannot cause a slicing panic.
    let bytes = s.as_bytes();
    let end = bytes[..bytes.len().min(12)].iter().position(|&b| b == b';')?;
    let body = &s[1..end];
    let ch = if let Some(num) = body.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse::<u32>().ok()?,
        };
        char::from_u32(code)?
    } else {
        match body {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            "nbsp" => ' ',
            _ => return None,
        }
    };
    Some((ch, end + 1))
}

/// Decode a response body to text. The charset comes from the Content-Type
/// header or, for HTML, a `<meta charset>` tag. Otherwise the body is UTF-8.
/// Invalid sequences become U+FFFD. A byte-order mark is also respected.
fn decode_body(bytes: &[u8], content_type: Option<&str>, is_html: bool) -> String {
    let label = content_type
        .and_then(charset_from_content_type)
        .or_else(|| if is_html { charset_from_meta(bytes) } else { None });
    let encoding = label
        .as_deref()
        .and_then(|l| Encoding::for_label(l.as_bytes()))
        .unwrap_or(UTF_8);
    encoding.decode(bytes).0.into_owned()
}

fn charset_from_content_type(ct: &str) -> Option<String> {
    ct.split(';').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        if key.trim().eq_ignore_ascii_case("charset") {
            let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
            (!value.is_empty()).then(|| value.to_string())
        } else {
            None
        }
    })
}

/// Look for `charset=` in the first 2 KiB of an HTML document, which covers
/// `<meta charset="...">` and `<meta http-equiv="Content-Type" ...>`.
fn charset_from_meta(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(2048)];
    let lower = String::from_utf8_lossy(head).to_ascii_lowercase();
    let start = lower.find("charset=")? + "charset=".len();
    let label: String = lower[start..]
        .trim_start_matches(|c| c == '"' || c == '\'' || c == ' ')
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    (!label.is_empty()).then_some(label)
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
    fn decodes_numeric_and_named_entities() {
        assert_eq!(html_to_text("don&#x27;t"), "don't");
        assert_eq!(html_to_text("it&#39;s &#8217; &#X2014;"), "it's \u{2019} \u{2014}");
        assert_eq!(html_to_text("a&amp;b &quot;q&quot; &apos;x&apos;"), "a&b \"q\" 'x'");
    }

    #[test]
    fn entities_decode_once() {
        assert_eq!(decode_entities("&amp;#39;"), "&#39;");
        assert_eq!(decode_entities("&amp;lt;"), "&lt;");
    }

    #[test]
    fn malformed_entities_are_left_alone() {
        assert_eq!(decode_entities("&#xZZ; &unknown; & alone &#;"), "&#xZZ; &unknown; & alone &#;");
        // Surrogate code points are not valid chars and must not be emitted.
        assert_eq!(decode_entities("&#xD800;"), "&#xD800;");
        // A multi-byte character right after '&' must not panic when slicing.
        assert_eq!(decode_entities("&é and &ü;"), "&é and &ü;");
    }

    #[test]
    fn decodes_body_using_header_charset() {
        let bytes = b"caf\xe9";
        assert_eq!(decode_body(bytes, Some("text/plain; charset=ISO-8859-1"), false), "café");
        assert_eq!(decode_body(bytes, Some("text/plain; charset=\"windows-1252\""), false), "café");
    }

    #[test]
    fn decodes_body_using_meta_charset_for_html() {
        let bytes = b"<html><head><meta charset=\"iso-8859-1\"></head><body>caf\xe9</body></html>";
        assert!(decode_body(bytes, Some("text/html"), true).contains("café"));
        // Without HTML, meta tags are not inspected.
        assert!(!decode_body(bytes, None, false).contains("café"));
    }

    #[test]
    fn defaults_to_utf8() {
        assert_eq!(decode_body("café".as_bytes(), None, false), "café");
        // Unknown charset labels fall back to UTF-8 instead of failing.
        assert_eq!(decode_body("café".as_bytes(), Some("text/plain; charset=bogus"), false), "café");
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
    async fn fetch_stops_at_byte_limit() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = [0u8; 1024];
            let _ = sock.read(&mut req).await;
            let body = "a".repeat(200);
            let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n{body}");
            let _ = sock.write_all(resp.as_bytes()).await;
        });

        let client = client().unwrap();
        let url = format!("http://{addr}/");
        let err = fetch_text_limited(&client, &url, 50).await.unwrap_err();
        assert!(err.contains("exceeds 50 bytes"), "{err}");
    }

    #[tokio::test]
    async fn fetch_rejects_non_http_schemes() {
        let client = client().unwrap();
        let err = fetch_text(&client, "file:///etc/passwd").await.unwrap_err();
        assert!(err.contains("http"), "{err}");
    }
}
