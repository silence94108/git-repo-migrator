//! The real `HttpTransport`.
//!
//! Adapters build requests with an `X-Credential-Ref` header and never see a
//! token. Resolving that reference into a real authorisation header is this
//! crate's job, and it is the only place in the workspace where a secret and a
//! network socket meet.
//!
//! Three properties are enforced here rather than left to callers:
//!
//! * a secret never reaches a URL, a log line or an error message;
//! * TLS is always verified — a pinned fingerprint is an *additional* accepted
//!   certificate, never a switch that turns verification off;
//! * `429`/`Retry-After` and transient network failures back off with jitter,
//!   while auth, permission and validation failures are returned immediately.

mod pinning;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use git_repo_migrator_credential_store::CredentialStore;
use git_repo_migrator_platform_core::transport::{
    HttpRequest, HttpResponse, HttpTransport, HttpTransportConfig, ProxyConfig, RateLimitInfo,
    TlsPolicy, TransportError,
};
use git_repo_migrator_platform_core::{CredentialRef, PlatformKind};

pub use pinning::PinnedOrPlatformVerifier;

/// Header the adapters use to say "authenticate this request", without ever
/// handling the secret themselves.
pub const CREDENTIAL_REF_HEADER: &str = "x-credential-ref";

/// Headers a caller may never set directly. Anything that could smuggle a
/// secret past the credential boundary is rejected before the request is sent.
const RESERVED_HEADERS: [&str; 4] = [
    "authorization",
    "private-token",
    "cookie",
    "proxy-authorization",
];

/// Hard ceiling for a server-supplied `Retry-After`. A hostile or misconfigured
/// header must not park a worker indefinitely, but the value is deliberately
/// well above `max_delay_ms`: retrying *sooner* than the server asked just
/// earns another 429 and spends an attempt for nothing.
const MAX_SERVER_DELAY: Duration = Duration::from_secs(120);

/// Deterministic backoff, so a retry storm cannot be produced by a hostile
/// `Retry-After`. The jitter is derived from the attempt rather than a random
/// source, which keeps the transport reproducible in tests.
fn backoff(config: &HttpTransportConfig, attempt: u32, retry_after: Option<Duration>) -> Duration {
    let base = config
        .retry
        .base_delay_ms
        .saturating_mul(1u64 << attempt.min(16));
    let capped = base.min(config.retry.max_delay_ms);
    let jittered = if config.retry.jitter {
        // 75%..100% of the window, spread by attempt number.
        capped - (capped / 4) * u64::from(attempt % 2)
    } else {
        capped
    };
    let computed = Duration::from_millis(jittered.max(1));
    match retry_after {
        // Never earlier than the server asked, never longer than the ceiling.
        Some(server) => computed.max(server.min(MAX_SERVER_DELAY)),
        None => computed,
    }
}

/// How a platform expects its token to be presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthScheme {
    Bearer,
    GitlabPrivateToken,
    None,
}

fn scheme_for(kind: PlatformKind) -> AuthScheme {
    match kind {
        PlatformKind::Gitlab => AuthScheme::GitlabPrivateToken,
        PlatformKind::Github
        | PlatformKind::Gitea
        | PlatformKind::Forgejo
        | PlatformKind::Gitee => AuthScheme::Bearer,
        // Generic Git has no API, so an API token would have nowhere to go.
        PlatformKind::GenericGit | PlatformKind::Unknown => AuthScheme::None,
    }
}

pub struct ReqwestTransport {
    client: reqwest::Client,
    config: HttpTransportConfig,
    credentials: Arc<CredentialStore>,
    scheme: AuthScheme,
}

impl ReqwestTransport {
    pub fn new(
        config: HttpTransportConfig,
        platform: PlatformKind,
        credentials: Arc<CredentialStore>,
    ) -> Result<Self, TransportError> {
        config.validate()?;
        let mut builder = reqwest::Client::builder()
            .user_agent(config.user_agent.clone())
            .connect_timeout(Duration::from_millis(config.connect_timeout_ms))
            .timeout(config.timeout())
            // Redirects are followed by us, not silently: a redirect to another
            // host must not carry the authorisation header with it.
            .redirect(reqwest::redirect::Policy::none());

        if let Some(proxy) = &config.proxy {
            builder = builder.proxy(build_proxy(proxy)?);
        }
        if let TlsPolicy::PinnedFingerprint { sha256 } = &config.tls {
            builder = builder.use_preconfigured_tls(pinning::client_config(sha256)?);
        }

        let client = builder.build().map_err(|error| {
            TransportError::InvalidConfig(format!("HTTP 客户端创建失败: {error}"))
        })?;
        Ok(Self {
            client,
            config,
            credentials,
            scheme: scheme_for(platform),
        })
    }

