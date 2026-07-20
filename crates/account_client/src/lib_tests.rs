//! Tests for the account client.
//!
//! The unit tests cover the pure pieces (fingerprint formatting, URL
//! validation). The integration tests boot the *real* `account_server` over a
//! genuine TLS listener with its self-signed certificate, so the whole
//! pin-and-REST path is exercised end to end — including that a wrong pin aborts
//! before any credential is sent.

use std::sync::Once;

use crate::{
    Account, AccountClient, AccountError, ServerConfig, SettingsWrite, fingerprint_eq,
    fingerprint_of_der,
};

#[test]
fn fingerprint_is_uppercase_colon_hex_matching_the_server() {
    // SHA-256 of the empty input is a well-known constant; the server formats
    // the same digest the same way, so a value copied from its startup log
    // compares equal to what the client computes.
    let fingerprint = fingerprint_of_der(b"");
    assert_eq!(
        fingerprint,
        "E3:B0:C4:42:98:FC:1C:14:9A:FB:F4:C8:99:6F:B9:24:\
         27:AE:41:E4:64:9B:93:4C:A4:95:99:1B:78:52:B8:55"
            .replace(['\\', ' ', '\n'], "")
    );
    assert_eq!(fingerprint.matches(':').count(), 31);
}

#[test]
fn fingerprint_eq_ignores_case_and_separators() {
    assert!(fingerprint_eq("A1:B2:C3", "a1b2c3"));
    assert!(fingerprint_eq("a1:b2:c3", "A1:B2:C3"));
    assert!(fingerprint_eq(" a1 b2 c3 ", "A1B2C3"));
    assert!(!fingerprint_eq("A1:B2:C3", "A1:B2:C4"));
}

#[test]
fn new_rejects_unusable_urls() {
    let empty = AccountClient::new(ServerConfig {
        base_url: "   ".to_owned(),
        pinned_fingerprint: None,
    });
    assert!(matches!(empty, Err(AccountError::Config(_))));

    let non_http = AccountClient::new(ServerConfig {
        base_url: "ftp://example".to_owned(),
        pinned_fingerprint: None,
    });
    assert!(matches!(non_http, Err(AccountError::Config(_))));

    let ok = AccountClient::new(ServerConfig {
        base_url: "https://127.0.0.1:10101/".to_owned(),
        pinned_fingerprint: Some("A1:B2".to_owned()),
    });
    assert!(ok.is_ok());
}

// ---- integration against a real TLS account_server -----------------------

/// rustls needs a process-default crypto provider for the server side
/// (`axum-server`); install it once per test binary.
fn install_crypto_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

/// Boots `account_server` on an ephemeral localhost port over TLS with a
/// freshly minted self-signed certificate. Returns the base URL, the
/// certificate fingerprint to pin, and the temp dir (kept alive by the caller,
/// since it holds the SQLite file and the certificate).
async fn spawn_tls_server() -> (String, String, tempfile::TempDir) {
    install_crypto_provider();
    let dir = tempfile::tempdir().expect("tempdir");
    let state =
        account_server::open_state(&dir.path().join("account-server.sqlite3")).expect("open db");
    let certificate =
        account_server::tls::ensure_certificate(dir.path(), vec!["localhost".to_owned()])
            .expect("certificate");
    let fingerprint = certificate.fingerprint.clone();
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
        &certificate.cert_path,
        &certificate.key_path,
    )
    .await
    .expect("rustls config");

    // Grab an ephemeral port, then hand the address to axum-server to bind.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe");
    let addr = probe.local_addr().expect("probe addr");
    drop(probe);

    let router = account_server::router(state);
    tokio::spawn(async move {
        let _ = axum_server::bind_rustls(addr, tls)
            .serve(router.into_make_service())
            .await;
    });

    // Wait until the listener is actually accepting connections. Without this a
    // test that skips the health-check loop (e.g. the wrong-pin test, which
    // cannot health-check with a bad pin) can race startup and get a plain
    // connection-refused before any TLS handshake happens.
    for _ in 0..300 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    (format!("https://{addr}"), fingerprint, dir)
}

