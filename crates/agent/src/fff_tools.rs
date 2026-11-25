//! FFF-backed agent tools.
//!
//! These replace the subprocess `grep` (ripgrep) and `find` tools with a
//! long-lived, resident file-search index ([`SharedFilePicker`]). Repeated
//! searches hit warm memory instead of re-spawning `rg`/`find` and
//! re-statting the tree each call.
//!
//! Capability highlights over the subprocess fallbacks:
//! - `find` matches the whole repo-relative path (not just filename), is
//!   typo-tolerant in `fuzzy` mode, and ranks by frecency.
//! - `grep` supports plain / regex / fuzzy modes, auto-falls back to fuzzy
//!   on zero exact hits, classifies definition lines, annotates git status,
//!   and paginates via a cursor.
//! - `multi_grep` searches several patterns in one pass.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use cherrypi_core::tool::{Tool, ToolHandler, ToolResult};
use fff_search::{
    FuzzySearchOptions, GrepMode, GrepSearchOptions, PaginationArgs, QueryParser,
    SharedFilePicker,
};

// ---------------------------------------------------------------------------
// Query-building helpers (ported from pi-fff's `buildQuery` / TS query.ts)
// ---------------------------------------------------------------------------

/// Fold a `path` include constraint and `exclude` list into a single query
/// string the FFF [`QueryParser`] understands.
///
/// Bare directories become `PathSegment` constraints (trailing `/`), glob
/// tokens (`*`, `?`, `[`, `{`) are passed through, and filename-like tokens
/// (`main.rs`) become `FilePath` constraints. Excludes are emitted as
/// `!<constraint>` tokens.
fn fff_build_query(path: Option<&str>, pattern: &str, exclude: Option<&[String]>) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(p) = path {
        if let Some(c) = normalize_path_constraint(p) {
            parts.push(c);
        }
    }

    if let Some(excludes) = exclude {
        for raw in excludes {
            for token in raw.split([',', ' ']).map(str::trim).filter(|s| !s.is_empty()) {
                let stripped = token.strip_prefix('!').unwrap_or(token);
                if let Some(c) = normalize_path_constraint(stripped) {
                    parts.push(format!("!{}", c));
                }
            }
        }
    }

    parts.push(pattern.to_string());
    parts.join(" ")
}

/// Normalise a path/include token into a constraint the FFF parser recognises.
/// Returns `None` when the constraint is a no-op (`.` or `**`).
fn normalize_path_constraint(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Collapse `dir/**` and `dir/**/*` recursive globs to a directory prefix.
    if let Some(dir) = strip_recursive_glob_suffix(trimmed) {
        if !dir.is_empty() && !has_glob_chars(&dir) {
            return Some(format!("{}/", dir));
        }
    }

    if matches!(trimmed, "." | "./" | "**" | "**/" | "**/*") {
        return None;
    }

    // Strip a leading `./` so `./**/*.rs` and `**/*.rs` behave identically.
    let trimmed = trimmed.strip_prefix("./").unwrap_or(trimmed);

    // Already signals path-constraint syntax.
    if trimmed.starts_with('/') || trimmed.ends_with('/') {
        return Some(trimmed.to_string());
    }
    // Globs are passed through for the parser.
    if has_glob_chars(trimmed) {
        return Some(trimmed.to_string());
    }
    // Filename with extension → FilePath constraint.
    let last_segment = trimmed.rsplit('/').next().unwrap_or("");
    if looks_like_filename(last_segment) {
        return Some(trimmed.to_string());
    }
    // Bare directory prefix → PathSegment.
    Some(format!("{}/", trimmed))
}

fn has_glob_chars(s: &str) -> bool {
    s.contains(['*', '?', '[', '{'])
}

fn looks_like_filename(seg: &str) -> bool {
    if let Some(dot_idx) = seg.rfind('.') {
        let ext = &seg[dot_idx + 1..];
        !ext.is_empty()
            && ext.len() <= 10
            && ext.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
    } else {
        false
    }
}

/// Strip a trailing `/**`, `/**/*`, or `/*` recursive glob suffix and return
/// the directory prefix. Replaces a regex `^(.*)/\*\*(?:/\*)?$` without
/// pulling in the regex crate at runtime.
fn strip_recursive_glob_suffix(s: &str) -> Option<String> {
    for suffix in ["/**/*", "/**", "/*"] {
        if let Some(prefix) = s.strip_suffix(suffix) {
            if !prefix.is_empty() {
                return Some(prefix.to_string());
            }
        }
    }
    None
}

