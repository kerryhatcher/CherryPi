//! Renders JavaScript-heavy pages in Servo and returns the visible text.
//!
//! Servo's `WebView` is `!Send` (it holds `Rc`), so one OS thread owns the
//! `Servo` instance and renders pages one at a time. Callers reach it through
//! [`render_page`], which sends a job over a channel and waits with a deadline.
//! See `docs/superpowers/specs/2026-11-25-servo-headless-fetch-design.md`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{mpsc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dpi::PhysicalSize;
use servo::{
    EventLoopWaker, JSValue, LoadStatus, Preferences, RenderingContext, Servo, ServoBuilder,
    SoftwareRenderingContext, WebView, WebViewBuilder,
};

use crate::web::USER_AGENT;

/// Time allowed for one page to load and settle.
pub const RENDER_TIMEOUT: Duration = Duration::from_secs(20);

const VIEWPORT_WIDTH: u32 = 1280;
const VIEWPORT_HEIGHT: u32 = 1024;
/// How often the page's text is sampled while waiting for it to settle.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(200);
/// Consecutive unchanged samples needed before the page counts as settled.
const STABLE_SAMPLES: u32 = 3;
/// Minimum time after load completes before the page can count as settled.
const MIN_SETTLE: Duration = Duration::from_secs(1);
/// Grace period beyond the job's timeout for the render thread to reply.
const REPLY_SLACK: Duration = Duration::from_secs(10);

/// Script run in the page to read its title and visible text. It works on a
/// copy of the body with script, style, and template content removed, so
/// source code does not end up in the text.
const SNAPSHOT_SCRIPT: &str = "(function () {\
    var body = document.body;\
    if (!body) { return {title: document.title, text: ''}; }\
    var copy = body.cloneNode(true);\
    copy.querySelectorAll('script, style, noscript, template').forEach(function (node) { node.remove(); });\
    return {title: document.title, text: copy.textContent};\
})()";

/// A rendered page: its title and the visible text, whitespace collapsed.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedPage {
    pub title: String,
    pub text: String,
}

struct Job {
    url: url::Url,
    timeout: Duration,
    reply: mpsc::Sender<Result<RenderedPage, String>>,
}

/// The running render thread. Dropping its job sender ends the thread, and
/// joining it lets Servo shut down before the process exits.
struct RenderThread {
    jobs: mpsc::Sender<Job>,
    handle: JoinHandle<()>,
}

static RENDERER: Mutex<Option<RenderThread>> = Mutex::new(None);

/// Render `url` in Servo and return its title and text. The page is loaded
/// fresh every call. Errors cover startup failures, a page that never
/// finishes loading, and a render that runs past `timeout`.
pub async fn render_page(url: url::Url, timeout: Duration) -> Result<RenderedPage, String> {
    tokio::task::spawn_blocking(move || -> Result<RenderedPage, String> {
        let jobs = renderer()?;
        let (reply, result) = mpsc::channel();
        jobs.send(Job { url, timeout, reply })
            .map_err(|_| "Servo render thread is not running".to_string())?;
        result
            .recv_timeout(timeout + REPLY_SLACK)
            .map_err(|e| format!("render did not finish: {e}"))?
    })
    .await
    .map_err(|e| format!("render task failed: {e}"))?
}

/// Start the render thread on first use and return a sender for its jobs.
fn renderer() -> Result<mpsc::Sender<Job>, String> {
    let mut guard = RENDERER.lock().map_err(|_| "render state poisoned".to_string())?;
    if let Some(thread) = guard.as_ref() {
        return Ok(thread.jobs.clone());
    }
    let (jobs, rx) = mpsc::channel::<Job>();
    let handle = thread::Builder::new()
        .name("servo-render".into())
        .spawn(move || render_thread(rx))
        .map_err(|e| format!("failed to start Servo render thread: {e}"))?;
    let sender = jobs.clone();
    *guard = Some(RenderThread { jobs, handle });
    Ok(sender)
}

/// Stop the render thread and shut Servo down. Call this before the process
/// exits: skipping it can crash during process teardown. Safe to call when
/// rendering was never started, and calling it twice is harmless.
pub fn shutdown_renderer() {
    let taken = RENDERER.lock().ok().and_then(|mut guard| guard.take());
    if let Some(RenderThread { jobs, handle }) = taken {
        // Closing the channel makes the render thread leave its loop and drop Servo.
        drop(jobs);
        let _ = handle.join();
    }
}

