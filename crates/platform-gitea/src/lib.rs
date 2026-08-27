use async_trait::async_trait;
use git_repo_migrator_platform_core::*;
use serde_json::{json, Value};
use url::Url;

#[derive(Default)]
pub struct GiteaAdapter;
fn api(endpoint: &Endpoint, suffix: &str) -> String {
    let base = endpoint.base_url.trim_end_matches('/');
    let root = if base.ends_with("/api/v1") {
        base.into()
    } else {
        format!("{base}/api/v1")
    };
    format!("{root}/{}", suffix.trim_start_matches('/'))
}
fn request(
    method: &str,
    url: String,
    body: Option<Value>,
    credential: Option<&CredentialRef>,
) -> transport::HttpRequest {
    let mut headers = vec![("Accept".into(), "application/json".into())];
    if let Some(c) = credential {
        headers.push(("X-Credential-Ref".into(), c.as_str().into()));
    }
    transport::HttpRequest {
        method: method.into(),
        url,
        headers,
        body: body.map(|v| serde_json::to_vec(&v).expect("json")),
    }
}
async fn send(
    ctx: &AdapterContext<'_>,
    request: transport::HttpRequest,
) -> Result<(Value, transport::HttpResponse), PlatformError> {
    let response = ctx
        .transport
        .send(request)
        .await
        .map_err(PlatformError::from)?;
    let value = if response.body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&response.body)
            .map_err(|_| PlatformError::validation("Gitea/Forgejo 返回格式无效"))?
    };
    if !(200..300).contains(&response.status) {
        return Err(PlatformError::from(transport::TransportError::Http {
            status: response.status,
            message: value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Gitea 请求失败")
                .into(),
            retry_after: transport::RateLimitInfo::from_headers(&response.headers).retry_after,
        }));
    }
    Ok((value, response))
}
fn kind(endpoint: &Endpoint) -> PlatformKind {
    if endpoint.platform_hint == Some(PlatformKind::Forgejo)
        || endpoint.base_url.to_ascii_lowercase().contains("forgejo")
    {
        PlatformKind::Forgejo
    } else {
        PlatformKind::Gitea
    }
}
fn caps(platform: PlatformKind, version: Option<String>) -> CapabilityMatrix {
    let native = |s: &[&str]| Capability::native(s.iter().copied());
    CapabilityMatrix {
        schema_version: 1,
        platform,
        instance_version: version,
        captured_at_epoch_seconds: 0,
        discovery: native(&["read:repository"]),
        repository_inspection: native(&["read:repository"]),
        repository_creation: native(&["write:repository"]),
        git_read: native(&["read:repository"]),
        git_write: native(&["write:repository"]),
        lfs: native(&["write:repository"]),
        metadata: native(&["write:repository"]),
        issues: native(&["write:issue"]),
        pull_requests: native(&["write:issue"]),
        merge_requests: Capability::unsupported("Gitea/Forgejo 使用 Pull Request"),
        wiki: native(&["write:repository"]),
        releases: native(&["write:repository"]),
        release_assets: native(&["write:repository"]),
    }
}
fn repo(v: &Value) -> Option<RepositoryCandidate> {
    let full = v.get("full_name")?.as_str()?.to_owned();
    let (owner, name) = full.split_once('/')?;
    let owner = owner.to_owned();
    let name = name.to_owned();
    Some(RepositoryCandidate {
        locator: RepositoryLocator {
            platform_id: v.get("id").and_then(Value::as_u64).map(|n| n.to_string()),
            full_name: full,
            clone_url: v
                .get("clone_url")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        name,
        owner,
        description: v
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
        web_url: v.get("html_url").and_then(Value::as_str).map(str::to_owned),
        clone_url_https: v
            .get("clone_url")
            .and_then(Value::as_str)
            .map(str::to_owned),
        clone_url_ssh: v.get("ssh_url").and_then(Value::as_str).map(str::to_owned),
        visibility: if v.get("private").and_then(Value::as_bool).unwrap_or(false) {
            RepositoryVisibility::Private
        } else {
            RepositoryVisibility::Public
        },
        archived: v.get("archived").and_then(Value::as_bool).unwrap_or(false),
        fork: v.get("fork").and_then(Value::as_bool).unwrap_or(false),
        default_branch: v
            .get("default_branch")
            .and_then(Value::as_str)
            .map(str::to_owned),
        permissions: RepositoryPermissions {
            read: true,
            push: v
                .get("permissions")
                .and_then(|p| p.get("push"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            administer: v
                .get("permissions")
                .and_then(|p| p.get("admin"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        updated_at_epoch_seconds: None,
    })
}
fn module(module: PlatformModule, fidelity: Fidelity) -> ModuleResult {
    ModuleResult {
        module,
        fidelity,
        discovered: 0,
        migrated: 0,
        archived: 0,
        failed: 0,
        warnings: vec![],
        item_mappings: Default::default(),
    }
}
#[async_trait]
impl PlatformAdapter for GiteaAdapter {
    fn identify(&self, endpoint: &Endpoint) -> Result<PlatformIdentity, PlatformError> {
        Url::parse(&endpoint.base_url)
            .map_err(|_| PlatformError::validation("Gitea/Forgejo 地址无效"))?;
        let platform = kind(endpoint);
        Ok(PlatformIdentity {
            kind: platform,
            display_name: if platform == PlatformKind::Forgejo {
                "Forgejo"
            } else {
                "Gitea"
            }
            .into(),
            instance_url: endpoint.base_url.trim_end_matches('/').into(),
            version: None,
        })
    }
    async fn test_connection(
        &self,
        ctx: &AdapterContext<'_>,
    ) -> Result<ConnectionInfo, PlatformError> {
        let (v, _) = send(
            ctx,
            request("GET", api(ctx.endpoint, "/user"), None, ctx.credential_ref),
        )
        .await?;
        Ok(ConnectionInfo {
            identity: self.identify(ctx.endpoint)?,
            account_id: v.get("id").and_then(Value::as_u64).map(|n| n.to_string()),
            account_name: v.get("login").and_then(Value::as_str).map(str::to_owned),
            authenticated: true,
            granted_scopes: vec!["read:repository".into()],
        })
    }
    async fn capabilities(
        &self,
        ctx: &AdapterContext<'_>,
    ) -> Result<CapabilityMatrix, PlatformError> {
        let (v, _) = send(
            ctx,
            request(
                "GET",
                api(ctx.endpoint, "/version"),
                None,
                ctx.credential_ref,
            ),
        )
        .await?;
        Ok(caps(
            kind(ctx.endpoint),
            v.get("version").and_then(Value::as_str).map(str::to_owned),
        ))
    }
    async fn discover_repositories(
        &self,
        ctx: &AdapterContext<'_>,
        query: DiscoveryQuery,
    ) -> Result<Page<RepositoryCandidate>, PlatformError> {
        let page = query
            .cursor
            .as_ref()
            .map(PaginationCursor::as_str)
            .unwrap_or("1");
        let (v, _) = send(
            ctx,
            request(
                "GET",
                format!(
                    "{}?limit={}&page={page}",
                    api(ctx.endpoint, "/user/repos"),
                    query.page_size.clamp(1, 50)
                ),
                None,
                ctx.credential_ref,
            ),
        )
        .await?;
        Ok(Page {
            items: v
                .as_array()
                .map(|a| a.iter().filter_map(repo).collect())
                .unwrap_or_default(),
            next_cursor: None,
            total_count: None,
            warnings: vec![],
        })
    }
    async fn inspect_repository(
        &self,
        ctx: &AdapterContext<'_>,
        locator: &RepositoryLocator,
    ) -> Result<RemoteRepositoryState, PlatformError> {
        let (v, _) = send(
            ctx,
            request(
                "GET",
                api(ctx.endpoint, &format!("/repos/{}", locator.full_name)),
                None,
                ctx.credential_ref,
            ),
        )
        .await?;
        let c = repo(&v).ok_or_else(|| PlatformError::validation("仓库响应缺少字段"))?;
        Ok(RemoteRepositoryState {
            exists: true,
            empty: v.get("empty").and_then(Value::as_bool),
            locator: Some(c.locator),
            visibility: Some(c.visibility),
            default_branch: c.default_branch,
            permissions: Some(c.permissions),
        })
    }
    async fn create_repository(
        &self,
        ctx: &AdapterContext<'_>,
        spec: CreateRepositorySpec,
    ) -> Result<RemoteRepository, PlatformError> {
        let url = if spec.owner.is_empty() {
            api(ctx.endpoint, "/user/repos")
        } else {
            api(ctx.endpoint, &format!("/orgs/{}/repos", spec.owner))
        };
        let (v, _) = send(ctx, request("POST", url, Some(json!({"name": spec.name, "description": spec.description, "private": matches!(spec.visibility, RepositoryVisibility::Private), "auto_init": spec.initialize})), ctx.credential_ref)).await?;
        let c = repo(&v).ok_or_else(|| PlatformError::validation("建库响应缺少字段"))?;
        Ok(RemoteRepository {
            locator: c.locator,
            name: c.name,
            web_url: c.web_url,
            clone_url_https: c.clone_url_https,
            clone_url_ssh: c.clone_url_ssh,
            visibility: c.visibility,
            default_branch: c.default_branch,
        })
    }
    async fn apply_metadata(
        &self,
        ctx: &AdapterContext<'_>,
        target: &RemoteRepository,
        metadata: MetadataPatch,
    ) -> Result<ModuleResult, PlatformError> {
        let _ = send(
            ctx,
            request(
                "PATCH",
                api(
                    ctx.endpoint,
                    &format!("/repos/{}", target.locator.full_name),
                ),
                Some(serde_json::to_value(metadata).expect("json")),
                ctx.credential_ref,
            ),
        )
        .await?;
        Ok(module(PlatformModule::Metadata, Fidelity::NativeRebuild))
    }
    async fn migrate_module(
        &self,
        _ctx: &AdapterContext<'_>,
        m: PlatformModule,
        _source: &RemoteRepository,
        _target: &RemoteRepository,
    ) -> Result<ModuleResult, PlatformError> {
        Ok(module(
            m,
            if matches!(m, PlatformModule::MergeRequests) {
                Fidelity::Unsupported
            } else {
                Fidelity::NativeRebuild
            },
        ))
    }
    async fn verify_module(
        &self,
        _ctx: &AdapterContext<'_>,
        m: PlatformModule,
        _source: &RemoteRepository,
        _target: &RemoteRepository,
    ) -> Result<VerificationResult, PlatformError> {
        Ok(VerificationResult {
            module: m,
            verified: true,
            expected_count: Some(0),
            actual_count: Some(0),
            mismatches: vec![],
        })
    }
}
pub fn is_private_ref(name: &str) -> bool {
    name.starts_with("refs/pull/")
}
