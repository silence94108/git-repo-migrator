//! Ports the IPC layer depends on.
//!
//! Keeping these as traits lets the command surface be tested without a
//! network, without a Tauri window and without touching a real remote, while
//! the production wiring stays a single `AppState::with_ports` call.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use git_repo_migrator_application::planning::TargetState;
use git_repo_migrator_application::IpcError;
use git_repo_migrator_domain::{ErrorCategory, Fidelity};
use git_repo_migrator_git_runner::{GitError, GitRunner, RunOptions};
use git_repo_migrator_platform_core::{DiscoveryQuery, PlatformKind, RepositoryCandidate};

use crate::errors;

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| i64::try_from(value.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or_default()
    }
}

/// Determines whether a target repository exists and whether it is empty.
/// A target whose state cannot be established stays `Unknown`, which blocks the
/// plan instead of silently defaulting to a write.
pub trait TargetProbe: Send + Sync {
    fn probe(&self, target_url: &str) -> Result<TargetState, IpcError>;
}

/// `git ls-remote` based probe. Works for every Git host without an API and
/// never passes credentials on the command line.
pub struct GitLsRemoteProbe {
    runner: GitRunner,
    timeout: Duration,
}

impl GitLsRemoteProbe {
    pub fn system() -> Result<Self, GitError> {
        Ok(Self {
            runner: GitRunner::system()?.with_timeout(Duration::from_secs(30)),
            timeout: Duration::from_secs(30),
        })
    }
}

impl TargetProbe for GitLsRemoteProbe {
    fn probe(&self, target_url: &str) -> Result<TargetState, IpcError> {
        let mut env = BTreeMap::new();
        // Never let Git open an interactive credential prompt from a GUI child
        // process; an unauthenticated probe must fail fast instead.
        env.insert("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned());
        let args = [
            "ls-remote".to_owned(),
            "--".to_owned(),
            target_url.to_owned(),
        ];
        match self.runner.run(
            &args,
            RunOptions {
                timeout: self.timeout,
                env,
                ..RunOptions::default()
            },
        ) {
            Ok(output) if output.stdout.trim().is_empty() => Ok(TargetState::Empty),
            Ok(_) => Ok(TargetState::NonEmpty),
            Err(GitError::Failed { stderr, .. }) => {
                let lowered = stderr.to_ascii_lowercase();
                if lowered.contains("not found")
                    || lowered.contains("does not exist")
                    || lowered.contains("repository not found")
                {
                    Ok(TargetState::Missing)
                } else if lowered.contains("authentication")
                    || lowered.contains("permission")
                    || lowered.contains("403")
                    || lowered.contains("401")
                {
                    Ok(TargetState::Inaccessible)
                } else {
                    Ok(TargetState::Unknown)
                }
            }
            Err(GitError::Timeout { .. }) => Ok(TargetState::Unknown),
            Err(other) => Err(errors::error(
                "ipc.git",
                ErrorCategory::Git,
                true,
                "preflight",
                format!("目标探测失败：{other}"),
                "请确认目标地址可达并已配置 Windows 凭据后重试",
            )),
        }
    }
}

/// API-based repository discovery. The credential is passed by reference only:
/// the gateway resolves it inside the transport, so no caller here ever holds a
/// token.
pub trait DiscoveryGateway: Send + Sync {
    fn discover(
        &self,
        endpoint: &str,
        platform: PlatformKind,
        credential_ref: Option<&str>,
        query: &DiscoveryQuery,
    ) -> Result<Vec<RepositoryCandidate>, IpcError>;
}

/// Probes a platform's real API: who the token belongs to, which instance
/// version answered, and what that instance can actually do. The credential is
/// resolved inside the transport exactly as in discovery.
pub trait ConnectionTester: Send + Sync {
    fn test(
        &self,
        endpoint: &str,
        platform: PlatformKind,
        credential_ref: Option<&str>,
    ) -> Result<ConnectionProbe, IpcError>;
}

/// What a real probe established. The capabilities come from the platform's own
/// capability matrix, not from a static table.
pub struct ConnectionProbe {
    pub account_name: Option<String>,
    pub instance_version: Option<String>,
    pub capabilities: Vec<ConnectionCapability>,
}

/// One row of the probe's capability answer, already flattened for the DTO.
pub struct ConnectionCapability {
    pub module: &'static str,
    pub supported: bool,
    pub permitted: bool,
    pub required_scopes: Vec<String>,
    pub fidelity: Fidelity,
    pub reason: Option<String>,
    pub degradation: Option<String>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TransportNotWired;

impl DiscoveryGateway for TransportNotWired {
    fn discover(
        &self,
        _endpoint: &str,
        platform: PlatformKind,
        _credential_ref: Option<&str>,
        _query: &DiscoveryQuery,
    ) -> Result<Vec<RepositoryCandidate>, IpcError> {
        Err(errors::unsupported(
            "discovery",
            format!("{platform:?} 的 API 发现依赖 HTTP 传输层，本版本尚未接入"),
            "请改用「手动 URL 导入」，或等待传输层接入后重新发现",
        ))
    }
}

/// Writes an export artefact. Separated so export validation is testable
/// without touching the file system.
pub trait ExportSink: Send + Sync {
    fn write(&self, path: &Path, contents: &str) -> Result<(), String>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct FileExportSink;

impl ExportSink for FileExportSink {
    fn write(&self, path: &Path, contents: &str) -> Result<(), String> {
        std::fs::write(path, contents).map_err(|error| error.to_string())
    }
}

/// Starts and stops the worker pool that actually migrates repositories.
///
/// `AppState` owns queue *state*; it must never own threads, a workspace or a
/// Git process. Keeping the pool behind this port is also what lets the command
/// tests run a full batch lifecycle without spawning anything.
pub trait BatchLauncher: Send + Sync {
    /// Called after `batch_start` and after a resume. Implementations must be
    /// idempotent: a second call for a batch that is already running is a no-op.
    fn launch(&self, batch_id: &str, concurrency: u16);
    /// Called when a batch is cancelled. Signals in-flight stages to stop at
    /// their next checkpoint; it never rolls back completed work.
    fn cancel(&self, batch_id: &str);
}

/// Opens the native credential-entry window.
///
/// The GUI process must never read a token, or CM-004 stops being true: a
/// secret in the webview is one crash report away from disk. Entry therefore
/// happens in a separate console process that this port launches with nothing
/// but a validated name.
pub trait IdentityEntryLauncher: Send + Sync {
    fn launch(&self, name: &str) -> Result<(), IpcError>;
}

/// File name of the console companion, shipped next to the application binary.
pub const CREDENTIAL_COMPANION: &str = if cfg!(windows) {
    "git-repo-migrator-credential.exe"
} else {
    "git-repo-migrator-credential"
};

/// Finds the companion next to `directory`.
///
/// Tauri ships an `externalBin` with its target triple in the file name, while a
/// `cargo build` produces the plain name. Both are accepted; nothing outside the
/// application's own directory is, and the match has to be exact enough that an
/// unrelated executable cannot be picked up.
fn find_companion(directory: &Path) -> Option<std::path::PathBuf> {
    let exact = directory.join(CREDENTIAL_COMPANION);
    if exact.is_file() {
        return Some(exact);
    }
    let stem = CREDENTIAL_COMPANION.trim_end_matches(".exe");
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    std::fs::read_dir(directory)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&format!("{stem}-")) && name.ends_with(suffix))
        })
}

