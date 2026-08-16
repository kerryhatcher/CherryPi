use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A size-rotating JSONL log writer.
///
/// Writes to `{dir}/{prefix}.{suffix}`. When the file exceeds `max_size`
/// bytes, it renames the current file to `{prefix}.{timestamp}.{suffix}.gz`
/// and starts a fresh one. Keeps at most `max_archives` compressed archives.
#[allow(dead_code)]
pub struct RotatingJsonlWriter {
    dir: PathBuf,
    prefix: String,
    suffix: String,
    max_size: u64,
    max_archives: usize,
    current_file: Mutex<Option<File>>,
    current_path: Mutex<Option<PathBuf>>,
    current_size: Mutex<u64>,
}

impl RotatingJsonlWriter {
    #[allow(dead_code)]
    pub fn new(
        dir: PathBuf,
        prefix: &str,
        suffix: &str,
        max_size: u64,
        max_archives: usize,
    ) -> io::Result<Self> {
        fs::create_dir_all(&dir)?;

        let writer = Self {
            dir,
            prefix: prefix.to_string(),
            suffix: suffix.to_string(),
            max_size,
            max_archives,
            current_file: Mutex::new(None),
            current_path: Mutex::new(None),
            current_size: Mutex::new(0),
        };

        writer.open_current()?;
        Ok(writer)
    }

    fn active_path(&self) -> PathBuf {
        self.dir.join(format!("{}.{}", self.prefix, self.suffix))
    }

    fn open_current(&self) -> io::Result<()> {
        let path = self.active_path();

        // Get current size if file exists
        let size = if path.exists() {
            fs::metadata(&path)?.len()
        } else {
            0
        };

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;

        *self.current_file.lock().unwrap() = Some(file);
        *self.current_path.lock().unwrap() = Some(path);
        *self.current_size.lock().unwrap() = size;

        Ok(())
    }

    #[allow(dead_code)]
    fn rotate(&self) -> io::Result<()> {
        let active = self.active_path();
        if !active.exists() {
            return self.open_current();
        }

        // Generate archive name with timestamp
        let ts = chrono::Utc::now().format("%Y%m%dT%H%M%S");
        let archive_name = format!("{}.{}.{}.gz", self.prefix, ts, self.suffix);
        let archive_path = self.dir.join(&archive_name);

        // Close current file
        *self.current_file.lock().unwrap() = None;
        *self.current_path.lock().unwrap() = None;
        *self.current_size.lock().unwrap() = 0;

        // Compress and rename
        compress_file(&active, &archive_path)?;

        // Remove the uncompressed active file
        let _ = fs::remove_file(&active);

        // Prune old archives
        self.prune_archives()?;

        // Open new active file
        self.open_current()
    }

    #[allow(dead_code)]
    fn prune_archives(&self) -> io::Result<()> {
        let mut archives: Vec<PathBuf> = Vec::new();
        let prefix_dot = format!("{}.", self.prefix);

        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if name_str.starts_with(&prefix_dot)
                && name_str.ends_with(&format!(".{}.gz", self.suffix))
                && name_str != format!("{}.{}", self.prefix, self.suffix)
            {
                archives.push(entry.path());
            }
        }

        // Sort by name (which includes timestamp), oldest first
        archives.sort();

        // Remove oldest if we have too many
        while archives.len() > self.max_archives {
            if let Some(oldest) = archives.first() {
                let _ = fs::remove_file(oldest);
                archives.remove(0);
            }
        }

        Ok(())
    }

    #[allow(dead_code)]
    fn write_all(&self, buf: &[u8]) -> io::Result<()> {
        let mut current_file = self.current_file.lock().unwrap();
        let mut current_size = self.current_size.lock().unwrap();

        if current_file.is_none() {
            drop(current_file);
            drop(current_size);
            self.open_current()?;
            return self.write_all(buf);
        }

        let file = current_file.as_mut().unwrap();
        file.write_all(buf)?;
        file.flush()?;
        *current_size += buf.len() as u64;

        if *current_size >= self.max_size {
            drop(current_file);
            drop(current_size);
            self.rotate()?;
        }

        Ok(())
    }
}

impl Write for &RotatingJsonlWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        RotatingJsonlWriter::write_all(self, buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(ref mut file) = *self.current_file.lock().unwrap() {
            file.flush()
        } else {
            Ok(())
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RotatingJsonlWriter {
    type Writer = &'a RotatingJsonlWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self
    }
}

/// Compress a file with gzip, writing the result to `dst`.
#[allow(dead_code)]
fn compress_file(src: &Path, dst: &Path) -> io::Result<()> {
    use std::io::{BufReader, Read, BufWriter};

    let src_file = File::open(src)?;
    let mut reader = BufReader::new(src_file);
    let dst_file = File::create(dst)?;
    let writer = BufWriter::new(dst_file);
    let mut encoder = flate2::write::GzEncoder::new(writer, flate2::Compression::default());

    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        encoder.write_all(&buf[..n])?;
    }
    encoder.finish()?;
    Ok(())
}
