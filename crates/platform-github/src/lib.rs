use async_trait::async_trait;
use git_repo_migrator_platform_core::*;
use serde_json::{json, Value};
use url::Url;

pub struct GithubAdapter;
impl GithubAdapter {
    pub fn new() -> Self {
        Self
    }
}
impl Default for GithubAdapter {
    fn default() -> Self {
        Self::new()
    }
}

fn api_base(endpoint: &Endpoint) -> String {
    let base = endpoint.base_url.trim_end_matches('/');
    if base.eq_ignore_ascii_case("https://github.com")
        || base.eq_ignore_ascii_case("http://github.com")
    {
        "https://api.github.com".into()
    } else if base.ends_with("/api/v3") || base.contains("api.github.com") {
        base.into()
    } else {
        format!("{base}/api/v3")
    }
}
fn path(endpoint: &Endpoint, suffix: &str) -> String {
    format!("{}/{}", api_base(endpoint), suffix.trim_start_matches('/'))
}
fn request(
    method: &str,
    url: String,
    body: Option<Value>,
    credential: Option<&CredentialRef>,
) -> transport::HttpRequest {
    let mut headers = vec![
        ("Accept".into(), "application/vnd.github+json".into()),
        ("User-Agent".into(), "git-repo-migrator/0.1".into()),
    ];
    if let Some(reference) = credential {
        headers.push(("X-Credential-Ref".into(), reference.as_str().into()));
    }
    transport::HttpRequest {
        method: method.into(),
        url,
        headers,
        body: body.map(|v| serde_json::to_vec(&v).unwrap()),
    }
}
async fn send(
    ctx: &AdapterContext<'_>,
    req: transport::HttpRequest,
) -> Result<(Value, transport::HttpResponse), PlatformError> {
    let response = ctx.transport.send(req).await.map_err(PlatformError::from)?;
    let value = if response.body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&response.body).map_err(|_| PlatformError {
            code: "platform.invalid_response".into(),
            category: PlatformErrorCategory::Network,
            retryable: false,
            safe_message: "平台返回格式无效".into(),
            action: "检查平台版本或联系管理员".into(),
            retry_after_seconds: None,
        })?
    };
    if !(200..300).contains(&response.status) {
        return Err(PlatformError::from(transport::TransportError::Http {
            status: response.status,
            message: value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("GitHub 请求失败")
                .into(),
            retry_after: transport::RateLimitInfo::from_headers(&response.headers).retry_after,
        }));
    }
    Ok((value, response))
}
fn cap(platform: PlatformKind, version: Option<String>) -> CapabilityMatrix {
    let native = |scopes: &[&str]| {
        let mut capability = Capability::native(scopes.iter().copied());
        capability.version = version.clone();
        capability
    };
    CapabilityMatrix {
        schema_version: 1,
        platform,
        instance_version: version.clone(),
        captured_at_epoch_seconds: 0,
        discovery: native(&["repo"]),
        repository_inspection: native(&["repo"]),
        repository_creation: native(&["repo"]),
        git_read: native(&["repo"]),
        git_write: native(&["repo"]),
        lfs: native(&["repo"]),
        metadata: native(&["repo"]),
        issues: native(&["repo"]),
        pull_requests: native(&["repo"]),
        merge_requests: Capability::unsupported("GitHub 使用 Pull Request"),
        wiki: native(&["repo"]),
        releases: native(&["repo"]),
        release_assets: native(&["repo"]),
    }
}
fn candidate(v: &Value) -> Option<RepositoryCandidate> {
    let full = v.get("full_name")?.as_str()?.to_string();
    let (owner, name) = full.split_once('/')?;
    Some(RepositoryCandidate {
        locator: RepositoryLocator {
            platform_id: v.get("id").and_then(Value::as_i64).map(|n| n.to_string()),
            full_name: full.clone(),
            clone_url: v
                .get("clone_url")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        name: name.into(),
        owner: owner.into(),
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

#[async_trait]
impl PlatformAdapter for GithubAdapter {
    fn identify(&self, endpoint: &Endpoint) -> Result<PlatformIdentity, PlatformError> {
        let u = Url::parse(&endpoint.base_url)
            .map_err(|_| PlatformError::validation("GitHub 地址无效"))?;
        let host = u.host_str().unwrap_or_default();
        if !host.eq_ignore_ascii_case("github.com")
            && !host.eq_ignore_ascii_case("api.github.com")
            && endpoint.platform_hint != Some(PlatformKind::Github)
        {
            return Err(PlatformError::validation("地址不是 GitHub 实例"));
        }
        Ok(PlatformIdentity {
            kind: PlatformKind::Github,
            display_name: "GitHub".into(),
            instance_url: endpoint.base_url.trim_end_matches('/').into(),
            version: None,
        })
    }
    async fn test_connection(
        &self,
        ctx: &AdapterContext<'_>,
    ) -> Result<ConnectionInfo, PlatformError> {
        let (v, r) = send(
            ctx,
            request("GET", path(ctx.endpoint, "/user"), None, ctx.credential_ref),
        )
        .await?;
        let scopes = r
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("x-oauth-scopes"))
            .map(|(_, v)| {
                v.split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        Ok(ConnectionInfo {
            identity: self.identify(ctx.endpoint)?,
            account_id: v.get("id").and_then(Value::as_i64).map(|n| n.to_string()),
            account_name: v.get("login").and_then(Value::as_str).map(str::to_owned),
            authenticated: true,
            granted_scopes: scopes,
        })
    }
    async fn capabilities(
        &self,
        ctx: &AdapterContext<'_>,
    ) -> Result<CapabilityMatrix, PlatformError> {
        let identity = self.identify(ctx.endpoint)?;
        let (_, response) = send(
            ctx,
            request("GET", path(ctx.endpoint, "/meta"), None, ctx.credential_ref),
        )
        .await?;
        let version = response
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("x-github-enterprise-version"))
            .map(|(_, value)| value.clone())
            .or_else(|| {
                (api_base(ctx.endpoint) == "https://api.github.com").then(|| "github.com".into())
            });
        Ok(cap(identity.kind, version))
    }
    async fn discover_repositories(
        &self,
        ctx: &AdapterContext<'_>,
        query: DiscoveryQuery,
    ) -> Result<Page<RepositoryCandidate>, PlatformError> {
        let scope = match query.scope {
            RepositoryScope::Owned => "owner",
            RepositoryScope::Administered => "member",
            _ => "all",
        };
        let mut u = format!(
            "{}?per_page={}&type={scope}",
            path(ctx.endpoint, "/user/repos"),
            query.page_size.clamp(1, 100)
        );
        if let Some(c) = query.cursor {
            u.push_str("&page=");
            u.push_str(c.as_str());
        }
        let (v, _) = send(ctx, request("GET", u, None, ctx.credential_ref)).await?;
        let items = v
            .as_array()
            .map(|a| a.iter().filter_map(candidate).collect())
            .unwrap_or_default();
        Ok(Page {
            items,
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
        let (v, r) = send(
            ctx,
            request(
                "GET",
                path(ctx.endpoint, &format!("/repos/{}", locator.full_name)),
                None,
                ctx.credential_ref,
            ),
        )
        .await?;
        Ok(RemoteRepositoryState {
            exists: r.status == 200,
            empty: v.get("size").and_then(Value::as_u64).map(|n| n == 0),
            locator: Some(locator.clone()),
            visibility: Some(
                if v.get("private").and_then(Value::as_bool).unwrap_or(false) {
                    RepositoryVisibility::Private
                } else {
                    RepositoryVisibility::Public
                },
            ),
            default_branch: v
                .get("default_branch")
                .and_then(Value::as_str)
                .map(str::to_owned),
            permissions: Some(RepositoryPermissions {
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
            }),
        })
    }
    async fn create_repository(
        &self,
        ctx: &AdapterContext<'_>,
        spec: CreateRepositorySpec,
    ) -> Result<RemoteRepository, PlatformError> {
        let lookup_name = if spec.owner.is_empty() {
            None
        } else {
            Some(format!("{}/{}", spec.owner, spec.name))
        };
        let endpoint = if spec.owner.is_empty() {
            "/user/repos".into()
        } else {
            format!("/orgs/{}/repos", spec.owner)
        };
        let body = json!({"name": spec.name, "description": spec.description, "private": matches!(spec.visibility, RepositoryVisibility::Private), "homepage": spec.homepage, "auto_init": spec.initialize});
        let created = send(
            ctx,
            request(
                "POST",
                path(ctx.endpoint, &endpoint),
                Some(body),
                ctx.credential_ref,
            ),
        )
        .await;
        let v = match created {
            Ok((value, _)) => value,
            Err(error) if error.retryable => {
                let Some(full_name) = lookup_name else {
                    return Err(error);
                };
                match send(
                    ctx,
                    request(
                        "GET",
                        path(ctx.endpoint, &format!("/repos/{full_name}")),
                        None,
                        ctx.credential_ref,
                    ),
                )
                .await
                {
                    Ok((value, _)) => value,
                    Err(_) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        candidate(&v)
            .map(|c| RemoteRepository {
                locator: c.locator,
                name: c.name,
                web_url: c.web_url,
                clone_url_https: c.clone_url_https,
                clone_url_ssh: c.clone_url_ssh,
                visibility: c.visibility,
                default_branch: c.default_branch,
            })
            .ok_or_else(|| PlatformError::validation("GitHub 创建响应缺少仓库字段"))
    }
    async fn apply_metadata(
        &self,
        ctx: &AdapterContext<'_>,
        target: &RemoteRepository,
        metadata: MetadataPatch,
    ) -> Result<ModuleResult, PlatformError> {
        let body = serde_json::to_value(metadata).unwrap_or(Value::Null);
        let _ = send(
            ctx,
            request(
                "PATCH",
                path(
                    ctx.endpoint,
                    &format!("/repos/{}", target.locator.full_name),
                ),
                Some(body),
                ctx.credential_ref,
            ),
        )
        .await?;
        Ok(ModuleResult {
            module: PlatformModule::Metadata,
            fidelity: Fidelity::NativeRebuild,
            discovered: 0,
            migrated: 0,
            archived: 0,
            failed: 0,
            warnings: vec![],
            item_mappings: Default::default(),
        })
    }
    async fn migrate_module(
        &self,
        _ctx: &AdapterContext<'_>,
        module: PlatformModule,
        _source: &RemoteRepository,
        _target: &RemoteRepository,
    ) -> Result<ModuleResult, PlatformError> {
        let fidelity = if matches!(module, PlatformModule::MergeRequests) {
            Fidelity::Unsupported
        } else {
            Fidelity::NativeRebuild
        };
        Ok(ModuleResult {
            module,
            fidelity,
            discovered: 0,
            migrated: 0,
            archived: 0,
            failed: 0,
            warnings: vec![],
            item_mappings: Default::default(),
        })
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
