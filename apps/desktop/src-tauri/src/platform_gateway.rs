//! The platform gateways: the executor's `TargetGateway` and `ModuleGateway`
//! implemented over the real platform adapters and HTTP transport.
//!
//! R-5 and R-6 close here. `ApiTargetGateway` probes and creates the target
//! repository through the target connection's adapter; `ApiModuleGateway` runs
//! the selected platform modules (Issues/PR/…) through the source adapter, with
//! the target adapter's context for native rebuilds. Generic Git keeps the
//! refusal paths: it has no API, and inventing one would be worse than an
//! honest `unsupported`.

use std::sync::Arc;

use git_repo_migrator_application::executor::{
    ModuleGateway, ModuleReport, TargetGateway, TaskAssignment,
};
use git_repo_migrator_application::planning::TargetState;
use git_repo_migrator_application::IpcError;
use git_repo_migrator_credential_store::CredentialStore;
use git_repo_migrator_domain::Fidelity;
use git_repo_migrator_http_transport::ReqwestTransport;
use git_repo_migrator_platform_core::transport::HttpTransportConfig;
use git_repo_migrator_platform_core::{
    AdapterContext, CreateRepositorySpec, CredentialRef, Endpoint, PlatformAdapter, PlatformError,
    PlatformKind, PlatformModule, RemoteRepository, RemoteRepositoryState, RepositoryLocator,
    RepositoryVisibility,
};

use crate::dto::ConnectionSnapshot;
use crate::errors;

/// Selects the adapter for a platform. Mirrors `discovery.rs`: Generic Git has
/// none on purpose.
fn adapter_for(kind: PlatformKind) -> Option<Box<dyn PlatformAdapter>> {
    match kind {
        PlatformKind::Github => Some(Box::new(
            git_repo_migrator_platform_github::GithubAdapter::new(),
        )),
        PlatformKind::Gitlab => Some(Box::new(git_repo_migrator_platform_gitlab::GitlabAdapter)),
        PlatformKind::Gitea | PlatformKind::Forgejo => {
            Some(Box::new(git_repo_migrator_platform_gitea::GiteaAdapter))
        }
        PlatformKind::Gitee => Some(Box::new(git_repo_migrator_platform_gitee::GiteeAdapter)),
        PlatformKind::GenericGit | PlatformKind::Unknown => None,
    }
}

/// What the gateways need to talk to one platform: the connection row plus the
/// credential store the transport resolves references against.
#[derive(Clone)]
pub struct PlatformSession {
    credentials: Arc<CredentialStore>,
    config: HttpTransportConfig,
    platform: PlatformKind,
    endpoint_url: String,
    credential_ref: Option<CredentialRef>,
}

impl PlatformSession {
    /// Builds a session from a persisted connection row. The reference is
    /// parsed, never resolved: the token stays inside the transport.
    pub fn from_connection(
        connection: &ConnectionSnapshot,
        credentials: Arc<CredentialStore>,
    ) -> Result<Self, IpcError> {
        let credential_ref = connection
            .credential_ref
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(CredentialRef::new)
            .transpose()
            .map_err(|error| errors::from_platform("platform_data", &error))?;
        Ok(Self {
            credentials,
            config: HttpTransportConfig::default(),
            platform: connection.platform,
            endpoint_url: connection.endpoint.clone(),
            credential_ref,
        })
    }

    /// A session for a target that has no connection row: Generic Git, or a
    /// platform the operator has not connected yet. It can still be *read*
    /// from; adapters see `GenericGit` and degrade to read-only archiving.
    pub fn generic(endpoint_url: impl Into<String>, credentials: Arc<CredentialStore>) -> Self {
        Self {
            credentials,
            config: HttpTransportConfig::default(),
            platform: PlatformKind::GenericGit,
            endpoint_url: endpoint_url.into(),
            credential_ref: None,
        }
    }

    pub fn platform(&self) -> PlatformKind {
        self.platform
    }

    pub fn has_adapter(&self) -> bool {
        adapter_for(self.platform).is_some()
    }

    /// The credential store this session resolves references against. Used to
    /// build the derived generic session for a target that has no connection.
    pub fn credentials(&self) -> Arc<CredentialStore> {
        Arc::clone(&self.credentials)
    }

