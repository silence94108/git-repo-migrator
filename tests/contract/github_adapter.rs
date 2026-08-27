use async_trait::async_trait;
use futures::executor::block_on;
use git_repo_migrator_platform_core::transport::{
    HttpRequest, HttpResponse, HttpTransport, HttpTransportConfig, TransportError,
};
use git_repo_migrator_platform_core::*;
use git_repo_migrator_platform_github::GithubAdapter;
use std::collections::VecDeque;
use std::sync::Mutex;

struct FixtureTransport {
    responses: Mutex<VecDeque<HttpResponse>>,
    requests: Mutex<Vec<HttpRequest>>,
    config: HttpTransportConfig,
}
impl FixtureTransport {
    fn new(responses: Vec<HttpResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(vec![]),
            config: HttpTransportConfig::default(),
        }
    }
}
#[async_trait]
impl HttpTransport for FixtureTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| TransportError::Network("missing fixture".into()))
    }
    fn config(&self) -> &HttpTransportConfig {
        &self.config
    }
}
fn response(status: u16, body: &str, headers: Vec<(String, String)>) -> HttpResponse {
    HttpResponse {
        status,
        headers,
        body: body.as_bytes().to_vec(),
    }
}
fn endpoint() -> Endpoint {
    Endpoint {
        base_url: "https://api.github.com".into(),
        platform_hint: Some(PlatformKind::Github),
    }
}

#[test]
fn public_and_enterprise_endpoints_use_their_real_api_roots() {
    let public = FixtureTransport::new(vec![response(200, r#"{"id":1,"login":"alice"}"#, vec![])]);
    let public_endpoint = Endpoint {
        base_url: "https://github.com".into(),
        platform_hint: Some(PlatformKind::Github),
    };
    let public_ctx = AdapterContext {
        connection_id: "c",
        endpoint: &public_endpoint,
        credential_ref: None,
        transport: &public,
    };
    block_on(GithubAdapter::new().test_connection(&public_ctx)).unwrap();
    assert_eq!(
        public.requests.lock().unwrap()[0].url,
        "https://api.github.com/user"
    );

    let enterprise =
        FixtureTransport::new(vec![response(200, r#"{"id":1,"login":"alice"}"#, vec![])]);
    let enterprise_endpoint = Endpoint {
        base_url: "https://github.enterprise.test".into(),
        platform_hint: Some(PlatformKind::Github),
    };
    let enterprise_ctx = AdapterContext {
        connection_id: "c",
        endpoint: &enterprise_endpoint,
        credential_ref: None,
        transport: &enterprise,
    };
    block_on(GithubAdapter::new().test_connection(&enterprise_ctx)).unwrap();
    assert_eq!(
        enterprise.requests.lock().unwrap()[0].url,
        "https://github.enterprise.test/api/v3/user"
    );
}

#[test]
fn discovery_preserves_real_permissions_and_never_embeds_a_token() {
    let transport = FixtureTransport::new(vec![response(
        200,
        r#"[{"id":1,"full_name":"team/repo","clone_url":"https://github.com/team/repo.git","ssh_url":"git@github.com:team/repo.git","html_url":"https://github.com/team/repo","private":true,"permissions":{"push":false,"admin":false}}]"#,
        vec![],
    )]);
    let reference = CredentialRef::new("credential/windows/ref-1").unwrap();
    let ep = endpoint();
    let ctx = AdapterContext {
        connection_id: "c1",
        endpoint: &ep,
        credential_ref: Some(&reference),
        transport: &transport,
    };
    let page = block_on(GithubAdapter::new().discover_repositories(
        &ctx,
        DiscoveryQuery {
            scope: RepositoryScope::Participated,
            search: None,
            visibility: None,
            include_archived: true,
            cursor: None,
            page_size: 100,
        },
    ))
    .unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(page.items[0].permissions.read);
    assert!(!page.items[0].permissions.push);
    let request = &transport.requests.lock().unwrap()[0];
    assert!(!request.url.contains("token"));
    assert!(request
        .headers
        .iter()
        .all(|(_, value)| !value.contains("fixture-secret")));
}

#[test]
fn rate_limit_retry_after_and_minimum_scopes_are_explicit() {
    let transport = FixtureTransport::new(vec![
        response(
            429,
            r#"{"message":"rate limited"}"#,
            vec![("Retry-After".into(), "12".into())],
        ),
        response(200, "{}", vec![]),
    ]);
    let ep = endpoint();
    let ctx = AdapterContext {
        connection_id: "c1",
        endpoint: &ep,
        credential_ref: None,
        transport: &transport,
    };
    let error = block_on(GithubAdapter::new().test_connection(&ctx)).unwrap_err();
    assert_eq!(error.category, PlatformErrorCategory::RateLimited);
    assert_eq!(error.retry_after_seconds, Some(12));
    let matrix = block_on(GithubAdapter::new().capabilities(&ctx)).unwrap();
    assert_eq!(matrix.instance_version.as_deref(), Some("github.com"));
    assert_eq!(
        matrix.repository_creation.version.as_deref(),
        Some("github.com")
    );
    assert!(matrix
        .repository_creation
        .required_scopes
        .contains(&"repo".into()));
    assert_eq!(matrix.merge_requests.fidelity, Fidelity::Unsupported);
}

#[test]
fn create_timeout_rechecks_repository_by_owner_and_name() {
    let transport = FixtureTransport::new(vec![
        response(504, r#"{"message":"gateway timeout"}"#, vec![]),
        response(
            200,
            r#"{"id":2,"full_name":"team/new-repo","clone_url":"https://github.com/team/new-repo.git","ssh_url":"git@github.com:team/new-repo.git","html_url":"https://github.com/team/new-repo","private":true,"permissions":{"push":true,"admin":true}}"#,
            vec![],
        ),
    ]);
    let ep = endpoint();
    let ctx = AdapterContext {
        connection_id: "c1",
        endpoint: &ep,
        credential_ref: None,
        transport: &transport,
    };
    let created = block_on(GithubAdapter::new().create_repository(
        &ctx,
        CreateRepositorySpec {
            owner: "team".into(),
            name: "new-repo".into(),
            description: None,
            visibility: RepositoryVisibility::Private,
            homepage: None,
            initialize: false,
            idempotency_key: "plan/task".into(),
        },
    ))
    .unwrap();
    assert_eq!(created.locator.full_name, "team/new-repo");
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests[0].method, "POST");
    assert!(requests[1].url.ends_with("/repos/team/new-repo"));
}
