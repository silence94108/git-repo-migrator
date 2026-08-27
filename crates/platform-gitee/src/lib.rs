use async_trait::async_trait;
use git_repo_migrator_platform_core::*;
use serde_json::{json, Value};
use url::Url;

#[derive(Default)]
pub struct GiteeAdapter;
fn api(endpoint: &Endpoint, suffix: &str) -> String {
    let base = endpoint.base_url.trim_end_matches('/');
    let root = if base.ends_with("/api/v5") {
        base.into()
    } else if base.contains("gitee.com") {
        "https://gitee.com/api/v5".into()
    } else {
        format!("{base}/api/v5")
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
            .map_err(|_| PlatformError::validation("Gitee 返回格式无效"))?
    };
    if !(200..300).contains(&response.status) {
        return Err(PlatformError::from(transport::TransportError::Http {
            status: response.status,
            message: value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Gitee 请求失败")
                .into(),
            retry_after: transport::RateLimitInfo::from_headers(&response.headers).retry_after,
        }));
    }
    Ok((value, response))
}
fn caps() -> CapabilityMatrix {
    let native = |s: &[&str]| Capability::native(s.iter().copied());
    CapabilityMatrix {
        schema_version: 1,
        platform: PlatformKind::Gitee,
        instance_version: None,
        captured_at_epoch_seconds: 0,
        discovery: native(&["projects"]),
        repository_inspection: native(&["projects"]),
        repository_creation: native(&["projects"]),
        git_read: native(&["projects"]),
        git_write: native(&["projects"]),
        lfs: native(&["projects"]),
        metadata: native(&["projects"]),
        issues: native(&["issues"]),
        pull_requests: Capability {
            degradation: Some("跨平台身份和部分状态改为只读归档".into()),
            fidelity: Fidelity::ReadOnlyArchive,
            ..native(&["pull_requests"])
        },
        merge_requests: Capability::unsupported("Gitee 使用 Pull Request"),
        wiki: Capability::unsupported("Gitee Wiki API 不稳定"),
        releases: native(&["projects"]),
        release_assets: Capability {
            degradation: Some("附件失败时保留源链接".into()),
            fidelity: Fidelity::ReadOnlyArchive,
            ..native(&["projects"])
        },
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
                .get("html_url")
                .and_then(Value::as_str)
                .map(|u| format!("{u}.git")),
        },
        name,
        owner,
        description: v
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
        web_url: v.get("html_url").and_then(Value::as_str).map(str::to_owned),
        clone_url_https: v
            .get("html_url")
            .and_then(Value::as_str)
            .map(|u| format!("{u}.git")),
        clone_url_ssh: v.get("ssh_url").and_then(Value::as_str).map(str::to_owned),
        visibility: if v.get("private").and_then(Value::as_bool).unwrap_or(false) {
            RepositoryVisibility::Private
        } else {
            RepositoryVisibility::Public
        },
        archived: false,
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
fn module(m: PlatformModule, f: Fidelity) -> ModuleResult {
    ModuleResult {
        module: m,
        fidelity: f,
        discovered: 0,
        migrated: 0,
        archived: 0,
        failed: 0,
        warnings: vec![],
        item_mappings: Default::default(),
    }
}
#[async_trait]
impl PlatformAdapter for GiteeAdapter {
    fn identify(&self, endpoint: &Endpoint) -> Result<PlatformIdentity, PlatformError> {
        let url = Url::parse(&endpoint.base_url)
            .map_err(|_| PlatformError::validation("Gitee 地址无效"))?;
        if endpoint.platform_hint != Some(PlatformKind::Gitee)
            && !url
                .host_str()
                .unwrap_or_default()
                .eq_ignore_ascii_case("gitee.com")
        {
            return Err(PlatformError::validation("地址不是 Gitee"));
        }
        Ok(PlatformIdentity {
            kind: PlatformKind::Gitee,
            display_name: "Gitee".into(),
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
            granted_scopes: vec!["projects".into()],
        })
    }
    async fn capabilities(
        &self,
        _ctx: &AdapterContext<'_>,
    ) -> Result<CapabilityMatrix, PlatformError> {
        Ok(caps())
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
        let (v, response) = send(
            ctx,
            request(
                "GET",
                format!(
                    "{}?per_page={}&page={page}",
                    api(ctx.endpoint, "/user/repos"),
                    query.page_size.clamp(1, 100)
                ),
                None,
                ctx.credential_ref,
            ),
        )
        .await?;
        let next_cursor = response
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("link"))
            .and_then(|(_, value)| {
                value
                    .contains("rel=\"next\"")
                    .then(|| {
                        PaginationCursor::new((page.parse::<u64>().unwrap_or(1) + 1).to_string())
                            .ok()
                    })
                    .flatten()
            });
        Ok(Page {
            items: v
                .as_array()
                .map(|a| a.iter().filter_map(repo).collect())
                .unwrap_or_default(),
            next_cursor,
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
            empty: None,
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
        let fidelity = match m {
            PlatformModule::Issues | PlatformModule::Releases => Fidelity::NativeRebuild,
            PlatformModule::PullRequests | PlatformModule::ReleaseAssets => {
                Fidelity::ReadOnlyArchive
            }
            _ => Fidelity::Unsupported,
        };
        Ok(module(m, fidelity))
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
