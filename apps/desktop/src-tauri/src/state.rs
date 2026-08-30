//! Application state behind the IPC boundary.
//!
//! SQLite is the single authoritative store. Every mutation writes rows first
//! and only then bumps `revision`; the renderer re-reads a full snapshot rather
//! than folding events, so a lost event degrades to a slightly stale UI, never
//! to a wrong one.
//!
//! Stage-recording helpers (`begin_stage`, `report_progress`, `fail_stage`,
//! `complete_task`, `record_module_result`) are intentionally *not* exposed as
//! Tauri commands: only the backend may claim that work happened.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use git_repo_migrator_application::executor::{ExecutionAction, TaskAssignment, WorkspacePolicy};
use git_repo_migrator_application::ipc_contract::{
    BatchStartInput, ConnectionAuthorizeInput, ConnectionTestInput, ReportExportInput,
    TaskRetryInput,
};
use git_repo_migrator_application::planning::{
    build_preview, Candidate, SelectionSet, TargetState,
};
use git_repo_migrator_application::report::{ExportFormat, Report, ReportRow};
use git_repo_migrator_application::verification::AggregateStatus;
use git_repo_migrator_application::{BatchControl, IpcError};
use git_repo_migrator_domain::{
    ConflictPolicy, ErrorCategory, Fidelity, MigrationPlan, ModuleSelection, RefPolicy,
    RepoTaskState, RepositoryMapping,
};
use git_repo_migrator_local_store::{AppendCheckpoint, LocalStore, StoreResult};
use git_repo_migrator_platform_core::{DiscoveryQuery, PlatformKind, RepositoryVisibility};
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};
use url::Url;

use crate::dto::{
    BatchIdInput, BatchSnapshot, CapabilitySummary, CleanupState, ConnectionRole,
    ConnectionSaveInput, ConnectionSnapshot, FieldMappingRow, MigrationSnapshot, MigrationStage,
    ModuleFidelityRow, PermissionLevel, PlanAction, PlanFreezeInput, PlanPreviewRequest,
    PlanPreviewSnapshot, PlanSnapshot, PreflightMetrics, PreflightRow, RefPolicySummary,
    ReportSnapshot, RepositoryImportInput, RepositoryImportIssue, RepositoryImportReport,
    RepositoryPage, RepositorySnapshot, TargetProbeInput, SNAPSHOT_SCHEMA_VERSION,
};
use crate::errors;
use git_repo_migrator_credential_store::prompt::{reference_for, validate_name};

use crate::ports::{
    BatchLauncher, Clock, CompanionProcessLauncher, ConnectionTester, DiscoveryGateway, ExportSink,
    FileExportSink, IdentityEntryLauncher, SystemClock, TargetProbe, TransportNotWired,
};
use crate::snapshot::{self, CandidateDetails, ConnectionDetails, PlanSelection, VerifySummary};

/// Lease time-to-live. A crashed stage becomes recoverable after this window.
const LEASE_TTL_MS: i64 = 60_000;
const MAX_CONCURRENCY: u16 = 8;
/// Modules the operator may choose beyond the always-on Git history.
const OPTIONAL_MODULES: [&str; 6] = [
    "lfs",
    "metadata",
    "issues",
    "pull_requests",
    "wiki",
    "releases",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreviewRecord {
    snapshot: PlanPreviewSnapshot,
    mappings: Vec<RepositoryMapping>,
    repository_ids: Vec<String>,
    actions: BTreeMap<String, String>,
    modules: ModuleSelection,
    policy: ConflictPolicy,
    ref_policy: RefPolicy,
    selected: Vec<String>,
    excluded: Vec<String>,
}

/// Outcome of a retry request. Rejections are returned explicitly so the UI can
/// explain why a row was not retried instead of silently ignoring it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RetryOutcome {
    pub retried: Vec<String>,
    pub rejected: Vec<RetryRejection>,
    pub batch: BatchSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RetryRejection {
    pub task_id: String,
    pub reason: String,
}

/// Result of opening the credential-entry window. Carries a reference and an
/// instruction, never a secret.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AuthorizeOutcome {
    pub credential_ref: String,
    pub instructions: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExportOutcome {
    pub path: String,
    pub bytes_written: u64,
    pub row_count: u32,
}

/// One platform module's outcome for a repository, as recorded by the backend.
#[derive(Debug, Clone, Copy)]
pub struct ModuleOutcome<'a> {
    pub module: &'a str,
    pub fidelity: Fidelity,
    pub source_count: u64,
    pub target_count: u64,
    pub error: Option<&'a IpcError>,
    pub source_links: &'a [String],
}

struct Inner {
    store: LocalStore,
    revision: u64,
    seq: u64,
    previews: BTreeMap<String, PreviewRecord>,
    /// Per-batch worker count. This is a UI preference rather than migration
    /// state, so it is deliberately not persisted; a restored batch falls back
    /// to the safe default of 1.
    concurrency: BTreeMap<String, u16>,
    cleanup: CleanupState,
}

pub struct AppState {
    inner: Mutex<Inner>,
    clock: Arc<dyn Clock>,
    probe: Option<Arc<dyn TargetProbe>>,
    discovery: Arc<dyn DiscoveryGateway>,
    /// Real network probe for `connection_test`. Absent in tests, which keep the
    /// static table so no test ever depends on a live platform.
    connection_tester: Option<Arc<dyn crate::ports::ConnectionTester>>,
    export: Arc<dyn ExportSink>,
    identity_entry: Arc<dyn IdentityEntryLauncher>,
    /// Installed after construction, because the worker pool needs a handle to
    /// the state it reports into. Absent in tests, which drive the stage
    /// recording API directly.
    launcher: Mutex<Option<Arc<dyn BatchLauncher>>>,
}

impl AppState {
    pub fn open(path: impl AsRef<Path>) -> StoreResult<Self> {
        Ok(Self::with_store(LocalStore::open(path)?))
    }

    pub fn in_memory() -> StoreResult<Self> {
        Ok(Self::with_store(LocalStore::open_in_memory()?))
    }

