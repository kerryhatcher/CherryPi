//! Renders JavaScript-heavy pages in Servo and returns the visible text.
//!
//! ```no_run
//! # async fn demo() -> Result<(), servo_fetch::RenderError> {
//! servo_fetch::configure(servo_fetch::RendererConfig::default())?; // optional
//! let url = url::Url::parse("https://example.com/").unwrap();
//! let result = servo_fetch::render_page(url, servo_fetch::RENDER_TIMEOUT).await;
//! servo_fetch::shutdown_renderer(); // before the process exits, on every path
//! let page = result?;
//! println!("{}\n{}", page.title, page.text);
//! # Ok(()) }
//! ```
//!
//! Servo can run only once per process, and its `WebView` is `!Send` (it
//! holds `Rc`), so one OS thread owns the `Servo` instance and renders pages
//! one at a time. It starts on the first [`render_page`] call, which sends a
//! job over a channel and waits with a deadline. Settings are therefore
//! process-wide: [`configure`] them before the first render. Every render
//! starts with no cookies, storage or cache from earlier renders.
//!
//! Two things keep a misbehaving page from hanging a render:
//!
//! - The page's own same-origin GET requests from scripts (XHR and fetch) are
//!   made by this module with [`REQUEST_TIMEOUT`], using the page's cookies,
//!   and handed back to Servo. Servo has no response timeout, so a synchronous
//!   XHR to a server that never answers would otherwise block the page's
//!   script thread for good.
//! - If the page's scripts stop answering anyway, the render returns the last
//!   text it captured and says that the page is incomplete.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{mpsc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use content_security_policy::Destination;
use dpi::PhysicalSize;
use reqwest::header::{
    HeaderMap, HeaderValue, COOKIE, REFERER, SET_COOKIE, USER_AGENT as UA_HEADER,
};
use servo::{
    CookieSource, EventLoopWaker, JSValue, LoadStatus, Preferences, RenderingContext, Servo,
    ServoBuilder, SoftwareRenderingContext, StorageType, WebResourceLoad, WebResourceResponse,
    WebView, WebViewBuilder, WebViewDelegate,
};

/// User-Agent sent when [`configure`] is not called. It claims Windows on
/// purpose: some sites hang requests from desktop Linux user agents.
pub const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:160.0) Gecko/20100101 Firefox/160.0";

/// Process-wide renderer settings. See [`configure`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RendererConfig {
    /// User-Agent for the page and every request it makes.
    pub user_agent: String,
}

impl Default for RendererConfig {
    fn default() -> Self {
        Self {
            user_agent: DEFAULT_USER_AGENT.to_string(),
        }
    }
}

/// Why a render, or the renderer's configuration, failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    /// The User-Agent cannot be sent as an HTTP header value.
    #[error("invalid user agent: it must be non-empty and valid in an HTTP header")]
    InvalidUserAgent,
    /// The renderer already has different settings. They are fixed once set,
    /// or once the first render starts.
    #[error("the renderer is already configured with different settings")]
    AlreadyConfigured,
    /// The timeout is zero or longer than [`MAX_TIMEOUT`].
    #[error("invalid timeout: it must be between 1 second and {} seconds", MAX_TIMEOUT.as_secs())]
    InvalidTimeout,
    /// The page could not be rendered: startup failed, the page produced no
    /// text in time, or the render thread stopped answering.
    #[error("{0}")]
    Failed(String),
}

static CONFIG: OnceLock<RendererConfig> = OnceLock::new();

/// Set the renderer's settings for this process. Call it before the first
/// render; without it, [`RendererConfig::default`] is used. Calling it again
/// with the same settings is fine. Different settings, or any settings after a
/// render has already used the defaults, give [`RenderError::AlreadyConfigured`].
pub fn configure(config: RendererConfig) -> Result<(), RenderError> {
    if config.user_agent.trim().is_empty() || HeaderValue::from_str(&config.user_agent).is_err() {
        return Err(RenderError::InvalidUserAgent);
    }
    let current = CONFIG.get_or_init(|| config.clone());
    if *current == config {
        Ok(())
    } else {
        Err(RenderError::AlreadyConfigured)
    }
}

/// The settings in effect, fixing the defaults if none were configured.
fn config() -> &'static RendererConfig {
    CONFIG.get_or_init(RendererConfig::default)
}

