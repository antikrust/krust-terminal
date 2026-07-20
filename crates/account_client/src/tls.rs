//! Client half of the server's fingerprint pinning.
//!
//! The account server presents a self-signed certificate and prints its
//! SHA-256 fingerprint at startup (see `account_server::tls`). There is no CA,
//! domain, or webpki root involved: the client trusts exactly one certificate,
//! identified by that fingerprint, the way SSH trusts a host key.
//!
//! This module builds a [`reqwest::Client`] whose TLS layer replaces the usual
//! chain-and-hostname validation with a single check — does the presented
//! certificate hash to the fingerprint we pinned? Two modes:
//!
//! * **pinned**: an expected fingerprint is known (the user pasted it, or we
//!   stored it on a previous connect). Anything else aborts the handshake, so a
//!   request body — a password on signup/login — is never sent to an impostor.
//! * **trust on first use**: no fingerprint is known yet. The first certificate
//!   is accepted and its fingerprint recorded via `observed`, so the caller can
//!   persist it and pin every connection afterwards. This is the one moment the
//!   connection is unauthenticated; comparing the recorded value against the
//!   fingerprint the server logged closes the gap.

use std::sync::{Arc, Mutex};

use rustls::DigitallySignedStruct;
use rustls::SignatureScheme;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};

use crate::AccountError;

/// SHA-256 over the DER certificate, formatted as uppercase colon-separated hex
/// (`A1:B2:…`). Byte-for-byte the same computation as the server's
/// `fingerprint_of`, so a fingerprint copied from the server log compares equal.
pub fn fingerprint_of_der(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Compares two fingerprints tolerantly: case-insensitive and ignoring any
/// separators, so `a1:b2`, `A1B2`, and `A1:B2` all match. Lets a user paste the
/// fingerprint in whatever shape they copied it.
pub fn fingerprint_eq(a: &str, b: &str) -> bool {
    normalize(a) == normalize(b)
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_hexdigit())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// A rustls verifier that pins one certificate by fingerprint. Signature
/// verification is still delegated to the crypto provider, so the handshake's
/// cryptographic integrity is intact — only the *identity* check is replaced.
#[derive(Debug)]
struct PinnedVerifier {
    /// The fingerprint we require, if known. `None` means trust on first use.
    expected: Option<String>,
    /// Records the fingerprint of the certificate we were presented, so a
    /// first-use connection can be pinned afterwards.
    observed: Arc<Mutex<Option<String>>>,
    supported_algs: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fingerprint = fingerprint_of_der(end_entity.as_ref());
        if let Ok(mut slot) = self.observed.lock() {
            *slot = Some(fingerprint.clone());
        }
        match &self.expected {
            Some(expected) if !fingerprint_eq(expected, &fingerprint) => Err(
                rustls::Error::General(format!("pinned certificate mismatch: got {fingerprint}")),
            ),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.supported_algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.supported_algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.supported_algs.supported_schemes()
    }
}

/// Builds a reqwest client that pins the server certificate by fingerprint.
///
/// `expected` is the pinned fingerprint, or `None` for trust-on-first-use.
/// `observed` receives the fingerprint of whatever certificate the server
/// presents, so a first-use caller can persist and pin it. Plain-HTTP base URLs
/// bypass all of this — the pinning only engages on an `https` handshake.
pub(crate) fn pinned_client(
    expected: Option<String>,
    observed: Arc<Mutex<Option<String>>>,
) -> Result<reqwest::Client, AccountError> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let supported_algs = provider.signature_verification_algorithms;

    let verifier = Arc::new(PinnedVerifier {
        expected,
        observed,
        supported_algs,
    });

    let tls_config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| AccountError::Config(format!("could not initialize TLS: {e}")))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();

    reqwest::Client::builder()
        .tls_backend_preconfigured(tls_config)
        .build()
        .map_err(|e| AccountError::Config(format!("could not build HTTP client: {e}")))
}