async fn wait_until_ready(client: &AccountClient) {
    for _ in 0..100 {
        if client.health().await.is_ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    client.health().await.expect("server never became reachable");
}

#[tokio::test]
async fn pinned_roundtrip_covers_every_endpoint() {
    let (base_url, fingerprint, _dir) = spawn_tls_server().await;
    let client = AccountClient::new(ServerConfig {
        base_url,
        pinned_fingerprint: Some(fingerprint.clone()),
    })
    .expect("client");

    wait_until_ready(&client).await;

    // The certificate the handshake presented is the one we pinned.
    assert!(fingerprint_eq(
        &client.observed_fingerprint().expect("observed a cert"),
        &fingerprint
    ));

    // Signup (email is normalized server-side).
    let token = client
        .signup("Person@Example.com", "hunter2!!")
        .await
        .expect("signup");
    assert!(!token.is_empty());

    // A second signup for the same email conflicts.
    assert!(matches!(
        client.signup("person@example.com", "hunter2!!").await,
        Err(AccountError::AccountExists)
    ));

    // Login works; a wrong password does not.
    let login_token = client
        .login("person@example.com", "hunter2!!")
        .await
        .expect("login");
    assert!(!login_token.is_empty());
    assert!(matches!(
        client.login("person@example.com", "wrong").await,
        Err(AccountError::InvalidCredentials)
    ));

    // Account read + team update.
    assert_eq!(
        client.get_account(&token).await.expect("account"),
        Account {
            email: "person@example.com".to_owned(),
            team: None,
        }
    );
    let updated = client.set_team(&token, Some("crew")).await.expect("team");
    assert_eq!(updated.team.as_deref(), Some("crew"));

    // Settings: create, conflict on a stale base_version, then a clean update.
    let created = client
        .put_settings(&token, "global", 0, &serde_json::json!({ "swear_mode": false }))
        .await
        .expect("create settings");
    assert!(matches!(created, SettingsWrite::Written { version: 1 }));

    match client
        .put_settings(&token, "global", 0, &serde_json::json!({ "swear_mode": true }))
        .await
        .expect("conflict")
    {
        SettingsWrite::Conflict { version, data } => {
            assert_eq!(version, 1);
            assert_eq!(data, serde_json::json!({ "swear_mode": false }));
        }
        other => panic!("expected a conflict, got {other:?}"),
    }

    let updated = client
        .put_settings(&token, "global", 1, &serde_json::json!({ "swear_mode": true }))
        .await
        .expect("update settings");
    assert!(matches!(updated, SettingsWrite::Written { version: 2 }));

    let settings = client.get_settings(&token).await.expect("get settings");
    let global = settings.scopes.get("global").expect("global scope");
    assert_eq!(global.version, 2);
    assert_eq!(global.data, serde_json::json!({ "swear_mode": true }));

    // Logout revokes the token; authenticated calls then fail closed.
    client.logout(&token).await.expect("logout");
    assert!(matches!(
        client.get_account(&token).await,
        Err(AccountError::Unauthorized)
    ));
}

#[tokio::test]
async fn wrong_pin_aborts_before_sending_credentials() {
    let (base_url, _fingerprint, _dir) = spawn_tls_server().await;
    // A fingerprint that is not the server's — the handshake must fail.
    let bogus = "00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:\
                 00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF"
        .replace(['\\', ' ', '\n'], "");
    let client = AccountClient::new(ServerConfig {
        base_url,
        pinned_fingerprint: Some(bogus),
    })
    .expect("client");

    let err = client
        .signup("a@b.com", "password1")
        .await
        .expect_err("signup must not succeed against an unpinned certificate");
    assert!(matches!(err, AccountError::Pinning(_)), "got {err:?}");
}

#[tokio::test]
async fn chat_roundtrip() {
    let (base_url, fingerprint, _dir) = spawn_tls_server().await;
    let client = AccountClient::new(ServerConfig {
        base_url,
        pinned_fingerprint: Some(fingerprint),
    })
    .expect("client");
    wait_until_ready(&client).await;

    let token = client
        .signup("chatter@example.com", "hunter2!!")
        .await
        .expect("signup");

    // The room starts empty.
    assert!(
        client
            .get_chat(&token, None)
            .await
            .expect("get chat")
            .is_empty()
    );

    // Posting trims and echoes the stored message with its author.
    let first = client
        .send_chat(&token, "  hello world  ")
        .await
        .expect("send first");
    assert_eq!(first.body, "hello world");
    assert_eq!(first.author_email, "chatter@example.com");
    let second = client.send_chat(&token, "second").await.expect("send second");
    assert!(second.id > first.id);

    // Reading returns the whole room, oldest first.
    let all = client.get_chat(&token, None).await.expect("get all");
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].body, "hello world");
    assert_eq!(all[1].body, "second");

    // `since` returns only newer messages.
    let newer = client
        .get_chat(&token, Some(first.id))
        .await
        .expect("get since");
    assert_eq!(newer.len(), 1);
    assert_eq!(newer[0].id, second.id);

    // Empty messages are rejected; unauthenticated access is refused.
    assert!(matches!(
        client.send_chat(&token, "   ").await,
        Err(AccountError::BadRequest(_))
    ));
    assert!(matches!(
        client.get_chat("bogus-token", None).await,
        Err(AccountError::Unauthorized)
    ));
    assert!(matches!(
        client.send_chat("bogus-token", "hi").await,
        Err(AccountError::Unauthorized)
    ));
}
