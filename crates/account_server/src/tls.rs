//! Self-signed TLS with a pinned fingerprint.
//!
//! The whole certificate-authority apparatus — domains, Let's Encrypt,
//! renewal cron, a reverse proxy in front — exists to make *browsers* trust a
//! server they have never seen. Nothing here talks to a browser: the only
//! client is the app, so it can skip the CA system entirely and pin this
//! server's certificate the way SSH pins a host key.
//!
//! So: on first boot the service mints its own certificate into its state
//! directory and logs the SHA-256 fingerprint. The client records that
//! fingerprint the first time it connects and refuses to talk to anything
//! else afterwards (trust on first use). No domain is required, an IP is
//! fine, nothing expires for a century, and a rogue CA cannot forge it.
//!
//! What this does *not* do is prove the server's identity on the very first
//! connection. If the first connection is intercepted, the client pins the
//! attacker. Comparing the logged fingerprint against what the app shows on
//! first connect closes that gap, which is why it is printed at startup.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use base64::Engine as _;
use sha2::{Digest, Sha256};

const CERT_FILE: &str = "cert.pem";
const KEY_FILE: &str = "key.pem";

/// Paths to the certificate and private key, plus the fingerprint clients pin.
pub struct Certificate {
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    /// Colon-separated hex SHA-256 of the DER certificate, e.g. `A1:B2:...`.
    pub fingerprint: String,
}

/// Loads the certificate in `dir`, generating one if this is the first boot.
///
/// `sans` are the subject alternative names baked into a freshly generated
/// certificate. They matter little when the client pins a fingerprint, but a
/// well-formed certificate keeps ordinary TLS tools usable for debugging.
pub fn ensure_certificate(dir: &Path, sans: Vec<String>) -> anyhow::Result<Certificate> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("could not create TLS directory {}", dir.display()))?;
    let cert_path = dir.join(CERT_FILE);
    let key_path = dir.join(KEY_FILE);

    if !cert_path.exists() || !key_path.exists() {
        generate_into(&cert_path, &key_path, sans)?;
        log::info!("generated a self-signed certificate in {}", dir.display());
    }

    let cert_pem = std::fs::read_to_string(&cert_path)
        .with_context(|| format!("could not read {}", cert_path.display()))?;
    let fingerprint = fingerprint_of(&cert_pem)?;

    Ok(Certificate {
        cert_path,
        key_path,
        fingerprint,
    })
}

fn generate_into(cert_path: &Path, key_path: &Path, sans: Vec<String>) -> anyhow::Result<()> {
    let mut params = rcgen::CertificateParams::new(sans)
        .context("subject alternative names were not usable in a certificate")?;
    let mut name = rcgen::DistinguishedName::new();
    name.push(rcgen::DnType::CommonName, "account-server");
    params.distinguished_name = name;
    // Pinned certificates gain nothing from expiring — a rotation would just
    // break every client until they re-pin. Outlive the deployment instead.
    params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    params.not_after = rcgen::date_time_ymd(2120, 1, 1);

    let key_pair = rcgen::KeyPair::generate().context("could not generate a key pair")?;
    let cert = params
        .self_signed(&key_pair)
        .context("could not self-sign the certificate")?;

    std::fs::write(cert_path, cert.pem())
        .with_context(|| format!("could not write {}", cert_path.display()))?;
    write_private_key(key_path, &key_pair.serialize_pem())?;
    Ok(())
}

/// Writes the private key readable only by its owner. The systemd unit already
/// confines the state directory, but a key that is world-readable on disk is
/// worth preventing regardless of who else is meant to be on the box.
#[cfg(unix)]
fn write_private_key(path: &Path, pem: &str) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("could not create {}", path.display()))?;
    file.write_all(pem.as_bytes())
        .with_context(|| format!("could not write {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private_key(path: &Path, pem: &str) -> anyhow::Result<()> {
    std::fs::write(path, pem).with_context(|| format!("could not write {}", path.display()))?;
    Ok(())
}

/// SHA-256 over the DER certificate — the same bytes a client sees during the
/// handshake, so both sides compute the same value independently.
fn fingerprint_of(cert_pem: &str) -> anyhow::Result<String> {
    let der = der_from_pem(cert_pem)?;
    let digest = Sha256::digest(&der);
    Ok(digest
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":"))
}

/// Extracts the DER body of the first certificate in a PEM file. Deliberately
/// hand-rolled rather than pulling a PEM crate: it is base64 between two
/// marker lines, and this service's dependency list is part of its design.
fn der_from_pem(pem: &str) -> anyhow::Result<Vec<u8>> {
    const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
    const END: &str = "-----END CERTIFICATE-----";

    let body: String = pem
        .lines()
        .skip_while(|line| line.trim() != BEGIN)
        .skip(1)
        .take_while(|line| line.trim() != END)
        .map(str::trim)
        .collect();
    if body.is_empty() {
        anyhow::bail!("no certificate found in PEM");
    }
    base64::engine::general_purpose::STANDARD
        .decode(body)
        .context("certificate body was not valid base64")
}

#[cfg(test)]
#[path = "tls_tests.rs"]
mod tests;