    /// Turns the adapter's request into a wire request, replacing the credential
    /// reference with the real header.
    fn build(&self, request: &HttpRequest) -> Result<reqwest::Request, TransportError> {
        let url = url::Url::parse(&request.url)
            .map_err(|_| TransportError::InvalidConfig("请求地址无效".into()))?;
        if !matches!(url.scheme(), "https" | "http") {
            return Err(TransportError::InvalidConfig(
                "只允许 http 或 https 请求".into(),
            ));
        }
        if url.password().is_some() || !url.username().is_empty() {
            return Err(TransportError::InvalidConfig(
                "请求地址不得包含用户名或密码".into(),
            ));
        }

        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|_| TransportError::InvalidConfig("HTTP 方法无效".into()))?;
        let mut builder = self.client.request(method, url);

        let mut credential = None;
        for (name, value) in &request.headers {
            let lowered = name.to_ascii_lowercase();
            if lowered == CREDENTIAL_REF_HEADER {
                credential = Some(value.clone());
                continue;
            }
            if RESERVED_HEADERS.contains(&lowered.as_str()) {
                return Err(TransportError::InvalidConfig(format!(
                    "调用方不得直接设置 {name} 头"
                )));
            }
            builder = builder.header(name, value);
        }

        if let Some(reference) = credential {
            builder = self.authorise(builder, &reference)?;
        }
        if let Some(body) = &request.body {
            builder = builder.body(body.clone());
        }
        builder
            .build()
            .map_err(|error| TransportError::InvalidConfig(format!("请求构造失败: {error}")))
    }

    /// Reads the secret, uses it for exactly one header and drops it. The value
    /// is never formatted into an error, so a failure here cannot leak it.
    fn authorise(
        &self,
        builder: reqwest::RequestBuilder,
        reference: &str,
    ) -> Result<reqwest::RequestBuilder, TransportError> {
        if self.scheme == AuthScheme::None {
            return Ok(builder);
        }
        let reference = CredentialRef::new(reference)
            .map_err(|_| TransportError::InvalidConfig("凭据引用无效".into()))?;
        let guard = self
            .credentials
            .get(&reference)
            .map_err(|_| TransportError::Http {
                status: 401,
                message: "本机凭据库中找不到该连接的凭据".into(),
                retry_after: None,
            })?;
        let secret = std::str::from_utf8(guard.expose())
            .map_err(|_| TransportError::InvalidConfig("凭据不是 UTF-8 文本".into()))?;
        let (name, value) = match self.scheme {
            AuthScheme::Bearer => ("Authorization", format!("Bearer {secret}")),
            AuthScheme::GitlabPrivateToken => ("PRIVATE-TOKEN", secret.to_owned()),
            AuthScheme::None => unreachable!("checked above"),
        };
        let mut header = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|_| TransportError::InvalidConfig("凭据包含无法作为 HTTP 头的字符".into()))?;
        // Marks the value so it is printed as `Sensitive` if anything ever debugs
        // the header map.
        header.set_sensitive(true);
        Ok(builder.header(name, header))
    }

    async fn send_once(&self, request: &HttpRequest) -> Result<HttpResponse, TransportError> {
        let wire = self.build(request)?;
        let response = self
            .client
            .execute(wire)
            .await
            .map_err(|error| classify(&error))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|text| (name.as_str().to_owned(), text.to_owned()))
            })
            .collect::<Vec<_>>();
        let body = response
            .bytes()
            .await
            .map_err(|error| classify(&error))?
            .to_vec();
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let mut attempt = 0u32;
        loop {
            let outcome = self.send_once(&request).await;
            let retry_after = match &outcome {
                Ok(response) if is_retryable_status(response.status) => {
                    RateLimitInfo::from_headers(&response.headers).retry_after
                }
                Ok(_) => return outcome,
                Err(error) if is_retryable_error(error) => None,
                Err(_) => return outcome,
            };

            attempt += 1;
            if attempt >= self.config.retry.max_attempts {
                return outcome;
            }
            sleep(backoff(&self.config, attempt, retry_after)).await;
        }
    }

    fn config(&self) -> &HttpTransportConfig {
        &self.config
    }
}

async fn sleep(duration: Duration) {
    // `reqwest` already pulls in a Tokio reactor; using its timer keeps the
    // transport usable from any Tokio runtime without a separate dependency.
    tokio_sleep(duration).await;
}

