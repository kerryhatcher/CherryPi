//! Kernel-enforced filesystem sandbox for the `bash` tool.
//!
//! Checking the paths a tool is given cannot constrain a shell, which can open
//! any path it can name. On Linux the bash child is confined with Landlock:
//!
//! - read and write only inside the working directory;
//! - read and execute the system library and binary directories, so ordinary
//!   commands still run;
//! - read the plugin bin directories, so installed plugin tools still run.
//!
//! Everything else is denied by the kernel: the home directory, `/tmp`, and
//! `/proc` (which would expose other processes' environments). Symlinks and
//! hard links are checked against their targets. On other platforms no sandbox
//! is available, so the bash tool refuses to run.
//!
//! Secrets injected by `sops exec-env` are also removed from the bash child's
//! environment, so a command cannot print them. See [`sops_secret_names`].

use std::path::{Path, PathBuf};

/// Names of the secrets `sops exec-env` injects, read from the user's secrets
/// file (`~/secrets/secrets.env`). SOPS dotenv keys are plaintext and only the
/// values are encrypted, so the names are available without decrypting. A
/// missing file yields no names.
pub fn sops_secret_names() -> Vec<String> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    match std::fs::read_to_string(home.join("secrets").join("secrets.env")) {
        Ok(content) => parse_dotenv_names(&content),
        Err(_) => Vec::new(),
    }
}

/// Variable names declared in dotenv `content`. Skips comments, blank lines and
/// SOPS metadata (`sops_*` keys). Values are never returned.
pub fn parse_dotenv_names(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
            let (key, _value) = line.split_once('=')?;
            let key = key.trim();
            if key.is_empty() || key.starts_with("sops_") {
                None
            } else {
                Some(key.to_string())
            }
        })
        .collect()
}

#[cfg(test)]
mod env_name_tests {
    use super::*;

