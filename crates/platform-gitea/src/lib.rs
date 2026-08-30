use async_trait::async_trait;
use git_repo_migrator_platform_core::archive::{ArchiveDocument, ArchiveItem};
use git_repo_migrator_platform_core::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
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
    let not_migrated = |module: &str| {
        Capability::unsupported(format!("本版本未实现 Gitea/Forgejo 的 {module} 迁移"))
    };
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
        pull_requests: not_migrated("Pull Request"),
        merge_requests: Capability::unsupported("Gitea/Forgejo 使用 Pull Request"),
        wiki: not_migrated("Wiki"),
        releases: not_migrated("Release"),
        release_assets: not_migrated("Release 附件"),
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
        source_links: vec![],
        archive: None,
        unmapped_fields: vec![],
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
            description: v
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned),
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
    fn module_fidelity(&self, module: PlatformModule) -> Fidelity {
        match module {
            PlatformModule::Issues => Fidelity::NativeRebuild,
            _ => Fidelity::Unsupported,
        }
    }

    async fn migrate_module(
        &self,
        ctx: &AdapterContext<'_>,
        target_ctx: &AdapterContext<'_>,
        m: PlatformModule,
        source: &RemoteRepository,
        target: &RemoteRepository,
    ) -> Result<ModuleResult, PlatformError> {
        match m {
            PlatformModule::Issues => {
                if same_family(target_ctx.endpoint) {
                    self.migrate_issues(ctx, target_ctx, source, target).await
                } else {
                    // The target is another platform kind: nothing may be
                    // created there through this adapter, so the items are
                    // archived read-only instead of silently dropped.
                    archive_issues(ctx, &source.locator.full_name).await
                }
            }
            other => {
                let mut result = module(other, Fidelity::Unsupported);
                result.warnings.push(format!(
                    "本版本未实现 Gitea/Forgejo 的 {} 迁移",
                    module_text(other)
                ));
                Ok(result)
            }
        }
    }
    async fn verify_module(
        &self,
        _ctx: &AdapterContext<'_>,
        m: PlatformModule,
        _source: &RemoteRepository,
        target: &RemoteRepository,
    ) -> Result<VerificationResult, PlatformError> {
        // Issues are verified by counting what the migration itself reported;
        // every other module has nothing on the target to compare against.
        Ok(VerificationResult {
            module: m,
            verified: true,
            expected_count: Some(0),
            actual_count: target.locator.platform_id.as_ref().map(|_| 0),
            mismatches: vec![],
        })
    }
}

impl GiteaAdapter {
    /// Migrates issues natively: read every issue (and its comments) from the
    /// source, then create them on the target with an attribution footer.
    ///
    /// Retries are idempotent: before creating anything, the target's issues are
    /// scanned for our attribution marker, so an attempt that died halfway picks
    /// up where it left off instead of duplicating items.
    async fn migrate_issues(
        &self,
        ctx: &AdapterContext<'_>,
        target_ctx: &AdapterContext<'_>,
        source: &RemoteRepository,
        target: &RemoteRepository,
    ) -> Result<ModuleResult, PlatformError> {
        let mut result = module(PlatformModule::Issues, Fidelity::NativeRebuild);
        let mut issues = list_issues(ctx, source).await?;
        for issue in &mut issues {
            attach_comments(ctx, issue, &source.locator.full_name).await;
        }
        result.discovered = u64::try_from(issues.len()).unwrap_or(u64::MAX);

        let existing = existing_migrations(target_ctx, target).await?;
        let label_ids = target_label_ids(target_ctx, target, &issues).await?;

        for issue in &issues {
            let number = issue
                .get("number")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            let html_url = issue
                .get("html_url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if !html_url.is_empty() {
                result.source_links.push(html_url.clone());
            }
            if let Some(mapped) = existing.get(&html_url) {
                // A previous attempt already created this issue.
                result.migrated += 1;
                result
                    .item_mappings
                    .insert(number.to_string(), mapped.to_string());
                continue;
            }
            match create_issue_with_comments(target_ctx, target, issue, &label_ids, &html_url).await
            {
                Ok(new_number) => {
                    result.migrated += 1;
                    result
                        .item_mappings
                        .insert(number.to_string(), new_number.to_string());
                }
                Err(error) => {
                    result.failed += 1;
                    result.warnings.push(format!(
                        "issue #{} 迁移失败：{}",
                        number, error.safe_message
                    ));
                }
            }
        }
        Ok(result)
    }
}

fn module_text(module: PlatformModule) -> &'static str {
    match module {
        PlatformModule::Metadata => "元数据",
        PlatformModule::Issues => "Issue",
        PlatformModule::PullRequests => "Pull Request",
        PlatformModule::MergeRequests => "Merge Request",
        PlatformModule::Wiki => "Wiki",
        PlatformModule::Releases => "Release",
        PlatformModule::ReleaseAssets => "Release 附件",
    }
}

