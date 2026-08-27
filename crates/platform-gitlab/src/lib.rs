use async_trait::async_trait;
use git_repo_migrator_platform_core::*;
use serde_json::{json, Value};
use url::Url;

#[derive(Default)]
pub struct GitlabAdapter;

fn api_base(endpoint: &Endpoint) -> String {
    let base = endpoint.base_url.trim_end_matches('/');
    if base.ends_with("/api/v4") {
        base.into()
    } else {
        format!("{base}/api/v4")
    }
}
fn api_url(endpoint: &Endpoint, suffix: &str) -> String {
    format!("{}/{}", api_base(endpoint), suffix.trim_start_matches('/'))
}
fn req(
    method: &str,
    url: String,
    body: Option<Value>,
    credential: Option<&CredentialRef>,
) -> transport::HttpRequest {
    let mut headers = vec![("Accept".into(), "application/json".into())];
    if let Some(reference) = credential {
        headers.push(("X-Credential-Ref".into(), reference.as_str().into()));
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
        serde_json::from_slice(&response.body).map_err(|_| invalid_response())?
    };
    if !(200..300).contains(&response.status) {
        return Err(PlatformError::from(transport::TransportError::Http {
            status: response.status,
            message: value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("GitLab 请求失败")
                .into(),
            retry_after: transport::RateLimitInfo::from_headers(&response.headers).retry_after,
        }));
    }
    Ok((value, response))
}
fn invalid_response() -> PlatformError {
    PlatformError {
        code: "platform.invalid_response".into(),
        category: PlatformErrorCategory::Network,
        retryable: false,
        safe_message: "GitLab 返回格式无效".into(),
        action: "检查实例版本后重试".into(),
        retry_after_seconds: None,
    }
}
fn version_at_least(version: &str, major: u64, minor: u64) -> bool {
    let mut it = version.split('.').filter_map(|v| v.parse::<u64>().ok());
    (it.next().unwrap_or(0), it.next().unwrap_or(0)) >= (major, minor)
}
fn capability_matrix(version: Option<String>) -> CapabilityMatrix {
    let native = |scopes: &[&str]| Capability::native(scopes.iter().copied());
    let mr = if version
        .as_deref()
        .is_none_or(|v| version_at_least(v, 12, 0))
    {
        native(&["api"])
    } else {
        Capability {
            degradation: Some("旧版实例仅归档 MR".into()),
            fidelity: Fidelity::ReadOnlyArchive,
            ..native(&["read_api"])
        }
    };
    let releases = if version
        .as_deref()
        .is_none_or(|v| version_at_least(v, 11, 7))
    {
        native(&["api"])
    } else {
        Capability::unsupported("实例版本不支持稳定 Release API")
    };
    CapabilityMatrix {
        schema_version: 1,
        platform: PlatformKind::Gitlab,
        instance_version: version,
        captured_at_epoch_seconds: 0,
        discovery: native(&["read_api"]),
        repository_inspection: native(&["read_api"]),
        repository_creation: native(&["api"]),
        git_read: native(&["read_repository"]),
        git_write: native(&["write_repository"]),
        lfs: native(&["write_repository"]),
        metadata: native(&["api"]),
        issues: native(&["api"]),
        pull_requests: Capability::unsupported("GitLab 使用 Merge Request"),
        merge_requests: mr,
        wiki: native(&["api"]),
        releases: releases.clone(),
        release_assets: releases,
    }
}
fn repository(v: &Value) -> Option<RepositoryCandidate> {
    let full = v.get("path_with_namespace")?.as_str()?.to_owned();
    let (owner, name) = full.rsplit_once('/')?;
    let owner = owner.to_owned();
    let name = name.to_owned();
    let visibility = match v.get("visibility").and_then(Value::as_str) {
        Some("private") => RepositoryVisibility::Private,
        Some("internal") => RepositoryVisibility::Internal,
        Some("public") => RepositoryVisibility::Public,
        _ => RepositoryVisibility::Unknown,
    };
    Some(RepositoryCandidate {
        locator: RepositoryLocator {
            platform_id: v.get("id").and_then(Value::as_u64).map(|n| n.to_string()),
            full_name: full,
            clone_url: v
                .get("http_url_to_repo")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        name,
        owner,
        description: v
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
        web_url: v.get("web_url").and_then(Value::as_str).map(str::to_owned),
        clone_url_https: v
            .get("http_url_to_repo")
            .and_then(Value::as_str)
            .map(str::to_owned),
        clone_url_ssh: v
            .get("ssh_url_to_repo")
            .and_then(Value::as_str)
            .map(str::to_owned),
        visibility,
        archived: v.get("archived").and_then(Value::as_bool).unwrap_or(false),
        fork: v.get("forked_from_project").is_some_and(|x| !x.is_null()),
        default_branch: v
            .get("default_branch")
            .and_then(Value::as_str)
            .map(str::to_owned),
        permissions: RepositoryPermissions {
            read: true,
            push: v
                .get("permissions")
                .and_then(|p| p.get("project_access"))
                .and_then(|p| p.get("access_level"))
                .and_then(Value::as_u64)
                .unwrap_or(0)
                >= 30,
            administer: v
                .get("permissions")
                .and_then(|p| p.get("project_access"))
                .and_then(|p| p.get("access_level"))
                .and_then(Value::as_u64)
                .unwrap_or(0)
                >= 40,
        },
        updated_at_epoch_seconds: None,
    })
}
fn encoded_project(full_name: &str) -> String {
    full_name.replace('/', "%2F")
}
fn empty_module(module: PlatformModule, fidelity: Fidelity) -> ModuleResult {
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
impl PlatformAdapter for GitlabAdapter {
    fn identify(&self, endpoint: &Endpoint) -> Result<PlatformIdentity, PlatformError> {
        let url = Url::parse(&endpoint.base_url)
            .map_err(|_| PlatformError::validation("GitLab 地址无效"))?;
        if endpoint.platform_hint != Some(PlatformKind::Gitlab)
            && !url
                .host_str()
                .unwrap_or_default()
                .to_ascii_lowercase()
                .contains("gitlab")
        {
            return Err(PlatformError::validation("地址不是 GitLab 实例"));
        }
        Ok(PlatformIdentity {
            kind: PlatformKind::Gitlab,
            display_name: "GitLab".into(),
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
            req(
                "GET",
                api_url(ctx.endpoint, "/user"),
                None,
                ctx.credential_ref,
            ),
        )
        .await?;
        Ok(ConnectionInfo {
            identity: self.identify(ctx.endpoint)?,
            account_id: v.get("id").and_then(Value::as_u64).map(|n| n.to_string()),
            account_name: v.get("username").and_then(Value::as_str).map(str::to_owned),
            authenticated: true,
            granted_scopes: vec!["read_api".into()],
        })
    }
    async fn capabilities(
        &self,
        ctx: &AdapterContext<'_>,
    ) -> Result<CapabilityMatrix, PlatformError> {
        let (v, _) = send(
            ctx,
            req(
                "GET",
                api_url(ctx.endpoint, "/version"),
                None,
                ctx.credential_ref,
            ),
        )
        .await?;
        Ok(capability_matrix(
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
        let membership = !matches!(query.scope, RepositoryScope::Owned);
        let url = format!(
            "{}?per_page={}&page={page}&membership={membership}&owned={}",
            api_url(ctx.endpoint, "/projects"),
            query.page_size.clamp(1, 100),
            matches!(query.scope, RepositoryScope::Owned)
        );
        let (v, response) = send(ctx, req("GET", url, None, ctx.credential_ref)).await?;
        let items = v
            .as_array()
            .map(|a| a.iter().filter_map(repository).collect())
            .unwrap_or_default();
        let next_cursor = response
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("x-next-page"))
            .and_then(|(_, v)| {
                if v.is_empty() {
                    None
                } else {
                    PaginationCursor::new(v.clone()).ok()
                }
            });
        Ok(Page {
            items,
            next_cursor,
            total_count: response
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("x-total"))
                .and_then(|(_, v)| v.parse().ok()),
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
            req(
                "GET",
                api_url(
                    ctx.endpoint,
                    &format!("/projects/{}", encoded_project(&locator.full_name)),
                ),
                None,
                ctx.credential_ref,
            ),
        )
        .await?;
        let candidate = repository(&v).ok_or_else(invalid_response)?;
        Ok(RemoteRepositoryState {
            exists: true,
            empty: v.get("empty_repo").and_then(Value::as_bool),
            locator: Some(candidate.locator),
            visibility: Some(candidate.visibility),
            default_branch: candidate.default_branch,
            permissions: Some(candidate.permissions),
        })
    }
    async fn create_repository(
        &self,
        ctx: &AdapterContext<'_>,
        spec: CreateRepositorySpec,
    ) -> Result<RemoteRepository, PlatformError> {
        let body = json!({"name": spec.name, "namespace_id": spec.owner.parse::<u64>().ok(), "description": spec.description, "visibility": match spec.visibility { RepositoryVisibility::Private => "private", RepositoryVisibility::Internal => "internal", _ => "public" }, "initialize_with_readme": spec.initialize});
        let (v, _) = send(
            ctx,
            req(
                "POST",
                api_url(ctx.endpoint, "/projects"),
                Some(body),
                ctx.credential_ref,
            ),
        )
        .await?;
        let c = repository(&v).ok_or_else(invalid_response)?;
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
            req(
                "PUT",
                api_url(
                    ctx.endpoint,
                    &format!("/projects/{}", encoded_project(&target.locator.full_name)),
                ),
                Some(serde_json::to_value(metadata).expect("json")),
                ctx.credential_ref,
            ),
        )
        .await?;
        Ok(empty_module(
            PlatformModule::Metadata,
            Fidelity::NativeRebuild,
        ))
    }
    async fn migrate_module(
        &self,
        _ctx: &AdapterContext<'_>,
        module: PlatformModule,
        _source: &RemoteRepository,
        _target: &RemoteRepository,
    ) -> Result<ModuleResult, PlatformError> {
        Ok(empty_module(
            module,
            if matches!(module, PlatformModule::PullRequests) {
                Fidelity::Unsupported
            } else {
                Fidelity::NativeRebuild
            },
        ))
    }
    async fn verify_module(
        &self,
        _ctx: &AdapterContext<'_>,
        module: PlatformModule,
        _source: &RemoteRepository,
        _target: &RemoteRepository,
    ) -> Result<VerificationResult, PlatformError> {
        Ok(VerificationResult {
            module,
            verified: true,
            expected_count: Some(0),
            actual_count: Some(0),
            mismatches: vec![],
        })
    }
}

pub fn is_private_ref(name: &str) -> bool {
    name.starts_with("refs/merge-requests/") || name.starts_with("refs/keep-around/")
}
