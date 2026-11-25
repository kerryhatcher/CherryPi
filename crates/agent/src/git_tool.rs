//! Read-only git access for the agent: `status`, `diff`, `log` and `branch`.
//!
//! Runs in-process through libgit2 rather than through `bash`, so it needs no
//! shell and cannot change the repository. Only the workspace's own repository
//! is opened; parent directories are never searched for a `.git`.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use cherrypi_core::tool::{Tool, ToolHandler, ToolResult};
use git2::{DiffFormat, DiffOptions, ErrorCode, Repository, Sort, Status, StatusOptions};
use serde_json::{json, Value};

const DEFAULT_LOG_LIMIT: usize = 10;
const MAX_LOG_LIMIT: usize = 50;
const MAX_OUTPUT_BYTES: usize = 65_536;

pub struct GitTool {
    working_dir: PathBuf,
}

impl GitTool {
    pub fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }

    fn run(&self, arguments: &Value) -> Result<String, String> {
        let action = arguments["action"]
            .as_str()
            .ok_or("missing 'action' (use status, diff, log or branch)")?;

        // `open` does not search upward, so only this directory's repository is used.
        let repo = Repository::open(&self.working_dir)
            .map_err(|e| format!("no git repository at the workspace root: {}", e.message()))?;

        match action {
            "status" => status(&repo),
            "branch" => branch(&repo),
            "log" => {
                let limit = arguments["limit"]
                    .as_u64()
                    .map(|n| n as usize)
                    .unwrap_or(DEFAULT_LOG_LIMIT)
                    .clamp(1, MAX_LOG_LIMIT);
                log(&repo, limit)
            }
            "diff" => diff(&repo, arguments),
            other => Err(format!("unknown action '{other}' (use status, diff, log or branch)")),
        }
    }
}

#[async_trait]
impl ToolHandler for GitTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "git".into(),
            description: "Read-only git for the workspace repository. \
                actions: status (changed files), diff (unstaged changes, or staged \
                with staged=true; optional path filter), log (recent commits, limit), \
                branch (current branch). Cannot stage, commit or change anything."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["status", "diff", "log", "branch"],
                        "description": "What to show"
                    },
                    "path": {
                        "type": "string",
                        "description": "diff only: limit to this relative path"
                    },
                    "staged": {
                        "type": "boolean",
                        "description": "diff only: show staged changes instead of unstaged"
                    },
                    "limit": {
                        "type": "number",
                        "description": "log only: number of commits (default 10, max 50)"
                    }
                },
                "required": ["action"]
            }),
        }
    }

    async fn execute(&self, arguments: Value) -> ToolResult {
        match self.run(&arguments) {
            Ok(content) => ToolResult {
                tool_call_id: String::new(),
                content: truncate(content),
                is_error: false,
            },
            Err(message) => ToolResult {
                tool_call_id: String::new(),
                content: message,
                is_error: true,
            },
        }
    }
}

fn err(e: git2::Error) -> String {
    e.message().to_string()
}

fn truncate(mut s: String) -> String {
    if s.len() > MAX_OUTPUT_BYTES {
        let mut end = MAX_OUTPUT_BYTES;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push_str("\n[truncated]");
    }
    s
}

/// Reject paths that leave the repository, as the file tools do.
fn check_relative(path: &str) -> Result<(), String> {
    if path.is_empty() || Path::new(path).is_absolute() || path.split('/').any(|p| p == "..") {
        return Err("path must be relative to the workspace and must not contain '..'".into());
    }
    Ok(())
}

/// Two-character status code in the style of `git status --short`.
fn short_status(s: Status) -> String {
    if s == Status::WT_NEW {
        return "??".into();
    }
    let index = [
        (Status::INDEX_NEW, 'A'),
        (Status::INDEX_MODIFIED, 'M'),
        (Status::INDEX_DELETED, 'D'),
        (Status::INDEX_RENAMED, 'R'),
        (Status::INDEX_TYPECHANGE, 'T'),
    ]
    .iter()
    .find(|(flag, _)| s.contains(*flag))
    .map_or(' ', |(_, c)| *c);
    let worktree = [
        (Status::WT_MODIFIED, 'M'),
        (Status::WT_DELETED, 'D'),
        (Status::WT_RENAMED, 'R'),
        (Status::WT_TYPECHANGE, 'T'),
    ]
    .iter()
    .find(|(flag, _)| s.contains(*flag))
    .map_or(' ', |(_, c)| *c);
    format!("{index}{worktree}")
}

fn status(repo: &Repository) -> Result<String, String> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    let statuses = repo.statuses(Some(&mut opts)).map_err(err)?;

    let mut out = String::new();
    for entry in statuses.iter() {
        let path = entry.path().unwrap_or("?");
        out.push_str(&format!("{} {}\n", short_status(entry.status()), path));
    }
    if out.is_empty() {
        out.push_str("working tree clean\n");
    }
    Ok(out)
}

