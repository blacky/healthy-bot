use healthy_bot::split_message;

#[test]
fn split_message_keeps_short_text_as_one_chunk() {
    assert_eq!(split_message("hello", 2000), vec!["hello"]);
}

#[test]
fn split_message_empty_yields_no_chunks() {
    assert!(split_message("", 2000).is_empty());
}

#[test]
fn split_message_splits_on_limit_and_reassembles() {
    let s = "a".repeat(2500);
    let chunks = split_message(&s, 2000);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].len(), 2000);
    assert_eq!(chunks[1].len(), 500);
    assert_eq!(chunks.concat(), s);
}

#[test]
fn split_message_never_splits_a_codepoint() {
    // 'ñ' is 2 bytes; an odd limit would land mid-codepoint on a naive byte cut.
    let s = "ñ".repeat(10); // 20 bytes, 10 chars
    let chunks = split_message(&s, 3);
    // Every chunk is valid UTF-8 (guaranteed by &str) and stays within the limit,
    // and the pieces reassemble to the original.
    assert!(chunks.iter().all(|c| c.len() <= 3));
    assert_eq!(chunks.concat(), s);
}
