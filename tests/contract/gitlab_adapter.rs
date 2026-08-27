use async_trait::async_trait;
use futures::executor::block_on;
use git_repo_migrator_platform_core::transport::{
    HttpRequest, HttpResponse, HttpTransport, HttpTransportConfig, TlsPolicy, TransportError,
};
use git_repo_migrator_platform_core::*;
use git_repo_migrator_platform_gitlab::{is_private_ref, GitlabAdapter};
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
fn endpoint() -> Endpoint {
    Endpoint {
        base_url: "https://gitlab.example.test".into(),
        platform_hint: Some(PlatformKind::Gitlab),
    }
}

#[test]
fn old_self_managed_version_degrades_merge_requests_to_archive() {
    let transport = FixtureTransport {
        responses: Mutex::new(
            vec![HttpResponse {
                status: 200,
                headers: vec![],
                body: br#"{"version":"11.6.2"}"#.to_vec(),
            }]
            .into(),
        ),
        config: HttpTransportConfig::default(),
    };
    let ep = endpoint();
    let ctx = AdapterContext {
        connection_id: "c",
        endpoint: &ep,
        credential_ref: None,
        transport: &transport,
    };
    let matrix = block_on(GitlabAdapter.capabilities(&ctx)).unwrap();
    assert_eq!(matrix.instance_version.as_deref(), Some("11.6.2"));
    assert_eq!(matrix.merge_requests.fidelity, Fidelity::ReadOnlyArchive);
    assert_eq!(matrix.releases.fidelity, Fidelity::Unsupported);
    assert!(is_private_ref("refs/merge-requests/7/head"));
}

#[test]
fn self_signed_tls_requires_a_specific_valid_fingerprint() {
    let invalid = HttpTransportConfig {
        tls: TlsPolicy::PinnedFingerprint {
            sha256: "accept-any".into(),
        },
        ..HttpTransportConfig::default()
    };
    assert!(invalid.validate().is_err());
    let valid = HttpTransportConfig {
        tls: TlsPolicy::PinnedFingerprint {
            sha256: "a".repeat(64),
        },
        ..HttpTransportConfig::default()
    };
    assert!(valid.validate().is_ok());
}

#[test]
fn gitlab_429_is_retryable() {
    let transport = FixtureTransport {
        responses: Mutex::new(
            vec![HttpResponse {
                status: 429,
                headers: vec![("Retry-After".into(), "9".into())],
                body: br#"{"message":"too many requests"}"#.to_vec(),
            }]
            .into(),
        ),
        config: HttpTransportConfig::default(),
    };
    let ep = endpoint();
    let ctx = AdapterContext {
        connection_id: "c",
        endpoint: &ep,
        credential_ref: None,
        transport: &transport,
    };
    let error = block_on(GitlabAdapter.test_connection(&ctx)).unwrap_err();
    assert!(error.retryable);
    assert_eq!(error.retry_after_seconds, Some(9));
}