    fn with_store(store: LocalStore) -> Self {
        Self {
            inner: Mutex::new(Inner {
                store,
                revision: 0,
                seq: 0,
                previews: BTreeMap::new(),
                concurrency: BTreeMap::new(),
                cleanup: CleanupState::Cleaned,
            }),
            clock: Arc::new(SystemClock),
            probe: None,
            discovery: Arc::new(TransportNotWired),
            connection_tester: None,
            export: Arc::new(FileExportSink),
            identity_entry: Arc::new(CompanionProcessLauncher),
            launcher: Mutex::new(None),
        }
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    pub fn with_target_probe(mut self, probe: Arc<dyn TargetProbe>) -> Self {
        self.probe = Some(probe);
        self
    }

    pub fn with_discovery(mut self, discovery: Arc<dyn DiscoveryGateway>) -> Self {
        self.discovery = discovery;
        self
    }

    pub fn with_connection_tester(mut self, tester: Arc<dyn ConnectionTester>) -> Self {
        self.connection_tester = Some(tester);
        self
    }

    pub fn with_export_sink(mut self, export: Arc<dyn ExportSink>) -> Self {
        self.export = export;
        self
    }

    /// Installs the worker pool. Takes `&self` because the pool needs an
    /// `Arc<AppState>`, which only exists once the state is constructed.
    pub fn install_launcher(&self, launcher: Arc<dyn BatchLauncher>) {
        match self.launcher.lock() {
            Ok(mut slot) => *slot = Some(launcher),
            Err(poisoned) => *poisoned.into_inner() = Some(launcher),
        }
    }

    pub fn with_identity_entry(mut self, entry: Arc<dyn IdentityEntryLauncher>) -> Self {
        self.identity_entry = entry;
        self
    }

    /// Opens the native credential-entry window and returns the reference the
    /// operator will get, so the connection form can be prefilled.
    ///
    /// No secret crosses this call in either direction.
    pub fn authorize_connection(
        &self,
        input: &ConnectionAuthorizeInput,
    ) -> Result<AuthorizeOutcome, IpcError> {
        let name = validate_name(&input.name)
            .map_err(|error| errors::from_platform("connection", &error))?;
        let reference =
            reference_for(name).map_err(|error| errors::from_platform("connection", &error))?;
        self.identity_entry.launch(name)?;
        Ok(AuthorizeOutcome {
            credential_ref: reference.as_str().to_owned(),
            instructions: format!(
                "已打开凭据录入窗口。请在该窗口中粘贴令牌两次；界面不会收到令牌本身。完成后凭据引用为 {}",
                reference.as_str()
            ),
        })
    }

    fn launcher(&self) -> Option<Arc<dyn BatchLauncher>> {
        match self.launcher.lock() {
            Ok(slot) => slot.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A poisoned lock means a previous command panicked. Recovering the
        // guard is safe here because every mutation is a completed SQLite
        // transaction; the in-memory caches are rebuildable.
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub fn revision(&self) -> u64 {
        self.lock().revision
    }

    /// Escape hatch used only by the contract tests to simulate an out-of-band
    /// edit of a persisted row.
    #[cfg(test)]
    pub fn with_connection_for_test<T>(
        &self,
        action: impl FnOnce(&rusqlite::Connection) -> Result<T, rusqlite::Error>,
    ) -> Result<T, rusqlite::Error> {
        let inner = self.lock();
        action(inner.store.connection())
    }

    // -- connections --------------------------------------------------------

    pub fn test_connection(
        &self,
        input: &ConnectionTestInput,
    ) -> Result<Vec<CapabilitySummary>, IpcError> {
        let platform = input.platform_hint.unwrap_or(PlatformKind::Unknown);
        validate_endpoint(&input.endpoint)?;
        validate_credential_ref(input.credential_ref.as_deref())?;
        // With a wired tester the platform itself answers: a wrong token
        // surfaces as an auth error instead of an optimistic capability table.
        if let Some(tester) = &self.connection_tester {
            let probe = tester.test(&input.endpoint, platform, input.credential_ref.as_deref())?;
            return Ok(probe
                .capabilities
                .into_iter()
                .map(|capability| CapabilitySummary {
                    module: capability.module.to_owned(),
                    supported: capability.supported,
                    permitted: capability.permitted,
                    required_scopes: capability.required_scopes,
                    fidelity: capability.fidelity,
                    reason: capability.reason,
                    degradation: capability.degradation,
                })
                .collect());
        }
        Ok(capabilities_for(platform))
    }

    pub fn save_connection(
        &self,
        input: &ConnectionSaveInput,
    ) -> Result<ConnectionSnapshot, IpcError> {
        let endpoint = validate_endpoint(&input.endpoint)?;
        validate_credential_ref(input.credential_ref.as_deref())?;
        if let Some(fingerprint) = input.trust_fingerprint_sha256.as_deref() {
            validate_fingerprint(fingerprint)?;
        }
        let platform = input.platform_hint.unwrap_or(PlatformKind::Unknown);
        let id = match input.role {
            ConnectionRole::Source => "source",
            ConnectionRole::Target => "target",
        };
        let details = ConnectionDetails {
            authenticated: input.credential_ref.is_some(),
            account_name: None,
            instance_version: None,
            // Acknowledging a self-signed certificate pins its fingerprint; it
            // never disables validation.
            tls_trusted: endpoint.starts_with("https://")
                || input.trust_fingerprint_sha256.is_some(),
            capabilities: capabilities_for(platform),
        };
        let details_json = serde_json::to_string(&details).unwrap_or_else(|_| "{}".to_owned());
        let platform_value = platform_value(platform);
        let credential_ref = input.credential_ref.clone().unwrap_or_default();

        let mut inner = self.lock();
        let now = self.clock.now_ms();
        inner
            .store
            .connection()
            .execute(
                "INSERT INTO connection (id, platform_type, endpoint, credential_ref, capabilities_json, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET
                    platform_type = excluded.platform_type,
                    endpoint = excluded.endpoint,
                    credential_ref = excluded.credential_ref,
                    capabilities_json = excluded.capabilities_json",
                params![id, platform_value, endpoint, credential_ref, details_json, now],
            )
            .map_err(|error| errors::store("connection", &error.into()))?;
        inner.revision += 1;

        snapshot::read_connections(&inner.store)
            .map_err(|error| errors::store("connection", &error))?
            .into_iter()
            .find(|connection| connection.id == id)
            .ok_or_else(|| errors::not_found("connection", "刚保存的连接"))
    }

    // -- repositories -------------------------------------------------------

    pub fn discover_repositories(
        &self,
        connection_id: &str,
        query: &DiscoveryQuery,
    ) -> Result<RepositoryPage, IpcError> {
        if query.page_size == 0 || query.page_size > 200 {
            return Err(errors::validation(
                "discovery",
                "分页大小必须在 1 到 200 之间",
                "请调整每页数量后重试",
            ));
        }
        let connection = self
            .connections()?
            .into_iter()
            .find(|connection| connection.id == connection_id)
            .ok_or_else(|| errors::not_found("discovery", "源连接"))?;

        let candidates = self.discovery.discover(
            &connection.endpoint,
            connection.platform,
            connection.credential_ref.as_deref(),
            query,
        )?;

        let mut inner = self.lock();
        let mut warnings = Vec::new();
        for candidate in &candidates {
            let permission = if candidate.permissions.administer {
                PermissionLevel::FullMigration
            } else if candidate.permissions.read {
                PermissionLevel::GitOnly
            } else {
                PermissionLevel::Insufficient
            };
            let source_url = candidate
                .clone_url_https
                .clone()
                .or_else(|| candidate.clone_url_ssh.clone())
                .unwrap_or_else(|| candidate.locator.full_name.clone());
            let details = CandidateDetails {
                git_capable: candidate.permissions.read,
                platform_capable: candidate.permissions.administer,
                updated_at_epoch_seconds: candidate.updated_at_epoch_seconds,
                ..CandidateDetails::default()
            };
            if let Err(error) = upsert_candidate(
                &inner.store,
                &candidate.locator.full_name,
                Some(connection_id),
                &source_url,
                &candidate.name,
                &candidate.owner,
                candidate.visibility,
                permission,
                &details,
            ) {
                warnings.push(format!("保留已加载结果；写入候选失败：{error}"));
            }
        }
        inner.revision += 1;
        let loaded = u64::try_from(candidates.len()).unwrap_or(0);
        let items = snapshot::read_repositories(&inner.store)
            .map_err(|error| errors::store("discovery", &error))?
            .into_iter()
            .filter(|item| item.connection_id == connection_id)
            .collect::<Vec<_>>();

        Ok(RepositoryPage {
            next_cursor: None,
            total_count: None,
            loaded,
            warnings,
            items,
        })
    }

    /// Manual URL import. This is the no-API fallback and works entirely
    /// offline, so it stays available when discovery is impossible.
    pub fn import_repositories(
        &self,
        input: &RepositoryImportInput,
    ) -> Result<RepositoryImportReport, IpcError> {
        if input.urls.trim().is_empty() {
            return Err(errors::validation(
                "discovery",
                "请至少输入一个仓库地址",
                "每行输入一个 HTTPS 或 SSH 仓库地址",
            ));
        }
        let report = git_repo_migrator_platform_generic::import_urls(&input.urls);
        let mut inner = self.lock();
        let mut imported = 0_u32;
        for url in &report.urls {
            let (namespace, name) = split_repository_path(url.as_str());
            let details = CandidateDetails {
                git_capable: true,
                platform_capable: false,
                ..CandidateDetails::default()
            };
            upsert_candidate(
                &inner.store,
                url.as_str(),
                Some(&input.connection_id),
                url.as_str(),
                &name,
                &namespace,
                RepositoryVisibility::Unknown,
                PermissionLevel::GitOnly,
                &details,
            )
            .map_err(|error| {
                errors::error(
                    "ipc.store",
                    ErrorCategory::Disk,
                    true,
                    "discovery",
                    format!("保存导入的仓库失败：{error}"),
                    "请确认磁盘可写后重试",
                )
            })?;
            imported += 1;
        }
        inner.revision += 1;

        Ok(RepositoryImportReport {
            imported,
            duplicate_count: u32::try_from(report.duplicate_count).unwrap_or(u32::MAX),
            issues: report
                .issues
                .into_iter()
                .map(|issue| RepositoryImportIssue {
                    line: u32::try_from(issue.line).unwrap_or(0),
                    value: issue.value,
                    message: issue.message,
                })
                .collect(),
        })
    }

    /// Establishes the real target state. Without this the plan stays blocked,
    /// which is what keeps an unknown target from being written to.
    pub fn probe_target(&self, input: &TargetProbeInput) -> Result<RepositorySnapshot, IpcError> {
        let target_url = validate_target_url(&input.target_url)?;
        let probe = self.probe.as_ref().ok_or_else(|| {
            errors::unsupported(
                "preflight",
                "未配置目标探测器，无法确认目标仓库状态",
                "请在设置中启用系统 Git，或手动确认目标后重新预检",
            )
        })?;
        let state = probe.probe(&target_url)?;

        let mut inner = self.lock();
        let existing = snapshot::read_repositories(&inner.store)
            .map_err(|error| errors::store("preflight", &error))?
            .into_iter()
            .find(|item| item.id == input.repository_id)
            .ok_or_else(|| errors::not_found("preflight", "仓库候选"))?;

        let (_, target_name) = split_repository_path(&target_url);
        let details = CandidateDetails {
            target_url: Some(target_url.clone()),
            target_name: Some(existing.target_name.clone().unwrap_or(target_name)),
            target_state: Some(enum_text(&state)),
            git_capable: existing.git_capable,
            platform_capable: existing.platform_capable,
            updated_at_epoch_seconds: existing.updated_at_epoch_seconds,
            unselectable_reason: existing.unselectable_reason.clone(),
        };
        let metadata = serde_json::to_string(&details).unwrap_or_else(|_| "{}".to_owned());
        inner
            .store
            .connection()
            .execute(
                "UPDATE repository_candidate SET metadata_json = ?2 WHERE id = ?1",
                params![input.repository_id, metadata],
            )
            .map_err(|error| errors::store("preflight", &error.into()))?;
        inner.revision += 1;

        snapshot::read_repositories(&inner.store)
            .map_err(|error| errors::store("preflight", &error))?
            .into_iter()
            .find(|item| item.id == input.repository_id)
            .ok_or_else(|| errors::not_found("preflight", "仓库候选"))
    }

    pub fn set_mapping(
        &self,
        repository_id: &str,
        target_url: &str,
        target_name: Option<&str>,
    ) -> Result<RepositorySnapshot, IpcError> {
        let target_url = validate_target_url(target_url)?;
        let mut inner = self.lock();
        let existing = snapshot::read_repositories(&inner.store)
            .map_err(|error| errors::store("mapping", &error))?
            .into_iter()
            .find(|item| item.id == repository_id)
            .ok_or_else(|| errors::not_found("mapping", "仓库候选"))?;

        let (_, derived_name) = split_repository_path(&target_url);
        // Changing the target invalidates the previously probed state; a fresh
        // probe is required before the plan can be executed.
        let details = CandidateDetails {
            target_url: Some(target_url),
            target_name: Some(target_name.unwrap_or(&derived_name).to_owned()),
            target_state: None,
            git_capable: existing.git_capable,
            platform_capable: existing.platform_capable,
            updated_at_epoch_seconds: existing.updated_at_epoch_seconds,
            unselectable_reason: existing.unselectable_reason.clone(),
        };
        let metadata = serde_json::to_string(&details).unwrap_or_else(|_| "{}".to_owned());
        inner
            .store
            .connection()
            .execute(
                "UPDATE repository_candidate SET metadata_json = ?2 WHERE id = ?1",
                params![repository_id, metadata],
            )
            .map_err(|error| errors::store("mapping", &error.into()))?;
        inner.revision += 1;

        snapshot::read_repositories(&inner.store)
            .map_err(|error| errors::store("mapping", &error))?
            .into_iter()
            .find(|item| item.id == repository_id)
            .ok_or_else(|| errors::not_found("mapping", "仓库候选"))
    }

    // -- planning -----------------------------------------------------------

    pub fn preview_plan(
        &self,
        request: &PlanPreviewRequest,
    ) -> Result<PlanPreviewSnapshot, IpcError> {
        let repositories = self.repositories()?;
        let connections = self.connections()?;
        let target = connections
            .iter()
            .find(|connection| connection.role == ConnectionRole::Target);
        let target_capabilities = target
            .map(|connection| connection.capabilities.clone())
            .unwrap_or_default();
        let target_can_create = target_capabilities
            .iter()
            .any(|capability| capability.module == "repository_creation" && capability.permitted);

        let overrides: HashMap<&str, &crate::dto::RepositoryMappingInput> = request
            .mappings
            .iter()
            .map(|mapping| (mapping.repository_id.as_str(), mapping))
            .collect();

        let policy = ConflictPolicy {
            reuse_empty: request.reuse_empty,
            skip_non_empty: request.skip_non_empty,
            auto_rename: request.auto_rename,
            allow_overwrite: request.allow_overwrite,
        };
        let modules = ModuleSelection {
            lfs: request.module_lfs,
            metadata: request.module_metadata,
            issues: request.module_issues,
            pull_requests: request.module_pull_requests,
            wiki: request.module_wiki,
            releases: request.module_releases,
        };
        let ref_policy = RefPolicy {
            include_archived_refs: request.include_archived_refs,
        };

        // "Select all" is the filtered result set minus explicit exclusions, not
        // the rows that happen to be on the current page.
        let mut selection = SelectionSet::select_all(request.selected_repository_ids.clone());
        for excluded in &request.excluded_repository_ids {
            selection.exclude(excluded.clone());
        }

        let mut candidates = Vec::new();
        let mut targets = HashMap::new();
        let mut by_id = HashMap::new();
        for repository in &repositories {
            let (target_url, target_name) = match overrides.get(repository.id.as_str()) {
                Some(override_mapping) => (
                    Some(override_mapping.target_url.clone()),
                    override_mapping.target_name.clone(),
                ),
                None => (
                    repository.target_url.clone(),
                    repository.target_name.clone(),
                ),
            };
            candidates.push(Candidate {
                id: repository.id.clone(),
                source_url: repository.source_url.clone(),
                name: repository.name.clone(),
                namespace: repository.namespace.clone(),
                target_url,
                target_name,
            });
            targets.insert(repository.id.clone(), repository.target_state);
            by_id.insert(repository.id.clone(), repository);
        }

        let capability_input = capability_fingerprint(target);
        let preview = build_preview(
            &selection,
            &candidates,
            &targets,
            policy.clone(),
            capability_input,
        );

        let module_rows = module_fidelity_rows(&modules, &target_capabilities);
        let mut rows = Vec::new();
        let mut metrics = PreflightMetrics {
            total: 0,
            executable: 0,
            blocked: 0,
            warnings: 0,
            create: 0,
            reuse: 0,
            skip: 0,
        };
        let mut blocking = preview.blocking.clone();
        let mut warnings = preview.warnings.clone();
        let mut overwrite_targets = Vec::new();
        let mut actions = BTreeMap::new();

        for id in selection.selected() {
            let Some(repository) = by_id.get(id) else {
                continue;
            };
            let target_url = overrides
                .get(id.as_str())
                .map(|mapping| mapping.target_url.clone())
                .or_else(|| repository.target_url.clone())
                .unwrap_or_default();
            let target_name = overrides
                .get(id.as_str())
                .and_then(|mapping| mapping.target_name.clone())
                .or_else(|| repository.target_name.clone())
                .unwrap_or_else(|| repository.name.clone());

            metrics.total += 1;
            let (action, blocking_reason, suggested_action) = decide_action(
                target_url.as_str(),
                repository.target_state,
                &policy,
                target_can_create,
                repository.permission,
            );
            match action {
                PlanAction::Create => metrics.create += 1,
                PlanAction::ReuseEmpty => metrics.reuse += 1,
                PlanAction::SkipNonEmpty => metrics.skip += 1,
                PlanAction::Overwrite => overwrite_targets.push(target_name.clone()),
                PlanAction::Rename => metrics.create += 1,
                PlanAction::Blocked => metrics.blocked += 1,
            }
            if action == PlanAction::Blocked {
                if let Some(reason) = blocking_reason.clone() {
                    if !blocking.contains(&reason) {
                        blocking.push(reason);
                    }
                }
            } else {
                metrics.executable += 1;
                actions.insert(id.clone(), enum_text(&action));
            }
            if action == PlanAction::SkipNonEmpty {
                let warning = format!("目标非空，按默认策略跳过：{target_url}");
                if !warnings.contains(&warning) {
                    warnings.push(warning);
                }
            }

            rows.push(PreflightRow {
                repository_id: repository.id.clone(),
                source_url: repository.source_url.clone(),
                target_url,
                target_name,
                action,
                permission: repository.permission,
                target_state: repository.target_state,
                module_fidelity: module_rows.clone(),
                disk_estimate_bytes: 0,
                blocking_reason,
                suggested_action,
                field_mapping: field_mapping_rows(repository),
            });
        }

        metrics.warnings = u32::try_from(warnings.len()).unwrap_or(0);
        let requires_confirmation = preview.requires_confirmation || !overwrite_targets.is_empty();
        let confirmation_phrase = match overwrite_targets.as_slice() {
            [] => None,
            [single] => Some(single.clone()),
            many => Some(format!("OVERWRITE-{}", many.len())),
        };

        let mappings = preview.mappings.clone();
        let repository_ids = rows
            .iter()
            .filter(|row| row.action != PlanAction::Blocked)
            .map(|row| row.repository_id.clone())
            .collect::<Vec<_>>();

        let snapshot = PlanPreviewSnapshot {
            preview_id: String::new(),
            metrics,
            rows,
            blocking,
            warnings,
            capability_snapshot_hash: preview.capability_snapshot_hash.clone(),
            requires_confirmation,
            confirmation_phrase,
            ref_policy: ref_policy_summary(&ref_policy),
            selected_count: u32::try_from(selection.len()).unwrap_or(0),
            excluded_count: u32::try_from(request.excluded_repository_ids.len()).unwrap_or(0),
        };

        let mut inner = self.lock();
        let preview_id = inner.next_id("preview", self.clock.now_ms());
        let mut snapshot = snapshot;
        snapshot.preview_id = preview_id.clone();
        inner.previews.insert(
            preview_id,
            PreviewRecord {
                snapshot: snapshot.clone(),
                mappings,
                repository_ids,
                actions,
                modules,
                policy,
                ref_policy,
                selected: request.selected_repository_ids.clone(),
                excluded: request.excluded_repository_ids.clone(),
            },
        );
        inner.revision += 1;
        Ok(snapshot)
    }

    pub fn freeze_plan(&self, input: &PlanFreezeInput) -> Result<PlanSnapshot, IpcError> {
        let mut inner = self.lock();
        let record = inner
            .previews
            .get(&input.preview_id)
            .cloned()
            .ok_or_else(|| errors::not_found("preflight", "预检结果；请重新运行预检"))?;

        if !record.snapshot.blocking.is_empty() {
            return Err(errors::conflict(
                "preflight",
                format!("仍有 {} 项阻断未解决", record.snapshot.blocking.len()),
                "请修正或排除阻断项后重新预检",
            ));
        }
        // The confirmation is verified against the phrase the backend issued,
        // so a renderer cannot self-authorise a destructive plan.
        if record.snapshot.requires_confirmation {
            let expected = record.snapshot.confirmation_phrase.as_deref().unwrap_or("");
            let provided = input.confirmation_text.as_deref().unwrap_or("");
            if expected.is_empty() || provided != expected {
                return Err(errors::validation(
                    "preflight",
                    "覆盖迁移需要二次确认",
                    format!("请输入确认文本「{expected}」后重试"),
                ));
            }
        }
        for row in &record.snapshot.rows {
            for module in row
                .module_fidelity
                .iter()
                .filter(|m| m.confirmation_required)
            {
                if !input.acknowledged_fidelity.contains(&module.module) {
                    return Err(errors::validation(
                        "preflight",
                        format!("模块 {} 只能归档或不支持迁移，需要确认", module.module),
                        "请在映射页确认降级模块后重新冻结计划",
                    ));
                }
            }
        }

        let plan = MigrationPlan::freeze(
            record.mappings.clone(),
            record.modules.clone(),
            record.policy.clone(),
        )
        .map_err(|reason| errors::validation("preflight", reason, "请修正映射后重新预检"))?;

        let selection = PlanSelection {
            selected: record.selected.clone(),
            excluded: record.excluded.clone(),
            mappings: plan.mappings.clone(),
            repository_ids: record.repository_ids.clone(),
            actions: record.actions.clone(),
            capability_snapshot_hash: record.snapshot.capability_snapshot_hash.clone(),
            dangerous_confirmed: record.snapshot.requires_confirmation,
            acknowledged_fidelity: input.acknowledged_fidelity.clone(),
        };
        let policy_json = serde_json::to_string(&(&plan.conflict_policy, &record.ref_policy))
            .unwrap_or_else(|_| "[]".to_owned());
        let module_json = serde_json::to_string(&plan.modules).unwrap_or_else(|_| "{}".to_owned());
        let selection_json = serde_json::to_string(&selection).unwrap_or_else(|_| "{}".to_owned());

        // A plan is immutable and identified by its hash: re-freezing the same
        // configuration reuses the existing plan, any change produces a new one.
        let existing: Option<String> = inner
            .store
            .connection()
            .query_row(
                "SELECT id FROM plan WHERE plan_hash = ?1",
                params![plan.plan_hash],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| errors::store("preflight", &error.into()))?;

        let plan_id = match existing {
            Some(id) => id,
            None => {
                let now = self.clock.now_ms();
                let id = inner.next_id("plan", now);
                inner
                    .store
                    .connection()
                    .execute(
                        "INSERT INTO plan (id, selection_json, policy_json, module_json, plan_hash, status, created_at_ms)
                         VALUES (?1, ?2, ?3, ?4, ?5, 'frozen', ?6)",
                        params![id, selection_json, policy_json, module_json, plan.plan_hash, now],
                    )
                    .map_err(|error| errors::store("preflight", &error.into()))?;
                id
            }
        };
        inner.revision += 1;

        snapshot::read_plan(&inner.store, &plan_id)
            .map_err(|error| errors::store("preflight", &error))?
            .ok_or_else(|| errors::not_found("preflight", "刚冻结的计划"))
    }

    // -- batches ------------------------------------------------------------

    pub fn start_batch(&self, input: &BatchStartInput) -> Result<BatchSnapshot, IpcError> {
        if !matches!(input.workspace_policy.as_str(), "reuse" | "clean") {
            return Err(errors::validation(
                "queue",
                "工作区策略必须是 reuse 或 clean",
                "请重新选择工作区策略",
            ));
        }
        let concurrency = input.concurrency.clamp(1, MAX_CONCURRENCY);
        let connections = self.connections()?;
        let target = connections
            .iter()
            .find(|connection| connection.role == ConnectionRole::Target);
        let repositories = self.repositories()?;

        let mut inner = self.lock();
        let row = inner
            .store
            .connection()
            .query_row(
                "SELECT selection_json, policy_json, module_json, plan_hash, status
                 FROM plan WHERE id = ?1",
                params![input.plan_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| errors::store("queue", &error.into()))?
            .ok_or_else(|| errors::not_found("queue", "冻结的计划"))?;
        let (selection_json, policy_json, module_json, plan_hash, status) = row;

        if status != "frozen" {
            return Err(errors::conflict(
                "queue",
                format!("计划状态为 {status}，无法启动"),
                "请重新预检并冻结计划",
            ));
        }

        let selection: PlanSelection = serde_json::from_str(&selection_json)
            .map_err(|_| errors::validation("queue", "计划数据无法解析", "请重新预检并冻结计划"))?;
        let modules: ModuleSelection = serde_json::from_str(&module_json)
            .map_err(|_| errors::validation("queue", "模块选择无法解析", "请重新预检"))?;
        let (policy, _ref_policy): (ConflictPolicy, RefPolicy) = serde_json::from_str(&policy_json)
            .map_err(|_| errors::validation("queue", "策略数据无法解析", "请重新预检"))?;

        // Recompute the hash from the persisted plan: a tampered row can never
        // be executed.
        let recomputed =
            MigrationPlan::freeze(selection.mappings.clone(), modules.clone(), policy.clone())
                .map_err(|reason| errors::validation("queue", reason, "请重新预检并冻结计划"))?;
        if recomputed.plan_hash != plan_hash {
            return Err(errors::conflict(
                "queue",
                "计划哈希与持久化内容不一致",
                "请重新预检并冻结计划；已完成的仓库不会受影响",
            ));
        }
        if policy.allow_overwrite && !selection.dangerous_confirmed {
            return Err(errors::conflict(
                "queue",
                "覆盖迁移未通过二次确认",
                "请返回预检页完成覆盖确认",
            ));
        }
        // Capabilities are re-probed at start; a stale snapshot sends the
        // operator back to preflight instead of executing an outdated plan.
        let current_capabilities = format!(
            "{:x}",
            Sha256::digest(capability_fingerprint(target).as_bytes())
        );
        if current_capabilities != selection.capability_snapshot_hash {
            return Err(errors::conflict(
                "queue",
                "目标平台能力快照已变化",
                "请重新运行预检以刷新能力矩阵",
            ));
        }

        let by_id: HashMap<&str, &RepositorySnapshot> = repositories
            .iter()
            .map(|repository| (repository.id.as_str(), repository))
            .collect();
        for repository_id in &selection.repository_ids {
            let repository = by_id
                .get(repository_id.as_str())
                .ok_or_else(|| errors::not_found("queue", "计划中的仓库候选"))?;
            if matches!(
                repository.target_state,
                TargetState::Unknown | TargetState::Inaccessible
            ) {
                return Err(errors::conflict(
                    "queue",
                    format!("{} 的目标状态未确认", repository.source_url),
                    "请重新探测目标状态后再启动",
                ));
            }
        }

        let now = self.clock.now_ms();
        let batch_id = inner.next_id("batch", now);
        let total = i64::try_from(selection.repository_ids.len()).unwrap_or(0);
        inner
            .store
            .connection()
            .execute(
                "INSERT INTO batch (id, plan_id, status, total, completed, failed, started_at_ms, workspace_policy)
                 VALUES (?1, ?2, 'running', ?3, 0, 0, ?4, ?5)",
                params![batch_id, input.plan_id, total, now, input.workspace_policy],
            )
            .map_err(|error| errors::store("queue", &error.into()))?;

        for repository_id in &selection.repository_ids {
            let repository = by_id
                .get(repository_id.as_str())
                .ok_or_else(|| errors::not_found("queue", "计划中的仓库候选"))?;
            let target_url = repository.target_url.clone().ok_or_else(|| {
                errors::conflict("queue", "计划中的目标 URL 缺失", "请返回映射页补齐目标")
            })?;
            let action = selection
                .actions
                .get(repository_id)
                .cloned()
                .unwrap_or_else(|| enum_text(&PlanAction::Blocked));
            let task_id = inner.next_id("task", now);
            inner
                .store
                .connection()
                .execute(
                    "INSERT INTO repository_task
                        (id, batch_id, candidate_id, target_url, action, status, attempt, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'planned', 0, ?6)",
                    params![task_id, batch_id, repository_id, target_url, action, now],
                )
                .map_err(|error| errors::store("queue", &error.into()))?;
        }
        inner.concurrency.insert(batch_id.clone(), concurrency);
        inner.revision += 1;

        let snapshot = snapshot::read_batch(&inner.store, &batch_id, concurrency)
            .map_err(|error| errors::store("queue", &error))?
            .ok_or_else(|| errors::not_found("queue", "刚创建的批次"))?;
        // The lock is released before the pool starts, otherwise the first
        // worker would block on the very command that created its batch.
        drop(inner);
        if let Some(launcher) = self.launcher() {
            launcher.launch(&batch_id, concurrency);
        }
        Ok(snapshot)
    }

    pub fn set_control(
        &self,
        input: &BatchIdInput,
        next: BatchControl,
    ) -> Result<BatchSnapshot, IpcError> {
        let mut inner = self.lock();
        let current = inner
            .store
            .connection()
            .query_row(
                "SELECT status FROM batch WHERE id = ?1",
                params![input.batch_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| errors::store("queue", &error.into()))?
            .ok_or_else(|| errors::not_found("queue", "批次"))?;
        let current = snapshot::parse_control(&current);

        let allowed = matches!(
            (current, next),
            (BatchControl::Running, BatchControl::Paused)
                | (BatchControl::Paused, BatchControl::Running)
                | (
                    BatchControl::Running | BatchControl::Paused,
                    BatchControl::Cancelled
                )
        );
        if !allowed {
            return Err(errors::conflict(
                "queue",
                format!(
                    "批次当前为 {}，不能切换到 {}",
                    snapshot::control_value(current),
                    snapshot::control_value(next)
                ),
                "请刷新队列后重试",
            ));
        }

        let now = self.clock.now_ms();
        let ended = if next == BatchControl::Cancelled {
            Some(now)
        } else {
            None
        };
        inner
            .store
            .connection()
            .execute(
                "UPDATE batch SET status = ?2, ended_at_ms = COALESCE(?3, ended_at_ms) WHERE id = ?1",
                params![input.batch_id, snapshot::control_value(next), ended],
            )
            .map_err(|error| errors::store("queue", &error.into()))?;
        inner.revision += 1;

        let concurrency = inner.concurrency.get(&input.batch_id).copied().unwrap_or(1);
        let snapshot = snapshot::read_batch(&inner.store, &input.batch_id, concurrency)
            .map_err(|error| errors::store("queue", &error))?
            .ok_or_else(|| errors::not_found("queue", "批次"))?;
        drop(inner);

        if let Some(launcher) = self.launcher() {
            match next {
                // Cancelling only signals; in-flight stages stop at their next
                // checkpoint and finished repositories are never rolled back.
                BatchControl::Cancelled => launcher.cancel(&input.batch_id),
                // Workers exit when a batch pauses, so resuming has to start
                // them again. `launch` is idempotent.
                BatchControl::Running => launcher.launch(&input.batch_id, concurrency),
                BatchControl::Paused | BatchControl::Completed => {}
            }
        }
        Ok(snapshot)
    }

    pub fn retry_tasks(&self, input: &TaskRetryInput) -> Result<RetryOutcome, IpcError> {
        let mut inner = self.lock();
        let now = self.clock.now_ms();
        let control = inner
            .store
            .connection()
            .query_row(
                "SELECT status FROM batch WHERE id = ?1",
                params![input.batch_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| errors::store("queue", &error.into()))?
            .map(|status| snapshot::parse_control(&status))
            .ok_or_else(|| errors::not_found("queue", "批次"))?;
        if control == BatchControl::Cancelled {
            return Err(errors::conflict(
                "queue",
                "批次已取消，无法重试",
                "请创建新批次；已完成的目标不会回滚",
            ));
        }

        let tasks = snapshot::read_tasks(&inner.store, &input.batch_id)
            .map_err(|error| errors::store("queue", &error))?;
        let mut retried = Vec::new();
        let mut rejected = Vec::new();

        for task_id in &input.task_ids {
            let Some(task) = tasks.iter().find(|task| &task.task_id == task_id) else {
                rejected.push(RetryRejection {
                    task_id: task_id.clone(),
                    reason: "任务不属于该批次".to_owned(),
                });
                continue;
            };
            if task.state != RepoTaskState::RetryableFailed {
                rejected.push(RetryRejection {
                    task_id: task_id.clone(),
                    reason: format!(
                        "任务状态为 {}，只有可重试失败才能重试",
                        snapshot::task_state_value(task.state)
                    ),
                });
                continue;
            }
            if let Some(error) = &task.error {
                if errors::is_blind_retry_forbidden(error.category) || !error.retryable {
                    rejected.push(RetryRejection {
                        task_id: task_id.clone(),
                        reason: format!("{}：{}", error.code, error.action),
                    });
                    continue;
                }
            }
            inner
                .store
                .connection()
                .execute(
                    "UPDATE repository_task
                     SET status = 'planned', attempt = attempt + 1, error_code = NULL,
                         lease_owner = NULL, lease_expires_at_ms = NULL, updated_at_ms = ?2
                     WHERE id = ?1",
                    params![task_id, now],
                )
                .map_err(|error| errors::store("queue", &error.into()))?;
            retried.push(task_id.clone());
        }
        if !retried.is_empty() {
            // Re-queueing work reopens a batch that had run out of runnable
            // tasks; a paused batch stays paused so retry never starts a stage.
            inner
                .store
                .connection()
                .execute(
                    "UPDATE batch SET status = 'running', ended_at_ms = NULL
                     WHERE id = ?1 AND status = 'completed'",
                    params![input.batch_id],
                )
                .map_err(|error| errors::store("queue", &error.into()))?;
            if let Some(first) = retried.first() {
                inner.refresh_batch_rollup(first, now)?;
            }
        }
        inner.revision += 1;

        let concurrency = inner.concurrency.get(&input.batch_id).copied().unwrap_or(1);
        let batch = snapshot::read_batch(&inner.store, &input.batch_id, concurrency)
            .map_err(|error| errors::store("queue", &error))?
            .ok_or_else(|| errors::not_found("queue", "批次"))?;
        drop(inner);

        // A retry that reopened the batch needs workers again; a paused batch
        // stays paused, so the pool is only started for a running one.
        if !retried.is_empty() && batch.control == BatchControl::Running {
            if let Some(launcher) = self.launcher() {
                launcher.launch(&input.batch_id, concurrency);
            }
        }
        Ok(RetryOutcome {
            retried,
            rejected,
            batch,
        })
    }

    // -- reporting ----------------------------------------------------------

    pub fn report(&self, batch_id: &str) -> Result<ReportSnapshot, IpcError> {
        let inner = self.lock();
        snapshot::read_report(&inner.store, batch_id, inner.cleanup.clone())
            .map_err(|error| errors::store("report", &error))
    }

    /// Records what happened to a task's temporary directory (FR-011).
    ///
    /// The report carries one batch-level cleanup line, so per-task outcomes are
    /// folded together: a deliberate retention outranks everything (the operator
    /// must learn where the mirror lives), a failed cleanup outranks a clean
    /// one, and the first retained path is kept — losing it would hide exactly
    /// the directory the operator asked to keep.
    pub fn set_cleanup_state(&self, cleanup: CleanupState) {
        let mut inner = self.lock();
        inner.cleanup = merge_cleanup(&inner.cleanup, cleanup);
        inner.revision += 1;
    }

    pub fn export_report(&self, input: &ReportExportInput) -> Result<ExportOutcome, IpcError> {
        let format = match input.format.as_str() {
            "json" => ExportFormat::Json,
            "csv" => ExportFormat::Csv,
            "mapping" => ExportFormat::Csv,
            other => {
                return Err(errors::validation(
                    "report",
                    format!("不支持的导出格式：{other}"),
                    "请选择 json、csv 或 mapping",
                ))
            }
        };
        let path = validate_export_path(&input.path, &input.format)?;
        let snapshot = self.report(&input.batch_id)?;
        let report = Report {
            rows: snapshot
                .rows
                .iter()
                .map(|row| ReportRow {
                    task_id: row.task_id.clone(),
                    source_url: row.source_url.clone(),
                    target_url: row.target_url.clone(),
                    status: row.status,
                    error_code: row.error_code.clone(),
                    excluded_refs: row.evidence.excluded_refs.clone(),
                })
                .collect(),
        };
        let contents = report
            .export(format)
            .map_err(|_| errors::validation("report", "报告序列化失败", "请重试导出"))?;
        self.export.write(&path, &contents).map_err(|reason| {
            errors::error(
                "ipc.export",
                ErrorCategory::Disk,
                true,
                "report",
                format!("写入导出文件失败：{reason}"),
                "请选择可写目录后重试；报告数据仍保存在本地状态库",
            )
        })?;
        Ok(ExportOutcome {
            path: path.to_string_lossy().into_owned(),
            bytes_written: u64::try_from(contents.len()).unwrap_or(0),
            row_count: u32::try_from(report.rows.len()).unwrap_or(0),
        })
    }

    // -- snapshot -----------------------------------------------------------

    pub fn connections(&self) -> Result<Vec<ConnectionSnapshot>, IpcError> {
        let inner = self.lock();
        snapshot::read_connections(&inner.store)
            .map_err(|error| errors::store("connection", &error))
    }

    pub fn repositories(&self) -> Result<Vec<RepositorySnapshot>, IpcError> {
        let inner = self.lock();
        snapshot::read_repositories(&inner.store)
            .map_err(|error| errors::store("discovery", &error))
    }

    pub fn snapshot(&self) -> Result<MigrationSnapshot, IpcError> {
        let inner = self.lock();
        let now = self.clock.now_ms();
        let store = &inner.store;
        let stage = "snapshot";
        let plan_id =
            snapshot::latest_plan_id(store).map_err(|error| errors::store(stage, &error))?;
        let active_plan = match plan_id {
            Some(id) => {
                snapshot::read_plan(store, &id).map_err(|error| errors::store(stage, &error))?
            }
            None => None,
        };
        let batch_id =
            snapshot::latest_batch_id(store).map_err(|error| errors::store(stage, &error))?;
        let active_batch = match &batch_id {
            Some(id) => {
                let concurrency = inner.concurrency.get(id).copied().unwrap_or(1);
                snapshot::read_batch(store, id, concurrency)
                    .map_err(|error| errors::store(stage, &error))?
            }
            None => None,
        };
        let report = match &batch_id {
            Some(id) => Some(
                snapshot::read_report(store, id, inner.cleanup.clone())
                    .map_err(|error| errors::store(stage, &error))?,
            ),
            None => None,
        };
        Ok(MigrationSnapshot {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            revision: inner.revision,
            connections: snapshot::read_connections(store)
                .map_err(|error| errors::store(stage, &error))?,
            repositories: snapshot::read_repositories(store)
                .map_err(|error| errors::store(stage, &error))?,
            active_preview: inner
                .previews
                .values()
                .last()
                .map(|record| record.snapshot.clone()),
            active_plan,
            active_batch,
            report,
            resumable: snapshot::read_resumable(store, now)
                .map_err(|error| errors::store(stage, &error))?,
        })
    }

    // -- backend-only stage recording ---------------------------------------
    //
    // These are not commands. Only the backend executor may assert that work
    // happened, so the renderer cannot fabricate progress or a success.

    /// Current batch control, as the worker pool sees it.
    pub fn batch_control(&self, batch_id: &str) -> BatchControl {
        let inner = self.lock();
        inner
            .store
            .connection()
            .query_row(
                "SELECT status FROM batch WHERE id = ?1",
                params![batch_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .map(|status| snapshot::parse_control(&status))
            // An unreadable batch must not look runnable.
            .unwrap_or(BatchControl::Cancelled)
    }

    /// Takes the next runnable repository for `owner`, claiming its lease in the
    /// same call so two workers can never pick up the same task.
    ///
    /// Returns `None` when the batch is not running or has no runnable rows
    /// left; the worker then exits instead of spinning.
    pub fn claim_next_task(
        &self,
        batch_id: &str,
        owner: &str,
    ) -> Result<Option<TaskAssignment>, IpcError> {
        let inner = self.lock();
        let now = self.clock.now_ms();

        let status = inner
            .store
            .connection()
            .query_row(
                "SELECT status, workspace_policy FROM batch WHERE id = ?1",
                params![batch_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|error| errors::store("queue", &error.into()))?
            .ok_or_else(|| errors::not_found("queue", "批次"))?;
        if snapshot::parse_control(&status.0) != BatchControl::Running {
            return Ok(None);
        }

        let (module_json, policy_json) = inner
            .store
            .connection()
            .query_row(
                "SELECT p.module_json, p.policy_json
                 FROM batch b JOIN plan p ON p.id = b.plan_id
                 WHERE b.id = ?1",
                params![batch_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|error| errors::store("queue", &error.into()))?
            .ok_or_else(|| errors::not_found("queue", "批次的冻结计划"))?;
        let modules: ModuleSelection = serde_json::from_str(&module_json)
            .map_err(|_| errors::validation("queue", "模块选择无法解析", "请重新预检"))?;
        let (policy, ref_policy): (ConflictPolicy, RefPolicy) = serde_json::from_str(&policy_json)
            .map_err(|_| errors::validation("queue", "策略数据无法解析", "请重新预检"))?;

        let candidate = inner
            .store
            .connection()
            .query_row(
                "SELECT t.id, t.target_url, t.action, t.attempt, c.source_url, c.name,
                        c.metadata_json
                 FROM repository_task t
                 JOIN repository_candidate c ON c.id = t.candidate_id
                 WHERE t.batch_id = ?1
                   AND t.status = 'planned'
                   AND (t.lease_owner IS NULL OR t.lease_expires_at_ms <= ?2)
                 ORDER BY t.id
                 LIMIT 1",
                params![batch_id, now],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| errors::store("queue", &error.into()))?;
        let Some((task_id, target_url, action, attempt, source_url, name, metadata)) = candidate
        else {
            return Ok(None);
        };

        if !inner
            .store
            .leases()
            .acquire(&task_id, owner, now, LEASE_TTL_MS)
            .map_err(|error| errors::store("queue", &error))?
        {
            // Another worker won the race; the caller asks again.
            return Ok(None);
        }

        // A `git` checkpoint from an earlier attempt is what distinguishes "we
        // already pushed to this target" from "someone else filled it in".
        let resumed_attempt = attempt > 0
            || inner
                .store
                .connection()
                .query_row(
                    "SELECT COUNT(*) FROM checkpoint WHERE task_id = ?1 AND stage = 'git'",
                    params![task_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(|error| errors::store("queue", &error.into()))?
                .unwrap_or(0)
                > 0;

        let details: CandidateDetails = serde_json::from_str(&metadata).unwrap_or_default();
        Ok(Some(TaskAssignment {
            batch_id: batch_id.to_owned(),
            task_id,
            source_url,
            target_url,
            target_name: details.target_name.unwrap_or(name),
            action: ExecutionAction::parse(&action).unwrap_or(ExecutionAction::Blocked),
            modules,
            ref_policy,
            allow_overwrite: policy.allow_overwrite,
            resumed_attempt,
        }))
    }

    /// The workspace policy a batch was started with (FR-011). Workers read it
    /// once per claim so a resumed batch keeps the policy its operator chose.
    pub fn workspace_policy_of(&self, batch_id: &str) -> WorkspacePolicy {
        let inner = self.lock();
        let policy = inner
            .store
            .connection()
            .query_row(
                "SELECT workspace_policy FROM batch WHERE id = ?1",
                params![batch_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .unwrap_or_else(|| "reuse".to_owned());
        WorkspacePolicy::parse(&policy).unwrap_or(WorkspacePolicy::Reuse)
    }

    pub fn begin_stage(
        &self,
        task_id: &str,
        stage: MigrationStage,
        owner: &str,
    ) -> Result<(), IpcError> {
        let mut inner = self.lock();
        let now = self.clock.now_ms();
        let stage_text = enum_text(&stage);
        if !inner
            .store
            .leases()
            .acquire(task_id, owner, now, LEASE_TTL_MS)
            .map_err(|error| errors::store(&stage_text, &error))?
        {
            return Err(errors::conflict(
                &stage_text,
                "任务租约由其他执行器持有",
                "请等待租约过期后再恢复",
            ));
        }
        let attempt = inner.attempt_of(task_id)?;
        let state = state_for_stage(stage);
        inner.append_checkpoint(
            owner,
            task_id,
            &stage_text,
            attempt,
            "started",
            "{}",
            true,
            now,
        )?;
        inner.set_task_state(task_id, state, None, now)?;
        inner.revision += 1;
        Ok(())
    }

    pub fn report_progress(
        &self,
        task_id: &str,
        stage: MigrationStage,
        owner: &str,
        completed: u64,
        total: Option<u64>,
    ) -> Result<(), IpcError> {
        let mut inner = self.lock();
        let now = self.clock.now_ms();
        let stage_text = enum_text(&stage);
        inner
            .store
            .leases()
            .heartbeat(task_id, owner, now, LEASE_TTL_MS)
            .map_err(|error| errors::store(&stage_text, &error))?;
        let attempt = inner.attempt_of(task_id)?;
        let payload = serde_json::to_string(&snapshot::ProgressSummary { completed, total })
            .unwrap_or_else(|_| "{}".to_owned());
        inner.append_checkpoint(
            owner,
            task_id,
            &stage_text,
            attempt,
            "heartbeat",
            &payload,
            true,
            now,
        )?;
        inner.revision += 1;
        Ok(())
    }

    pub fn fail_stage(
        &self,
        task_id: &str,
        stage: MigrationStage,
        owner: &str,
        error: &IpcError,
    ) -> Result<(), IpcError> {
        let mut inner = self.lock();
        let now = self.clock.now_ms();
        let stage_text = enum_text(&stage);
        let attempt = inner.attempt_of(task_id)?;
        inner.append_checkpoint(
            owner,
            task_id,
            &stage_text,
            attempt,
            "failed",
            "{}",
            error.retryable,
            now,
        )?;
        let next_state = if matches!(
            error.category,
            ErrorCategory::Permission | ErrorCategory::Conflict
        ) {
            RepoTaskState::Skipped
        } else {
            RepoTaskState::RetryableFailed
        };
        inner.set_task_state(task_id, next_state, Some(&error.code), now)?;
        inner.append_log(task_id, "error", &stage_text, error, now)?;
        inner.refresh_batch_rollup(task_id, now)?;
        inner
            .store
            .leases()
            .release(task_id, owner, now)
            .map_err(|store_error| errors::store(&stage_text, &store_error))?;
        inner.revision += 1;
        Ok(())
    }

    pub fn record_module_result(
        &self,
        task_id: &str,
        outcome: &ModuleOutcome<'_>,
    ) -> Result<(), IpcError> {
        let mut inner = self.lock();
        let now = self.clock.now_ms();
        let id = inner.next_id("module", now);
        let status = if outcome.error.is_some() {
            "failed"
        } else {
            "succeeded"
        };
        let error_json = outcome
            .error
            .and_then(|value| serde_json::to_string(value).ok())
            .unwrap_or_else(|| "{}".to_owned());
        let links = serde_json::to_string(outcome.source_links).unwrap_or_else(|_| "[]".to_owned());
        inner
            .store
            .connection()
            .execute(
                "INSERT INTO module_result
                    (id, task_id, module, fidelity, status, source_count, target_count,
                     source_links_json, error_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id,
                    task_id,
                    outcome.module,
                    enum_text(&outcome.fidelity),
                    status,
                    i64::try_from(outcome.source_count).unwrap_or(0),
                    i64::try_from(outcome.target_count).unwrap_or(0),
                    links,
                    error_json,
                ],
            )
            .map_err(|store_error| errors::store("platform_data", &store_error.into()))?;
        inner.revision += 1;
        Ok(())
    }

    pub fn complete_task(
        &self,
        task_id: &str,
        owner: &str,
        summary: &VerifySummary,
        status: AggregateStatus,
    ) -> Result<(), IpcError> {
        let mut inner = self.lock();
        let now = self.clock.now_ms();
        let attempt = inner.attempt_of(task_id)?;
        let payload = serde_json::to_string(summary).unwrap_or_else(|_| "{}".to_owned());
        inner.append_checkpoint(
            owner,
            task_id,
            "verify",
            attempt,
            "succeeded",
            &payload,
            false,
            now,
        )?;
        let state = match status {
            AggregateStatus::Succeeded => RepoTaskState::Succeeded,
            AggregateStatus::Partial => RepoTaskState::Partial,
            AggregateStatus::Skipped => RepoTaskState::Skipped,
            AggregateStatus::Failed | AggregateStatus::RetryableFailed => {
                RepoTaskState::RetryableFailed
            }
        };
        inner.set_task_state(task_id, state, None, now)?;
        inner.refresh_batch_rollup(task_id, now)?;
        inner
            .store
            .leases()
            .release(task_id, owner, now)
            .map_err(|store_error| errors::store("verify", &store_error))?;
        inner.revision += 1;
        Ok(())
    }
}

impl Inner {
    fn next_id(&mut self, kind: &str, now_ms: i64) -> String {
        self.seq += 1;
        format!("{kind}-{now_ms:x}-{:x}", self.seq)
    }

    fn attempt_of(&self, task_id: &str) -> Result<i64, IpcError> {
        self.store
            .connection()
            .query_row(
                "SELECT attempt FROM repository_task WHERE id = ?1",
                params![task_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| errors::store("queue", &error.into()))?
            .ok_or_else(|| errors::not_found("queue", "任务"))
    }

    #[allow(clippy::too_many_arguments)]
    fn append_checkpoint(
        &self,
        owner: &str,
        task_id: &str,
        stage: &str,
        attempt: i64,
        transition: &str,
        output: &str,
        resumable: bool,
        now_ms: i64,
    ) -> Result<(), IpcError> {
        let input_hash = format!(
            "{:x}",
            Sha256::digest(format!("{task_id}:{stage}:{attempt}"))
        );
        // The idempotency key makes a replayed transition a no-op instead of a
        // second logical write.
        let idempotency_key = format!("{stage}:{attempt}:{transition}:{now_ms}");
        let id = format!(
            "checkpoint-{:x}",
            Sha256::digest(format!("{task_id}:{idempotency_key}"))
        );
        self.store
            .checkpoints()
            .append(
                owner,
                now_ms,
                &AppendCheckpoint {
                    id: &id,
                    task_id,
                    stage,
                    attempt,
                    transition,
                    input_hash: &input_hash,
                    output_summary_json: output,
                    resumable,
                    idempotency_key: &idempotency_key,
                    created_at_ms: now_ms,
                },
            )
            .map(|_| ())
            .map_err(|error| errors::store(stage, &error))
    }

    fn set_task_state(
        &self,
        task_id: &str,
        state: RepoTaskState,
        error_code: Option<&str>,
        now_ms: i64,
    ) -> Result<(), IpcError> {
        self.store
            .connection()
            .execute(
                "UPDATE repository_task SET status = ?2, error_code = ?3, updated_at_ms = ?4
                 WHERE id = ?1",
                params![
                    task_id,
                    snapshot::task_state_value(state),
                    error_code,
                    now_ms
                ],
            )
            .map(|_| ())
            .map_err(|error| errors::store("queue", &error.into()))
    }

    fn append_log(
        &self,
        task_id: &str,
        level: &str,
        stage: &str,
        error: &IpcError,
        now_ms: i64,
    ) -> Result<(), IpcError> {
        let context = serde_json::to_string(error).unwrap_or_else(|_| "{}".to_owned());
        self.store
            .connection()
            .execute(
                "INSERT INTO log_event (task_id, level, stage, message_code, safe_context_json, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![task_id, level, stage, error.code, context, now_ms],
            )
            .map(|_| ())
            .map_err(|store_error| errors::store(stage, &store_error.into()))
    }

    /// Recomputes the batch roll-up from the task rows. Deriving instead of
    /// incrementing keeps the counters correct across retries and duplicate
    /// stage reports.
    fn refresh_batch_rollup(&self, task_id: &str, now_ms: i64) -> Result<(), IpcError> {
        self.store
            .connection()
            .execute(
                "UPDATE batch
                 SET completed = (SELECT COUNT(*) FROM repository_task t
                                  WHERE t.batch_id = batch.id
                                    AND t.status IN ('succeeded','partial','skipped')),
                     failed = (SELECT COUNT(*) FROM repository_task t
                               WHERE t.batch_id = batch.id
                                 AND t.status = 'retryable_failed'),
                     status = CASE
                        WHEN status = 'cancelled' THEN 'cancelled'
                        WHEN (SELECT COUNT(*) FROM repository_task t
                              WHERE t.batch_id = batch.id
                                AND t.status NOT IN ('succeeded','partial','skipped','retryable_failed')) = 0
                            THEN 'completed'
                        ELSE status
                     END,
                     ended_at_ms = CASE
                        WHEN (SELECT COUNT(*) FROM repository_task t
                              WHERE t.batch_id = batch.id
                                AND t.status NOT IN ('succeeded','partial','skipped','retryable_failed')) = 0
                            THEN ?2
                        ELSE ended_at_ms
                     END
                 WHERE id = (SELECT batch_id FROM repository_task WHERE id = ?1)",
                params![task_id, now_ms],
            )
            .map(|_| ())
            .map_err(|error| errors::store("queue", &error.into()))
    }
}

// ---------------------------------------------------------------------------
// Validation and derivation helpers
// ---------------------------------------------------------------------------

fn enum_text<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        _ => String::new(),
    }
}

fn platform_value(platform: PlatformKind) -> String {
    enum_text(&platform)
}

fn validate_endpoint(endpoint: &str) -> Result<String, IpcError> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() {
        return Err(errors::validation(
            "connection",
            "服务地址为空",
            "请输入形如 https://gitlab.example.com 的地址",
        ));
    }
    let parsed = Url::parse(trimmed).map_err(|_| {
        errors::validation(
            "connection",
            "服务地址格式不正确",
            "请输入形如 https://gitlab.example.com 的地址",
        )
    })?;
    if !matches!(parsed.scheme(), "https" | "http" | "ssh") {
        return Err(errors::validation(
            "connection",
            format!("不支持的协议：{}", parsed.scheme()),
            "请使用 HTTPS、HTTP 或 SSH 地址",
        ));
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(errors::validation(
            "connection",
            "服务地址缺少主机名",
            "请补全主机名后重试",
        ));
    }
    // Inline credentials would end up in argv, logs and crash reports.
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(errors::validation(
            "connection",
            "服务地址不得包含用户名或密码",
            "请移除地址中的凭据，令牌只保存在 Windows 凭据管理器",
        ));
    }
    Ok(trimmed.trim_end_matches('/').to_owned())
}

fn validate_target_url(target_url: &str) -> Result<String, IpcError> {
    git_repo_migrator_platform_generic::GenericGitUrl::parse(target_url)
        .map(|url| url.as_str().to_owned())
        .map_err(|error| {
            errors::validation(
                "mapping",
                format!("目标地址无效：{error}"),
                "请输入不含凭据和查询参数的完整仓库地址",
            )
        })
}

/// A credential reference is an opaque handle. Anything that looks like a secret
/// is rejected at the boundary rather than stored.
fn validate_credential_ref(credential_ref: Option<&str>) -> Result<(), IpcError> {
    let Some(value) = credential_ref else {
        return Ok(());
    };
    if value.trim().is_empty() {
        return Err(errors::validation(
            "connection",
            "凭据引用为空",
            "请先在凭据管理器中保存令牌，再选择该凭据",
        ));
    }
    if value.len() > 256 {
        return Err(errors::validation(
            "connection",
            "凭据引用过长",
            "请重新选择凭据",
        ));
    }
    let lowered = value.to_ascii_lowercase();
    for marker in ["ghp_", "glpat-", "bearer ", "token=", "password="] {
        if lowered.contains(marker) {
            return Err(errors::validation(
                "connection",
                "凭据引用看起来是一个明文令牌",
                "请只传递凭据引用；令牌必须保存在 Windows 凭据管理器",
            ));
        }
    }
    Ok(())
}

fn validate_fingerprint(fingerprint: &str) -> Result<(), IpcError> {
    let normalized = fingerprint.replace(':', "");
    if normalized.len() != 64 || !normalized.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(errors::validation(
            "connection",
            "证书指纹必须是 64 位十六进制 SHA-256 值",
            "请从服务器证书复制完整 SHA-256 指纹",
        ));
    }
    Ok(())
}

fn validate_export_path(path: &str, format: &str) -> Result<PathBuf, IpcError> {
    let candidate = Path::new(path);
    if !candidate.is_absolute() {
        return Err(errors::validation(
            "report",
            "导出路径必须是绝对路径",
            "请使用文件对话框重新选择导出位置",
        ));
    }
    if path.contains("..") {
        return Err(errors::validation(
            "report",
            "导出路径不得包含上级目录片段",
            "请重新选择导出位置",
        ));
    }
    let expected = match format {
        "json" => "json",
        _ => "csv",
    };
    let extension = candidate
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if extension != expected {
        return Err(errors::validation(
            "report",
            format!("导出文件扩展名应为 .{expected}"),
            format!("请把文件名改为 .{expected} 后重试"),
        ));
    }
    match candidate.parent() {
        Some(parent) if parent.is_dir() => Ok(candidate.to_path_buf()),
        _ => Err(errors::error(
            "ipc.export",
            ErrorCategory::Disk,
            true,
            "report",
            "导出目录不存在或不可访问",
            "请选择已存在的目录后重试",
        )),
    }
}

fn split_repository_path(url: &str) -> (String, String) {
    let trimmed = url.trim_end_matches('/').trim_end_matches(".git");
    let tail = trimmed
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(trimmed)
        .to_owned();
    let namespace = trimmed
        .strip_suffix(&tail)
        .map(|prefix| {
            prefix
                .trim_end_matches(['/', ':'])
                .rsplit(['/', ':'])
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .unwrap_or_default();
    (namespace, tail)
}

/// Capabilities we can assert without an HTTP transport. Generic Git is fully
/// determined by Git itself; API platforms report `unsupported` with a reason
/// rather than an optimistic guess.
fn capabilities_for(platform: PlatformKind) -> Vec<CapabilitySummary> {
    let native = |module: &str| CapabilitySummary {
        module: module.to_owned(),
        supported: true,
        permitted: true,
        required_scopes: vec![],
        fidelity: Fidelity::NativeRebuild,
        reason: None,
        degradation: None,
    };
    let unsupported = |module: &str, reason: &str| CapabilitySummary {
        module: module.to_owned(),
        supported: false,
        permitted: false,
        required_scopes: vec![],
        fidelity: Fidelity::Unsupported,
        reason: Some(reason.to_owned()),
        degradation: Some("该模块不会写入目标；将只迁移 Git 数据".to_owned()),
    };

    match platform {
        PlatformKind::GenericGit => {
            let mut capabilities = vec![
                native("git_read"),
                native("git_write"),
                native("lfs"),
                unsupported("discovery", "通用 Git 服务没有仓库发现 API"),
                unsupported("repository_creation", "通用 Git 服务没有建库 API"),
            ];
            for module in ["metadata", "issues", "pull_requests", "wiki", "releases"] {
                capabilities.push(unsupported(module, "通用 Git 服务没有平台数据 API"));
            }
            capabilities
        }
        other => {
            let reason = format!("{other:?} 的 API 能力探测依赖 HTTP 传输层，本版本尚未接入");
            let mut capabilities = vec![native("git_read"), native("git_write"), native("lfs")];
            for module in [
                "discovery",
                "repository_creation",
                "metadata",
                "issues",
                "pull_requests",
                "wiki",
                "releases",
            ] {
                capabilities.push(unsupported(module, &reason));
            }
            capabilities
        }
    }
}

/// Fingerprint of everything that can invalidate a frozen plan: which platform
/// the target is, which instance version answered, and what it can actually do.
/// Re-pointing the target elsewhere must change this even when the capability
/// rows happen to look alike.
fn capability_fingerprint(target: Option<&ConnectionSnapshot>) -> String {
    let Some(target) = target else {
        return "target:absent".to_owned();
    };
    let mut parts: Vec<String> = target
        .capabilities
        .iter()
        .map(|capability| {
            format!(
                "{}:{}:{}:{}",
                capability.module,
                capability.supported,
                capability.permitted,
                enum_text(&capability.fidelity)
            )
        })
        .collect();
    parts.sort();
    parts.insert(
        0,
        format!(
            "platform:{}|endpoint:{}|version:{}",
            enum_text(&target.platform),
            target.endpoint,
            target.instance_version.as_deref().unwrap_or("unknown")
        ),
    );
    parts.join("|")
}

/// Folds a per-task cleanup outcome into the batch-level report line (FR-011):
/// retention outranks a failed cleanup, which outranks a clean one, and the
/// first retained path is never overwritten by a later outcome.
fn merge_cleanup(current: &CleanupState, next: CleanupState) -> CleanupState {
    use CleanupState::*;
    match (current, next) {
        (RetainedTempDirectory { .. }, _) => current.clone(),
        (_, retained @ RetainedTempDirectory { .. }) => retained,
        (CleanupFailed { .. }, _) => current.clone(),
        (_, failed @ CleanupFailed { .. }) => failed,
        (Cleaned, Cleaned) => Cleaned,
    }
}

fn module_fidelity_rows(
    modules: &ModuleSelection,
    capabilities: &[CapabilitySummary],
) -> Vec<ModuleFidelityRow> {
    let selected = [
        ("lfs", modules.lfs),
        ("metadata", modules.metadata),
        ("issues", modules.issues),
        ("pull_requests", modules.pull_requests),
        ("wiki", modules.wiki),
        ("releases", modules.releases),
    ];
    debug_assert_eq!(selected.len(), OPTIONAL_MODULES.len());

    selected
        .into_iter()
        .filter(|(_, enabled)| *enabled)
        .map(|(module, _)| {
            let capability = capabilities
                .iter()
                .find(|capability| capability.module == module);
            let fidelity = match capability {
                Some(capability) if capability.supported && capability.permitted => {
                    capability.fidelity
                }
                Some(_) => Fidelity::Unsupported,
                None => Fidelity::Unsupported,
            };
            ModuleFidelityRow {
                module: module.to_owned(),
                fidelity,
                reason: capability.and_then(|capability| capability.reason.clone()),
                confirmation_required: !matches!(fidelity, Fidelity::NativeRebuild),
            }
        })
        .collect()
}

fn ref_policy_summary(policy: &RefPolicy) -> RefPolicySummary {
    let mut excluded = vec![
        "refs/pull/*".to_owned(),
        "refs/merge-requests/*".to_owned(),
        "refs/changes/*".to_owned(),
        "refs/keep-around/*".to_owned(),
        "refs/remotes/*".to_owned(),
    ];
    if policy.include_archived_refs {
        excluded.clear();
    }
    RefPolicySummary {
        mode: if policy.include_archived_refs {
            "include_read_only_archive_refs".to_owned()
        } else {
            "git_heads_tags_only".to_owned()
        },
        allowed_refspecs: vec![
            "refs/heads/*:refs/heads/*".to_owned(),
            "refs/tags/*:refs/tags/*".to_owned(),
        ],
        excluded_refs: excluded,
        explanation:
            "默认只迁移 refs/heads 与 refs/tags；平台私有 refs 与远程跟踪 refs 不会写入目标。"
                .to_owned(),
    }
}

fn decide_action(
    target_url: &str,
    target_state: TargetState,
    policy: &ConflictPolicy,
    target_can_create: bool,
    permission: PermissionLevel,
) -> (PlanAction, Option<String>, Option<String>) {
    if target_url.is_empty() {
        return (
            PlanAction::Blocked,
            Some("目标 URL 未设置".to_owned()),
            Some("请在映射页填写目标地址".to_owned()),
        );
    }
    if permission == PermissionLevel::Insufficient {
        return (
            PlanAction::Blocked,
            Some(format!("对 {target_url} 的权限不足")),
            Some("请提升凭据权限或排除该仓库".to_owned()),
        );
    }
    match target_state {
        TargetState::Unknown => (
            PlanAction::Blocked,
            Some(format!("目标状态待复检：{target_url}")),
            Some("请点击「探测目标」确认目标是否存在".to_owned()),
        ),
        TargetState::Inaccessible => (
            PlanAction::Blocked,
            Some(format!("目标不可访问：{target_url}")),
            Some("请检查目标凭据权限后重新探测".to_owned()),
        ),
        TargetState::Missing if target_can_create => (PlanAction::Create, None, None),
        TargetState::Missing => (
            PlanAction::Blocked,
            Some(format!("目标不存在且目标平台无建库能力：{target_url}")),
            Some("请先手动创建目标仓库，或配置显式建库脚本".to_owned()),
        ),
        TargetState::Empty if policy.reuse_empty => (PlanAction::ReuseEmpty, None, None),
        TargetState::Empty => (
            PlanAction::Blocked,
            Some(format!("目标为空但未启用空仓复用：{target_url}")),
            Some("请启用「空仓库复用」策略".to_owned()),
        ),
        TargetState::NonEmpty if policy.allow_overwrite => (PlanAction::Overwrite, None, None),
        TargetState::NonEmpty if policy.skip_non_empty => (
            PlanAction::SkipNonEmpty,
            None,
            Some("目标非空，默认跳过；如需继续请改名或启用覆盖".to_owned()),
        ),
        TargetState::NonEmpty => (
            PlanAction::Blocked,
            Some(format!("目标非空且未配置处理策略：{target_url}")),
            Some("请选择跳过、改名或显式启用覆盖".to_owned()),
        ),
    }
}

fn field_mapping_rows(repository: &RepositorySnapshot) -> Vec<FieldMappingRow> {
    vec![
        FieldMappingRow {
            field: "default_branch".to_owned(),
            source_value: None,
            target_value: None,
            result: "按 Git 推送结果继承".to_owned(),
        },
        FieldMappingRow {
            field: "visibility".to_owned(),
            source_value: Some(enum_text(&repository.visibility)),
            target_value: None,
            result: if repository.platform_capable {
                "将按源可见性设置".to_owned()
            } else {
                "目标平台不支持写入可见性；保持目标现状".to_owned()
            },
        },
        FieldMappingRow {
            field: "description".to_owned(),
            source_value: None,
            target_value: None,
            result: if repository.platform_capable {
                "将写入目标".to_owned()
            } else {
                "不支持：目标无平台数据写入能力".to_owned()
            },
        },
    ]
}

fn state_for_stage(stage: MigrationStage) -> RepoTaskState {
    match stage {
        MigrationStage::Preflight => RepoTaskState::Preflighted,
        MigrationStage::PrepareTarget => RepoTaskState::Preparing,
        MigrationStage::Git => RepoTaskState::Git,
        MigrationStage::Lfs => RepoTaskState::Lfs,
        MigrationStage::Metadata => RepoTaskState::Metadata,
        MigrationStage::PlatformData => RepoTaskState::PlatformModules,
        MigrationStage::Verify | MigrationStage::Complete => RepoTaskState::Verifying,
    }
}

#[allow(clippy::too_many_arguments)]
fn upsert_candidate(
    store: &LocalStore,
    id: &str,
    connection_id: Option<&str>,
    source_url: &str,
    name: &str,
    namespace: &str,
    visibility: RepositoryVisibility,
    permission: PermissionLevel,
    details: &CandidateDetails,
) -> Result<(), rusqlite::Error> {
    let metadata = serde_json::to_string(details).unwrap_or_else(|_| "{}".to_owned());
    store
        .connection()
        .execute(
            "INSERT INTO repository_candidate
                (id, connection_id, source_url, name, namespace, visibility, role, metadata_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                connection_id = excluded.connection_id,
                source_url = excluded.source_url,
                name = excluded.name,
                namespace = excluded.namespace,
                visibility = excluded.visibility,
                role = excluded.role",
            params![
                id,
                connection_id,
                source_url,
                name,
                namespace,
                enum_text(&visibility),
                enum_text(&permission),
                metadata,
            ],
        )
        .map(|_| ())
}
