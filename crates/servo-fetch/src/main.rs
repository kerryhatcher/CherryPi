//! `servo-fetch`: render web pages in Servo and print their visible text.
//!
//! Page data goes to stdout and diagnostics to stderr. Exit status: 0 when
//! every page rendered fully, 2 when at least one was incomplete, 1 when at
//! least one failed or the arguments were bad.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::Parser;
use serde::Serialize;
use servo_fetch::{RenderedPage, RendererConfig, DEFAULT_USER_AGENT, MAX_TIMEOUT, RENDER_TIMEOUT};

#[derive(Parser)]
#[command(
    name = "servo-fetch",
    version,
    about = "Render web pages in Servo and print their visible text",
    after_help = "Exit status: 0 all pages complete, 2 some page incomplete, 1 some page failed."
)]
struct Args {
    /// http(s) URLs to render, one after another
    #[arg(required = true)]
    urls: Vec<String>,

    /// Seconds allowed for each page to load and settle
    #[arg(
        long,
        default_value_t = RENDER_TIMEOUT.as_secs(),
        value_parser = clap::value_parser!(u64).range(1..=MAX_TIMEOUT.as_secs())
    )]
    timeout: u64,

    /// User-Agent for the page and every request it makes
    #[arg(long, default_value = DEFAULT_USER_AGENT)]
    user_agent: String,

    /// Print one JSON object per URL (JSON Lines) instead of text
    #[arg(long)]
    json: bool,

    /// Keep at most this many characters of each page's text
    #[arg(long)]
    max_chars: Option<usize>,
}

/// One URL's result, as printed with `--json`.
#[derive(Serialize)]
struct Record {
    url: String,
    ok: bool,
    title: Option<String>,
    text: Option<String>,
    /// Why the page is incomplete, when it did not settle in time.
    incomplete: Option<String>,
    /// The text was cut to `--max-chars`.
    truncated: bool,
    error: Option<String>,
    elapsed_ms: u128,
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
enum Outcome {
    Complete,
    Incomplete,
    Failed,
}

/// Shuts the renderer down on every path out of `main`. Skipping this can
/// crash during process teardown.
struct Shutdown;

impl Drop for Shutdown {
    fn drop(&mut self) {
        servo_fetch::shutdown_renderer();
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    // Exit status 2 means an incomplete render, so a usage error, which clap
    // would report with 2, exits with 1. Help and version still exit 0.
    let args = match Args::try_parse() {
        Ok(args) => args,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            };
        }
    };
    init_logging();
    let _shutdown = Shutdown;

    if let Err(e) = servo_fetch::configure(RendererConfig {
        user_agent: args.user_agent.clone(),
    }) {
        eprintln!("servo-fetch: {e}");
        return ExitCode::from(1);
    }

    let timeout = Duration::from_secs(args.timeout);
    let mut worst = Outcome::Complete;
    for (index, raw) in args.urls.iter().enumerate() {
        let started = Instant::now();
        let result = match parse_url(raw) {
            Ok(url) => servo_fetch::render_page(url, timeout)
                .await
                .map_err(|e| e.to_string()),
            Err(e) => Err(e),
        };
        let record = to_record(raw, result, args.max_chars, started.elapsed());
        let outcome = if !record.ok {
            Outcome::Failed
        } else if record.incomplete.is_some() {
            Outcome::Incomplete
        } else {
            Outcome::Complete
        };
        worst = worst.max(outcome);

        if args.json {
            println!(
                "{}",
                serde_json::to_string(&record).expect("record serializes")
            );
        } else {
            print_text(&record, index, args.urls.len(), args.max_chars);
        }
    }

    match worst {
        Outcome::Complete => ExitCode::SUCCESS,
        Outcome::Incomplete => ExitCode::from(2),
        Outcome::Failed => ExitCode::from(1),
    }
}

/// Log to stderr. `RUST_LOG` overrides the default of warnings from this crate only.
fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("servo_fetch=warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn parse_url(raw: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(raw).map_err(|e| format!("invalid URL: {e}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        other => Err(format!(
            "unsupported URL scheme {other:?}: use http or https"
        )),
    }
}

fn to_record(
    url: &str,
    result: Result<RenderedPage, String>,
    max_chars: Option<usize>,
    elapsed: Duration,
) -> Record {
    let elapsed_ms = elapsed.as_millis();
    match result {
        Ok(page) => {
            let (text, truncated) = truncate(page.text, max_chars);
            Record {
                url: url.to_string(),
                ok: true,
                title: Some(page.title),
                text: Some(text),
                incomplete: page.incomplete,
                truncated,
                error: None,
                elapsed_ms,
            }
        }
        Err(error) => Record {
            url: url.to_string(),
            ok: false,
            title: None,
            text: None,
            incomplete: None,
            truncated: false,
            error: Some(error),
            elapsed_ms,
        },
    }
}

fn truncate(text: String, max_chars: Option<usize>) -> (String, bool) {
    match max_chars {
        Some(max) if text.chars().count() > max => (text.chars().take(max).collect(), true),
        _ => (text, false),
    }
}

/// Text output: a `==> url <==` header when there are several URLs, then
/// the title, any note, and the text. Errors go to stderr.
fn print_text(record: &Record, index: usize, count: usize, max_chars: Option<usize>) {
    if count > 1 {
        if index > 0 {
            println!();
        }
        println!("==> {} <==", record.url);
    }
    if let Some(error) = &record.error {
        eprintln!("servo-fetch: {}: {error}", record.url);
        return;
    }
    println!("Title: {}", record.title.as_deref().unwrap_or(""));
    if let Some(reason) = &record.incomplete {
        println!("Note: incomplete render: {reason}");
    }
    if record.truncated {
        println!(
            "Note: text truncated to the first {} characters (--max-chars)",
            max_chars.unwrap_or_default()
        );
    }
    println!();
    println!("{}", record.text.as_deref().unwrap_or(""));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_http_urls() {
        assert!(parse_url("https://example.com/").is_ok());
        assert!(parse_url("file:///etc/passwd")
            .unwrap_err()
            .contains("scheme"));
        assert!(parse_url("not a url").unwrap_err().contains("invalid URL"));
    }

    #[test]
    fn truncates_by_characters() {
        assert_eq!(truncate("héllo".into(), Some(2)), ("hé".into(), true));
        assert_eq!(truncate("hi".into(), Some(2)), ("hi".into(), false));
        assert_eq!(truncate("hi".into(), None), ("hi".into(), false));
    }
}
