use git_repo_migrator_domain::{ErrorCategory, Fidelity};
use git_repo_migrator_platform_core::{DiscoveryQuery, PlatformKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionTestInput {
    pub endpoint: String,
    pub platform_hint: Option<PlatformKind>,
    pub credential_ref: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryDiscoverInput {
    pub connection_id: String,
    pub query: DiscoveryQuery,
}
/// Asks the backend to open the native credential-entry window.
///
/// The payload is a *name*, never a secret: the token is typed into a separate
/// console process and written straight to Windows Credential Manager, so it
/// never crosses this boundary. `deny_unknown_fields` makes a renderer that
/// tries to smuggle one fail loudly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionAuthorizeInput {
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanPreviewInput {
    pub selected_repository_ids: Vec<String>,
    pub conflict_policy: String,
    pub modules: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchStartInput {
    pub plan_id: String,
    pub concurrency: u16,
    pub workspace_policy: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRetryInput {
    pub batch_id: String,
    pub task_ids: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportExportInput {
    pub batch_id: String,
    pub format: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IpcError {
    pub code: String,
    pub category: ErrorCategory,
    pub retryable: bool,
    pub stage: String,
    pub safe_message: String,
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MigrationEvent {
    BatchStarted {
        batch_id: String,
    },
    TaskStageChanged {
        batch_id: String,
        task_id: String,
        stage: String,
    },
    TaskProgress {
        batch_id: String,
        task_id: String,
        completed: u64,
        total: Option<u64>,
    },
    TaskWarning {
        batch_id: String,
        task_id: String,
        code: String,
        safe_message: String,
    },
    TaskCompleted {
        batch_id: String,
        task_id: String,
        status: String,
        fidelity: Vec<Fidelity>,
    },
    BatchCompleted {
        batch_id: String,
        status: String,
    },
}

pub fn typescript_contract() -> &'static str {
    include_str!("../../../apps/desktop/src/generated/ipc.ts")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn command_inputs_reject_unknown_fields() {
        let value = r#"{"endpoint":"https://github.com","platform_hint":"github","credential_ref":null,"token":"secret"}"#;
        assert!(serde_json::from_str::<ConnectionTestInput>(value).is_err());
    }

    /// The authorize command is the closest thing to a credential command in the
    /// surface, so it is the one most worth pinning: only a name gets through.
    #[test]
    fn authorize_accepts_a_name_and_nothing_else() {
        assert!(serde_json::from_str::<ConnectionAuthorizeInput>(r#"{"name":"source"}"#).is_ok());
        for value in [
            r#"{"name":"source","token":"ghp-secret"}"#,
            r#"{"name":"source","secret":"x"}"#,
            r#"{"name":"source","password":"x"}"#,
        ] {
            assert!(
                serde_json::from_str::<ConnectionAuthorizeInput>(value).is_err(),
                "{value} must be rejected"
            );
        }
    }
    #[test]
    fn events_have_no_secret_or_response_fields() {
        let event = MigrationEvent::TaskWarning {
            batch_id: "b".into(),
            task_id: "t".into(),
            code: "rate_limited".into(),
            safe_message: "稍后重试".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("token"));
        assert!(!json.contains("response"));
    }
}
