//! rustls client configs shared by network clients (MQTT, PostgreSQL), and the process-wide
//! default provider used by reqwest (HTTP functions, S3).
//!
//! ring is the only crypto provider: reqwest is built with `rustls-no-provider`, so it uses the
//! default provider installed by [`install_default_provider`].

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use std::sync::Arc;

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Installs ring as the process-wide default rustls provider; must be called before a reqwest
/// client is built. Later calls do nothing.
pub(crate) fn install_default_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// TLS config verifying the server certificate against the platform root certificates.
pub(crate) fn client_config() -> Result<ClientConfig, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    let config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("invalid TLS config: {}", e))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(config)
}

/// TLS config that encrypts without verifying the server certificate or host name
/// (libpq `sslmode=prefer/require` semantics).
pub(crate) fn client_config_no_verify() -> Result<ClientConfig, String> {
    let provider = provider();
    let config = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("invalid TLS config: {}", e))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoCertVerifier(provider)))
        .with_no_client_auth();
    Ok(config)
}

/// Accepts any server certificate. Handshake signatures are not checked either: without a trusted
/// certificate they prove nothing, and webpki would reject X.509 v1 self-signed certificates.
#[derive(Debug)]
struct NoCertVerifier(Arc<CryptoProvider>);

impl ServerCertVerifier for NoCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