/// Owns the Servo instance and rendering context, and renders jobs in order.
fn render_thread(jobs: mpsc::Receiver<Job>) {
    // Servo's network stack needs a process-wide TLS crypto provider. Another
    // component may already have installed one, so a failure here is ignored.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let mut prefs = Preferences::default();
    prefs.user_agent = USER_AGENT.to_string();
    let servo = ServoBuilder::default()
        .preferences(prefs)
        .event_loop_waker(Box::new(NoopWaker))
        .build();

    let context = match SoftwareRenderingContext::new(PhysicalSize::new(
        VIEWPORT_WIDTH,
        VIEWPORT_HEIGHT,
    )) {
        Ok(context) => Rc::new(context),
        Err(e) => {
            let message = format!("failed to create Servo rendering context: {e:?}");
            for job in jobs {
                let _ = job.reply.send(Err(message.clone()));
            }
            return;
        }
    };
    let _ = context.make_current();

    for job in jobs {
        let result = render_one(&servo, &context, job.url, job.timeout);
        let _ = job.reply.send(result);
    }
}

/// Servo calls `wake` from its own threads. This thread spins the event loop
/// on a fixed cadence, so it does not need to be woken.
struct NoopWaker;

impl EventLoopWaker for NoopWaker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(NoopWaker)
    }

    fn wake(&self) {}
}

/// What the page script last reported, and how often it has reported.
#[derive(Default)]
struct Snapshot {
    title: String,
    text: String,
    /// A script evaluation is in flight.
    pending: bool,
    /// Successful samples received so far.
    samples: u32,
}

fn render_one(
    servo: &Servo,
    context: &Rc<SoftwareRenderingContext>,
    url: url::Url,
    timeout: Duration,
) -> Result<RenderedPage, String> {
    let rendering: Rc<dyn RenderingContext> = context.clone();
    let webview: WebView = WebViewBuilder::new(servo, rendering).url(url).build();

    let snapshot = Rc::new(RefCell::new(Snapshot::default()));
    let deadline = Instant::now() + timeout;
    let mut loaded_at: Option<Instant> = None;
    let mut last_sample: Option<Instant> = None;
    let mut seen_samples = 0;
    let mut stable = 0;
    let mut last_text: Option<String> = None;

    loop {
        // Servo's callbacks run inside spin_event_loop, so no RefCell borrow is
        // held across this call.
        servo.spin_event_loop();
        let now = Instant::now();

        if loaded_at.is_none() && webview.load_status() == LoadStatus::Complete {
            loaded_at = Some(now);
        }

        if let Some(loaded) = loaded_at {
            let due = last_sample.is_none_or(|t| now.duration_since(t) >= SAMPLE_INTERVAL);
            if due && !snapshot.borrow().pending {
                last_sample = Some(now);
                snapshot.borrow_mut().pending = true;
                let shared = snapshot.clone();
                webview.evaluate_javascript(SNAPSHOT_SCRIPT, move |result| {
                    let mut snap = shared.borrow_mut();
                    snap.pending = false;
                    if let Ok(JSValue::Object(fields)) = result {
                        snap.title = string_field(&fields, "title");
                        snap.text = string_field(&fields, "text");
                        snap.samples += 1;
                    }
                });
            }

            // Count a sample only once it has arrived, and only compare texts
            // from distinct samples.
            let snap = snapshot.borrow();
            if snap.samples > seen_samples {
                seen_samples = snap.samples;
                if last_text.as_deref() == Some(snap.text.as_str()) {
                    stable += 1;
                } else {
                    stable = 0;
                    last_text = Some(snap.text.clone());
                }
            }
            let settled =
                stable >= STABLE_SAMPLES && now.duration_since(loaded) >= MIN_SETTLE;
            if settled {
                break;
            }
        }

        if now >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    let snap = snapshot.borrow();
    if loaded_at.is_none() {
        return Err(format!(
            "page did not finish loading within {} seconds",
            timeout.as_secs()
        ));
    }
    Ok(RenderedPage {
        title: snap.title.trim().to_string(),
        text: snap.text.split_whitespace().collect::<Vec<_>>().join(" "),
    })
}

fn string_field(fields: &HashMap<String, JSValue>, key: &str) -> String {
    match fields.get(key) {
        Some(JSValue::String(value)) => value.clone(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// The visible text is written by a script after load, so the raw HTML
    /// only contains "loading".
    const SPA: &str = "<html><head><title>SPA Test</title></head><body>\
        <div id=\"app\">loading</div>\
        <script>setTimeout(function(){document.getElementById('app').textContent='rendered-by-javascript-marker';},300);</script>\
        </body></html>";

    /// Serve `body` as text/html to every connection on a local port.
    fn serve(body: &'static str) -> url::Url {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        url::Url::parse(&format!("http://{addr}/")).unwrap()
    }

    #[tokio::test]
    async fn renders_javascript_generated_text() {
        let url = serve(SPA);
        let page = render_page(url, Duration::from_secs(60))
            .await
            .expect("render should succeed");
        assert_eq!(page.title, "SPA Test");
        assert!(
            page.text.contains("rendered-by-javascript-marker"),
            "text was: {:?}",
            page.text
        );
        assert!(
            !page.text.contains("setTimeout"),
            "script source leaked into text: {:?}",
            page.text
        );
        shutdown_renderer();
    }
}
