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
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
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

/// Signs up an account and returns its bearer token.
async fn signup(state: &AppState, email: &str) -> String {
    let (status, body) = call(
        state,
        "POST",
        "/v1/signup",
        None,
        Some(serde_json::json!({ "email": email, "password": "hunter22-strong" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["token"].as_str().expect("token").to_string()
}

#[tokio::test]
async fn share_round_trip_is_readable_without_an_account() {
    let (state, _dir) = test_state();
    let token = signup(&state, "sharer@example.com").await;

    let (status, body) = call(
        &state,
        "POST",
        "/v1/shares",
        Some(&token),
        Some(serde_json::json!({
            "kind": "workflow",
            "name": "Deploy the thing",
            "data": "ciphertext-goes-here",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().expect("share id").to_string();
    assert_eq!(body["kind"], "workflow");
    assert_eq!(body["name"], "Deploy the thing");

    // The whole point: a recipient with no token can fetch it.
    let (status, body) = call(&state, "GET", &format!("/v1/shares/{id}"), None, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"], "ciphertext-goes-here");
    assert_eq!(body["kind"], "workflow");
}

#[tokio::test]
async fn unknown_share_id_is_not_found() {
    let (state, _dir) = test_state();
    let (status, _) = call(&state, "GET", "/v1/shares/nope", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn creating_a_share_requires_a_token() {
    let (state, _dir) = test_state();
    let (status, _) = call(
        &state,
        "POST",
        "/v1/shares",
        None,
        Some(serde_json::json!({ "kind": "rule", "name": "n", "data": "d" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn shares_list_only_your_own_and_omit_payloads() {
    let (state, _dir) = test_state();
    let mine = signup(&state, "mine@example.com").await;
    let theirs = signup(&state, "theirs@example.com").await;

    for name in ["one", "two"] {
        let (status, _) = call(
            &state,
            "POST",
            "/v1/shares",
            Some(&mine),
            Some(serde_json::json!({ "kind": "notebook", "name": name, "data": "c" })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let (status, _) = call(
        &state,
        "POST",
        "/v1/shares",
        Some(&theirs),
        Some(serde_json::json!({ "kind": "notebook", "name": "not yours", "data": "c" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = call(&state, "GET", "/v1/shares", Some(&mine), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed = body["shares"].as_array().expect("shares array");
    assert_eq!(listed.len(), 2, "only the caller's own shares");
    assert!(
        listed.iter().all(|s| s.get("data").is_none()),
        "listings must not ship ciphertext"
    );
}

#[tokio::test]
async fn only_the_owner_can_revoke_a_share() {
    let (state, _dir) = test_state();
    let owner = signup(&state, "owner@example.com").await;
    let stranger = signup(&state, "stranger@example.com").await;

    let (_, body) = call(
        &state,
        "POST",
        "/v1/shares",
        Some(&owner),
        Some(serde_json::json!({ "kind": "env_vars", "name": "prod", "data": "c" })),
    )
    .await;
    let id = body["id"].as_str().expect("share id").to_string();

    // Someone else holding the id can read it but cannot revoke it.
    let (status, _) = call(
        &state,
        "DELETE",
        &format!("/v1/shares/{id}"),
        Some(&stranger),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&state, "GET", &format!("/v1/shares/{id}"), None, None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "stranger's delete must not have worked"
    );

    // The owner can, and the link dies with it.
    let (status, _) = call(
        &state,
        "DELETE",
        &format!("/v1/shares/{id}"),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&state, "GET", &format!("/v1/shares/{id}"), None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn shares_capture_the_team_at_time_of_sharing() {
    let (state, _dir) = test_state();
    let token = signup(&state, "teamed@example.com").await;

    let (status, _) = call(
        &state,
        "PUT",
        "/v1/account",
        Some(&token),
        Some(serde_json::json!({ "team": "mountains" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, body) = call(
        &state,
        "POST",
        "/v1/shares",
        Some(&token),
        Some(serde_json::json!({ "kind": "workflow", "name": "w", "data": "c" })),
    )
    .await;
    assert_eq!(body["team"], "mountains");

    // Leaving the team must not re-scope what was already shared.
    let (status, _) = call(
        &state,
        "PUT",
        "/v1/account",
        Some(&token),
        Some(serde_json::json!({ "team": null })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = call(&state, "GET", "/v1/shares", Some(&token), None).await;
    assert_eq!(body["shares"][0]["team"], "mountains");
}

#[tokio::test]
async fn share_field_bounds_are_enforced() {
    let (state, _dir) = test_state();
    let token = signup(&state, "bounds@example.com").await;

    for body in [
        serde_json::json!({ "kind": "", "name": "n", "data": "d" }),
        serde_json::json!({ "kind": "k", "name": "", "data": "d" }),
        serde_json::json!({ "kind": "k", "name": "n", "data": "" }),
        serde_json::json!({ "kind": "k".repeat(64), "name": "n", "data": "d" }),
        serde_json::json!({ "kind": "k", "name": "n".repeat(500), "data": "d" }),
    ] {
        let (status, _) = call(&state, "POST", "/v1/shares", Some(&token), Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