#[derive(Debug, Default, Clone, Copy)]
pub struct CompanionProcessLauncher;

impl IdentityEntryLauncher for CompanionProcessLauncher {
    fn launch(&self, name: &str) -> Result<(), IpcError> {
        // No shell, no string interpolation: the executable is resolved next to
        // our own binary and the name is passed as a single argv entry.
        let executable = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().and_then(find_companion))
            .ok_or_else(|| {
                errors::error(
                    "credential.companion_missing",
                    ErrorCategory::Validation,
                    false,
                    "connection",
                    format!("找不到凭据录入程序 {CREDENTIAL_COMPANION}"),
                    "请重新安装应用；或在命令行中直接运行该程序录入凭据",
                )
            })?;
        std::process::Command::new(executable)
            .arg(name)
            .spawn()
            .map(|_| ())
            .map_err(|error| {
                errors::error(
                    "credential.companion_failed",
                    ErrorCategory::Validation,
                    true,
                    "connection",
                    format!("无法启动凭据录入程序：{error}"),
                    "请确认应用目录可执行后重试",
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `cargo build` produces the plain name and a Tauri `externalBin` adds a
    /// target triple; both have to be found, and nothing else may be.
    #[test]
    fn the_companion_is_found_under_either_shipped_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(find_companion(dir.path()).is_none());

        let sidecar = dir.path().join(if cfg!(windows) {
            "git-repo-migrator-credential-x86_64-pc-windows-msvc.exe"
        } else {
            "git-repo-migrator-credential-x86_64-unknown-linux-gnu"
        });
        std::fs::write(&sidecar, b"stub").expect("sidecar");
        assert_eq!(find_companion(dir.path()), Some(sidecar));

        let exact = dir.path().join(CREDENTIAL_COMPANION);
        std::fs::write(&exact, b"stub").expect("exact");
        assert_eq!(
            find_companion(dir.path()),
            Some(exact),
            "the plain name wins when both are present"
        );
    }

    #[test]
    fn an_unrelated_executable_is_never_mistaken_for_the_companion() {
        let dir = tempfile::tempdir().expect("temp dir");
        for name in [
            "git.exe",
            "credential.exe",
            "git-repo-migrator.exe",
            "notepad.exe",
        ] {
            std::fs::write(dir.path().join(name), b"stub").expect("decoy");
        }
        assert_eq!(find_companion(dir.path()), None);
    }

    #[test]
    fn transport_not_wired_reports_an_actionable_unsupported_error() {
        let error = TransportNotWired
            .discover(
                "https://github.com",
                PlatformKind::Github,
                None,
                &DiscoveryQuery {
                    scope: git_repo_migrator_platform_core::RepositoryScope::Owned,
                    search: None,
                    visibility: None,
                    include_archived: false,
                    cursor: None,
                    page_size: 50,
                },
            )
            .unwrap_err();
        assert_eq!(error.category, ErrorCategory::Unsupported);
        assert!(!error.retryable);
        assert!(error.action.contains("手动 URL 导入"));
    }
}
