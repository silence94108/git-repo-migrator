use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

/// TLS verification is intentionally secure by default.  There is no global
/// "accept any certificate" switch; a non-system policy must identify the
/// exact certificate fingerprint that the user approved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TlsPolicy {
    #[default]
    System,
    PinnedFingerprint {
        sha256: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub url: String,
    #[serde(default)]
    pub bypass_hosts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub jitter: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base_delay_ms: 500,
            max_delay_ms: 30_000,
            jitter: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpTransportConfig {
    pub tls: TlsPolicy,
    pub timeout_ms: u64,
    pub connect_timeout_ms: u64,
    pub proxy: Option<ProxyConfig>,
    pub retry: RetryPolicy,
    pub user_agent: String,
}

impl Default for HttpTransportConfig {
    fn default() -> Self {
        Self {
            tls: TlsPolicy::System,
            timeout_ms: 30_000,
            connect_timeout_ms: 10_000,
            proxy: None,
            retry: RetryPolicy::default(),
            user_agent: "git-repo-migrator/0.1".to_owned(),
        }
    }
}

impl HttpTransportConfig {
    pub fn validate(&self) -> Result<(), TransportError> {
        if self.timeout_ms == 0 || self.connect_timeout_ms == 0 {
            return Err(TransportError::InvalidConfig("超时时间必须大于 0".into()));
        }
        if self.retry.max_attempts == 0 {
            return Err(TransportError::InvalidConfig(
                "最大重试次数必须大于 0".into(),
            ));
        }
        if let TlsPolicy::PinnedFingerprint { sha256 } = &self.tls {
            let valid = sha256.len() == 64 && sha256.chars().all(|c| c.is_ascii_hexdigit());
            if !valid {
                return Err(TransportError::InvalidConfig(
                    "TLS 指纹必须是 64 位 SHA-256 十六进制值".into(),
                ));
            }
        }
        if let Some(proxy) = &self.proxy {
            let parsed = url::Url::parse(&proxy.url)
                .map_err(|_| TransportError::InvalidConfig("代理地址无效".into()))?;
            if !matches!(parsed.scheme(), "http" | "https" | "socks5") {
                return Err(TransportError::InvalidConfig(
                    "代理只支持 http、https 或 socks5".into(),
                ));
            }
        }
        Ok(())
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TransportError {
    #[error("传输配置无效: {0}")]
    InvalidConfig(String),
    #[error("网络错误: {0}")]
    Network(String),
    #[error("HTTP 请求失败: {status}")]
    Http {
        status: u16,
        message: String,
        retry_after: Option<Duration>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimitInfo {
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    pub reset_epoch_seconds: Option<u64>,
    pub retry_after: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RateLimitKey {
    pub connection_id: String,
    pub host: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateLimitPermit {
    Ready,
    Wait(Duration),
}

#[async_trait]
pub trait RateLimiter: Send + Sync {
    async fn acquire(&self, key: &RateLimitKey) -> RateLimitPermit;
    async fn update(&self, key: &RateLimitKey, info: &RateLimitInfo);
}

impl RateLimitInfo {
    pub fn from_headers(headers: &[(String, String)]) -> Self {
        let get = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        };
        Self {
            limit: get("x-ratelimit-limit").and_then(|v| v.parse().ok()),
            remaining: get("x-ratelimit-remaining").and_then(|v| v.parse().ok()),
            reset_epoch_seconds: get("x-ratelimit-reset").and_then(|v| v.parse().ok()),
            retry_after: get("retry-after").and_then(parse_retry_after),
        }
    }
}

pub fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

#[async_trait]
pub trait HttpTransport: Send + Sync {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError>;
    fn config(&self) -> &HttpTransportConfig;
}
