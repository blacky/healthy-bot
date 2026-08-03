use healthy_bot::memory::{
    build_extraction_messages, format_memory_context, parse_extracted_facts,
};

#[test]
fn parses_a_plain_json_array() {
    let facts = parse_extracted_facts(
        r#"[{"user_id":"1","fact":"likes hiking"},{"user_id":"2","fact":"hates Mondays"}]"#,
    );
    assert_eq!(facts.len(), 2);
    assert_eq!(facts[0].user_id, "1");
    assert_eq!(facts[0].fact, "likes hiking");
    assert_eq!(facts[1].fact, "hates Mondays");
}

#[test]
fn salvages_json_wrapped_in_fences_and_prose() {
    let content = "Sure! Here are the facts:\n```json\n[{\"user_id\":\"7\",\"fact\":\"has a dog\"}]\n```\nHope that helps.";
    let facts = parse_extracted_facts(content);
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].user_id, "7");
    assert_eq!(facts[0].fact, "has a dog");
}

#[test]
fn drops_empty_ids_or_facts_and_trims() {
    let facts = parse_extracted_facts(
        r#"[{"user_id":" 5 ","fact":"  runs marathons "},{"user_id":"","fact":"x"},{"user_id":"9","fact":"  "}]"#,
    );
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].user_id, "5");
    assert_eq!(facts[0].fact, "runs marathons");
}

#[test]
fn returns_empty_on_garbage_or_empty_array() {
    assert!(parse_extracted_facts("not json at all").is_empty());
    assert!(parse_extracted_facts("[]").is_empty());
    assert!(parse_extracted_facts("").is_empty());
}

#[test]
fn format_context_none_when_no_facts() {
    assert!(format_memory_context(&[]).is_none());
    assert!(format_memory_context(&[("Alice".to_string(), vec![])]).is_none());
}

#[test]
fn format_context_lists_people_and_facts() {
    let people = vec![
        (
            "Alice".to_string(),
            vec!["likes hiking".to_string(), "has a dog".to_string()],
        ),
        ("Bob".to_string(), vec!["hates Mondays".to_string()]),
    ];
    let out = format_memory_context(&people).unwrap();
    assert!(out.contains("- Alice: likes hiking; has a dog"));
    assert!(out.contains("- Bob: hates Mondays"));
}

#[test]
fn extraction_prompt_embeds_participant_ids_and_transcript() {
    let participants = vec![
        ("111".to_string(), "Alice".to_string()),
        ("222".to_string(), "Bob".to_string()),
    ];
    let msgs = build_extraction_messages(&participants, "Alice: hi\nBob: yo");
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].role, "system");
    let system = msgs[0].content.to_string();
    assert!(system.contains("111 (Alice)"));
    assert!(system.contains("222 (Bob)"));
    assert_eq!(msgs[1].role, "user");
    assert_eq!(msgs[1].content.to_string(), "Alice: hi\nBob: yo");
}