/// Format a [`git2::Status`] for agent-facing output. Returns `""` for clean
/// or `None` so the path line is unaffected.
fn format_git_status_short(status: Option<git2::Status>) -> &'static str {
    use git2::Status;
    match status {
        None | Some(Status::CURRENT) => "",
        Some(s) if s.contains(Status::INDEX_NEW) || s.contains(Status::WT_NEW) => " (new)",
        Some(s) if s.contains(Status::INDEX_MODIFIED) || s.contains(Status::WT_MODIFIED) => " (mod)",
        Some(s) if s.contains(Status::INDEX_DELETED) || s.contains(Status::WT_DELETED) => " (del)",
        Some(s)
            if s.contains(Status::INDEX_RENAMED) || s.contains(Status::WT_RENAMED) =>
        {
            " (ren)"
        }
        _ => "",
    }
}

// ---------------------------------------------------------------------------
// find
// ---------------------------------------------------------------------------

/// FFF-backed fuzzy/glob path search. Replaces the subprocess `find` tool.
pub struct FffFindTool {
    picker: SharedFilePicker,
    #[allow(dead_code)]
    working_dir: PathBuf,
}

impl FffFindTool {
    pub fn new(picker: SharedFilePicker, working_dir: PathBuf) -> Self {
        Self { picker, working_dir }
    }
}

#[async_trait]
impl ToolHandler for FffFindTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "find".into(),
            description: "Find files by name or path using a resident frecency-ranked index. \
                Matches the whole repo-relative path, not just the filename. \
                Typo-tolerant in `fuzzy` mode (default); use `glob` mode for \
                exact glob patterns (e.g. `src/**/*.rs`). Returns paths ranked \
                by frecency with git-status annotations.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "File name or path fragment to search for (e.g. 'button', 'main.rs', 'src/api')"
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["fuzzy", "glob"],
                        "description": "`fuzzy` (default) for typo-tolerant path search; `glob` for exact glob patterns"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or path prefix to constrain the search (relative to cwd)"
                    },
                    "exclude": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Path/glob patterns to exclude (e.g. [\"test/\", \"*.min.js\"])"
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum results (default 50)"
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> ToolResult {
        let pattern = arguments["pattern"].as_str().unwrap_or("");
        let mode = arguments["mode"].as_str().unwrap_or("fuzzy");
        let path = arguments.get("path").and_then(|v| v.as_str());
        let exclude: Option<Vec<String>> = arguments
            .get("exclude")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect());
        let limit = arguments["limit"].as_u64().unwrap_or(50) as usize;

        let guard = match self.picker.read() {
            Ok(g) => g,
            Err(e) => {
                return ToolResult {
                    tool_call_id: String::new(),
                    content: format!("Failed to acquire FFF picker lock: {}", e),
                    is_error: true,
                }
            }
        };
        let picker = match guard.as_ref() {
            Some(p) => p,
            None => {
                return ToolResult {
                    tool_call_id: String::new(),
                    content: "FFF picker not initialised.".into(),
                    is_error: true,
                }
            }
        };

        let opts = FuzzySearchOptions {
            pagination: PaginationArgs { offset: 0, limit },
            project_path: Some(picker.base_path()),
            ..Default::default()
        };

        // Glob mode bypasses the query parser; when path/exclude constraints
        // are present we still route through the parser so scope is honoured.
        let result = if mode == "glob" && path.is_none() && exclude.is_none() {
            picker.glob(pattern, opts)
        } else {
            let q = fff_build_query(path, pattern, exclude.as_deref());
            let parsed = QueryParser::default().parse(&q);
            picker.fuzzy_search(&parsed, None, opts)
        };

        if result.items.is_empty() {
            return ToolResult {
                tool_call_id: String::new(),
                content: format!(
                    "No files found for '{}' ({} of {} indexed).",
                    pattern, result.total_matched, result.total_files
                ),
                is_error: false,
            };
        }

        let mut out = String::new();
        for (i, item) in result.items.iter().enumerate() {
            let rel = item.relative_path(picker);
            let git = format_git_status_short(item.git_status);
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&format!("{}{}", rel, git));
        }
        out.push_str(&format!(
            "\n\n{} match(es) of {} indexed files",
            result.total_matched, result.total_files
        ));

        ToolResult { tool_call_id: String::new(), content: out, is_error: false }
    }
}

// ---------------------------------------------------------------------------
// grep
// ---------------------------------------------------------------------------

/// FFF-backed content grep. Replaces the subprocess `grep` (ripgrep) tool.
pub struct FffGrepTool {
    picker: SharedFilePicker,
    #[allow(dead_code)]
    working_dir: PathBuf,
    cancel_flag: Arc<AtomicBool>,
}

