//! API discovery over the real HTTP transport.
//!
//! This is where the Wave 4 adapters, the Wave 5 command surface and the
//! transport meet. The renderer still only ever sends a connection id: the
//! endpoint, the platform kind and the credential *reference* come from SQLite,
//! and the token itself is resolved inside the transport.

use std::sync::Arc;

use git_repo_migrator_application::IpcError;
use git_repo_migrator_credential_store::CredentialStore;
use git_repo_migrator_http_transport::ReqwestTransport;
use git_repo_migrator_platform_core::transport::{HttpTransportConfig, TlsPolicy};
use git_repo_migrator_platform_core::{
    AdapterContext, CredentialRef, DiscoveryQuery, Endpoint, PlatformAdapter, PlatformKind,
    RepositoryCandidate,
};
use git_repo_migrator_platform_gitea::GiteaAdapter;
use git_repo_migrator_platform_gitee::GiteeAdapter;
use git_repo_migrator_platform_github::GithubAdapter;
use git_repo_migrator_platform_gitlab::GitlabAdapter;

use crate::errors;
use crate::ports::DiscoveryGateway;

/// Selects the adapter for a platform. Generic Git deliberately has none: it has
/// no API, and pretending otherwise would produce an empty result set that looks
/// like "this account owns no repositories".
fn adapter_for(kind: PlatformKind) -> Option<Box<dyn PlatformAdapter>> {
    match kind {
        PlatformKind::Github => Some(Box::new(GithubAdapter::new())),
        PlatformKind::Gitlab => Some(Box::new(GitlabAdapter)),
        PlatformKind::Gitea | PlatformKind::Forgejo => Some(Box::new(GiteaAdapter)),
        PlatformKind::Gitee => Some(Box::new(GiteeAdapter)),
        PlatformKind::GenericGit | PlatformKind::Unknown => None,
    }
}

pub struct ApiDiscoveryGateway {
    credentials: Arc<CredentialStore>,
    config: HttpTransportConfig,
}

impl ApiDiscoveryGateway {
    pub fn new(credentials: Arc<CredentialStore>) -> Self {
        Self {
            credentials,
            config: HttpTransportConfig::default(),
        }
    }

    /// Pins a self-signed instance's certificate. Everything else still goes
    /// through the operating system trust store.
    pub fn with_pinned_certificate(mut self, sha256: impl Into<String>) -> Self {
        self.config.tls = TlsPolicy::PinnedFingerprint {
            sha256: sha256.into(),
        };
        self
    }
}

impl DiscoveryGateway for ApiDiscoveryGateway {
    fn discover(
        &self,
        endpoint: &str,
        platform: PlatformKind,
        credential_ref: Option<&str>,
        query: &DiscoveryQuery,
    ) -> Result<Vec<RepositoryCandidate>, IpcError> {
        let Some(adapter) = adapter_for(platform) else {
            return Err(errors::unsupported(
                "discovery",
                "通用 Git 服务没有仓库发现 API",
                "请使用「手动 URL 导入」逐条或批量粘贴仓库地址",
            ));
        };
        let transport =
            ReqwestTransport::new(self.config.clone(), platform, Arc::clone(&self.credentials))
                .map_err(|error| {
                    errors::error(
                        "transport.config",
                        git_repo_migrator_domain::ErrorCategory::Validation,
                        false,
                        "discovery",
                        format!("传输层配置无效：{error}"),
                        "请检查代理地址与证书指纹设置后重试",
                    )
                })?;

        let credential = credential_ref
            .filter(|value| !value.is_empty())
            .map(CredentialRef::new)
            .transpose()
            .map_err(|error| errors::from_platform("discovery", &error))?;
        let endpoint = Endpoint {
            base_url: endpoint.to_owned(),
            platform_hint: Some(platform),
        };
        let context = AdapterContext {
            connection_id: "discovery",
            endpoint: &endpoint,
            credential_ref: credential.as_ref(),
            transport: &transport,
        };

        // The command surface is synchronous; the adapters are async. Blocking
        // here keeps the async boundary inside this module instead of leaking a
        // runtime requirement into every caller.
        let page =
            tauri::async_runtime::block_on(adapter.discover_repositories(&context, query.clone()))
                .map_err(|error| errors::from_platform("discovery", &error))?;
        Ok(page.items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_api_platform_has_an_adapter_and_generic_git_has_none() {
        for kind in [
            PlatformKind::Github,
            PlatformKind::Gitlab,
            PlatformKind::Gitea,
            PlatformKind::Forgejo,
            PlatformKind::Gitee,
        ] {
            assert!(adapter_for(kind).is_some(), "{kind:?} needs an adapter");
        }
        for kind in [PlatformKind::GenericGit, PlatformKind::Unknown] {
            assert!(adapter_for(kind).is_none());
        }
    }

    #[test]
    fn generic_git_discovery_points_the_operator_at_manual_import() {
        let gateway = ApiDiscoveryGateway::new(Arc::new(CredentialStore::in_memory()));
        let error = gateway
            .discover(
                "https://git.internal.test",
                PlatformKind::GenericGit,
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
            .expect_err("generic git has no discovery API");
        assert_eq!(
            error.category,
            git_repo_migrator_domain::ErrorCategory::Unsupported
        );
        assert!(error.action.contains("手动 URL 导入"));
    }

    #[test]
    fn an_invalid_pin_is_rejected_before_any_request() {
        let gateway = ApiDiscoveryGateway::new(Arc::new(CredentialStore::in_memory()))
            .with_pinned_certificate("not-a-fingerprint");
        let error = gateway
            .discover(
                "https://gitlab.internal.test",
                PlatformKind::Gitlab,
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
            .expect_err("a malformed pin must not fall back to no pinning");
        assert_eq!(error.code, "transport.config");
        assert!(!error.retryable);
    }
}
