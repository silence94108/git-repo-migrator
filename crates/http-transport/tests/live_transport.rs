//! The transport against a real socket.
//!
//! The unit tests cover request construction; these cover what only shows up on
//! the wire — that the resolved token really is sent as a header, that a 429 is
//! retried with the server's own delay, and that a permission failure is not
//! retried at all.

use std::sync::Arc;
use std::time::{Duration, Instant};

use git_repo_migrator_credential_store::CredentialStore;
use git_repo_migrator_http_transport::ReqwestTransport;
use git_repo_migrator_platform_core::transport::{
    HttpRequest, HttpTransport, HttpTransportConfig, RateLimitInfo, RetryPolicy,
};
use git_repo_migrator_platform_core::PlatformKind;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// A canned HTTP/1.1 reply.
struct Reply {
    status: u16,
    reason: &'static str,
    headers: Vec<(String, String)>,
    body: &'static str,
}

impl Reply {
    fn ok(body: &'static str) -> Self {
        Self {
            status: 200,
            reason: "OK",
            headers: vec![],
            body,
        }
    }
    fn status(status: u16, reason: &'static str) -> Self {
        Self {
            status,
            reason,
            headers: vec![],
            body: "{\"message\":\"nope\"}",
        }
    }
    fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
    fn render(&self) -> String {
        let mut text = format!("HTTP/1.1 {} {}\r\n", self.status, self.reason);
        text.push_str("Content-Type: application/json\r\n");
        text.push_str(&format!("Content-Length: {}\r\n", self.body.len()));
        for (name, value) in &self.headers {
            text.push_str(&format!("{name}: {value}\r\n"));
        }
        text.push_str("Connection: close\r\n\r\n");
        text.push_str(self.body);
        text
    }
}

/// Serves `replies` in order, and reports the raw request head of each call.
async fn serve(replies: Vec<Reply>) -> (String, mpsc::UnboundedReceiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    let (sender, receiver) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        for reply in replies {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut head = Vec::new();
            let mut buffer = [0u8; 1024];
            while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => head.extend_from_slice(&buffer[..read]),
                }
            }
            let _ = sender.send(String::from_utf8_lossy(&head).into_owned());
            let _ = stream.write_all(reply.render().as_bytes()).await;
            let _ = stream.flush().await;
        }
    });

    (format!("http://{address}"), receiver)
}

fn config() -> HttpTransportConfig {
    HttpTransportConfig {
        retry: RetryPolicy {
            max_attempts: 3,
            base_delay_ms: 20,
            max_delay_ms: 200,
            jitter: false,
        },
        ..HttpTransportConfig::default()
    }
}

fn get(url: String, credential: Option<&str>) -> HttpRequest {
    let mut headers = vec![("Accept".to_owned(), "application/json".to_owned())];
    if let Some(reference) = credential {
        headers.push(("X-Credential-Ref".to_owned(), reference.to_owned()));
    }
    HttpRequest {
        method: "GET".to_owned(),
        url,
        headers,
        body: None,
    }
}

#[tokio::test]
async fn the_resolved_token_travels_as_a_header_and_never_in_the_url() {
    let store = Arc::new(CredentialStore::in_memory());
    let reference = store.put("github", b"ghp-live-secret").expect("stored");
    let (base, mut requests) = serve(vec![Reply::ok("{\"login\":\"alice\"}")]).await;
    let transport =
        ReqwestTransport::new(config(), PlatformKind::Github, store).expect("transport");

    let response = transport
        .send(get(format!("{base}/user"), Some(reference.as_str())))
        .await
        .expect("request sent");
    assert_eq!(response.status, 200);

    let head = requests.recv().await.expect("request head");
    assert!(head.contains("GET /user HTTP/1.1"));
    assert!(
        head.contains("authorization: Bearer ghp-live-secret")
            || head.contains("Authorization: Bearer ghp-live-secret"),
        "the resolved token must be sent as an Authorization header: {head}"
    );
    assert!(
        !head.contains("x-credential-ref"),
        "the credential reference must not leave the process: {head}"
    );
    assert!(!head.contains("/user?"), "no token may be added to the URL");
}