fn user_agent() -> &'static str {
    &config().user_agent
}

/// Time allowed for one page to load and settle.
pub const RENDER_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest timeout [`render_page`] accepts.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(600);
/// Time allowed for one request made by the page's scripts.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

const VIEWPORT_WIDTH: u32 = 1280;
const VIEWPORT_HEIGHT: u32 = 1024;
/// How often the page's text is sampled while waiting for it to settle.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(200);
/// Consecutive unchanged samples needed before the page counts as settled.
const STABLE_SAMPLES: u32 = 3;
/// Minimum time after load completes before the page can count as settled.
const MIN_SETTLE: Duration = Duration::from_secs(1);
/// The page counts as settled only after its network has been quiet this long.
/// Pages often keep content hidden until their requests come back, and a
/// framework can take a moment between loading its scripts and requesting data.
const NETWORK_QUIET: Duration = Duration::from_millis(1500);
/// A text sample unanswered for this long means the page's scripts are stuck.
const STALL_AFTER: Duration = Duration::from_secs(3);
/// Grace period beyond the job's timeout for the render thread to reply.
const REPLY_SLACK: Duration = Duration::from_secs(10);
/// Largest response body accepted for a request made on the page's behalf.
const MAX_PROXY_BODY: usize = 5 * 1024 * 1024;
/// Response headers that describe the connection rather than the content.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "transfer-encoding",
    "content-length",
];

/// Script run in the page to read its title and visible text. `innerText`
/// follows layout, so hidden elements such as collapsed menus and dropdowns
/// are left out, along with script and style source. If `innerText` is not
/// available, it falls back to the text of a copy of the body with script,
/// style, and template content removed.
const SNAPSHOT_SCRIPT: &str = "(function () {\
    var body = document.body;\
    if (!body) { return {title: document.title, text: ''}; }\
    var text = body.innerText;\
    if (typeof text !== 'string') {\
        var copy = body.cloneNode(true);\
        copy.querySelectorAll('script, style, noscript, template').forEach(function (node) { node.remove(); });\
        text = copy.textContent;\
    }\
    return {title: document.title, text: text};\
})()";

/// A rendered page: its title and the visible text, one line per block.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderedPage {
    pub title: String,
    pub text: String,
    /// Set when the page did not settle before the timeout, saying why. The
    /// text is then the last snapshot taken and may be missing content.
    pub incomplete: Option<String>,
}

struct Job {
    url: url::Url,
    timeout: Duration,
    /// Runtime for the HTTP requests made on the page's behalf.
    runtime: tokio::runtime::Handle,
    reply: mpsc::Sender<Result<RenderedPage, String>>,
}

/// The running render thread. Dropping its job sender ends the thread, and
/// joining it lets Servo shut down before the process exits.
struct RenderThread {
    jobs: mpsc::Sender<Job>,
    handle: JoinHandle<()>,
}

/// Where the renderer is in its once-per-process life.
enum Lifecycle {
    NotStarted,
    Running(RenderThread),
    /// [`shutdown_renderer`] was called. Servo cannot start again.
    ShutDown,
}

static RENDERER: Mutex<Lifecycle> = Mutex::new(Lifecycle::NotStarted);

/// Render `url` in Servo and return its title and text. The page is loaded
/// fresh every call. Errors cover startup failures, a page that never produced
/// any text, and a render thread that does not answer. A page that loads but
/// does not settle in time is returned with [`RenderedPage::incomplete`] set.
///
/// Must be called from within a Tokio runtime, which also carries the HTTP
/// requests the page's scripts make.
pub async fn render_page(url: url::Url, timeout: Duration) -> Result<RenderedPage, RenderError> {
    if timeout.is_zero() || timeout > MAX_TIMEOUT {
        return Err(RenderError::InvalidTimeout);
    }
    let runtime = tokio::runtime::Handle::current();
    let result = tokio::task::spawn_blocking(move || -> Result<RenderedPage, String> {
        let jobs = renderer()?;
        let (reply, result) = mpsc::channel();
        jobs.send(Job {
            url,
            timeout,
            runtime,
            reply,
        })
        .map_err(|_| "Servo render thread is not running".to_string())?;
        result
            .recv_timeout(timeout + REPLY_SLACK)
            .map_err(|e| format!("render did not finish: {e}"))?
    })
    .await
    .map_err(|e| format!("render task failed: {e}"))
    .and_then(|r| r);
    result.map_err(RenderError::Failed)
}