fn branch(repo: &Repository) -> Result<String, String> {
    match repo.head() {
        Ok(head) if head.is_branch() => {
            Ok(format!("on branch {}\n", head.shorthand().unwrap_or("?")))
        }
        Ok(head) => {
            let at = head.target().map(|oid| oid.to_string()).unwrap_or_default();
            Ok(format!("detached HEAD at {at}\n"))
        }
        Err(e) if e.code() == ErrorCode::UnbornBranch => Ok("no commits yet\n".into()),
        Err(e) => Err(err(e)),
    }
}

fn log(repo: &Repository, limit: usize) -> Result<String, String> {
    let mut walk = repo.revwalk().map_err(err)?;
    if let Err(e) = walk.push_head() {
        return if e.code() == ErrorCode::UnbornBranch {
            Ok("no commits yet\n".into())
        } else {
            Err(err(e))
        };
    }
    walk.set_sorting(Sort::TIME).map_err(err)?;

    let mut out = String::new();
    for oid in walk.take(limit) {
        let commit = repo.find_commit(oid.map_err(err)?).map_err(err)?;
        let id = commit.id().to_string();
        let short = &id[..7.min(id.len())];
        let date = chrono::DateTime::from_timestamp(commit.time().seconds(), 0)
            .map(|t| t.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        out.push_str(&format!(
            "{short} {date} {}: {}\n",
            commit.author().name().unwrap_or("?"),
            commit.summary().unwrap_or("")
        ));
    }
    if out.is_empty() {
        out.push_str("no commits yet\n");
    }
    Ok(out)
}

fn diff(repo: &Repository, arguments: &Value) -> Result<String, String> {
    let staged = arguments["staged"].as_bool().unwrap_or(false);

    let mut opts = DiffOptions::new();
    if let Some(path) = arguments["path"].as_str() {
        check_relative(path)?;
        opts.pathspec(path);
    }

    let diff = if staged {
        // Compare HEAD's tree with the index. On an unborn branch, compare against nothing.
        let tree = match repo.head() {
            Ok(head) => Some(head.peel_to_tree().map_err(err)?),
            Err(e) if e.code() == ErrorCode::UnbornBranch => None,
            Err(e) => return Err(err(e)),
        };
        repo.diff_tree_to_index(tree.as_ref(), None, Some(&mut opts)).map_err(err)?
    } else {
        repo.diff_index_to_workdir(None, Some(&mut opts)).map_err(err)?
    };

    let mut out = String::new();
    diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        // File and hunk headers carry no prefix; content lines do.
        if matches!(line.origin(), '+' | '-' | ' ') {
            out.push(line.origin());
        }
        out.push_str(&String::from_utf8_lossy(line.content()));
        true
    })
    .map_err(err)?;

    if out.is_empty() {
        out.push_str("no changes\n");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Signature;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cherrypi-git-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Init a repo with one committed file, `a.txt`, containing "one\n".
    fn repo_with_commit(dir: &Path) {
        let repo = Repository::init(dir).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("a.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = Signature::now("Test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "first commit", &tree, &[]).unwrap();
    }

    fn call(dir: &Path, args: Value) -> ToolResult {
        let tool = GitTool::new(dir.to_path_buf());
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(tool.execute(args))
    }

    #[test]
    fn status_branch_and_log_report_a_committed_repo() {
        let dir = scratch("basic");
        repo_with_commit(&dir);

        let status = call(&dir, json!({"action": "status"}));
        assert!(!status.is_error);
        assert!(status.content.contains("working tree clean"), "{}", status.content);

        let branch = call(&dir, json!({"action": "branch"}));
        assert!(branch.content.contains("on branch"), "{}", branch.content);

        let log = call(&dir, json!({"action": "log"}));
        assert!(log.content.contains("first commit"), "{}", log.content);
        assert!(log.content.contains("Test"), "{}", log.content);
    }

    #[test]
    fn status_and_diff_show_unstaged_and_untracked_changes() {
        let dir = scratch("changes");
        repo_with_commit(&dir);
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        std::fs::write(dir.join("new.txt"), "fresh\n").unwrap();

        let status = call(&dir, json!({"action": "status"}));
        assert!(status.content.contains(" M a.txt"), "{}", status.content);
        assert!(status.content.contains("?? new.txt"), "{}", status.content);

        let diff = call(&dir, json!({"action": "diff"}));
        assert!(diff.content.contains("-one"), "{}", diff.content);
        assert!(diff.content.contains("+two"), "{}", diff.content);
    }

    #[test]
    fn rejects_paths_outside_the_repository() {
        let dir = scratch("paths");
        repo_with_commit(&dir);

        let result = call(&dir, json!({"action": "diff", "path": "../secret"}));
        assert!(result.is_error);
        let absolute = call(&dir, json!({"action": "diff", "path": "/etc/passwd"}));
        assert!(absolute.is_error);
    }

    #[test]
    fn unknown_action_and_non_repo_are_errors() {
        let dir = scratch("unknown");
        repo_with_commit(&dir);
        assert!(call(&dir, json!({"action": "commit"})).is_error);

        let plain = scratch("not-a-repo");
        let result = call(&plain, json!({"action": "status"}));
        assert!(result.is_error);
        assert!(result.content.contains("no git repository"), "{}", result.content);
    }
}