#[tokio::test]
async fn a_rate_limited_response_is_retried_after_the_server_supplied_delay() {
    let (base, mut requests) = serve(vec![
        Reply::status(429, "Too Many Requests")
            .header("Retry-After", "1")
            .header("X-RateLimit-Remaining", "0"),
        Reply::ok("{\"items\":[]}"),
    ])
    .await;
    let transport = ReqwestTransport::new(
        config(),
        PlatformKind::Github,
        Arc::new(CredentialStore::in_memory()),
    )
    .expect("transport");

    let started = Instant::now();
    let response = transport
        .send(get(format!("{base}/search"), None))
        .await
        .expect("request sent");
    let elapsed = started.elapsed();

    assert_eq!(
        response.status, 200,
        "the retry must be what the caller sees"
    );
    assert!(
        elapsed >= Duration::from_millis(900),
        "the server's Retry-After must be honoured, waited {elapsed:?}"
    );
    assert!(requests.recv().await.is_some());
    assert!(
        requests.recv().await.is_some(),
        "a 429 must produce a second attempt"
    );
}

#[tokio::test]
async fn a_permission_failure_is_returned_without_a_second_attempt() {
    let (base, mut requests) = serve(vec![
        Reply::status(403, "Forbidden"),
        Reply::ok("{\"never\":\"reached\"}"),
    ])
    .await;
    let transport = ReqwestTransport::new(
        config(),
        PlatformKind::Github,
        Arc::new(CredentialStore::in_memory()),
    )
    .expect("transport");

    let response = transport
        .send(get(format!("{base}/orgs/ops/repos"), None))
        .await
        .expect("a 403 is a response, not a transport failure");
    assert_eq!(response.status, 403);

    assert!(requests.recv().await.is_some());
    // Retrying a permission failure would burn rate limit and hide the fix.
    assert!(
        tokio::time::timeout(Duration::from_millis(300), requests.recv())
            .await
            .is_err(),
        "a 403 must not be retried"
    );
}

#[tokio::test]
async fn rate_limit_headers_reach_the_caller_for_the_queue_to_pace_itself() {
    let (base, _requests) = serve(vec![Reply::ok("{\"items\":[]}")
        .header("X-RateLimit-Limit", "5000")
        .header("X-RateLimit-Remaining", "17")
        .header("X-RateLimit-Reset", "1760000000")])
    .await;
    let transport = ReqwestTransport::new(
        config(),
        PlatformKind::Github,
        Arc::new(CredentialStore::in_memory()),
    )
    .expect("transport");

    let response = transport
        .send(get(format!("{base}/user/repos"), None))
        .await
        .expect("request sent");
    let info = RateLimitInfo::from_headers(&response.headers);
    assert_eq!(info.limit, Some(5000));
    assert_eq!(info.remaining, Some(17));
    assert_eq!(info.reset_epoch_seconds, Some(1_760_000_000));
}

#[tokio::test]
async fn a_persistent_server_error_gives_up_after_the_configured_attempts() {
    let (base, mut requests) = serve(vec![
        Reply::status(503, "Service Unavailable"),
        Reply::status(503, "Service Unavailable"),
        Reply::status(503, "Service Unavailable"),
        Reply::ok("{\"never\":\"reached\"}"),
    ])
    .await;
    let transport = ReqwestTransport::new(
        config(),
        PlatformKind::Github,
        Arc::new(CredentialStore::in_memory()),
    )
    .expect("transport");

    let response = transport
        .send(get(format!("{base}/user"), None))
        .await
        .expect("the last response is returned");
    assert_eq!(response.status, 503);

    let mut attempts = 0;
    while tokio::time::timeout(Duration::from_millis(300), requests.recv())
        .await
        .is_ok_and(|value| value.is_some())
    {
        attempts += 1;
    }
    assert_eq!(
        attempts, 3,
        "max_attempts bounds the retries; a fourth call would be a retry storm"
    );
}

#[tokio::test]
async fn an_unreachable_host_is_a_network_error_rather_than_a_panic() {
    let transport = ReqwestTransport::new(
        config(),
        PlatformKind::Github,
        Arc::new(CredentialStore::in_memory()),
    )
    .expect("transport");
    // Port 1 on the loopback interface is reliably closed.
    let error = transport
        .send(get("http://127.0.0.1:1/user".to_owned(), None))
        .await
        .expect_err("a closed port must be an error");
    assert!(
        format!("{error}").contains("网络") || format!("{error}").contains("连接"),
        "the operator has to see an actionable network error: {error}"
    );
}
