use std::path::Path;

use serde_json::json;

use super::*;

#[test]
fn size_is_width_by_height_within_range() {
    assert_eq!(parse_size("1024x768"), Some((1024, 768)));
    assert_eq!(parse_size(" 512 X 512 "), Some((512, 512)));
    assert_eq!(parse_size("100x100"), None);
    assert_eq!(parse_size("4096x4096"), None);
    assert_eq!(parse_size("big"), None);
}

#[test]
fn a_request_needs_a_prompt_and_reads_the_rest() {
    assert!(ImageRequest::from_arguments(&json!({})).is_err());
    assert!(ImageRequest::from_arguments(&json!({ "prompt": "   " })).is_err());
    assert!(ImageRequest::from_arguments(&json!({ "prompt": "a knob", "size": "9x9" })).is_err());

    let request = ImageRequest::from_arguments(&json!({
        "prompt": " a chrome knob ",
        "size": "512x256",
        "seed": 7,
        "transparent": true,
        "out": "/tmp/knob.png",
        "reference_image": "/tmp/ref.png",
    }))
    .expect("a valid request");
    assert_eq!(request.prompt, "a chrome knob");
    assert_eq!((request.width, request.height), (512, 256));
    assert_eq!(request.seed, Some(7));
    assert!(request.transparent);
    assert_eq!(request.out, Some(Path::new("/tmp/knob.png").to_path_buf()));
    assert_eq!(request.reference, Some(Path::new("/tmp/ref.png").to_path_buf()));

    let plain = ImageRequest::from_arguments(&json!({ "prompt": "x" })).expect("a valid request");
    assert_eq!((plain.width, plain.height), (1024, 1024));
    assert!(!plain.transparent);
    assert_eq!(plain.backend, None);
}

#[test]
fn file_names_come_from_the_first_words() {
    assert_eq!(slug("A chrome push-button, top-down!"), "a-chrome-push-button-top-down");
    assert_eq!(slug("???"), "image");
    assert!(slug(&"word ".repeat(30)).len() <= 40);
}

#[test]
fn a_png_header_gives_its_size() {
    let dir = std::env::temp_dir().join(format!("krust-image-gen-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp folder");
    let path = dir.join("tiny.png");
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend([0, 0, 0, 13]);
    bytes.extend(b"IHDR");
    bytes.extend(300u32.to_be_bytes());
    bytes.extend(200u32.to_be_bytes());
    std::fs::write(&path, &bytes).expect("a temp file");
    assert_eq!(png_size(&path), Some((300, 200)));
    std::fs::write(&path, b"not a png at all, not at all").expect("a temp file");
    assert_eq!(png_size(&path), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn picture_data_decodes_with_or_without_a_data_url_prefix() {
    assert_eq!(decode_image("aGVsbG8=").expect("base64"), b"hello");
    assert_eq!(
        decode_image("data:image/png;base64,aGVsbG8=").expect("base64"),
        b"hello"
    );
    assert!(decode_image("!!!").is_err());
}

#[test]
fn an_engine_answer_is_an_error_or_a_saved_file() {
    let dir = std::env::temp_dir().join(format!("krust-image-gen-engine-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp folder");
    let out = dir.join("made.png");

    let error = check_command_output("{\"error\": \"no model\"}\n", &out);
    assert_eq!(error, Err("no model".to_string()));
    assert!(check_command_output("", &out).is_err());
    assert!(check_command_output("warming up\nnot json\n", &out).is_err());
    // Reported success but no file.
    assert!(check_command_output("{\"path\": \"x\"}\n", &out).is_err());
    std::fs::write(&out, b"png").expect("a temp file");
    assert_eq!(check_command_output("noise\n{\"path\": \"x\"}\n", &out), Ok(()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_picture_is_drawn_in_chunks_that_add_back_up() {
    let png: Vec<u8> = (0..10_000u32).map(|n| (n % 251) as u8).collect();
    let sequence = kitty_sequence(&png, 40);
    assert!(sequence.starts_with("\x1b_Ga=T,f=100,t=d,c=40,m=1;"));
    assert!(sequence.ends_with("\x1b\\"));

    let mut data = String::new();
    let mut more_flags = Vec::new();
    for piece in sequence.split("\x1b_G").skip(1) {
        let (control, rest) = piece.split_once(';').expect("a control part");
        let chunk = rest.trim_end_matches("\x1b\\");
        assert!(chunk.len() <= 4096);
        more_flags.push(control.ends_with("m=1"));
        data.push_str(chunk);
    }
    // Every chunk but the last says more follows.
    assert_eq!(more_flags.last(), Some(&false));
    assert!(more_flags[..more_flags.len() - 1].iter().all(|more| *more));
    assert_eq!(decode_image(&data).expect("base64"), png);
}

#[test]
fn a_result_reports_a_markdown_image() {
    let result = ImageResult {
        path: Path::new("/tmp/knob.png").to_path_buf(),
        width: 512,
        height: 512,
        backend: "local command".to_string(),
        seconds: 12.34,
    };
    let json = result.to_json();
    assert_eq!(json["path"], "/tmp/knob.png");
    assert_eq!(json["markdown"], "![](/tmp/knob.png)");
    assert_eq!(json["seconds"], 12.3);
}
