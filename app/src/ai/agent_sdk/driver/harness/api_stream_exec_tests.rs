use ai::agent::TurnEvent;
use serde_json::{json, Value};

use super::{
    api_curl_args, assistant_tool_call_message, clip_for_model, compact_old_tool_results,
    error_from_body, ApiChatAccumulator, ApiToolCall, API_KEY_ENV, DROPPED_RESULT_PREFIX,
};
use crate::ai::api_providers::provider;

fn feed(lines: &[&str]) -> (ApiChatAccumulator, Vec<TurnEvent>) {
    let mut accumulator = ApiChatAccumulator::default();
    let mut events = Vec::new();
    for line in lines {
        // The stream runner hands each line over with its newline.
        events.extend(accumulator.push_line(&format!("{line}\n")));
    }
    (accumulator, events)
}

#[test]
fn curl_args_take_the_key_from_the_environment_not_the_arguments() {
    let zai = provider("zai").expect("Z.ai is a provider");
    let args = api_curl_args(zai);
    // The key is never an argument — only the variable that names it.
    assert!(args.contains(&format!("%{API_KEY_ENV}")), "{args:?}");
    let header = args
        .iter()
        .find(|arg| arg.starts_with("Authorization:"))
        .expect("an authorization header");
    assert_eq!(header, &format!("Authorization: Bearer {{{{{API_KEY_ENV}}}}}"));
    assert_eq!(
        args.last().map(String::as_str),
        Some("https://api.z.ai/api/paas/v4/chat/completions")
    );
    // The body goes over stdin.
    assert!(args.windows(2).any(|pair| pair == ["--data-binary", "@-"]));
}

#[test]
fn streamed_text_and_reasoning() {
    let (accumulator, events) = feed(&[
        r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"Let me think."}}]}"#,
        "",
        r#"data: {"choices":[{"index":0,"delta":{"content":"Hello"}}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"content":", world"}}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":5,"total_tokens":17}}"#,
        "data: [DONE]",
    ]);
    // Reasoning streams as it arrives; text is held for the end.
    assert_eq!(
        events,
        vec![TurnEvent::Thinking {
            text: "Let me think.".to_string()
        }]
    );
    let result = accumulator.finish().expect("a complete reply");
    assert_eq!(result.content, "Hello, world");
    assert!(result.tool_calls.is_empty());
    assert_eq!((result.prompt_tokens, result.completion_tokens), (12, 5));
}

#[test]
fn groq_reasoning_field_is_thinking_too() {
    let (_, events) = feed(&[r#"data: {"choices":[{"index":0,"delta":{"reasoning":"Hmm."}}]}"#]);
    assert_eq!(
        events,
        vec![TurnEvent::Thinking {
            text: "Hmm.".to_string()
        }]
    );
}

#[test]
fn tool_call_arguments_are_joined_across_fragments() {
    let (accumulator, _) = feed(&[
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"read_file","arguments":""}}]}}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]}}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"notes.txt\"}"}}]}}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_def","function":{"name":"run_code","arguments":"{\"language\":\"bash\",\"code\":\"ls\"}"}}]}}]}"#,
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
    ]);
    let result = accumulator.finish().expect("a complete reply");
    assert_eq!(result.tool_calls.len(), 2);
    let first = &result.tool_calls[0];
    assert_eq!(first.id, "call_abc");
    assert_eq!(first.name, "read_file");
    assert_eq!(first.arguments, Ok(json!({ "path": "notes.txt" })));
    let second = &result.tool_calls[1];
    assert_eq!(second.id, "call_def");
    assert_eq!(
        second.arguments,
        Ok(json!({ "language": "bash", "code": "ls" }))
    );
}

#[test]
fn a_whole_tool_call_without_index_or_id_gets_an_id() {
    // Gemini's shape: the call arrives in one piece, sometimes with no id.
    let (accumulator, _) = feed(&[
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"function":{"name":"read_file","arguments":"{\"path\":\"a\"}"}}]},"finish_reason":"tool_calls"}]}"#,
    ]);
    let result = accumulator.finish().expect("a finish reason ends it");
    assert_eq!(result.tool_calls.len(), 1);
    assert!(result.tool_calls[0].id.starts_with("call_"));
    assert_eq!(result.tool_calls[0].arguments, Ok(json!({ "path": "a" })));
}

#[test]
fn broken_tool_arguments_become_an_error_for_the_model() {
    let (accumulator, _) = feed(&[
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_x","function":{"name":"read_file","arguments":"{\"path\":"}}]},"finish_reason":"tool_calls"}]}"#,
    ]);
    let result = accumulator.finish().expect("still a reply");
    let error = result.tool_calls[0]
        .arguments
        .clone()
        .expect_err("not valid JSON");
    assert!(error.contains("not valid JSON"), "{error}");
}