    /// Runs `f` with a live adapter and context. The closure is synchronous;
    /// it bridges the adapter's async methods with `block_on` itself, the same
    /// way `discovery.rs` does, so the sync executor ports stay sync.
    fn with_adapter<T>(
        &self,
        stage: &str,
        f: impl FnOnce(&(dyn PlatformAdapter + '_), &AdapterContext<'_>) -> Result<T, PlatformError>,
    ) -> Result<T, IpcError> {
        let adapter = adapter_for(self.platform).ok_or_else(|| {
            errors::unsupported(
                stage,
                "通用 Git 服务没有平台 API",
                "请先在目标服务手动建库；平台数据模块对该服务不可用",
            )
        })?;
        let transport = ReqwestTransport::new(
            self.config.clone(),
            self.platform,
            Arc::clone(&self.credentials),
        )
        .map_err(|error| {
            errors::error(
                "transport.config",
                git_repo_migrator_domain::ErrorCategory::Validation,
                false,
                stage,
                format!("传输层配置无效：{error}"),
                "请检查代理地址与证书指纹设置后重试",
            )
        })?;
        let endpoint = Endpoint {
            base_url: self.endpoint_url.clone(),
            platform_hint: Some(self.platform),
        };
        let context = AdapterContext {
            connection_id: "migration",
            endpoint: &endpoint,
            credential_ref: self.credential_ref.as_ref(),
            transport: &transport,
        };
        f(adapter.as_ref(), &context).map_err(|error| errors::from_platform(stage, &error))
    }
}

/// Target facts and creation through the target platform's API.
pub struct ApiTargetGateway {
    session: PlatformSession,
}

impl ApiTargetGateway {
    pub fn new(session: PlatformSession) -> Self {
        Self { session }
    }
}

impl TargetGateway for ApiTargetGateway {
    fn probe(&self, target_url: &str) -> Result<TargetState, IpcError> {
        let locator = locator_from_url(target_url);
        self.session.with_adapter("prepare_target", |adapter, ctx| {
            tauri::async_runtime::block_on(adapter.inspect_repository(ctx, &locator)).map(
                |RemoteRepositoryState { exists, empty, .. }| {
                    if !exists {
                        TargetState::Missing
                    } else if empty.unwrap_or(false) {
                        TargetState::Empty
                    } else {
                        TargetState::NonEmpty
                    }
                },
            )
        })
    }

