use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreationScriptInput {
    pub target_url: String,
    pub name: String,
    pub description: Option<String>,
    pub visibility: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreationScriptResult {
    pub created: bool,
    pub target_url: String,
    pub repository_id: Option<String>,
    pub message: Option<String>,
}
#[derive(Debug, Clone)]
pub struct ScriptSpec {
    pub executable: PathBuf,
    pub cwd: PathBuf,
    pub timeout: Duration,
    pub env: BTreeMap<String, String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptError {
    Invalid(String),
    Io(String),
    Timeout,
    Failed(i32, String),
    InvalidOutput(String),
}
impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(s) => write!(f, "脚本配置无效: {s}"),
            Self::Io(s) => write!(f, "脚本 I/O 失败: {s}"),
            Self::Timeout => write!(f, "脚本执行超时"),
            Self::Failed(c, s) => write!(f, "脚本退出码 {c}: {s}"),
            Self::InvalidOutput(s) => write!(f, "脚本输出不是有效创建结果: {s}"),
        }
    }
}
impl std::error::Error for ScriptError {}
pub struct ScriptRunner;

/// Generic Git 没有建库 API。只有用户明确配置脚本后才允许创建目标。
pub fn create_target_repository(
    script: Option<&ScriptSpec>,
    input: &CreationScriptInput,
) -> Result<CreationScriptResult, ScriptError> {
    let script = script.ok_or_else(|| {
        ScriptError::Invalid("目标不存在，且未配置外部建库脚本；请先建库或选择脚本".into())
    })?;
    ScriptRunner::run(script, input)
}

impl ScriptRunner {
    pub fn run(
        spec: &ScriptSpec,
        input: &CreationScriptInput,
    ) -> Result<CreationScriptResult, ScriptError> {
        // 脚本接收的 JSON 也属于安全边界；先校验目标地址，避免把
        // `https://user:token@...` 之类的凭据传给外部进程。
        crate::GenericGitUrl::parse(&input.target_url)
            .map_err(|error| ScriptError::Invalid(format!("目标地址无效: {error}")))?;
        validate_spec(spec)?;
        let payload = serde_json::to_vec(input).map_err(|e| ScriptError::Invalid(e.to_string()))?;
        let mut command = Command::new(&spec.executable);
        command
            .current_dir(&spec.cwd)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &spec.env {
            command.env(key, value);
        }
        let mut child = command
            .spawn()
            .map_err(|e| ScriptError::Io(e.to_string()))?;
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin
                .write_all(&payload)
                .map_err(|e| ScriptError::Io(e.to_string()))?;
        }
        let started = Instant::now();
        loop {
            if started.elapsed() >= spec.timeout {
                let _ = child.kill();
                return Err(ScriptError::Timeout);
            }
            match child
                .try_wait()
                .map_err(|e| ScriptError::Io(e.to_string()))?
            {
                Some(status) => {
                    let output = child
                        .wait_with_output()
                        .map_err(|e| ScriptError::Io(e.to_string()))?;
                    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                    if !status.success() {
                        return Err(ScriptError::Failed(
                            status.code().unwrap_or(-1),
                            sanitize(stderr),
                        ));
                    }
                    let result: CreationScriptResult = serde_json::from_slice(&output.stdout)
                        .map_err(|e| ScriptError::InvalidOutput(e.to_string()))?;
                    crate::GenericGitUrl::parse(&result.target_url)
                        .map_err(|error| ScriptError::InvalidOutput(error.to_string()))?;
                    return Ok(result);
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }
}
fn validate_spec(spec: &ScriptSpec) -> Result<(), ScriptError> {
    if !spec.executable.is_absolute() {
        return Err(ScriptError::Invalid("脚本路径必须是绝对路径".into()));
    }
    if !spec.executable.is_file() {
        return Err(ScriptError::Invalid("脚本不存在或不是文件".into()));
    }
    if !spec.cwd.is_absolute() || !spec.cwd.is_dir() {
        return Err(ScriptError::Invalid("cwd 必须是存在的绝对目录".into()));
    }
    if spec.timeout.is_zero() {
        return Err(ScriptError::Invalid("超时必须大于 0".into()));
    }
    for (key, value) in &spec.env {
        if !is_safe_env_key(key) || is_secret_name(key) || is_secret_value(value) {
            return Err(ScriptError::Invalid(format!("禁止敏感环境变量: {key}")));
        }
    }
    Ok(())
}
fn is_safe_env_key(key: &str) -> bool {
    !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}
fn is_secret_name(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k.contains("token")
        || k.contains("password")
        || k.contains("secret")
        || k.contains("credential")
        || k.contains("private")
        || k.contains("cookie")
}
fn is_secret_value(value: &str) -> bool {
    let v = value.to_ascii_lowercase();
    v.contains("bearer ") || v.contains("authorization") || v.contains("-----begin")
}
fn sanitize(value: String) -> String {
    if value.len() > 512 {
        format!("{}…", &value[..512])
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_secret_environment_and_relative_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join(if cfg!(windows) {
            "script.cmd"
        } else {
            "script.sh"
        });
        std::fs::write(&script, b"x").unwrap();
        let e = ScriptRunner::run(
            &ScriptSpec {
                executable: script,
                cwd: PathBuf::from("."),
                timeout: Duration::from_secs(1),
                env: [("API_TOKEN".into(), "x".into())].into_iter().collect(),
            },
            &CreationScriptInput {
                target_url: "x".into(),
                name: "x".into(),
                description: None,
                visibility: None,
            },
        )
        .unwrap_err();
        assert!(matches!(e, ScriptError::Invalid(_)));
    }

    #[test]
    fn missing_creation_script_is_a_blocker() {
        let error = create_target_repository(
            None,
            &CreationScriptInput {
                target_url: "https://example.test/repo.git".into(),
                name: "repo".into(),
                description: None,
                visibility: None,
            },
        )
        .unwrap_err();
        assert!(matches!(error, ScriptError::Invalid(message) if message.contains("未配置")));
    }

    #[test]
    fn result_schema_rejects_unknown_fields_and_credential_url() {
        let unknown = br#"{"created":true,"target_url":"https://example.test/a.git","extra":1}"#;
        assert!(serde_json::from_slice::<CreationScriptResult>(unknown).is_err());
        let credential = br#"{"created":true,"target_url":"https://u:p@example.test/a.git"}"#;
        let result: CreationScriptResult = serde_json::from_slice(credential).unwrap();
        assert!(crate::GenericGitUrl::parse(&result.target_url).is_err());
    }

    #[test]
    fn input_credential_url_is_rejected_before_script_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("missing-script");
        let error = ScriptRunner::run(
            &ScriptSpec {
                executable: script,
                cwd: dir.path().to_path_buf(),
                timeout: Duration::from_secs(1),
                env: BTreeMap::new(),
            },
            &CreationScriptInput {
                target_url: "https://u:p@example.test/repo.git".into(),
                name: "repo".into(),
                description: None,
                visibility: None,
            },
        )
        .unwrap_err();
        assert!(matches!(error, ScriptError::Invalid(message) if message.contains("目标地址")));
    }
}