/// Start the render thread on first use and return a sender for its jobs.
fn renderer() -> Result<mpsc::Sender<Job>, String> {
    let mut guard = RENDERER
        .lock()
        .map_err(|_| "render state poisoned".to_string())?;
    match &*guard {
        Lifecycle::Running(thread) => return Ok(thread.jobs.clone()),
        Lifecycle::ShutDown => return Err("the renderer has been shut down".to_string()),
        Lifecycle::NotStarted => {}
    }
    let (jobs, rx) = mpsc::channel::<Job>();
    let handle = thread::Builder::new()
        .name("servo-render".into())
        .spawn(move || render_thread(rx))
        .map_err(|e| format!("failed to start Servo render thread: {e}"))?;
    let sender = jobs.clone();
    *guard = Lifecycle::Running(RenderThread { jobs, handle });
    Ok(sender)
}

/// Stop the render thread and shut Servo down. Call this before the process
/// exits: skipping it can crash during process teardown. Safe to call when
/// rendering was never started, and calling it twice is harmless. Servo runs
/// once per process, so later renders fail instead of starting it again.
pub fn shutdown_renderer() {
    // Mark the renderer shut down before waiting, so no render can start a
    // second Servo while this one stops.
    let previous = match RENDERER.lock() {
        Ok(mut guard) => std::mem::replace(&mut *guard, Lifecycle::ShutDown),
        Err(_) => return,
    };
    if let Lifecycle::Running(RenderThread { jobs, handle }) = previous {
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

    let prefs = Preferences {
        user_agent: user_agent().to_string(),
        ..Default::default()
    };
    let servo = ServoBuilder::default()
        .preferences(prefs)
        .event_loop_waker(Box::new(NoopWaker))
        .build();

    let context =
        match SoftwareRenderingContext::new(PhysicalSize::new(VIEWPORT_WIDTH, VIEWPORT_HEIGHT)) {
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
        let result = render_one(&servo, &context, &job);
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
    /// Which document the snapshot belongs to. Replies from an earlier
    /// document's evaluation are ignored.
    generation: u32,
    title: String,
    text: String,
    /// A script evaluation is in flight.
    pending: bool,
    /// Successful samples received so far.
    samples: u32,
}

/// Receives the page's resource requests. Requests this module makes itself
/// are held here until the render loop picks them up; the rest are dropped,
/// which lets Servo load them as usual.
#[derive(Default)]
struct PageDelegate {
    held: RefCell<Vec<WebResourceLoad>>,
    /// When the page last started any request.
    last_request: Cell<Option<Instant>>,
}

impl WebViewDelegate for PageDelegate {
    fn load_web_resource(&self, _webview: WebView, load: WebResourceLoad) {
        self.last_request.set(Some(Instant::now()));
        if should_proxy(
            load.request.destination,
            load.request.method.as_str(),
            load.request.url.as_url(),
            load.request.referrer_url.as_ref().map(|u| u.as_url()),
        ) {
            self.held.borrow_mut().push(load);
        }
    }
}

/// Whether a request is made here instead of by Servo: a GET from a script
/// (XHR or fetch, which have no destination) to the page's own origin. The
/// request body is not available to the embedder, so other methods are left
/// to Servo, and cross-origin requests are left to Servo's CORS handling.
fn should_proxy(
    destination: Destination,
    method: &str,
    url: &url::Url,
    referrer: Option<&url::Url>,
) -> bool {
    destination == Destination::None
        && method == "GET"
        && matches!(url.scheme(), "http" | "https")
        && referrer.is_some_and(|r| r.origin() == url.origin())
}

/// A response fetched on the page's behalf.
struct ProxiedResponse {
    url: url::Url,
    status: reqwest::StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

type ProxyResult = Result<ProxiedResponse, String>;

/// Requests made on the page's behalf that have not been answered yet.
struct Proxy {
    client: reqwest::Client,
    runtime: tokio::runtime::Handle,
    next_id: u64,
    in_flight: HashMap<u64, WebResourceLoad>,
    /// When a request last started or finished.
    last_activity: Option<Instant>,
    done_tx: mpsc::Sender<(u64, ProxyResult)>,
    done_rx: mpsc::Receiver<(u64, ProxyResult)>,
}

impl Proxy {
    fn new(runtime: tokio::runtime::Handle) -> Self {
        let (done_tx, done_rx) = mpsc::channel();
        let client = reqwest::Client::builder()
            .user_agent(user_agent())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self {
            client,
            runtime,
            next_id: 0,
            in_flight: HashMap::new(),
            last_activity: None,
            done_tx,
            done_rx,
        }
    }

    /// No request is in flight and none has started or finished for `quiet`.
    fn is_idle(&self, now: Instant, quiet: Duration) -> bool {
        self.in_flight.is_empty()
            && self
                .last_activity
                .is_none_or(|t| now.duration_since(t) >= quiet)
    }

    /// Start a request for `load` with the cookies Servo holds for its URL.
    fn start(&mut self, servo: &Servo, load: WebResourceLoad) {
        let request = &load.request;
        let url = request.url.clone().into_url();
        let mut headers = request.headers.clone();
        let cookies = servo
            .site_data_manager()
            .cookies_for_url(url.clone(), CookieSource::HTTP);
        if !cookies.is_empty() {
            let pairs: Vec<String> = cookies
                .iter()
                .map(|c| format!("{}={}", c.name(), c.value()))
                .collect();
            if let Ok(value) = HeaderValue::from_str(&pairs.join("; ")) {
                headers.insert(COOKIE, value);
            }
        }
        if let Some(referrer) = request.referrer_url.as_ref().map(|u| u.as_url()) {
            if let Ok(value) = HeaderValue::from_str(referrer.as_str()) {
                headers.insert(REFERER, value);
            }
        }
        if let Ok(value) = HeaderValue::from_str(user_agent()) {
            headers.insert(UA_HEADER, value);
        }

        let id = self.next_id;
        self.next_id += 1;
        self.in_flight.insert(id, load);
        self.last_activity = Some(Instant::now());
        let client = self.client.clone();
        let done = self.done_tx.clone();
        self.runtime.spawn(async move {
            let result = tokio::time::timeout(REQUEST_TIMEOUT, fetch(client, url, headers))
                .await
                .unwrap_or_else(|_| {
                    Err(format!(
                        "no response within {} seconds",
                        REQUEST_TIMEOUT.as_secs()
                    ))
                });
            let _ = done.send((id, result));
        });
    }

    /// Hand finished requests back to Servo. A failed request becomes a
    /// network error in the page.
    fn deliver_finished(&mut self, servo: &Servo) {
        while let Ok((id, result)) = self.done_rx.try_recv() {
            let Some(load) = self.in_flight.remove(&id) else {
                continue;
            };
            self.last_activity = Some(Instant::now());
            match result {
                Ok(response) => {
                    store_cookies(servo, &response);
                    let mut headers = response.headers;
                    for name in HOP_BY_HOP {
                        headers.remove(*name);
                    }
                    let reason = response
                        .status
                        .canonical_reason()
                        .unwrap_or("")
                        .as_bytes()
                        .to_vec();
                    let mut intercepted = load.intercept(
                        WebResourceResponse::new(response.url)
                            .headers(headers)
                            .status_code(response.status)
                            .status_message(reason),
                    );
                    if !response.body.is_empty() {
                        intercepted.send_body_data(response.body);
                    }
                    intercepted.finish();
                }
                Err(e) => {
                    tracing::debug!(url = %load.request.url, error = %e, "page request failed");
                    let url = load.request.url.clone().into_url();
                    load.intercept(WebResourceResponse::new(url)).cancel();
                }
            }
        }
    }
}

async fn fetch(client: reqwest::Client, url: url::Url, headers: HeaderMap) -> ProxyResult {
    let mut response = client
        .get(url)
        .headers(headers)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let url = response.url().clone();
    let status = response.status();
    let headers = response.headers().clone();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if body.len() + chunk.len() > MAX_PROXY_BODY {
            return Err(format!("response body exceeds {MAX_PROXY_BODY} bytes"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(ProxiedResponse {
        url,
        status,
        headers,
        body,
    })
}

/// Servo stores cookies only from responses it fetched itself, so the cookies
/// set by a proxied response are stored here.
fn store_cookies(servo: &Servo, response: &ProxiedResponse) {
    for value in response.headers.get_all(SET_COOKIE) {
        let Ok(text) = value.to_str() else { continue };
        if let Ok(cookie) = cookie::Cookie::parse(text.to_string()) {
            servo.site_data_manager().set_cookie_for_url(
                response.url.clone(),
                cookie.into_owned(),
                None,
            );
        }
    }
}

/// Forget everything earlier renders left behind: cookies, web storage and
/// cached responses. Each render then behaves like a first visit, so a site
/// whose bot protection flagged one visit does not block every later one.
fn reset_site_state(servo: &Servo) {
    let sites = servo.site_data_manager().site_data(StorageType::all());
    let names: Vec<String> = sites.iter().map(|site| site.name()).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    servo
        .site_data_manager()
        .clear_site_data(&names, StorageType::Local | StorageType::Session);
    servo.site_data_manager().clear_cookies(None);
    servo.network_manager().clear_cache();
}

fn render_one(
    servo: &Servo,
    context: &Rc<SoftwareRenderingContext>,
    job: &Job,
) -> Result<RenderedPage, String> {
    reset_site_state(servo);
    let rendering: Rc<dyn RenderingContext> = context.clone();
    let delegate = Rc::new(PageDelegate::default());
    let webview: WebView = WebViewBuilder::new(servo, rendering)
        .url(job.url.clone())
        .delegate(delegate.clone())
        .build();
    let mut proxy = Proxy::new(job.runtime.clone());

    let snapshot = Rc::new(RefCell::new(Snapshot::default()));
    let deadline = Instant::now() + job.timeout;
    let mut loaded_at: Option<Instant> = None;
    let mut last_sample: Option<Instant> = None;
    let mut seen_samples = 0;
    let mut stable = 0;
    let mut last_text: Option<String> = None;
    let mut settled;

    loop {
        // Servo's callbacks run inside spin_event_loop, so no RefCell borrow is
        // held across this call.
        servo.spin_event_loop();
        let now = Instant::now();

        let held: Vec<WebResourceLoad> = delegate.held.borrow_mut().drain(..).collect();
        for load in held {
            proxy.start(servo, load);
        }
        proxy.deliver_finished(servo);

        let status = webview.load_status();
        if status == LoadStatus::Complete {
            loaded_at.get_or_insert(now);
        } else if loaded_at.is_some() {
            // A new document started loading after the last one finished, for
            // example when a bot check reloads the page. Wait for it instead.
            // Its text, and any evaluation still running in it, belong to
            // the old document, so start over.
            loaded_at = None;
            stable = 0;
            last_text = None;
            last_sample = None;
            seen_samples = 0;
            let generation = snapshot.borrow().generation.wrapping_add(1);
            *snapshot.borrow_mut() = Snapshot {
                generation,
                ..Snapshot::default()
            };
        }

        // Sample as soon as the document exists, so a page that never finishes
        // loading still yields its text. Only a loaded page can settle.
        if status != LoadStatus::Started {
            let due = last_sample.is_none_or(|t| now.duration_since(t) >= SAMPLE_INTERVAL);
            if due && !snapshot.borrow().pending {
                last_sample = Some(now);
                snapshot.borrow_mut().pending = true;
                let generation = snapshot.borrow().generation;
                let shared = snapshot.clone();
                webview.evaluate_javascript(SNAPSHOT_SCRIPT, move |result| {
                    let mut snap = shared.borrow_mut();
                    if snap.generation != generation {
                        return;
                    }
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
        }

        let network_quiet = proxy.is_idle(now, NETWORK_QUIET)
            && delegate
                .last_request
                .get()
                .is_none_or(|t| now.duration_since(t) >= NETWORK_QUIET);
        // An empty page is not settled: a bot check may be about to reload it.
        let has_text = last_text.as_deref().is_some_and(|t| !t.trim().is_empty());
        settled = stable >= STABLE_SAMPLES
            && has_text
            && network_quiet
            && loaded_at.is_some_and(|loaded| now.duration_since(loaded) >= MIN_SETTLE);
        if settled || now >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    let snap = snapshot.borrow();
    let stalled = snap.pending
        && last_sample.is_some_and(|sent| Instant::now().duration_since(sent) >= STALL_AFTER);
    let secs = job.timeout.as_secs();
    if snap.samples == 0 {
        return Err(if stalled {
            format!("page scripts stopped responding before any text could be read (waited {secs} seconds)")
        } else {
            format!("page did not finish loading within {secs} seconds")
        });
    }
    let text = normalize_text(&snap.text);
    let incomplete = if settled {
        None
    } else if text.is_empty() {
        Some(format!(
            "the page showed no readable text after {secs} seconds. The site may be blocking \
             automated browsers, or the page may need a login. Fetching it again is unlikely to help."
        ))
    } else if stalled {
        Some(
            "the page's scripts stopped responding, probably waiting on a request that never \
             finished. This text was captured before that and may be missing content."
                .to_string(),
        )
    } else if loaded_at.is_none() {
        Some(format!(
            "the page was still loading after {secs} seconds. Some content may be missing."
        ))
    } else if !proxy.in_flight.is_empty() {
        Some(format!(
            "the page was still waiting on {} of its own requests after {secs} seconds. Some content may be missing.",
            proxy.in_flight.len()
        ))
    } else {
        Some(format!(
            "the page was still changing after {secs} seconds. Some content may be missing."
        ))
    };
    Ok(RenderedPage {
        title: snap.title.trim().to_string(),
        text,
        incomplete,
    })
}

/// Collapse whitespace within each line and drop empty lines.
fn normalize_text(text: &str) -> String {
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
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
    use std::io::{BufRead, BufReader, Write};
    use std::net::{TcpListener, TcpStream};

    /// The visible text is written by a script after load, so the raw HTML
    /// only contains "loading". A hidden element must not show up.
    const SPA: &str = "<html><head><title>SPA Test</title></head><body>\
        <div id=\"app\">loading</div>\
        <div style=\"display:none\">hidden-dropdown-marker</div>\
        <script>setTimeout(function(){document.getElementById('app').textContent='rendered-by-javascript-marker ua:'+navigator.userAgent;},300);</script>\
        </body></html>";

    /// Makes a synchronous XHR to an endpoint that never answers, then one
    /// that echoes the request's cookies and sets a new one.
    const SYNC_XHR: &str = "<html><head><title>Sync XHR</title></head><body>\
        <div id=\"out\">before</div>\
        <script>\
        document.cookie = 'page=1';\
        var out = [];\
        try { var x = new XMLHttpRequest(); x.open('GET', '/hang', false); x.send(); out.push('hang-answered'); }\
        catch (e) { out.push('hang-failed'); }\
        var y = new XMLHttpRequest(); y.open('GET', '/cookies', false); y.send();\
        out.push('server-saw:' + y.responseText);\
        out.push('page-sees:' + document.cookie);\
        document.getElementById('out').textContent = out.join(' ');\
        </script></body></html>";

    /// Reports the cookies and storage left by an earlier visit, then leaves
    /// some of its own for the next visit to find.
    const STATE: &str = "<html><head><title>State</title></head><body>\
        <div id=\"out\">before</div>\
        <script>\
        document.getElementById('out').textContent = 'cookie:[' + document.cookie + '] stored:[' \
            + localStorage.getItem('left') + '] session:[' + sessionStorage.getItem('left') + ']';\
        document.cookie = 'left=1; Max-Age=3600';\
        localStorage.setItem('left', '1');\
        sessionStorage.setItem('left', '1');\
        </script></body></html>";

    /// Serve `page` at `/`. `/hang` never answers, and `/cookies` echoes the
    /// request's Cookie header and sets `server=2`.
    fn serve(page: &'static str) -> url::Url {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                thread::spawn(move || handle(stream, page));
            }
        });
        url::Url::parse(&format!("http://{addr}/")).unwrap()
    }

    fn handle(mut stream: TcpStream, page: &str) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        let _ = reader.read_line(&mut request_line);
        let mut cookies = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("cookie") {
                    cookies = value.trim().to_string();
                }
            }
        }
        let path = request_line.split_whitespace().nth(1).unwrap_or("/");
        let (content_type, extra, body) = match path {
            "/hang" => {
                thread::sleep(Duration::from_secs(600));
                return;
            }
            "/cookies" => ("text/plain", "Set-Cookie: server=2; Path=/\r\n", cookies),
            _ => ("text/html; charset=utf-8", "", page.to_string()),
        };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n{extra}\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    }

    // Servo can be started only once per process, so all render scenarios run
    // in one test.
    #[tokio::test(flavor = "multi_thread")]
    async fn renders_pages() {
        let config = RendererConfig {
            user_agent: "servo-fetch-test/1.0".into(),
        };
        assert_eq!(
            configure(RendererConfig {
                user_agent: "bad\nagent".into()
            }),
            Err(RenderError::InvalidUserAgent)
        );
        assert_eq!(configure(config.clone()), Ok(()));
        assert_eq!(configure(config), Ok(()), "same settings again are fine");
        assert_eq!(
            configure(RendererConfig::default()),
            Err(RenderError::AlreadyConfigured)
        );

        let page = render_page(serve(SPA), Duration::from_secs(60))
            .await
            .expect("render should succeed");
        assert_eq!(page.title, "SPA Test");
        assert_eq!(page.incomplete, None);
        assert!(
            page.text.contains("rendered-by-javascript-marker"),
            "text was: {:?}",
            page.text
        );
        assert!(
            page.text.contains("ua:servo-fetch-test/1.0"),
            "configured user agent not used: {:?}",
            page.text
        );
        assert!(
            !page.text.contains("setTimeout"),
            "script source leaked: {:?}",
            page.text
        );
        assert!(
            !page.text.contains("hidden-dropdown-marker"),
            "hidden text leaked: {:?}",
            page.text
        );

        let started = Instant::now();
        let page = render_page(serve(SYNC_XHR), Duration::from_secs(60))
            .await
            .expect("render should succeed");
        assert!(
            started.elapsed() < REQUEST_TIMEOUT + Duration::from_secs(15),
            "render took {:?}",
            started.elapsed()
        );
        assert_eq!(page.incomplete, None, "text was: {:?}", page.text);
        assert!(
            page.text.contains("hang-failed"),
            "text was: {:?}",
            page.text
        );
        assert!(
            page.text.contains("server-saw:page=1"),
            "text was: {:?}",
            page.text
        );
        assert!(page.text.contains("server=2"), "text was: {:?}", page.text);

        // Each render starts clean: a site that flags one visit (as bot
        // protection does) must not poison the next.
        let state = serve(STATE);
        for visit in 1..=2 {
            let page = render_page(state.clone(), Duration::from_secs(60))
                .await
                .expect("render should succeed");
            assert!(
                page.text.contains("cookie:[] stored:[null] session:[null]"),
                "visit {visit} saw earlier state: {:?}",
                page.text
            );
        }

        let url = url::Url::parse("http://127.0.0.1:9/").unwrap();
        for bad in [
            Duration::ZERO,
            MAX_TIMEOUT + Duration::from_secs(1),
            Duration::MAX,
        ] {
            assert_eq!(
                render_page(url.clone(), bad).await,
                Err(RenderError::InvalidTimeout)
            );
        }

        shutdown_renderer();
        shutdown_renderer();
        let after = render_page(url, Duration::from_secs(5)).await;
        assert!(
            matches!(&after, Err(RenderError::Failed(e)) if e.contains("shut down")),
            "{after:?}"
        );
    }

    #[test]
    fn proxies_only_same_origin_script_gets() {
        let page = url::Url::parse("https://example.com/page").unwrap();
        let same = url::Url::parse("https://example.com/api").unwrap();
        let other = url::Url::parse("https://other.example/api").unwrap();
        assert!(should_proxy(Destination::None, "GET", &same, Some(&page)));
        assert!(!should_proxy(Destination::None, "POST", &same, Some(&page)));
        assert!(!should_proxy(
            Destination::Script,
            "GET",
            &same,
            Some(&page)
        ));
        assert!(!should_proxy(Destination::None, "GET", &other, Some(&page)));
        assert!(!should_proxy(Destination::None, "GET", &same, None));
    }

    #[test]
    fn normalizes_text_lines() {
        assert_eq!(normalize_text("  a   b \n\n \t\n c\n"), "a b\nc");
    }
}
