# Servo headless fetch for web_fetch — design

Status: approved in conversation ("Spidermonkey is fine. No feature flag needed, just add it in as part of a branch").

## Intent

`web_fetch` only sees static HTML today, so pages that build their content with JavaScript (SPAs) come back empty. Add a rendering path that runs the page in Servo, a Rust browser engine, and returns the text the page shows after its scripts run. The engine must not be Chrome, Chromium, or anything Google ships (no V8).

## Goals

- `web_fetch` gets an optional `render` argument. When true, the page is rendered in Servo and its title and visible text are returned.
- Output uses the same 20,000-character cap as the static path.
- Rendering has a hard time limit, so a hung page cannot stall the agent.

## Non-goals

- Interactive browsing (clicks, forms, navigation). Only load, wait, and read.
- Screenshots. The Servo API supports them, but the tool returns text.
- Replacing the reqwest path. Static pages stay on reqwest, which is faster and has no build cost.

## Anti-goals

- Rendering sites in a shared engine instance that persists state between calls. Each call gets a fresh page.

## Constraints

- Servo crate `=0.8.0` (pinned; the API changes between monthly releases).
- Requires Rust 1.88 or newer. The toolchain here is 1.100.
- Builds SpiderMonkey (C++) from source. That is approved.
- `WebView` is `!Send` (it holds `Rc`), so every Servo object lives on one dedicated thread.

## Approaches

1. **Servo crate on a dedicated thread (chosen).** Full layout and JavaScript. Heaviest build.
2. Pure-Rust JavaScript engine (Boa) with an HTML parser. Light, but has no layout, and most SPAs would fail. Rejected for SPA fidelity.
3. Headless Chromium over CDP. Rejected: it is Chrome, which is out of scope.

## Design

Owner's decisions:
- No Cargo feature flag. Servo is an ordinary dependency of `cherrypi-agent`.

My calls (marked as such so they can be reviewed):
- **Module:** `crates/agent/src/render.rs`. It owns one OS thread that creates a single `Servo` instance on first use, then renders one page at a time from a job queue.
- **Calling pattern:** the async tool hands a job to that thread through `spawn_blocking` and waits with a deadline. The thread never blocks the Tokio runtime.
- **Rendering:** a fresh `WebView` per request, sized 1280x1024, using `SoftwareRenderingContext` (no window or GPU needed). The `WebView` is dropped when the job finishes.
- **Waiting for the page:** spin the Servo event loop until `LoadStatus::Complete`, then poll `document.body.textContent` every 200 ms. The page counts as settled when the text is unchanged for three samples in a row, and at least one second has passed since load completed. Stop at the job's deadline either way and return whatever text is there.
- **Timeout:** 20 seconds per render, configurable in code.
- **User agent:** the same browser User-Agent string used by the reqwest path (`web::USER_AGENT`), set through `Preferences`.
- **TLS:** install the rustls `aws_lc_rs` crypto provider once on the render thread, as Servo's example does.
- **Output:** `Title: <title>` on the first line, a blank line, then the text. Whitespace is collapsed and the result is capped at 20,000 characters.
- **Security:** the same http and https check as the static path. Like the static path, rendering does not block localhost or private addresses. That is still an open item from the earlier review.

## Left to the builder

- Exact Servo event-loop pacing (the 10 ms spin interval) and how to treat Servo's `Stopped` load status.
- Whether `paint()` must be called for JavaScript-only pages to settle. Decide by testing.
