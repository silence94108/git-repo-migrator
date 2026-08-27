use async_trait::async_trait;
use futures::executor::block_on;
use git_repo_migrator_platform_core::transport::{
    HttpRequest, HttpResponse, HttpTransport, HttpTransportConfig, TransportError,
};
use git_repo_migrator_platform_core::*;
use git_repo_migrator_platform_gitea::{is_private_ref, GiteaAdapter};
use git_repo_migrator_platform_gitee::GiteeAdapter;
use std::collections::VecDeque;
use std::sync::Mutex;

struct FixtureTransport {
    responses: Mutex<VecDeque<HttpResponse>>,
    config: HttpTransportConfig,
}
#[async_trait]
impl HttpTransport for FixtureTransport {
    async fn send(&self, _: HttpRequest) -> Result<HttpResponse, TransportError> {
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

#[test]
fn forgejo_identity_and_version_snapshot_are_preserved() {
    let transport = FixtureTransport {
        responses: Mutex::new(
            vec![HttpResponse {
                status: 200,
                headers: vec![],
                body: br#"{"version":"9.0.1"}"#.to_vec(),
            }]
            .into(),
        ),
        config: HttpTransportConfig::default(),
    };
    let endpoint = Endpoint {
        base_url: "https://forgejo.example.test".into(),
        platform_hint: Some(PlatformKind::Forgejo),
    };
    let ctx = AdapterContext {
        connection_id: "c",
        endpoint: &endpoint,
        credential_ref: None,
        transport: &transport,
    };
    let matrix = block_on(GiteaAdapter.capabilities(&ctx)).unwrap();
    assert_eq!(matrix.platform, PlatformKind::Forgejo);
    assert_eq!(matrix.instance_version.as_deref(), Some("9.0.1"));
    assert_eq!(matrix.pull_requests.fidelity, Fidelity::NativeRebuild);
    assert!(is_private_ref("refs/pull/3/head"));
}

#[test]
fn gitee_capabilities_expose_field_level_degradation() {
    let transport = FixtureTransport {
        responses: Mutex::new(VecDeque::new()),
        config: HttpTransportConfig::default(),
    };
    let endpoint = Endpoint {
        base_url: "https://gitee.com".into(),
        platform_hint: Some(PlatformKind::Gitee),
    };
    let ctx = AdapterContext {
        connection_id: "c",
        endpoint: &endpoint,
        credential_ref: None,
        transport: &transport,
    };
    let matrix = block_on(GiteeAdapter.capabilities(&ctx)).unwrap();
    assert_eq!(matrix.pull_requests.fidelity, Fidelity::ReadOnlyArchive);
    assert!(matrix.pull_requests.degradation.is_some());
    assert_eq!(matrix.wiki.fidelity, Fidelity::Unsupported);
    assert!(!matrix.repository_creation.required_scopes.is_empty());
}
