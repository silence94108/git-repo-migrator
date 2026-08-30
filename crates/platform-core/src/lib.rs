pub mod archive;
pub mod transport;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;
use transport::{HttpTransport, TransportError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformKind {
    Github,
    Gitlab,
    Gitee,
    Gitea,
    Forgejo,
    GenericGit,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub base_url: String,
    pub platform_hint: Option<PlatformKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformIdentity {
    pub kind: PlatformKind,
    pub display_name: String,
    pub instance_url: String,
    pub version: Option<String>,
}

/// An opaque reference to a secret kept by the credential store.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CredentialRef(String);

impl CredentialRef {
    pub fn new(value: impl Into<String>) -> Result<Self, PlatformError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(PlatformError::validation("credential_ref 不能为空"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for CredentialRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CredentialRef([REDACTED])")
    }
}

pub struct AdapterContext<'a> {
    pub connection_id: &'a str,
    pub endpoint: &'a Endpoint,
    pub credential_ref: Option<&'a CredentialRef>,
    pub transport: &'a dyn HttpTransport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fidelity {
    NativeRebuild,
    ReadOnlyArchive,
    Unsupported,
}

impl Fidelity {
    /// Bridges to the domain crate's identically-shaped enum. The two live in
    /// separate crates on purpose (adapters must not depend on domain rules);
    /// their wire values are the shared contract, so the bridge round-trips
    /// through serde instead of duplicating a match arm list.
    pub fn to_domain(self) -> git_repo_migrator_domain::Fidelity {
        serde_json::from_value(serde_json::to_value(self).expect("fidelity serialises"))
            .expect("wire values agree")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub supported: bool,
    pub permitted: bool,
    pub required_scopes: Vec<String>,
    pub version: Option<String>,
    pub reason: Option<String>,
    pub degradation: Option<String>,
    pub fidelity: Fidelity,
}

impl Capability {
    pub fn native(required_scopes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            supported: true,
            permitted: true,
            required_scopes: required_scopes.into_iter().map(Into::into).collect(),
            version: None,
            reason: None,
            degradation: None,
            fidelity: Fidelity::NativeRebuild,
        }
    }

    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            supported: false,
            permitted: false,
            required_scopes: Vec::new(),
            version: None,
            reason: Some(reason.into()),
            degradation: None,
            fidelity: Fidelity::Unsupported,
        }
    }
}

