use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;

/// Logs every chat interaction to a JSONL transcript file.
///
/// One JSON record per line: `{"timestamp":"...","cwd":"...","role":"user|assistant|system|error","content":"..."}`
pub struct ChatLogger {
    writer: Mutex<BufWriter<File>>,
    cwd: String,
}

impl ChatLogger {
    /// Create a new chat logger writing to `{log_dir}/chat.jsonl`.
    pub fn new(log_dir: PathBuf, cwd: &str) -> std::io::Result<Self> {
        std::fs::create_dir_all(&log_dir)?;
        let path = log_dir.join("chat.jsonl");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let writer = Mutex::new(BufWriter::new(file));
        Ok(Self {
            writer,
            cwd: cwd.to_string(),
        })
    }

    /// Log a chat message.
    pub fn log(&self, role: &str, content: &str) {
        let record = serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "cwd": self.cwd,
            "role": role,
            "content": content,
        });

        if let Ok(mut w) = self.writer.lock() {
            let _ = serde_json::to_writer(&mut *w, &record);
            let _ = w.write_all(b"\n");
            let _ = w.flush();
        }
    }
}
