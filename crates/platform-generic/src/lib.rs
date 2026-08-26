//! Generic Git 适配器的安全边界。

mod create_script;
pub use create_script::{
    create_target_repository, CreationScriptInput, CreationScriptResult, ScriptError, ScriptRunner,
    ScriptSpec,
};

use serde::{Deserialize, Serialize};
use std::{fmt, path::Path, time::Duration};
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GenericGitUrl(String);

impl GenericGitUrl {
    pub fn parse(input: &str) -> Result<Self, UrlValidationError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(UrlValidationError::Empty);
        }
        if trimmed.chars().any(char::is_whitespace) {
            return Err(UrlValidationError::Whitespace);
        }
        if is_scp_style(trimmed) {
            let (user_host, path) = trimmed.split_once(':').expect("scp");
            let (user, host) = user_host.split_once('@').expect("scp");
            if user.is_empty()
                || host.is_empty()
                || path.is_empty()
                || host.contains('@')
                || path.starts_with('/')
                || path.contains("..")
            {
                return Err(UrlValidationError::Malformed);
            }
            return Ok(Self(format!(
                "{}@{}:{}",
                user,
                host.to_ascii_lowercase(),
                path
            )));
        }
        if Path::new(trimmed).is_absolute() {
            return Ok(Self(trimmed.to_string()));
        }
        let mut parsed = Url::parse(trimmed).map_err(|_| UrlValidationError::Malformed)?;
        match parsed.scheme().to_ascii_lowercase().as_str() {
            "https" | "http" | "ssh" | "git" | "file" => {}
            scheme => return Err(UrlValidationError::UnsupportedScheme(scheme.to_string())),
        }
        if parsed.fragment().is_some() {
            return Err(UrlValidationError::Fragment);
        }
        if parsed.password().is_some()
            || (!parsed.username().is_empty() && parsed.scheme() != "ssh")
        {
            return Err(UrlValidationError::Credentials);
        }
        if parsed.query().is_some() {
            return Err(UrlValidationError::Query);
        }
        if let Some(host) = parsed.host_str() {
            let lower = host.to_ascii_lowercase();
            let _ = parsed.set_host(Some(&lower));
        }
        let path = parsed.path().trim_end_matches('/').to_string();
        if path.is_empty() {
            return Err(UrlValidationError::MissingRepositoryPath);
        }
        parsed.set_path(&path);
        Ok(Self(parsed.to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for GenericGitUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UrlValidationError {
    Empty,
    Whitespace,
    Malformed,
    Credentials,
    Query,
    Fragment,
    MissingRepositoryPath,
    UnsupportedScheme(String),
}
impl fmt::Display for UrlValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "地址为空"),
            Self::Whitespace => write!(f, "地址不能包含空白字符"),
            Self::Malformed => write!(f, "地址格式无效"),
            Self::Credentials => write!(f, "地址不得包含用户名、密码或令牌"),
            Self::Query => write!(f, "地址不得包含查询参数"),
            Self::Fragment => write!(f, "地址不得包含片段"),
            Self::MissingRepositoryPath => write!(f, "地址缺少仓库路径"),
            Self::UnsupportedScheme(s) => write!(f, "不支持的协议: {s}"),
        }
    }
}
impl std::error::Error for UrlValidationError {}
fn is_scp_style(value: &str) -> bool {
    !value.contains("://")
        && value.contains('@')
        && value.find(':').is_some_and(|i| i > 0)
        && !value[..value.find(':').unwrap_or(0)].contains('/')
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlImportIssue {
    pub line: usize,
    pub value: String,
    pub message: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlImportReport {
    pub urls: Vec<GenericGitUrl>,
    pub issues: Vec<UrlImportIssue>,
    pub duplicate_count: usize,
}

pub fn import_urls(text: &str) -> UrlImportReport {
    let mut urls = Vec::new();
    let mut issues = Vec::new();
    let mut duplicate_count = 0;
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        let value = line.trim();
        if value.is_empty() || value.starts_with('#') {
            continue;
        }
        match GenericGitUrl::parse(value) {
            Ok(url) if urls.contains(&url) => duplicate_count += 1,
            Ok(url) => urls.push(url),
            Err(error) => issues.push(UrlImportIssue {
                line: line_number,
                value: value.to_string(),
                message: error.to_string(),
            }),
        }
    }
    UrlImportReport {
        urls,
        issues,
        duplicate_count,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    pub readable: bool,
    pub writable: Option<bool>,
    pub summary: String,
    pub elapsed: Duration,
}
pub fn probe_read(
    runner: &git_repo_migrator_git_runner::GitRunner,
    url: &GenericGitUrl,
) -> Result<ProbeResult, String> {
    let started = std::time::Instant::now();
    runner
        .run_str_args(&["ls-remote", url.as_str()])
        .map(|output| ProbeResult {
            readable: true,
            writable: None,
            summary: format!("读取探测成功（{} 行引用）", output.stdout.lines().count()),
            elapsed: started.elapsed(),
        })
        .map_err(|error| format!("读取探测失败: {error}"))
}
pub fn probe_write(
    runner: &git_repo_migrator_git_runner::GitRunner,
    local_repo: &Path,
    url: &GenericGitUrl,
    refspec: &str,
) -> Result<ProbeResult, String> {
    if refspec.is_empty()
        || refspec.contains("mirror")
        || refspec.contains("--")
        || refspec.starts_with(':')
    {
        return Err("写入探测要求非空且不含危险操作的明确 refspec".into());
    }
    let started = std::time::Instant::now();
    runner
        .run(
            &[
                "push".into(),
                "--dry-run".into(),
                url.as_str().into(),
                refspec.into(),
            ],
            git_repo_migrator_git_runner::RunOptions {
                current_dir: Some(local_repo.to_path_buf()),
                ..Default::default()
            },
        )
        .map(|_| ProbeResult {
            readable: true,
            writable: Some(true),
            summary: "写入探测成功（dry-run）".into(),
            elapsed: started.elapsed(),
        })
        .map_err(|error| format!("写入探测失败: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imports_deduplicated_urls_and_reports_each_bad_line() {
        let report =
            import_urls("https://Example.com/a.git/\nhttps://example.com/a.git\nnope://x/y\n");
        assert_eq!(report.urls.len(), 1);
        assert_eq!(report.duplicate_count, 1);
        assert_eq!(report.issues[0].line, 3);
    }
    #[test]
    fn rejects_credentials_and_accepts_scp() {
        assert!(matches!(
            GenericGitUrl::parse("https://u:p@example.com/a"),
            Err(UrlValidationError::Credentials)
        ));
        assert_eq!(
            GenericGitUrl::parse("git@Example.com:team/a.git")
                .unwrap()
                .as_str(),
            "git@example.com:team/a.git"
        );
    }
}
