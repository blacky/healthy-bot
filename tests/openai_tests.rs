use healthy_bot::openai::sanitize_name;

#[test]
fn keeps_valid_names_unchanged() {
    assert_eq!(sanitize_name("cool_user-123"), "cool_user-123");
}

#[test]
fn replaces_spaces_and_dots() {
    assert_eq!(sanitize_name("John Doe.42"), "John_Doe_42");
}

#[test]
fn replaces_unicode() {
    // Each non-ASCII char becomes a single `_`.
    assert_eq!(sanitize_name("café"), "caf_");
    assert_eq!(sanitize_name("🎉party🎉"), "_party_");
}

#[test]
fn falls_back_when_nothing_valid_remains() {
    assert_eq!(sanitize_name(""), "user");
}

#[test]
fn truncates_to_64_chars() {
    let long = "a".repeat(100);
    assert_eq!(sanitize_name(&long).chars().count(), 64);
}

#[test]
fn test_chat_request_max_tokens_serialization() {
    use healthy_bot::openai::ChatRequest;

    let req = ChatRequest {
        model: "gpt-4o".to_string(),
        messages: vec![],
        max_completion_tokens: Some(300),
    };
    let json = serde_json::to_string(&req).unwrap();
    assert!(json.contains("\"max_completion_tokens\":300"));

    let req_no_tokens = ChatRequest {
        model: "gpt-4o".to_string(),
        messages: vec![],
        max_completion_tokens: None,
    };
    let json_no_tokens = serde_json::to_string(&req_no_tokens).unwrap();
    assert!(!json_no_tokens.contains("max_completion_tokens"));
}

#[test]
fn test_chat_response_usage_deserialization() {
    use healthy_bot::openai::ChatResponse;

    let raw = r#"{
        "choices": [{"message": {"content": "Hello"}, "finish_reason": "stop"}],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 20,
            "total_tokens": 30
        }
    }"#;

    let resp: ChatResponse = serde_json::from_str(raw).unwrap();
    assert_eq!(resp.choices.len(), 1);
    assert_eq!(resp.choices[0].message.content_text(), "Hello");
    assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("stop"));
    let usage = resp.usage.unwrap();
    assert_eq!(usage.prompt_tokens, 10);
    assert_eq!(usage.completion_tokens, 20);
    assert_eq!(usage.total_tokens, 30);
}

#[test]
fn test_chat_response_truncation_and_refusal_deserialization() {
    use healthy_bot::openai::ChatResponse;

    // Test length finish_reason with null content (e.g. max tokens hit during reasoning)
    let raw_length = r#"{
        "choices": [{"message": {"content": null}, "finish_reason": "length"}]
    }"#;
    let resp_length: ChatResponse = serde_json::from_str(raw_length).unwrap();
    assert_eq!(resp_length.choices[0].message.content_text(), "");
    assert_eq!(
        resp_length.choices[0].finish_reason.as_deref(),
        Some("length")
    );

    // Test safety refusal
    let raw_refusal = r#"{
        "choices": [{"message": {"content": null, "refusal": "Cannot process"}, "finish_reason": "stop"}]
    }"#;
    let resp_refusal: ChatResponse = serde_json::from_str(raw_refusal).unwrap();
    assert_eq!(resp_refusal.choices[0].message.content_text(), "");
    assert_eq!(
        resp_refusal.choices[0].message.refusal.as_deref(),
        Some("Cannot process")
    );
}