impl FffGrepTool {
    pub fn new(
        picker: SharedFilePicker,
        working_dir: PathBuf,
        cancel_flag: Arc<AtomicBool>,
    ) -> Self {
        Self { picker, working_dir, cancel_flag }
    }

    fn parse_mode(s: &str) -> GrepMode {
        match s {
            "regex" => GrepMode::Regex,
            "fuzzy" => GrepMode::Fuzzy,
            _ => GrepMode::PlainText,
        }
    }
}

#[async_trait]
impl ToolHandler for FffGrepTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "grep".into(),
            description: "Search file contents using a resident index with plain, regex, and \
                fuzzy modes. Auto-falls back to fuzzy matching when an exact search \
                returns zero hits. Returns matches with line numbers, optional \
                context, and git-status annotations. Supports cursor pagination \
                for large result sets.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Search pattern (literal text or regex depending on `mode`)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file path to constrain the search (relative to cwd)"
                    },
                    "exclude": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Path/glob patterns to exclude (e.g. [\"test/\", \"*.min.js\"])"
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["plain", "regex", "fuzzy"],
                        "description": "How to interpret `pattern` (default `plain`)"
                    },
                    "case_sensitive": {
                        "type": "boolean",
                        "description": "Case-sensitive match (default false = smart-case)"
                    },
                    "context": {
                        "type": "number",
                        "description": "Lines of context to show before and after each match (default 0)"
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum matches to return (default 100)"
                    },
                    "cursor": {
                        "type": "number",
                        "description": "Pagination cursor from a previous result's `next_cursor`. Omit for the first page."
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> ToolResult {
        let pattern = arguments["pattern"].as_str().unwrap_or("");
        let path = arguments.get("path").and_then(|v| v.as_str());
        let exclude: Option<Vec<String>> = arguments
            .get("exclude")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect());
        let mode = arguments["mode"].as_str().unwrap_or("plain");
        let case_sensitive = arguments["case_sensitive"].as_bool().unwrap_or(false);
        let context = arguments["context"].as_u64().unwrap_or(0) as usize;
        let limit = arguments["limit"].as_u64().unwrap_or(100) as usize;
        let file_offset = arguments["cursor"].as_u64().unwrap_or(0) as usize;

        let guard = match self.picker.read() {
            Ok(g) => g,
            Err(e) => {
                return ToolResult {
                    tool_call_id: String::new(),
                    content: format!("Failed to acquire FFF picker lock: {}", e),
                    is_error: true,
                }
            }
        };
        let picker = match guard.as_ref() {
            Some(p) => p,
            None => {
                return ToolResult {
                    tool_call_id: String::new(),
                    content: "FFF picker not initialised.".into(),
                    is_error: true,
                }
            }
        };

        let q = fff_build_query(path, pattern, exclude.as_deref());
        let parsed = QueryParser::default().parse(&q);

        // Reset the shared cancel flag for this search.
        self.cancel_flag.store(false, Ordering::SeqCst);

        let opts = GrepSearchOptions {
            mode: Self::parse_mode(mode),
            smart_case: !case_sensitive,
            page_limit: limit,
            file_offset,
            before_context: context,
            after_context: context,
            classify_definitions: true,
            trim_whitespace: true,
            abort_signal: Some(self.cancel_flag.clone()),
            ..Default::default()
        };

        let result = picker.grep(&parsed, &opts);

        if result.matches.is_empty() {
            let extra = result
                .regex_fallback_error
                .as_ref()
                .map(|error| format!(" (regex fallback: {error})"))
                .unwrap_or_default();
            return ToolResult {
                tool_call_id: String::new(),
                content: format!(
                    "No matches for '{}' (searched {} of {} files).{}",
                    pattern, result.total_files_searched, result.total_files, extra
                ),
                is_error: false,
            };
        }

        let mut out = String::new();
        for (i, m) in result.matches.iter().enumerate() {
            let file = &result.files[m.file_index];
            let rel = file.relative_path(picker);
            let git = format_git_status_short(file.git_status);
            let def = if m.is_definition { " [def]" } else { "" };

            if i > 0 {
                out.push('\n');
            }

            // context before
            for (j, line) in m.context_before.iter().enumerate() {
                if j > 0 || i > 0 {
                    out.push('\n');
                }
                out.push_str(&format!("{}-{}: {}", rel, m.line_number.saturating_sub((m.context_before.len() - j) as u64), line));
            }

            if !m.context_before.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("{}:{}:{}{}{}: {}", rel, m.line_number, m.col, git, def, m.line_content));

            for line in &m.context_after {
                out.push('\n');
                out.push_str(&format!("{}+{}: {}", rel, m.line_number + 1, line));
            }
        }

        out.push_str(&format!(
            "\n\n{} match(es) in {} file(s) (searched {} of {} files)",
            result.matches.len(),
            result.files_with_matches,
            result.total_files_searched,
            result.total_files
        ));
        if result.next_file_offset > 0 {
            out.push_str(&format!("\nMore results: pass `cursor: {}`", result.next_file_offset));
        }

        ToolResult { tool_call_id: String::new(), content: out, is_error: false }
    }
}

