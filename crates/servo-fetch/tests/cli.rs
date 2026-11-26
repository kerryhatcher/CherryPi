//! Runs the `servo-fetch` binary against a local page.
#![cfg(feature = "cli")]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::Command;
use std::thread;

/// The text is written by a script, so only a real render sees it.
const PAGE: &str = "<html><head><title>CLI Test</title></head><body>\
    <div id=\"app\">loading</div>\
    <script>setTimeout(function(){document.getElementById('app').textContent='cli-rendered-marker ua:'+navigator.userAgent;},200);</script>\
    </body></html>";

fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
            }
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{PAGE}",
                PAGE.len()
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    format!("http://{addr}/")
}

fn servo_fetch() -> Command {
    Command::new(env!("CARGO_BIN_EXE_servo-fetch"))
}

#[test]
fn renders_to_json_lines_and_reports_failures() {
    let url = serve();
    let output = servo_fetch()
        .args([
            "--json",
            "--user-agent",
            "cli-test/1.0",
            "--max-chars",
            "200",
            &url,
            "ftp://example.com/",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let records: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("each line is JSON"))
        .collect();
    assert_eq!(records.len(), 2, "stdout: {stdout}");

    let page = &records[0];
    assert_eq!(page["ok"], true, "{page}");
    assert_eq!(page["title"], "CLI Test");
    assert!(page["incomplete"].is_null(), "{page}");
    let text = page["text"].as_str().unwrap();
    assert!(text.contains("cli-rendered-marker"), "{text}");
    assert!(text.contains("ua:cli-test/1.0"), "{text}");

    assert_eq!(records[1]["ok"], false);
    assert!(records[1]["error"].as_str().unwrap().contains("scheme"));
    assert_eq!(output.status.code(), Some(1), "a failed URL exits 1");
}

#[test]
fn rejects_an_invalid_user_agent() {
    let output = servo_fetch()
        .args(["--user-agent", "", "http://127.0.0.1:9/"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid user agent"));
}

#[test]
fn usage_errors_exit_1_and_help_exits_0() {
    for args in [
        &["--timeout", "nope", "http://127.0.0.1:9/"][..],
        &["--timeout", "0", "http://127.0.0.1:9/"],
        &[],
    ] {
        let output = servo_fetch().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(1), "args {args:?}");
    }
    let help = servo_fetch().arg("--help").output().unwrap();
    assert_eq!(help.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&help.stdout).contains("Exit status"));
}