#[cfg(not(target_family = "wasm"))]
async fn tokio_sleep(duration: Duration) {
    tokio::time::sleep(duration).await;
}

fn is_retryable_status(status: u16) -> bool {
    // 429 and 5xx are transient. 401/403/404/409 are not, and retrying them
    // burns rate limit while hiding the real fix from the operator.
    status == 429 || (500..600).contains(&status)
}

fn is_retryable_error(error: &TransportError) -> bool {
    match error {
        TransportError::Network(_) => true,
        TransportError::Http { status, .. } => is_retryable_status(*status),
        TransportError::InvalidConfig(_) => false,
    }
}

fn build_proxy(proxy: &ProxyConfig) -> Result<reqwest::Proxy, TransportError> {
    let parsed = reqwest::Proxy::all(&proxy.url)
        .map_err(|_| TransportError::InvalidConfig("代理地址无效".into()))?;
    if proxy.bypass_hosts.is_empty() {
        return Ok(parsed);
    }
    let bypass = proxy.bypass_hosts.clone();
    Ok(parsed.no_proxy(reqwest::NoProxy::from_string(&bypass.join(","))))
}

/// Maps a client failure onto the transport error contract without ever
/// including the request body or the authorisation header.
fn classify(error: &reqwest::Error) -> TransportError {
    if error.is_timeout() {
        return TransportError::Network("请求超时".into());
    }
    if error.is_connect() {
        return TransportError::Network("无法连接到该实例（可能是网络、代理或证书问题）".into());
    }
    if error.is_request() {
        return TransportError::InvalidConfig("请求无效".into());
    }
    TransportError::Network("网络请求失败".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use git_repo_migrator_platform_core::transport::RetryPolicy;

    fn config() -> HttpTransportConfig {
        HttpTransportConfig::default()
    }

    fn transport(kind: PlatformKind) -> ReqwestTransport {
        ReqwestTransport::new(config(), kind, Arc::new(CredentialStore::in_memory()))
            .expect("transport")
    }

    #[test]
    fn only_gitlab_uses_a_private_token_header() {
        assert_eq!(
            scheme_for(PlatformKind::Gitlab),
            AuthScheme::GitlabPrivateToken
        );
        for kind in [
            PlatformKind::Github,
            PlatformKind::Gitea,
            PlatformKind::Forgejo,
            PlatformKind::Gitee,
        ] {
            assert_eq!(scheme_for(kind), AuthScheme::Bearer);
        }
        for kind in [PlatformKind::GenericGit, PlatformKind::Unknown] {
            assert_eq!(scheme_for(kind), AuthScheme::None);
        }
    }

    #[test]
    fn a_caller_cannot_set_an_authorisation_header_itself() {
        let transport = transport(PlatformKind::Github);
        for header in ["Authorization", "PRIVATE-TOKEN", "Cookie", "authorization"] {
            let error = transport
                .build(&HttpRequest {
                    method: "GET".into(),
                    url: "https://api.github.test/user".into(),
                    headers: vec![(header.into(), "sneaky".into())],
                    body: None,
                })
                .expect_err("reserved header must be rejected");
            assert!(matches!(error, TransportError::InvalidConfig(_)));
        }
    }

    #[test]
    fn a_url_may_not_carry_credentials_or_a_non_http_scheme() {
        let transport = transport(PlatformKind::Github);
        for url in [
            "https://user:token@api.github.test/user",
            "https://user@api.github.test/user",
            "file:///etc/passwd",
            "ftp://example.test/x",
        ] {
            assert!(transport
                .build(&HttpRequest {
                    method: "GET".into(),
                    url: url.into(),
                    headers: vec![],
                    body: None,
                })
                .is_err());
        }
    }

    #[test]
    fn the_credential_reference_becomes_a_header_and_never_a_query_parameter() {
        let store = Arc::new(CredentialStore::in_memory());
        let reference = store.put("github", b"secret-token").expect("stored");
        let transport =
            ReqwestTransport::new(config(), PlatformKind::Github, store).expect("transport");

        let wire = transport
            .build(&HttpRequest {
                method: "GET".into(),
                url: "https://api.github.test/user/repos".into(),
                headers: vec![
                    ("Accept".into(), "application/vnd.github+json".into()),
                    ("X-Credential-Ref".into(), reference.as_str().into()),
                ],
                body: None,
            })
            .expect("request built");

        assert_eq!(
            wire.headers()
                .get(reqwest::header::AUTHORIZATION)
                .map(reqwest::header::HeaderValue::is_sensitive),
            Some(true)
        );
        assert!(
            !wire.url().as_str().contains("secret-token"),
            "a token must never appear in a URL"
        );
        assert!(wire.headers().get("x-credential-ref").is_none());
        // Debug output is what ends up in a crash report.
        assert!(!format!("{:?}", wire.headers()).contains("secret-token"));
    }

    #[test]
    fn gitlab_sends_the_raw_token_in_its_own_header() {
        let store = Arc::new(CredentialStore::in_memory());
        let reference = store.put("gitlab", b"glpat-example").expect("stored");
        let transport =
            ReqwestTransport::new(config(), PlatformKind::Gitlab, store).expect("transport");

        let wire = transport
            .build(&HttpRequest {
                method: "GET".into(),
                url: "https://gitlab.test/api/v4/projects".into(),
                headers: vec![("X-Credential-Ref".into(), reference.as_str().into())],
                body: None,
            })
            .expect("request built");
        let header = wire.headers().get("private-token").expect("private token");
        assert!(header.is_sensitive());
    }

    #[test]
    fn a_missing_credential_is_an_unauthenticated_error_not_an_anonymous_request() {
        let transport = transport(PlatformKind::Github);
        let error = transport
            .build(&HttpRequest {
                method: "GET".into(),
                url: "https://api.github.test/user".into(),
                headers: vec![(
                    "X-Credential-Ref".into(),
                    "credential/windows/missing".into(),
                )],
                body: None,
            })
            .expect_err("a missing credential must not fall back to anonymous");
        match error {
            TransportError::Http {
                status, message, ..
            } => {
                assert_eq!(status, 401);
                assert!(!message.is_empty());
            }
            other => panic!("expected an HTTP 401, got {other:?}"),
        }
    }

    #[test]
    fn generic_git_never_attaches_a_token() {
        let store = Arc::new(CredentialStore::in_memory());
        let reference = store.put("generic", b"unused").expect("stored");
        let transport =
            ReqwestTransport::new(config(), PlatformKind::GenericGit, store).expect("transport");
        let wire = transport
            .build(&HttpRequest {
                method: "GET".into(),
                url: "https://git.test/info/refs".into(),
                headers: vec![("X-Credential-Ref".into(), reference.as_str().into())],
                body: None,
            })
            .expect("request built");
        assert!(wire.headers().get(reqwest::header::AUTHORIZATION).is_none());
    }

    #[test]
    fn only_transient_statuses_are_retried() {
        for status in [429, 500, 502, 503, 504] {
            assert!(is_retryable_status(status), "{status} should be retried");
        }
        for status in [200, 201, 304, 400, 401, 403, 404, 409, 422] {
            assert!(!is_retryable_status(status), "{status} must not be retried");
        }
    }

    #[test]
    fn a_hostile_retry_after_cannot_park_a_worker_forever() {
        let mut config = config();
        config.retry = RetryPolicy {
            max_attempts: 4,
            base_delay_ms: 500,
            max_delay_ms: 30_000,
            jitter: true,
        };
        let waited = backoff(&config, 1, Some(Duration::from_secs(86_400)));
        assert_eq!(waited, MAX_SERVER_DELAY);
    }

    #[test]
    fn a_server_delay_is_never_shortened_into_another_rate_limit() {
        let mut config = config();
        config.retry = RetryPolicy {
            max_attempts: 3,
            base_delay_ms: 20,
            max_delay_ms: 200,
            jitter: false,
        };
        // Retrying after 200ms when the server asked for a second would just
        // earn a second 429 and spend an attempt.
        assert_eq!(
            backoff(&config, 1, Some(Duration::from_secs(1))),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn backoff_grows_and_stays_inside_the_ceiling() {
        let config = config();
        let mut previous = Duration::ZERO;
        for attempt in 1..6 {
            let waited = backoff(&config, attempt, None);
            assert!(waited <= Duration::from_millis(config.retry.max_delay_ms));
            assert!(waited > Duration::ZERO);
            if attempt > 1 {
                assert!(waited >= previous / 2, "backoff must not collapse");
            }
            previous = waited;
        }
    }

    #[test]
    fn an_invalid_configuration_is_rejected_before_a_socket_is_opened() {
        let mut config = config();
        config.tls = TlsPolicy::PinnedFingerprint {
            sha256: "not-a-fingerprint".into(),
        };
        assert!(ReqwestTransport::new(
            config,
            PlatformKind::Github,
            Arc::new(CredentialStore::in_memory())
        )
        .is_err());
    }
}