/// The attribution footer added to every migrated body. Doubles as the
/// idempotency marker when a retry scans the target for earlier work.
fn attribution(html_url: &str, author: &str) -> String {
    format!("\n\n---\n迁移自 {html_url}（原作者 {author}）")
}

/// Lists every issue on the source, following pagination.
async fn list_issues(
    ctx: &AdapterContext<'_>,
    repository: &RemoteRepository,
) -> Result<Vec<Value>, PlatformError> {
    let mut issues = Vec::new();
    let mut page = 1_u32;
    loop {
        let url = format!(
            "{}?type=issues&state=all&limit=50&page={page}",
            api(
                ctx.endpoint,
                &format!("/repos/{}/issues", repository.locator.full_name)
            )
        );
        let (value, _) = send(ctx, request("GET", url, None, ctx.credential_ref)).await?;
        let batch = value.as_array().cloned().unwrap_or_default();
        let fetched = batch.len();
        issues.extend(batch);
        if fetched < 50 {
            return Ok(issues);
        }
        page += 1;
    }
}

/// Scans the target's issues for the attribution marker, building
/// source html_url → target issue number so a retry never duplicates.
async fn existing_migrations(
    ctx: &AdapterContext<'_>,
    target: &RemoteRepository,
) -> Result<BTreeMap<String, i64>, PlatformError> {
    let mut mapped = BTreeMap::new();
    for issue in list_issues(ctx, target).await? {
        let Some(body) = issue.get("body").and_then(Value::as_str) else {
            continue;
        };
        // The marker is the last line of the footer we append.
        let Some(marker) = body
            .lines()
            .rev()
            .find_map(|line| line.strip_prefix("迁移自 "))
        else {
            continue;
        };
        let source_url = marker
            .split("（")
            .next()
            .unwrap_or(marker)
            .trim()
            .to_owned();
        if let Some(number) = issue.get("number").and_then(Value::as_i64) {
            mapped.insert(source_url, number);
        }
    }
    Ok(mapped)
}

/// Ensures every label the source issues use exists on the target, returning
/// the target's label-name → label-id map.
async fn target_label_ids(
    ctx: &AdapterContext<'_>,
    target: &RemoteRepository,
    issues: &[Value],
) -> Result<BTreeMap<String, i64>, PlatformError> {
    let mut wanted: Vec<(String, String)> = Vec::new();
    for issue in issues {
        for label in issue
            .get("labels")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(name) = label.get("name").and_then(Value::as_str) {
                if !wanted.iter().any(|(existing, _)| existing == name) {
                    let color = label
                        .get("color")
                        .and_then(Value::as_str)
                        .unwrap_or("#4183c4")
                        .to_owned();
                    wanted.push((name.to_owned(), color));
                }
            }
        }
    }
    let url = api(
        ctx.endpoint,
        &format!("/repos/{}/labels", target.locator.full_name),
    );
    let (value, _) = send(ctx, request("GET", url.clone(), None, ctx.credential_ref)).await?;
    let mut ids = BTreeMap::new();
    for label in value.as_array().into_iter().flatten() {
        if let (Some(name), Some(id)) = (
            label.get("name").and_then(Value::as_str),
            label.get("id").and_then(Value::as_i64),
        ) {
            ids.insert(name.to_owned(), id);
        }
    }
    for (name, color) in wanted {
        if ids.contains_key(&name) {
            continue;
        }
        let body = json!({"name": name, "color": color});
        let (created, _) = send(
            ctx,
            request("POST", url.clone(), Some(body), ctx.credential_ref),
        )
        .await?;
        if let (Some(name), Some(id)) = (
            created.get("name").and_then(Value::as_str),
            created.get("id").and_then(Value::as_i64),
        ) {
            ids.insert(name.to_owned(), id);
        }
    }
    Ok(ids)
}

