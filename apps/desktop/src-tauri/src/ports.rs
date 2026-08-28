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
use git_repo_migrator_domain::ErrorCategory;
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

/// API-based repository discovery. Requires an HTTP transport implementation;
/// until one is wired the runtime port reports an explicit, actionable error
/// rather than pretending the source platform has no repositories.
pub trait DiscoveryGateway: Send + Sync {
    fn discover(
        &self,
        endpoint: &str,
        platform: PlatformKind,
        query: &DiscoveryQuery,
    ) -> Result<Vec<RepositoryCandidate>, IpcError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TransportNotWired;

impl DiscoveryGateway for TransportNotWired {
    fn discover(
        &self,
        _endpoint: &str,
        platform: PlatformKind,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_not_wired_reports_an_actionable_unsupported_error() {
        let error = TransportNotWired
            .discover(
                "https://github.com",
                PlatformKind::Github,
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
