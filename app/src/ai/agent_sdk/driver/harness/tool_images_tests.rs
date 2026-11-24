use std::time::{Duration, SystemTime};

use ai::agent::{ToolCallOutcome, TurnEvent};
use serde_json::json;

use super::*;

#[test]
fn finds_images_in_each_shape() {
    let messages_api = json!({
        "type": "image",
        "source": { "type": "base64", "media_type": "image/jpeg", "data": "AAAA" }
    });
    let mcp = json!({ "type": "image", "mimeType": "image/png", "data": "BBBB" });
    let read = json!({ "type": "image", "file": { "base64": "CCCC", "type": "image/webp" } });
    let answer = json!([{ "type": "text", "text": "done" }, messages_api, { "nested": [mcp] }, read]);
    assert_eq!(
        images_in(&answer),
        vec![
            ("image/jpeg".to_string(), "AAAA".to_string()),
            ("image/png".to_string(), "BBBB".to_string()),
            ("image/webp".to_string(), "CCCC".to_string()),
        ]
    );
}

#[test]
fn replaces_images_with_their_paths() {
    let mut answer = json!([
        { "type": "text", "text": "done" },
        { "type": "image", "mimeType": "image/png", "data": "BBBB" }
    ]);
    replace_images(&mut answer, &["/tmp/shot.png".to_string()]);
    assert_eq!(
        answer,
        json!([
            { "type": "text", "text": "done" },
            { "type": "image", "path": "/tmp/shot.png" }
        ])
    );
}

#[test]
fn shell_command_shows_the_image_it_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old.png");
    std::fs::write(&old, b"png").unwrap();
    let started = SystemTime::now() + Duration::from_secs(60);
    // Written before the call started (as far as the check can tell): not shown.
    assert!(images_written_by("cat old.png", started, dir.path()).is_empty());

    let started = SystemTime::now() - Duration::from_secs(1);
    std::fs::write(dir.path().join("shot.png"), b"png").unwrap();
    let paths = images_written_by("screencapture -x 'shot.png' && echo ok", started, dir.path());
    assert_eq!(paths.len(), 1);
    assert!(paths[0].ends_with("shot.png"));
}

#[test]
fn marker_lines_name_saved_images() {
    let dir = tempfile::tempdir().unwrap();
    let shot = dir.path().join("shot.png");
    std::fs::write(&shot, b"png").unwrap();
    let shot = shot.to_string_lossy().into_owned();
    let mut watch = ToolImageWatch::default();
    let event = TurnEvent::ToolCallResult {
        call_id: "call".to_string(),
        outcome: ToolCallOutcome::Success(json!({
            "result": format!("{}\nthe page", marker(&shot))
        })),
    };
    assert_eq!(watch.images_after(&event, dir.path()), vec![shot]);
}
