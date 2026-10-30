use std::path::Path;

use serde_json::json;
use uuid::Uuid;

use super::*;

fn image(path: &str) -> TurnImage {
    TurnImage {
        mime_type: "image/png".to_string(),
        path: path.to_string(),
    }
}

/// A small file with known bytes in a fresh directory, kept alive by the returned guard.
fn image_file() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("knob.png");
    std::fs::write(&path, b"abc").unwrap();
    (dir, path.to_string_lossy().into_owned())
}

#[test]
fn a_turns_images_are_taken_once() {
    let session = Uuid::new_v4();
    set_turn_images(&session, vec![image("/a.png")]);
    assert_eq!(take_turn_images(&session), vec![image("/a.png")]);
    assert!(take_turn_images(&session).is_empty());
}

#[test]
fn a_message_with_no_images_is_left_alone() {
    let mut message = json!({ "role": "user", "content": "hello" });
    attach_images(&mut message, &[]);
    let before = message.clone();
    to_ollama_message(&mut message);
    to_openai_message(&mut message);
    assert_eq!(message, before);
}

#[test]
fn the_history_holds_a_link_not_the_bytes() {
    let mut message = json!({ "role": "user", "content": "what is this" });
    attach_images(&mut message, &[image("/a.png")]);
    assert_eq!(
        message[IMAGES_FIELD],
        json!([{ "mime_type": "image/png", "path": "/a.png" }])
    );
}

#[test]
fn ollama_gets_a_list_of_bare_base64() {
    let (_dir, path) = image_file();
    let mut message = json!({ "role": "user", "content": "what is this" });
    attach_images(&mut message, &[image(&path)]);
    to_ollama_message(&mut message);
    assert_eq!(message["images"], json!(["YWJj"]));
    assert_eq!(message["content"], json!("what is this"));
}

#[test]
fn openai_gets_text_then_image_parts() {
    let (_dir, path) = image_file();
    let mut message = json!({ "role": "user", "content": "what is this" });
    attach_images(&mut message, &[image(&path)]);
    to_openai_message(&mut message);
    assert_eq!(
        message["content"],
        json!([
            { "type": "text", "text": "what is this" },
            { "type": "image_url", "image_url": { "url": "data:image/png;base64,YWJj" } },
        ])
    );
}

#[test]
fn a_linked_file_that_is_gone_leaves_the_text() {
    let mut message = json!({ "role": "user", "content": "what is this" });
    attach_images(&mut message, &[image("/nonexistent-krust-test-dir/a.png")]);
    to_openai_message(&mut message);
    assert_eq!(message["content"], json!("what is this"));
    to_ollama_message(&mut message);
    assert!(message.get("images").is_none());
}

#[test]
fn only_image_files_have_a_mime_type() {
    assert_eq!(mime_type_for(Path::new("a/knob.PNG")), Some("image/png"));
    assert_eq!(mime_type_for(Path::new("a.jpeg")), Some("image/jpeg"));
    assert_eq!(mime_type_for(Path::new("a.txt")), None);
    assert_eq!(mime_type_for(Path::new("noextension")), None);
}

#[test]
fn relative_paths_are_under_the_working_dir() {
    let dir = Path::new("/work");
    assert_eq!(resolved_path("pics/a.png", dir), Path::new("/work/pics/a.png"));
    assert_eq!(resolved_path("/abs/a.png", dir), Path::new("/abs/a.png"));
}

#[test]
fn a_task_names_files_that_are_checked_and_linked() {
    let (dir, path) = image_file();
    let loaded = load_image_files(&["knob.png".to_string()], dir.path()).unwrap();
    assert_eq!(loaded, vec![image(&path)]);
    assert_eq!(image_paths(&loaded), vec![path]);
}

#[test]
fn a_file_that_is_not_an_image_or_not_there_is_an_error_naming_it() {
    let dir = Path::new("/nonexistent-krust-test-dir");
    let not_image = load_image_files(&["notes.txt".to_string()], dir).unwrap_err();
    assert!(not_image.contains("notes.txt"));
    let missing = load_image_files(&["gone.png".to_string()], dir).unwrap_err();
    assert!(missing.contains("gone.png"));
    assert_eq!(load_image_files(&[], dir), Ok(Vec::new()));
}

#[test]
fn image_paths_follow_the_prompt() {
    assert_eq!(prompt_with_image_paths("look", &[]), "look");
    assert_eq!(
        prompt_with_image_paths("look", &["/a.png".to_string(), "/b.png".to_string()]),
        "look\n\nImages to look at:\n/a.png\n/b.png"
    );
}