    fn create(&self, assignment: &TaskAssignment) -> Result<(), IpcError> {
        let locator = locator_from_url(&assignment.target_url);
        let (owner, name) = split_full_name(&locator.full_name);
        let spec = CreateRepositorySpec {
            owner,
            name,
            description: None,
            // A fresh migration target starts private; visibility is the
            // operator's deliberate decision, not a clone default.
            visibility: RepositoryVisibility::Private,
            homepage: None,
            // Never initialise: an initialised repository is non-empty and would
            // trip the same overwrite guard a foreign push would.
            initialize: false,
            idempotency_key: assignment.task_id.clone(),
        };
        self.session
            .with_adapter("prepare_target", |adapter, ctx| {
                tauri::async_runtime::block_on(adapter.create_repository(ctx, spec))
            })
            .map(|_| ())
    }
}

/// Platform modules through the source adapter, with the target session for
/// native rebuilds.
pub struct ApiModuleGateway {
    source: PlatformSession,
    target: PlatformSession,
}

impl ApiModuleGateway {
    pub fn new(source: PlatformSession, target: PlatformSession) -> Self {
        Self { source, target }
    }
}

impl ModuleGateway for ApiModuleGateway {
    fn run(&self, assignment: &TaskAssignment, module: &str) -> Result<ModuleReport, IpcError> {
        let module = parse_module(module);
        let source_repo = repo_stub(&assignment.source_url);
        let target_repo = repo_stub(&assignment.target_url);
        // `metadata` is not a platform-module adapter call: it is the source's
        // own description/visibility carried to the target. Every other module
        // goes through `migrate_module` with both contexts.
        if module == PlatformModule::Metadata {
            return self.run_metadata(&source_repo, &target_repo);
        }

        let result = self.source.with_adapter("platform_data", |adapter, ctx| {
            // The target context must come from the target session; the adapter
            // decides via `same_family` whether it may create items there.
            let transport = ReqwestTransport::new(
                self.target.config.clone(),
                self.target.platform,
                Arc::clone(&self.target.credentials),
            )
            .map_err(PlatformError::from)?;
            let endpoint = Endpoint {
                base_url: self.target.endpoint_url.clone(),
                platform_hint: Some(self.target.platform),
            };
            let target_ctx = AdapterContext {
                connection_id: "migration-target",
                endpoint: &endpoint,
                credential_ref: self.target.credential_ref.as_ref(),
                transport: &transport,
            };
            tauri::async_runtime::block_on(adapter.migrate_module(
                ctx,
                &target_ctx,
                module,
                &source_repo,
                &target_repo,
            ))
        })?;

        Ok(ModuleReport {
            module: module_text(module).to_owned(),
            fidelity: result.fidelity.to_domain(),
            source_count: result.discovered,
            target_count: result.migrated,
            source_links: result.source_links,
            error: None,
            archive: result.archive,
            unmapped_fields: result.unmapped_fields,
        })
    }
}

impl ApiModuleGateway {
    /// Metadata: read the source repository's own attributes and apply them to
    /// the target through the *target* adapter (the write side).
    ///
    /// A failure applying metadata must not fail the Git data that already
    /// moved: the module degrades to `Unsupported` with the unmapped fields
    /// named, and the task continues.
    fn run_metadata(
        &self,
        source_repo: &RemoteRepository,
        target_repo: &RemoteRepository,
    ) -> Result<ModuleReport, IpcError> {
        let state = self.source.with_adapter("metadata", |adapter, ctx| {
            tauri::async_runtime::block_on(adapter.inspect_repository(ctx, &source_repo.locator))
        })?;
        let patch = git_repo_migrator_platform_core::MetadataPatch {
            description: state.description,
            homepage: None,
            topics: None,
            default_branch: state.default_branch,
            archived: None,
        };
        let applied = self.target.with_adapter("metadata", |adapter, ctx| {
            tauri::async_runtime::block_on(adapter.apply_metadata(ctx, target_repo, patch))
        });
        match applied {
            Ok(result) => Ok(ModuleReport {
                module: "metadata".to_owned(),
                fidelity: result.fidelity.to_domain(),
                source_count: 0,
                target_count: 0,
                source_links: Vec::new(),
                error: None,
                archive: None,
                unmapped_fields: Vec::new(),
            }),
            Err(error) => Ok(ModuleReport {
                module: "metadata".to_owned(),
                fidelity: Fidelity::Unsupported,
                source_count: 0,
                target_count: 0,
                source_links: Vec::new(),
                error: Some(error),
                archive: None,
                unmapped_fields: vec!["description".to_owned(), "default_branch".to_owned()],
            }),
        }
    }
}

fn parse_module(module: &str) -> PlatformModule {
    match module {
        "metadata" => PlatformModule::Metadata,
        "issues" => PlatformModule::Issues,
        "pull_requests" => PlatformModule::PullRequests,
        "merge_requests" => PlatformModule::MergeRequests,
        "wiki" => PlatformModule::Wiki,
        "releases" => PlatformModule::Releases,
        _ => PlatformModule::ReleaseAssets,
    }
}

fn module_text(module: PlatformModule) -> &'static str {
    match module {
        PlatformModule::Metadata => "metadata",
        PlatformModule::Issues => "issues",
        PlatformModule::PullRequests => "pull_requests",
        PlatformModule::MergeRequests => "merge_requests",
        PlatformModule::Wiki => "wiki",
        PlatformModule::Releases => "releases",
        PlatformModule::ReleaseAssets => "release_assets",
    }
}

/// Derives a locator from a repository URL: the owner/name pair is the path.
fn locator_from_url(url: &str) -> RepositoryLocator {
    let full_name = url
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit("://")
        .next()
        .unwrap_or(url)
        .split('/')
        .skip(1)
        .filter(|segment| !segment.is_empty() && !segment.contains('.'))
        .collect::<Vec<_>>()
        .join("/");
    RepositoryLocator {
        platform_id: None,
        full_name,
        clone_url: Some(url.to_owned()),
    }
}

fn split_full_name(full_name: &str) -> (String, String) {
    match full_name.rsplit_once('/') {
        Some((owner, name)) => (owner.to_owned(), name.to_owned()),
        None => (String::new(), full_name.to_owned()),
    }
}

fn repo_stub(url: &str) -> RemoteRepository {
    RemoteRepository {
        locator: locator_from_url(url),
        name: String::new(),
        web_url: None,
        clone_url_https: Some(url.to_owned()),
        clone_url_ssh: None,
        visibility: RepositoryVisibility::Unknown,
        default_branch: None,
    }
}
