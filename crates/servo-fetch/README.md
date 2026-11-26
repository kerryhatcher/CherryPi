# servo-fetch

Renders JavaScript-heavy web pages in [Servo](https://servo.org) and returns
their title and visible text. It runs as a library or as a command-line tool.
No Chrome, Chromium or V8 is involved.

## CLI

```sh
cargo build --release -p servo-fetch
servo-fetch https://example.com/
servo-fetch --json --max-chars 20000 https://a.example/ https://b.example/
```

| Option | Default | Meaning |
|---|---|---|
| `--timeout <secs>` | 20 | Time allowed for each page to load and settle |
| `--user-agent <str>` | Windows Firefox 160 | User-Agent for the page and its requests |
| `--json` | off | One JSON object per URL (JSON Lines) instead of text |
| `--max-chars <n>` | none | Keep at most this many characters of each page's text |

URLs are rendered one after another. Each one starts with no cookies,
storage or cache left by the ones before it.

Page data goes to stdout and diagnostics to stderr (set `RUST_LOG` for more).
Each `--json` record looks like this:

```json
{"url":"…","ok":true,"title":"…","text":"…","incomplete":null,"truncated":false,"error":null,"elapsed_ms":9784}
```

`incomplete` says why when a page did not settle in time; its text is then
the last snapshot taken. `error` is set when `ok` is false.

**Exit status:** 0 when every page rendered fully, 2 when at least one was
incomplete, 1 when at least one failed or the arguments were bad.

## Library

```rust
servo_fetch::configure(servo_fetch::RendererConfig {
    user_agent: "MyTool/1.0".into(),
})?; // optional; call before the first render
let page = servo_fetch::render_page(url, servo_fetch::RENDER_TIMEOUT).await?;
servo_fetch::shutdown_renderer(); // before the process exits
```

Servo runs once per process, on its own thread, so the settings apply to the
whole process and are fixed after the first render. `render_page` must be
called from within a Tokio runtime. Build with `default-features = false` to
leave out the CLI's dependencies.

## Why the default User-Agent claims Windows

Some sites never answer requests from desktop Linux user agents. Simbli
(eboardsolutions.com) is one: a page there waits forever on a synchronous
request. To keep such a page from hanging the render, the page's same-origin
script requests are made with an 8-second timeout.