    #[test]
    fn parses_names_and_skips_sops_metadata_and_values() {
        let content = "\
# comment
OPENROUTER_API_KEY=ENC[AES256_GCM,data:abc==,iv:x,tag:y,type:str]
export GITHUB_TOKEN=ENC[AES256_GCM,data:def==]

sops_version=3.9.0
sops_age__list_0__map_recipient=age1xyz
";
        assert_eq!(
            parse_dotenv_names(content),
            vec!["OPENROUTER_API_KEY".to_string(), "GITHUB_TOKEN".to_string()]
        );
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use landlock::{
        path_beneath_rules, Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr,
        RulesetCreated, RulesetCreatedAttr, RulesetStatus, ABI,
    };
    use std::io;

    /// ABI v2 adds `REFER`, which stops hard links and renames from reaching
    /// files outside the sandbox. It needs Linux 5.19 or newer. Without it, a
    /// hard link inside the workspace could read a file outside it.
    const ABI_LEVEL: ABI = ABI::V2;

    /// System locations a command needs in order to run. Read and execute only.
    const SYSTEM_READ_PATHS: &[&str] = &[
        "/usr",
        "/lib",
        "/lib64",
        "/bin",
        "/sbin",
        "/etc/ld.so.cache",
        "/etc/ld.so.conf",
        "/etc/ld.so.conf.d",
    ];

    /// Device files commands commonly use.
    const DEVICE_READ_WRITE_PATHS: &[&str] = &["/dev/null"];
    const DEVICE_READ_PATHS: &[&str] = &["/dev/zero", "/dev/urandom"];

    /// A Landlock ruleset built in the parent process, ready to apply in the child.
    pub struct Sandbox {
        ruleset: RulesetCreated,
    }

    impl Sandbox {
        /// Build the ruleset. Fails if the kernel cannot enforce the full policy.
        pub fn prepare(working_dir: &Path, extra_read_dirs: &[PathBuf]) -> Result<Self, String> {
            let created = Ruleset::default()
                .set_compatibility(CompatLevel::HardRequirement)
                .handle_access(AccessFs::from_all(ABI_LEVEL))
                .and_then(|attr| attr.create())
                .map_err(|e| format!("bash sandbox unavailable (needs Linux 5.19+ with Landlock): {e}"))?;

            let rules = path_beneath_rules([working_dir], AccessFs::from_all(ABI_LEVEL))
                .chain(path_beneath_rules(DEVICE_READ_WRITE_PATHS, AccessFs::from_all(ABI_LEVEL)))
                .chain(path_beneath_rules(DEVICE_READ_PATHS, AccessFs::from_read(ABI_LEVEL)))
                .chain(path_beneath_rules(SYSTEM_READ_PATHS, AccessFs::from_read(ABI_LEVEL)))
                .chain(path_beneath_rules(extra_read_dirs, AccessFs::from_read(ABI_LEVEL)));

            let ruleset = created
                .add_rules(rules)
                .map_err(|e| format!("bash sandbox rules could not be added: {e}"))?;
            Ok(Self { ruleset })
        }

        /// Confine `cmd` (and everything it spawns) to this sandbox. The child
        /// fails to start if the restriction cannot be applied in full.
        pub fn confine(self, cmd: &mut std::process::Command) {
            use std::os::unix::process::CommandExt;
            let mut ruleset = Some(self.ruleset);
            // SAFETY: the closure runs in the forked child before exec. It only
            // calls restrict_self on a ruleset that was built before the fork.
            unsafe {
                cmd.pre_exec(move || {
                    let ruleset = ruleset
                        .take()
                        .ok_or_else(|| io::Error::other("sandbox already applied"))?;
                    let status = ruleset.restrict_self().map_err(io::Error::other)?;
                    if matches!(status.ruleset, RulesetStatus::FullyEnforced) {
                        Ok(())
                    } else {
                        Err(io::Error::other("landlock sandbox was not fully enforced"))
                    }
                });
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::process::{Command, Output};

        fn scratch(name: &str) -> PathBuf {
            let dir = std::env::temp_dir()
                .join(format!("cherrypi-sandbox-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn run(workspace: &Path, script: &str) -> Output {
            let sandbox = Sandbox::prepare(workspace, &[]).expect("sandbox should prepare");
            let mut cmd = Command::new("bash");
            cmd.arg("-c").arg(script).current_dir(workspace);
            sandbox.confine(&mut cmd);
            cmd.output().expect("bash should spawn")
        }

        fn stdout(out: &Output) -> String {
            String::from_utf8_lossy(&out.stdout).to_string()
        }

        #[test]
        fn allows_reads_and_writes_inside_workspace() {
            let ws = scratch("inside");
            let out = run(&ws, "echo hi > ok.txt && cat ok.txt && ls . >/dev/null && wc -c ok.txt");
            assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
            assert!(stdout(&out).contains("hi"));
        }

        #[test]
        fn common_tools_still_run_inside_workspace() {
            let ws = scratch("tools");
            std::fs::write(ws.join("notes.txt"), "alpha\nbeta\n").unwrap();
            let out = run(
                &ws,
                "python3 -c 'print(40 + 2)' && grep -c beta notes.txt && git --version",
            );
            assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
            assert!(stdout(&out).contains("42"));
        }

        #[test]
        fn denies_reads_outside_workspace() {
            let ws = scratch("read-ws");
            let outside = scratch("read-out");
            let secret = outside.join("secret.txt");
            std::fs::write(&secret, "s3cret").unwrap();

            let out = run(&ws, &format!("cat '{}'", secret.display()));
            assert!(!out.status.success());
            assert!(!stdout(&out).contains("s3cret"));
        }

        #[test]
        fn denies_writes_outside_workspace() {
            let ws = scratch("write-ws");
            let outside = scratch("write-out");
            let target = outside.join("new.txt");

            let out = run(&ws, &format!("echo x > '{}'", target.display()));
            assert!(!out.status.success());
            assert!(!target.exists());
        }

        #[test]
        fn denies_cd_out_of_workspace() {
            let ws = scratch("cd-ws");
            let outside = scratch("cd-out");
            std::fs::write(outside.join("secret.txt"), "s3cret").unwrap();

            let out = run(&ws, &format!("cd '{}' && cat secret.txt", outside.display()));
            assert!(!out.status.success());
            assert!(!stdout(&out).contains("s3cret"));
        }

        #[test]
        fn denies_symlink_escape() {
            let ws = scratch("symlink-ws");
            let outside = scratch("symlink-out");
            let secret = outside.join("secret.txt");
            std::fs::write(&secret, "s3cret").unwrap();

            let out = run(&ws, &format!("ln -s '{}' link.txt && cat link.txt", secret.display()));
            assert!(!out.status.success());
            assert!(!stdout(&out).contains("s3cret"));
        }

        #[test]
        fn denies_hard_link_escape() {
            let ws = scratch("hardlink-ws");
            let outside = scratch("hardlink-out");
            let secret = outside.join("secret.txt");
            std::fs::write(&secret, "s3cret").unwrap();

            let out = run(&ws, &format!("ln '{}' hard.txt && cat hard.txt", secret.display()));
            assert!(!out.status.success());
            assert!(!stdout(&out).contains("s3cret"));
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::Sandbox;

/// Stand-in on platforms without a supported sandbox. `prepare` always fails,
/// so the bash tool refuses to run instead of running unconfined.
#[cfg(not(target_os = "linux"))]
pub struct Sandbox;

#[cfg(not(target_os = "linux"))]
impl Sandbox {
    pub fn prepare(_working_dir: &Path, _extra_read_dirs: &[PathBuf]) -> Result<Self, String> {
        Err("bash is disabled: its filesystem sandbox requires Linux (Landlock)".into())
    }

    pub fn confine(self, _cmd: &mut std::process::Command) {}
}
