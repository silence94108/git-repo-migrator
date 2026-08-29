//! Certificate pinning for self-signed instances.
//!
//! A pinned fingerprint *adds* one certificate the operator explicitly
//! approved. It never disables verification: anything that is not the pinned
//! certificate still has to satisfy the operating system's trust store, which
//! is what the connection page promises the user.

use std::sync::Arc;

use git_repo_migrator_platform_core::transport::TransportError;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme};
use sha2::{Digest, Sha256};

/// Accepts either the pinned leaf certificate or anything the platform trust
/// store already accepts.
#[derive(Debug)]
pub struct PinnedOrPlatformVerifier {
    pinned_sha256: [u8; 32],
    platform: Arc<dyn ServerCertVerifier>,
}

impl PinnedOrPlatformVerifier {
    fn matches_pin(&self, certificate: &CertificateDer<'_>) -> bool {
        let digest = Sha256::digest(certificate.as_ref());
        // Constant-time is unnecessary for a public certificate digest, but the
        // comparison is still over the full 32 bytes rather than a prefix.
        digest.as_slice() == self.pinned_sha256
    }
}

impl ServerCertVerifier for PinnedOrPlatformVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        if self.matches_pin(end_entity) {
            return Ok(ServerCertVerified::assertion());
        }
        self.platform
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        self.platform.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        self.platform.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.platform.supported_verify_schemes()
    }
}

pub(crate) fn parse_fingerprint(value: &str) -> Result<[u8; 32], TransportError> {
    let cleaned: String = value
        .chars()
        .filter(|c| !matches!(c, ':' | ' ' | '-'))
        .collect();
    if cleaned.len() != 64 || !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(TransportError::InvalidConfig(
            "TLS 指纹必须是 64 位 SHA-256 十六进制值".into(),
        ));
    }
    let mut bytes = [0u8; 32];
    for (index, slot) in bytes.iter_mut().enumerate() {
        let start = index * 2;
        *slot = u8::from_str_radix(&cleaned[start..start + 2], 16)
            .map_err(|_| TransportError::InvalidConfig("TLS 指纹无法解析".into()))?;
    }
    Ok(bytes)
}

pub(crate) fn client_config(fingerprint: &str) -> Result<ClientConfig, TransportError> {
    let pinned_sha256 = parse_fingerprint(fingerprint)?;
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));

    // Everything that is not the pinned certificate still goes through the
    // operating system trust store.
    let platform_verifier: Arc<dyn ServerCertVerifier> = Arc::new(
        rustls_platform_verifier::Verifier::new(Arc::clone(&provider)).map_err(|error| {
            TransportError::InvalidConfig(format!("系统证书校验不可用: {error}"))
        })?,
    );

    ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| TransportError::InvalidConfig(format!("TLS 配置无效: {error}")))
        .map(|builder| {
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(PinnedOrPlatformVerifier {
                    pinned_sha256,
                    platform: platform_verifier,
                }))
                .with_no_client_auth()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_are_accepted_in_the_shapes_operators_paste() {
        let plain = "a".repeat(64);
        let colons = (0..32).map(|_| "AA").collect::<Vec<_>>().join(":");
        assert!(parse_fingerprint(&plain).is_ok());
        assert!(parse_fingerprint(&colons).is_ok());
        assert_eq!(parse_fingerprint(&colons).unwrap(), [0xAAu8; 32]);
    }

    #[test]
    fn a_short_or_non_hex_fingerprint_is_refused() {
        for value in ["", "abc", &"z".repeat(64), &"a".repeat(63)] {
            assert!(
                parse_fingerprint(value).is_err(),
                "{value:?} must not be accepted as a pin"
            );
        }
    }
}
