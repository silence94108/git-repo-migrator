use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Windows process-creation flag (winbase.h `CREATE_NO_WINDOW`): the child
/// runs without a console window. `std` does not export the constant.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitExecutable {
    Git,
    GitLfs,
}

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub timeout: Duration,
    pub cancel: Option<Arc<AtomicBool>>,
    pub current_dir: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30 * 60),
            cancel: None,
            current_dir: None,
            env: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
    pub elapsed: Duration,
}

#[derive(Debug)]
pub enum GitError {
    InvalidExecutable(PathBuf),
    InvalidArgument(String),
    Io(String),
    Timeout {
        timeout: Duration,
        stderr: String,
    },
    Cancelled {
        stderr: String,
    },
    Failed {
        code: Option<i32>,
        stderr: String,
        stdout: String,
    },
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidExecutable(p) => {
                write!(f, "git executable is not allowlisted: {}", p.display())
            }
            Self::InvalidArgument(a) => write!(f, "unsafe git argument: {a}"),
            Self::Io(e) => write!(f, "git process I/O failed: {e}"),
            Self::Timeout { timeout, .. } => write!(f, "git process timed out after {timeout:?}"),
            Self::Cancelled { .. } => write!(f, "git process cancelled"),
            Self::Failed { code, stderr, .. } => write!(f, "git exited with {code:?}: {stderr}"),
        }
    }
}
impl std::error::Error for GitError {}

#[derive(Debug, Clone)]
pub struct GitRunner {
    git: PathBuf,
    lfs: Option<PathBuf>,
    redactions: Vec<String>,
    default_timeout: Duration,
}

impl GitRunner {
    pub fn new(git: impl Into<PathBuf>) -> Result<Self, GitError> {
        let git = git.into();
        validate_executable(&git, GitExecutable::Git)?;
        Ok(Self {
            git,
            lfs: None,
            redactions: Vec::new(),
            default_timeout: Duration::from_secs(30 * 60),
        })
    }
    pub fn system() -> Result<Self, GitError> {
        Self::new(if cfg!(windows) {
            PathBuf::from("git.exe")
        } else {
            PathBuf::from("git")
        })
    }
    pub fn with_lfs(mut self, lfs: impl Into<PathBuf>) -> Result<Self, GitError> {
        let path = lfs.into();
        validate_executable(&path, GitExecutable::GitLfs)?;
        self.lfs = Some(path);
        Ok(self)
    }
    pub fn with_redactions(mut self, values: impl IntoIterator<Item = String>) -> Self {
        self.redactions.extend(values);
        self
    }
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }
    pub fn run(&self, args: &[String], options: RunOptions) -> Result<GitOutput, GitError> {
        self.run_executable(GitExecutable::Git, args, options)
    }
    pub fn run_str_args(&self, args: &[&str]) -> Result<GitOutput, GitError> {
        self.run(
            &args.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
            RunOptions {
                timeout: self.default_timeout,
                ..Default::default()
            },
        )
    }
    pub fn run_executable(
        &self,
        executable: GitExecutable,
        args: &[String],
        mut options: RunOptions,
    ) -> Result<GitOutput, GitError> {
        let path = match executable {
            GitExecutable::Git => &self.git,
            GitExecutable::GitLfs => self.lfs.as_ref().unwrap_or(&self.git),
        };
        validate_executable(path, executable)?;
        for arg in args {
            if contains_userinfo(arg) {
                return Err(GitError::InvalidArgument(
                    "URL userinfo is forbidden; use Credential Manager".into(),
                ));
            }
        }
        if options.timeout == Duration::ZERO {
            options.timeout = self.default_timeout;
        }
        let started = Instant::now();
        let mut cmd = Command::new(path);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // The GUI process has no console, so Windows would otherwise flash a
        // new console window for every Git invocation — one per clone, push and
        // probe. CREATE_NO_WINDOW keeps the child invisible while its pipes
        // stay fully functional.
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        if let Some(dir) = options.current_dir {
            cmd.current_dir(dir);
        }
        // Only explicitly supplied, non-secret environment values are accepted.
        for (key, value) in options.env {
            if is_secret_key(&key) {
                return Err(GitError::InvalidArgument(format!(
                    "secret environment variable is forbidden: {key}"
                )));
            }
            cmd.env(key, value);
        }
        let mut child = cmd.spawn().map_err(|e| GitError::Io(e.to_string()))?;
        loop {
            if options
                .cancel
                .as_ref()
                .is_some_and(|c| c.load(Ordering::Relaxed))
            {
                let _ = child.kill();
                let out = child
                    .wait_with_output()
                    .map_err(|e| GitError::Io(e.to_string()))?;
                return Err(GitError::Cancelled {
                    stderr: redact(&String::from_utf8_lossy(&out.stderr), &self.redactions),
                });
            }
            if started.elapsed() >= options.timeout {
                let _ = child.kill();
                let out = child
                    .wait_with_output()
                    .map_err(|e| GitError::Io(e.to_string()))?;
                return Err(GitError::Timeout {
                    timeout: options.timeout,
                    stderr: redact(&String::from_utf8_lossy(&out.stderr), &self.redactions),
                });
            }
            match child.try_wait().map_err(|e| GitError::Io(e.to_string()))? {
                Some(status) => {
                    let out = child
                        .wait_with_output()
                        .map_err(|e| GitError::Io(e.to_string()))?;
                    let stdout = redact(&String::from_utf8_lossy(&out.stdout), &self.redactions);
                    let stderr = redact(&String::from_utf8_lossy(&out.stderr), &self.redactions);
                    let result = GitOutput {
                        status: status.code().unwrap_or(-1),
                        stdout,
                        stderr,
                        elapsed: started.elapsed(),
                    };
                    if status.success() {
                        return Ok(result);
                    }
                    return Err(GitError::Failed {
                        code: status.code(),
                        stderr: result.stderr,
                        stdout: result.stdout,
                    });
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }
}

fn validate_executable(path: &Path, expected: GitExecutable) -> Result<(), GitError> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let ok = match expected {
        GitExecutable::Git => name == "git" || name == "git.exe",
        GitExecutable::GitLfs => name == "git-lfs" || name == "git-lfs.exe",
    };
    if ok {
        Ok(())
    } else {
        Err(GitError::InvalidExecutable(path.to_path_buf()))
    }
}
fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k.contains("token")
        || k.contains("password")
        || k.contains("secret")
        || k.contains("private_key")
        || k == "http_extraheader"
}
fn contains_userinfo(arg: &str) -> bool {
    arg.find("://")
        .and_then(|i| arg[i + 3..].find('@'))
        .is_some()
}
pub(crate) fn redact(value: &str, secrets: &[String]) -> String {
    let mut out = value.to_string();
    for secret in secrets {
        if !secret.is_empty() {
            out = out.replace(secret, "[REDACTED]");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_url_credentials() {
        let r = GitRunner::new("git").unwrap();
        let e = r
            .run_str_args(&["clone", "https://u:p@example.test/r.git"])
            .unwrap_err();
        assert!(matches!(e, GitError::InvalidArgument(_)));
    }
    #[test]
    fn rejects_secret_env() {
        let r = GitRunner::new("git").unwrap();
        let e = r
            .run(
                &[],
                RunOptions {
                    env: [("TOKEN".into(), "x".into())].into_iter().collect(),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(matches!(e, GitError::InvalidArgument(_)));
    }
}