// ---------------------------------------------------------------------------
// multi_grep
// ---------------------------------------------------------------------------

/// Search for multiple patterns in a single pass over the indexed files.
pub struct FffMultiGrepTool {
    picker: SharedFilePicker,
    #[allow(dead_code)]
    working_dir: PathBuf,
}

impl FffMultiGrepTool {
    pub fn new(picker: SharedFilePicker, working_dir: PathBuf) -> Self {
        Self { picker, working_dir }
    }
}

#[async_trait]
impl ToolHandler for FffMultiGrepTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "multi_grep".into(),
            description: "Search file contents for multiple patterns in one pass. Faster than \
                repeated `grep` calls when you need several patterns at once \
                (e.g. find every TODO, FIXME, and XXX). Returns matches grouped \
                by file with line numbers.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "patterns": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Patterns to search for (literal text)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file path to constrain the search (relative to cwd)"
                    },
                    "exclude": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Path/glob patterns to exclude"
                    },
                    "case_sensitive": {
                        "type": "boolean",
                        "description": "Case-sensitive match (default false = smart-case)"
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum total matches to return (default 100)"
                    }
                },
                "required": ["patterns"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> ToolResult {
        let patterns: Vec<String> = arguments
            .get("patterns")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if patterns.is_empty() {
            return ToolResult {
                tool_call_id: String::new(),
                content: "No patterns provided.".into(),
                is_error: true,
            };
        }
        let path = arguments.get("path").and_then(|v| v.as_str());
        let exclude: Option<Vec<String>> = arguments
            .get("exclude")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect());
        let case_sensitive = arguments["case_sensitive"].as_bool().unwrap_or(false);
        let limit = arguments["limit"].as_u64().unwrap_or(100) as usize;

        let guard = match self.picker.read() {
            Ok(g) => g,
            Err(e) => {
                return ToolResult {
                    tool_call_id: String::new(),
                    content: format!("Failed to acquire FFF picker lock: {}", e),
                    is_error: true,
                }
            }
        };
        let picker = match guard.as_ref() {
            Some(p) => p,
            None => {
                return ToolResult {
                    tool_call_id: String::new(),
                    content: "FFF picker not initialised.".into(),
                    is_error: true,
                }
            }
        };

        // Build a constraint-only query (no fuzzy text) to scope the search.
        let q = fff_build_query(path, "", exclude.as_deref());
        let parsed = QueryParser::default().parse(&q);
        let constraints: Vec<fff_search::Constraint> = parsed.constraints.clone();

        let pattern_refs: Vec<&str> = patterns.iter().map(String::as_str).collect();
        let opts = GrepSearchOptions {
            mode: GrepMode::PlainText,
            smart_case: !case_sensitive,
            page_limit: limit,
            classify_definitions: true,
            trim_whitespace: true,
            ..Default::default()
        };

        let result = picker.multi_grep(&pattern_refs, &constraints, &opts);

        if result.matches.is_empty() {
            return ToolResult {
                tool_call_id: String::new(),
                content: format!(
                    "No matches for {:?} (searched {} of {} files).",
                    patterns, result.total_files_searched, result.total_files
                ),
                is_error: false,
            };
        }

        let mut out = String::new();
        for (i, m) in result.matches.iter().enumerate() {
            let file = &result.files[m.file_index];
            let rel = file.relative_path(picker);
            let git = format_git_status_short(file.git_status);
            let def = if m.is_definition { " [def]" } else { "" };
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&format!("{}:{}:{}{}{}: {}", rel, m.line_number, m.col, git, def, m.line_content));
        }
        out.push_str(&format!(
            "\n\n{} match(es) in {} file(s) (searched {} of {} files)",
            result.matches.len(),
            result.files_with_matches,
            result.total_files_searched,
            result.total_files
        ));

        ToolResult { tool_call_id: String::new(), content: out, is_error: false }
    }
}