#[test]
fn zai_error_body_is_reported() {
    let (accumulator, _) = feed(&[r#"{"error":{"code":"1000","message":"Authentication Failed"}}"#]);
    let error = accumulator.finish().err().expect("an error");
    assert_eq!(error, "Authentication Failed (code 1000)");
}

#[test]
fn gemini_pretty_printed_array_error_is_reported() {
    let (accumulator, _) = feed(&[
        "[{",
        r#"  "error": {"#,
        r#"    "code": 400,"#,
        r#"    "message": "API key not valid. Please pass a valid API key.","#,
        r#"    "status": "INVALID_ARGUMENT""#,
        "  }",
        "}]",
    ]);
    let error = accumulator.finish().err().expect("an error");
    assert_eq!(
        error,
        "API key not valid. Please pass a valid API key. (code 400)"
    );
}

#[test]
fn an_error_event_mid_stream_wins() {
    let (accumulator, _) = feed(&[
        r#"data: {"choices":[{"index":0,"delta":{"content":"partial"}}]}"#,
        r#"data: {"error":{"message":"Rate limit reached for model","type":"tokens","code":"rate_limit_exceeded"}}"#,
    ]);
    let error = accumulator.finish().err().expect("an error");
    assert!(error.starts_with("Rate limit reached"), "{error}");
}

#[test]
fn a_stream_cut_short_is_an_error() {
    let (accumulator, _) = feed(&[r#"data: {"choices":[{"index":0,"delta":{"content":"Hel"}}]}"#]);
    let error = accumulator.finish().err().expect("incomplete");
    assert!(error.contains("before it was complete"), "{error}");
}

#[test]
fn an_empty_response_is_an_error() {
    let (accumulator, _) = feed(&[]);
    assert!(accumulator.finish().is_err());
}

#[test]
fn flagged_content_is_reported_plainly() {
    let (accumulator, _) = feed(&[
        r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"sensitive"}]}"#,
        "data: [DONE]",
    ]);
    let error = accumulator.finish().err().expect("declined");
    assert!(error.contains("declined"), "{error}");
}

#[test]
fn error_bodies_of_each_shape() {
    assert_eq!(
        error_from_body(r#"{"error":{"message":"Invalid API Key","type":"invalid_request_error"}}"#),
        Some("Invalid API Key".to_string())
    );
    assert_eq!(
        error_from_body(r#"{"message":"Forbidden"}"#),
        Some("Forbidden".to_string())
    );
    assert_eq!(error_from_body("<html>Bad gateway</html>"), None);
}

#[test]
fn tool_call_turn_is_sent_back_with_ids_and_string_arguments() {
    let calls = [ApiToolCall {
        id: "call_abc".to_string(),
        name: "read_file".to_string(),
        arguments: Ok(json!({ "path": "notes.txt" })),
        extra_content: None,
    }];
    let message = assistant_tool_call_message("", &calls);
    assert_eq!(message["role"], "assistant");
    // Nothing said: null, not an empty string.
    assert_eq!(message["content"], Value::Null);
    let call = &message["tool_calls"][0];
    assert_eq!(call["id"], "call_abc");
    assert_eq!(call["type"], "function");
    assert_eq!(call["function"]["name"], "read_file");
    assert_eq!(call["function"]["arguments"], r#"{"path":"notes.txt"}"#);
}

#[test]
fn long_tool_results_keep_their_head_and_tail() {
    let limit = 60_000;
    let text = format!("{}MIDDLE{}", "a".repeat(limit), "z".repeat(limit));
    let clipped = clip_for_model(&text, limit);
    assert!(clipped.chars().count() < limit + 200);
    assert!(clipped.starts_with('a'));
    assert!(clipped.ends_with('z'));
    assert!(clipped.contains("characters omitted"));
    // Short results pass through untouched.
    assert_eq!(clip_for_model("short", limit), "short");
}

fn tool_message(content: &str) -> Value {
    json!({ "role": "tool", "tool_call_id": "call_x", "content": content })
}

#[test]
fn old_long_tool_results_are_dropped_and_recent_ones_kept() {
    let long = "x".repeat(5_000);
    let mut messages = vec![
        json!({ "role": "system", "content": "system" }),
        json!({ "role": "user", "content": "do it" }),
        tool_message(&long), // oldest: dropped
        tool_message("short"), // old but short: kept
        tool_message(&long), // within the two latest: kept
        json!({ "role": "assistant", "content": null }),
        tool_message(&long), // latest: kept
    ];
    compact_old_tool_results(&mut messages, 2, usize::MAX);

    let content = |index: usize| messages[index]["content"].as_str().unwrap_or("").to_string();
    assert!(content(2).starts_with(DROPPED_RESULT_PREFIX), "{}", content(2));
    assert!(content(2).contains("5000 characters"), "{}", content(2));
    assert_eq!(messages[2]["tool_call_id"], "call_x", "the message stays, only its text shrinks");
    assert_eq!(content(3), "short");
    assert_eq!(content(4), long);
    assert_eq!(content(6), long);
    // Other messages are untouched.
    assert_eq!(content(1), "do it");

    // A second pass changes nothing more.
    let before = messages.clone();
    compact_old_tool_results(&mut messages, 2, usize::MAX);
    assert_eq!(messages, before);
}

#[test]
fn gemini_thought_signature_goes_back_with_its_call() {
    let (accumulator, _) = feed(&[
        r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_g","type":"function","extra_content":{"google":{"thought_signature":"sig-123"}},"function":{"name":"run_code","arguments":"{\"language\":\"bash\",\"code\":\"ls\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        "data: [DONE]",
    ]);
    let result = accumulator.finish().expect("a complete reply");
    let message = assistant_tool_call_message("", &result.tool_calls);
    assert_eq!(
        message["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
        "sig-123"
    );
    // A call without one sends none.
    let plain = assistant_tool_call_message(
        "",
        &[ApiToolCall {
            id: "call_p".to_string(),
            name: "read_file".to_string(),
            arguments: Ok(json!({})),
            extra_content: None,
        }],
    );
    assert!(plain["tool_calls"][0].get("extra_content").is_none());
}

#[test]
fn tool_output_over_budget_is_dropped_but_the_latest_stays() {
    let big = "y".repeat(10_000);
    let mut messages = vec![tool_message(&big), tool_message(&big), tool_message(&big)];
    // Room for one result: only the latest stays, though three are "recent".
    compact_old_tool_results(&mut messages, 6, 15_000);
    let dropped = |index: usize| {
        messages[index]["content"]
            .as_str()
            .unwrap_or("")
            .starts_with(DROPPED_RESULT_PREFIX)
    };
    assert!(dropped(0));
    assert!(dropped(1));
    assert!(!dropped(2));
}
