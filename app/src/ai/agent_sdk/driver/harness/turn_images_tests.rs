use std::path::Path;

use serde_json::json;
use uuid::Uuid;

use super::*;

fn image(data: &str) -> TurnImage {
    TurnImage {
        mime_type: "image/png".to_string(),
        data: data.to_string(),
    }
}

#[test]
fn a_turns_images_are_taken_once() {
    let session = Uuid::new_v4();
    set_turn_images(&session, vec![image("AAAA")]);
    assert_eq!(take_turn_images(&session), vec![image("AAAA")]);
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
    assert_eq!(image_chars(&message), 0);
}

#[test]
fn ollama_gets_a_list_of_bare_base64() {
    let mut message = json!({ "role": "user", "content": "what is this" });
    attach_images(&mut message, &[image("AAAA"), image("BBBB")]);
    to_ollama_message(&mut message);
    assert_eq!(message["images"], json!(["AAAA", "BBBB"]));
    assert_eq!(message["content"], json!("what is this"));
}

#[test]
fn openai_gets_text_then_image_parts() {
    let mut message = json!({ "role": "user", "content": "what is this" });
    attach_images(&mut message, &[image("AAAA")]);
    to_openai_message(&mut message);
    assert_eq!(
        message["content"],
        json!([
            { "type": "text", "text": "what is this" },
            { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } },
        ])
    );
}

#[test]
fn images_are_measured_apart_from_the_text() {
    let mut message = json!({ "role": "user", "content": "hi" });
    attach_images(&mut message, &[image("AAAA")]);
    assert!(image_chars(&message) >= 4);
    assert!(message.to_string().len() > image_chars(&message));
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
