//! Certificate bootstrap: generated once, reused after, fingerprint stable.

use super::*;

#[test]
fn first_boot_generates_a_certificate_and_later_boots_reuse_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tls");

    let first = ensure_certificate(&path, vec!["localhost".to_owned()]).expect("first boot");
    assert!(first.cert_path.exists());
    assert!(first.key_path.exists());

    // A restart must not mint a new certificate — every pinned client would
    // start refusing to connect.
    let second = ensure_certificate(&path, vec!["localhost".to_owned()]).expect("second boot");
    assert_eq!(first.fingerprint, second.fingerprint);
}

#[test]
fn separate_deployments_get_distinct_certificates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let one = ensure_certificate(&dir.path().join("a"), vec!["localhost".to_owned()])
        .expect("first server");
    let two = ensure_certificate(&dir.path().join("b"), vec!["localhost".to_owned()])
        .expect("second server");
    assert_ne!(one.fingerprint, two.fingerprint);
}

#[test]
fn fingerprint_is_colon_separated_sha256_hex() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cert = ensure_certificate(dir.path(), vec!["localhost".to_owned()]).expect("cert");

    let parts: Vec<&str> = cert.fingerprint.split(':').collect();
    assert_eq!(parts.len(), 32, "SHA-256 is 32 bytes: {}", cert.fingerprint);
    assert!(
        parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit())),
        "each byte is two hex digits: {}",
        cert.fingerprint
    );
}

#[test]
fn fingerprint_matches_the_der_a_client_would_hash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cert = ensure_certificate(dir.path(), vec!["localhost".to_owned()]).expect("cert");

    // Recompute independently from the PEM on disk, the way a client computes
    // it from the certificate the handshake presents.
    let pem = std::fs::read_to_string(&cert.cert_path).expect("read cert");
    let der = der_from_pem(&pem).expect("der");
    let expected: String = Sha256::digest(&der)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":");
    assert_eq!(cert.fingerprint, expected);
}

#[test]
fn malformed_pem_is_rejected_rather_than_hashed() {
    assert!(der_from_pem("not a certificate").is_err());
    assert!(der_from_pem("-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----").is_err());
}

#[cfg(unix)]
#[test]
fn private_key_is_not_world_readable() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let cert = ensure_certificate(dir.path(), vec!["localhost".to_owned()]).expect("cert");
    let mode = std::fs::metadata(&cert.key_path)
        .expect("key metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "group/other bits set: {mode:o}");
}