/// A versioned capability snapshot. Plans persist this complete structure and
/// compare it with a fresh probe before execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityMatrix {
    pub schema_version: u32,
    pub platform: PlatformKind,
    pub instance_version: Option<String>,
    pub captured_at_epoch_seconds: u64,
    pub discovery: Capability,
    pub repository_inspection: Capability,
    pub repository_creation: Capability,
    pub git_read: Capability,
    pub git_write: Capability,
    pub lfs: Capability,
    pub metadata: Capability,
    pub issues: Capability,
    pub pull_requests: Capability,
    pub merge_requests: Capability,
    pub wiki: Capability,
    pub releases: Capability,
    pub release_assets: Capability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionInfo {
    pub identity: PlatformIdentity,
    pub account_id: Option<String>,
    pub account_name: Option<String>,
    pub authenticated: bool,
    pub granted_scopes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PaginationCursor(String);

impl PaginationCursor {
    pub fn new(value: impl Into<String>) -> Result<Self, PlatformError> {
        let value = value.into();
        if value.is_empty() || value.len() > 4096 {
            return Err(PlatformError::validation("分页 cursor 长度无效"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryScope {
    Owned,
    Administered,
    Participated,
    AllAccessible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryVisibility {
    Public,
    Internal,
    Private,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryQuery {
    pub scope: RepositoryScope,
    pub search: Option<String>,
    pub visibility: Option<RepositoryVisibility>,
    pub include_archived: bool,
    pub cursor: Option<PaginationCursor>,
    pub page_size: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<PaginationCursor>,
    pub total_count: Option<u64>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryPermissions {
    pub read: bool,
    pub push: bool,
    pub administer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryLocator {
    pub platform_id: Option<String>,
    pub full_name: String,
    pub clone_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryCandidate {
    pub locator: RepositoryLocator,
    pub name: String,
    pub owner: String,
    pub description: Option<String>,
    pub web_url: Option<String>,
    pub clone_url_https: Option<String>,
    pub clone_url_ssh: Option<String>,
    pub visibility: RepositoryVisibility,
    pub archived: bool,
    pub fork: bool,
    pub default_branch: Option<String>,
    pub permissions: RepositoryPermissions,
    pub updated_at_epoch_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteRepositoryState {
    pub exists: bool,
    pub empty: Option<bool>,
    pub locator: Option<RepositoryLocator>,
    pub visibility: Option<RepositoryVisibility>,
    pub default_branch: Option<String>,
    pub permissions: Option<RepositoryPermissions>,
    /// The source description, so a metadata module can carry it to the target.
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateRepositorySpec {
    pub owner: String,
    pub name: String,
    pub description: Option<String>,
    pub visibility: RepositoryVisibility,
    pub homepage: Option<String>,
    pub initialize: bool,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteRepository {
    pub locator: RepositoryLocator,
    pub name: String,
    pub web_url: Option<String>,
    pub clone_url_https: Option<String>,
    pub clone_url_ssh: Option<String>,
    pub visibility: RepositoryVisibility,
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MetadataPatch {
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub topics: Option<Vec<String>>,
    pub default_branch: Option<String>,
    pub archived: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformModule {
    Metadata,
    Issues,
    PullRequests,
    MergeRequests,
    Wiki,
    Releases,
    ReleaseAssets,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleResult {
    pub module: PlatformModule,
    pub fidelity: Fidelity,
    pub discovered: u64,
    pub migrated: u64,
    pub archived: u64,
    pub failed: u64,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub item_mappings: BTreeMap<String, String>,
    /// Links back to the source items, so a report can always point at what was
    /// (or was not) migrated.
    #[serde(default)]
    pub source_links: Vec<String>,
    /// The read-only archive a `ReadOnlyArchive` module produced. `None` for any
    /// other fidelity: an archive that is not handed over is a contract violation.
    #[serde(default)]
    pub archive: Option<archive::ArchiveDocument>,
    /// Source fields this adapter cannot map onto the target, e.g. `reactions`.
    /// Flows into the report's unmapped-fields column so the operator can see
    /// what a "successful" migration still dropped.
    #[serde(default)]
    pub unmapped_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationResult {
    pub module: PlatformModule,
    pub verified: bool,
    pub expected_count: Option<u64>,
    pub actual_count: Option<u64>,
    #[serde(default)]
    pub mismatches: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformErrorCategory {
    Auth,
    Permission,
    Conflict,
    RateLimited,
    Network,
    Validation,
    Unsupported,
    Verification,
}

#[derive(Debug, Error, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[error("{safe_message}")]
pub struct PlatformError {
    pub code: String,
    pub category: PlatformErrorCategory,
    pub retryable: bool,
    pub safe_message: String,
    pub action: String,
    pub retry_after_seconds: Option<u64>,
}

impl PlatformError {
    pub fn validation(message: impl Into<String>) -> Self {
        Self {
            code: "platform.validation".into(),
            category: PlatformErrorCategory::Validation,
            retryable: false,
            safe_message: message.into(),
            action: "请修正输入后重试".into(),
            retry_after_seconds: None,
        }
    }
}

impl From<TransportError> for PlatformError {
    fn from(error: TransportError) -> Self {
        match error {
            TransportError::InvalidConfig(message) => Self::validation(message),
            TransportError::Network(message) => Self {
                code: "platform.network".into(),
                category: PlatformErrorCategory::Network,
                retryable: true,
                safe_message: message,
                action: "请检查网络连接后重试".into(),
                retry_after_seconds: None,
            },
            TransportError::Http {
                status,
                message,
                retry_after,
            } => {
                let (code, category, action) = match status {
                    401 => (
                        "platform.auth",
                        PlatformErrorCategory::Auth,
                        "请更新或重新授权凭据",
                    ),
                    403 => (
                        "platform.permission",
                        PlatformErrorCategory::Permission,
                        "请授予所需最小权限后重试",
                    ),
                    409 => (
                        "platform.conflict",
                        PlatformErrorCategory::Conflict,
                        "请检查目标资源状态和冲突策略",
                    ),
                    429 => (
                        "platform.rate_limited",
                        PlatformErrorCategory::RateLimited,
                        "请等待限流窗口结束后重试",
                    ),
                    _ => (
                        "platform.http",
                        PlatformErrorCategory::Network,
                        "请稍后重试",
                    ),
                };
                Self {
                    code: code.into(),
                    category,
                    retryable: status == 408 || status == 429 || status >= 500,
                    safe_message: message,
                    action: action.into(),
                    retry_after_seconds: retry_after.map(|duration| duration.as_secs()),
                }
            }
        }
    }
}

#[async_trait]
pub trait PlatformAdapter: Send + Sync {
    fn identify(&self, endpoint: &Endpoint) -> Result<PlatformIdentity, PlatformError>;

    async fn test_connection(
        &self,
        ctx: &AdapterContext<'_>,
    ) -> Result<ConnectionInfo, PlatformError>;

    async fn capabilities(
        &self,
        ctx: &AdapterContext<'_>,
    ) -> Result<CapabilityMatrix, PlatformError>;

    async fn discover_repositories(
        &self,
        ctx: &AdapterContext<'_>,
        query: DiscoveryQuery,
    ) -> Result<Page<RepositoryCandidate>, PlatformError>;

    async fn inspect_repository(
        &self,
        ctx: &AdapterContext<'_>,
        locator: &RepositoryLocator,
    ) -> Result<RemoteRepositoryState, PlatformError>;

    async fn create_repository(
        &self,
        ctx: &AdapterContext<'_>,
        spec: CreateRepositorySpec,
    ) -> Result<RemoteRepository, PlatformError>;

    async fn apply_metadata(
        &self,
        ctx: &AdapterContext<'_>,
        target: &RemoteRepository,
        metadata: MetadataPatch,
    ) -> Result<ModuleResult, PlatformError>;

    /// The best fidelity this adapter can deliver for `module`, independent of
    /// any particular instance. `ReadOnlyArchive` needs only a readable source;
    /// `NativeRebuild` additionally requires the target to sit on an instance of
    /// the same platform kind.
    fn module_fidelity(&self, module: PlatformModule) -> Fidelity {
        let _ = module;
        Fidelity::Unsupported
    }

    /// Reads `module`'s items from `source` via `ctx`, and — when the fidelity
    /// allows it — creates them on `target` via `target_ctx`, an instance of the
    /// same platform kind. An adapter that can only archive ignores `target_ctx`
    /// and returns a `ReadOnlyArchive` result carrying the archive document.
    async fn migrate_module(
        &self,
        ctx: &AdapterContext<'_>,
        target_ctx: &AdapterContext<'_>,
        module: PlatformModule,
        source: &RemoteRepository,
        target: &RemoteRepository,
    ) -> Result<ModuleResult, PlatformError>;

    async fn verify_module(
        &self,
        ctx: &AdapterContext<'_>,
        module: PlatformModule,
        source: &RemoteRepository,
        target: &RemoteRepository,
    ) -> Result<VerificationResult, PlatformError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{HttpTransportConfig, RateLimitInfo, TlsPolicy, TransportError};
    use std::time::Duration;

    #[test]
    fn capability_matrix_serializes_every_contract_field() {
        let capability = Capability::native(["repo:read"]);
        let matrix = CapabilityMatrix {
            schema_version: 1,
            platform: PlatformKind::Github,
            instance_version: Some("enterprise".into()),
            captured_at_epoch_seconds: 42,
            discovery: capability.clone(),
            repository_inspection: capability.clone(),
            repository_creation: capability.clone(),
            git_read: capability.clone(),
            git_write: capability.clone(),
            lfs: capability.clone(),
            metadata: capability.clone(),
            issues: capability.clone(),
            pull_requests: capability.clone(),
            merge_requests: Capability::unsupported("GitHub 使用 Pull Request"),
            wiki: capability.clone(),
            releases: capability.clone(),
            release_assets: capability,
        };
        let value = serde_json::to_value(matrix).unwrap();
        for field in [
            "schema_version",
            "platform",
            "instance_version",
            "captured_at_epoch_seconds",
            "discovery",
            "repository_inspection",
            "repository_creation",
            "git_read",
            "git_write",
            "lfs",
            "metadata",
            "issues",
            "pull_requests",
            "merge_requests",
            "wiki",
            "releases",
            "release_assets",
        ] {
            assert!(
                value.get(field).is_some(),
                "missing capability field: {field}"
            );
        }
    }

    #[test]
    fn cursor_round_trips_as_an_opaque_string() {
        let cursor = PaginationCursor::new("next:page/2==").unwrap();
        let encoded = serde_json::to_string(&cursor).unwrap();
        let decoded: PaginationCursor = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, cursor);
        assert_eq!(decoded.as_str(), "next:page/2==");
    }

    #[test]
    fn tls_defaults_to_system_validation_and_rejects_invalid_pin() {
        let config = HttpTransportConfig::default();
        assert_eq!(config.tls, TlsPolicy::System);
        assert!(config.validate().is_ok());

        let invalid = HttpTransportConfig {
            tls: TlsPolicy::PinnedFingerprint {
                sha256: "not-a-fingerprint".into(),
            },
            ..HttpTransportConfig::default()
        };
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn retry_after_is_preserved_for_rate_limited_errors() {
        let info = RateLimitInfo::from_headers(&[("Retry-After".into(), "17".into())]);
        assert_eq!(info.retry_after, Some(Duration::from_secs(17)));

        let error = PlatformError::from(TransportError::Http {
            status: 429,
            message: "请求过于频繁".into(),
            retry_after: info.retry_after,
        });
        assert_eq!(error.category, PlatformErrorCategory::RateLimited);
        assert!(error.retryable);
        assert_eq!(error.retry_after_seconds, Some(17));
    }

    #[test]
    fn http_auth_permission_and_conflict_are_classified() {
        for (status, category) in [
            (401, PlatformErrorCategory::Auth),
            (403, PlatformErrorCategory::Permission),
            (409, PlatformErrorCategory::Conflict),
        ] {
            let error = PlatformError::from(TransportError::Http {
                status,
                message: "safe".into(),
                retry_after: None,
            });
            assert_eq!(error.category, category);
            assert!(!error.retryable);
        }
    }

    #[test]
    fn credential_reference_debug_output_is_redacted() {
        let reference = CredentialRef::new("credential/windows/secret-id").unwrap();
        let output = format!("{reference:?}");
        assert!(!output.contains(reference.as_str()));
        assert!(output.contains("REDACTED"));
    }
}