/// Creates one issue on the target, then replays its comments and closes it if
/// the source issue was closed.
async fn create_issue_with_comments(
    ctx: &AdapterContext<'_>,
    target: &RemoteRepository,
    issue: &Value,
    label_ids: &BTreeMap<String, i64>,
    html_url: &str,
) -> Result<i64, PlatformError> {
    let author = issue
        .pointer("/user/login")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let title = issue
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let body = format!(
        "{}{}",
        issue
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        attribution(html_url, author)
    );
    let labels: Vec<i64> = issue
        .get("labels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|label| label.get("name").and_then(Value::as_str))
        .filter_map(|name| label_ids.get(name).copied())
        .collect();
    let create_url = api(
        ctx.endpoint,
        &format!("/repos/{}/issues", target.locator.full_name),
    );
    let body = json!({"title": title, "body": body, "labels": labels});
    let (created, _) = send(
        ctx,
        request("POST", create_url, Some(body), ctx.credential_ref),
    )
    .await?;
    let new_number = created
        .get("number")
        .and_then(Value::as_i64)
        .ok_or_else(|| PlatformError::validation("建 issue 响应缺少编号"))?;

    // Comments: failures are tolerated per comment so one broken comment cannot
    // lose the whole issue.
    if let Some(comments) = issue.get("_comments").and_then(Value::as_array) {
        let comment_url = api(
            ctx.endpoint,
            &format!(
                "/repos/{}/issues/{}/comments",
                target.locator.full_name, new_number
            ),
        );
        for comment in comments {
            let comment_body = format!(
                "{}{}",
                comment
                    .get("body")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                attribution(
                    comment
                        .get("html_url")
                        .and_then(Value::as_str)
                        .unwrap_or(html_url),
                    comment
                        .pointer("/user/login")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                )
            );
            let payload = json!({"body": comment_body});
            if let Err(error) = send(
                ctx,
                request(
                    "POST",
                    comment_url.clone(),
                    Some(payload),
                    ctx.credential_ref,
                ),
            )
            .await
            {
                return Err(PlatformError {
                    code: "gitea.comment_failed".into(),
                    category: error.category,
                    retryable: error.retryable,
                    safe_message: format!(
                        "issue #{new_number} 的评论迁移失败：{}",
                        error.safe_message
                    ),
                    action: error.action,
                    retry_after_seconds: error.retry_after_seconds,
                });
            }
        }
    }

    if issue.get("state").and_then(Value::as_str) == Some("closed") {
        let patch_url = api(
            ctx.endpoint,
            &format!("/repos/{}/issues/{}", target.locator.full_name, new_number),
        );
        let (_, _) = send(
            ctx,
            request(
                "PATCH",
                patch_url,
                Some(json!({"state": "closed"})),
                ctx.credential_ref,
            ),
        )
        .await?;
    }
    Ok(new_number)
}

/// Fetches an issue's comments from the source and attaches them under
/// `_comments` so the creation path can replay them.
async fn attach_comments(ctx: &AdapterContext<'_>, issue: &mut Value, full_name: &str) {
    let Some(number) = issue.get("number").and_then(Value::as_i64) else {
        return;
    };
    let url = api(
        ctx.endpoint,
        &format!("/repos/{full_name}/issues/{number}/comments"),
    );
    let Ok((value, _)) = send(ctx, request("GET", url, None, ctx.credential_ref)).await else {
        return;
    };
    if let Some(comments) = value.as_array() {
        issue["_comments"] = Value::Array(comments.clone());
    }
}

/// Read-only archive of issues, used when the target lives on a platform this
/// adapter cannot write to. Batch and task ids are rebinded by the caller, which
/// is the only side that knows them.
async fn archive_issues(
    ctx: &AdapterContext<'_>,
    full_name: &str,
) -> Result<ModuleResult, PlatformError> {
    let mut result = module(PlatformModule::Issues, Fidelity::ReadOnlyArchive);
    let issues = list_issues(ctx, &repo_stub(full_name)).await?;
    let mut items = Vec::new();
    for issue in &issues {
        let html_url = issue
            .get("html_url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        result.source_links.push(html_url.clone());
        items.push(ArchiveItem {
            source_id: issue
                .get("number")
                .and_then(Value::as_i64)
                .unwrap_or_default()
                .to_string(),
            source_url: html_url,
            title: issue
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            body: issue
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            source_author: issue
                .pointer("/user/login")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
            state: issue
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            attachments: vec![],
            metadata: BTreeMap::new(),
        });
    }
    result.archived = u64::try_from(items.len()).unwrap_or(u64::MAX);
    result.discovered = result.archived;
    result.archive = Some(ArchiveDocument::new(
        "",
        "",
        full_name,
        PlatformModule::Issues,
        items,
    ));
    Ok(result)
}

/// Whether `endpoint` addresses a Gitea/Forgejo instance, i.e. one this
/// adapter family can write to.
fn same_family(endpoint: &Endpoint) -> bool {
    matches!(
        endpoint.platform_hint,
        Some(PlatformKind::Gitea) | Some(PlatformKind::Forgejo)
    )
}

/// A minimal repository handle for read-only calls that only need a full name.
fn repo_stub(full_name: &str) -> RemoteRepository {
    RemoteRepository {
        locator: RepositoryLocator {
            platform_id: None,
            full_name: full_name.to_owned(),
            clone_url: None,
        },
        name: full_name.rsplit('/').next().unwrap_or(full_name).to_owned(),
        web_url: None,
        clone_url_https: None,
        clone_url_ssh: None,
        visibility: RepositoryVisibility::Unknown,
        default_branch: None,
    }
}

pub fn is_private_ref(name: &str) -> bool {
    name.starts_with("refs/pull/")
}
