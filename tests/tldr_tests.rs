use healthy_bot::commands::{build_transcript, TranscriptEntry};
use healthy_bot::split_message;

fn entry(name: &str, content: &str, is_bot: bool) -> TranscriptEntry {
    TranscriptEntry {
        name: name.to_string(),
        content: content.to_string(),
        is_bot,
    }
}

#[test]
fn transcript_is_chronological_from_newest_first_input() {
    // Discord returns messages newest-first; the transcript must read oldest-first.
    let msgs = vec![
        entry("Bob", "third", false),
        entry("Alice", "second", false),
        entry("Bob", "first", false),
    ];
    let out = build_transcript(&msgs).unwrap();
    assert_eq!(out, "Bob: first\nAlice: second\nBob: third");
}

#[test]
fn transcript_drops_bots_and_blank_messages() {
    let msgs = vec![
        entry("HealthyBot", "beep boop", true),
        entry("Alice", "   ", false),
        entry("Bob", "real message", false),
    ];
    let out = build_transcript(&msgs).unwrap();
    assert_eq!(out, "Bob: real message");
}

#[test]
fn transcript_is_none_when_nothing_summarizable() {
    let msgs = vec![entry("HealthyBot", "beep", true), entry("Alice", "", false)];
    assert!(build_transcript(&msgs).is_none());
}

#[test]
fn transcript_is_none_for_empty_input() {
    assert!(build_transcript(&[]).is_none());
}

#[test]
fn transcript_trims_message_whitespace() {
    let msgs = vec![entry("Alice", "  hello  ", false)];
    assert_eq!(build_transcript(&msgs).unwrap(), "Alice: hello");
}

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
