//! Round-trip tests against the real router + a temp-file SQLite database:
//! signup → auth'd settings write/read, version conflicts, token hygiene.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use tower::ServiceExt as _;

use super::*;

fn test_state() -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = open_state(&dir.path().join("test.sqlite3")).expect("open db");
    (state, dir)
}

async fn call(
    state: &AppState,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let request = match body {
        Some(body) => request
            .header("content-type", "application/json")
            .body(Body::from(body.to_string())),
        None => request.body(Body::empty()),
    }
    .expect("request");

    let response = router(state.clone()).oneshot(request).await.expect("call");
    let status = response.status();
    let bytes = response.into_body().collect().await.expect("body").to_bytes();
    let value = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json body")
    };
    (status, value)
}

#[tokio::test]
async fn signup_login_and_settings_round_trip() {
    let (state, _dir) = test_state();

    // Sign up and get a token.
    let (status, body) = call(
        &state,
        "POST",
        "/v1/signup",
        None,
        Some(serde_json::json!({ "email": "Rob@Example.com", "password": "hunter22-strong" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let token = body["token"].as_str().expect("token").to_string();

    // Duplicate signup conflicts (email is normalized).
    let (status, _) = call(
        &state,
        "POST",
        "/v1/signup",
        None,
        Some(serde_json::json!({ "email": "rob@example.com ", "password": "hunter22-strong" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // First settings write starts at version 1.
    let (status, body) = call(
        &state,
        "PUT",
        "/v1/settings/global",
        Some(&token),
        Some(serde_json::json!({ "base_version": 0, "data": { "swear_mode": true } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], 1);

    // Read it back.
    let (status, body) = call(&state, "GET", "/v1/settings", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["scopes"]["global"]["version"], 1);
    assert_eq!(body["scopes"]["global"]["data"]["swear_mode"], true);

    // Login from "another machine" works and sees the same document.
    let (status, body) = call(
        &state,
        "POST",
        "/v1/login",
        None,
        Some(serde_json::json!({ "email": "rob@example.com", "password": "hunter22-strong" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let second_token = body["token"].as_str().expect("token").to_string();
    let (status, body) = call(&state, "GET", "/v1/settings", Some(&second_token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["scopes"]["global"]["version"], 1);
}

#[tokio::test]
async fn stale_writes_conflict_and_return_the_current_document() {
    let (state, _dir) = test_state();
    let (_, body) = call(
        &state,
        "POST",
        "/v1/signup",
        None,
        Some(serde_json::json!({ "email": "a@b.c", "password": "longenough" })),
    )
    .await;
    let token = body["token"].as_str().expect("token").to_string();

    for (base, expected) in [(0, 1), (1, 2)] {
        let (status, body) = call(
            &state,
            "PUT",
            "/v1/settings/global",
            Some(&token),
            Some(serde_json::json!({ "base_version": base, "data": { "n": expected } })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["version"], expected);
    }

    // A write based on version 1 loses: 409 plus the live version + data so
    // the client can merge without a second round trip.
    let (status, body) = call(
        &state,
        "PUT",
        "/v1/settings/global",
        Some(&token),
        Some(serde_json::json!({ "base_version": 1, "data": { "n": 99 } })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["version"], 2);
    assert_eq!(body["data"]["n"], 2);
}

#[tokio::test]
async fn auth_is_required_and_revocable_and_wrong_passwords_fail() {
    let (state, _dir) = test_state();

    let (status, _) = call(&state, "GET", "/v1/settings", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(&state, "GET", "/v1/settings", Some("bogus"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (_, body) = call(
        &state,
        "POST",
        "/v1/signup",
        None,
        Some(serde_json::json!({ "email": "a@b.c", "password": "longenough" })),
    )
    .await;
    let token = body["token"].as_str().expect("token").to_string();

    let (status, _) = call(
        &state,
        "POST",
        "/v1/login",
        None,
        Some(serde_json::json!({ "email": "a@b.c", "password": "wrong-password" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Unknown emails fail identically.
    let (status, _) = call(
        &state,
        "POST",
        "/v1/login",
        None,
        Some(serde_json::json!({ "email": "nobody@b.c", "password": "longenough" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Logout revokes the token.
    let (status, _) = call(&state, "POST", "/v1/logout", Some(&token), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&state, "GET", "/v1/settings", Some(&token), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn team_is_one_account_field() {
    let (state, _dir) = test_state();
    let (_, body) = call(
        &state,
        "POST",
        "/v1/signup",
        None,
        Some(serde_json::json!({ "email": "a@b.c", "password": "longenough" })),
    )
    .await;
    let token = body["token"].as_str().expect("token").to_string();

    let (status, body) = call(&state, "GET", "/v1/account", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["email"], "a@b.c");
    assert_eq!(body["team"], serde_json::Value::Null);

    let (status, body) = call(
        &state,
        "PUT",
        "/v1/account",
        Some(&token),
        Some(serde_json::json!({ "team": "l0 crew" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["team"], "l0 crew");
}
