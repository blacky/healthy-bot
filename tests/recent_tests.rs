use healthy_bot::recent::{build_chat_messages, build_transcript, RecentMessage};

fn msg(name: &str, content: &str, is_bot: bool) -> RecentMessage {
    RecentMessage {
        author_name: name.to_string(),
        content: content.to_string(),
        is_bot,
    }
}

// --- build_transcript (tldr) ---

#[test]
fn transcript_is_chronological_from_newest_first_input() {
    // Discord returns messages newest-first; the transcript must read oldest-first.
    let msgs = vec![
        msg("Bob", "third", false),
        msg("Alice", "second", false),
        msg("Bob", "first", false),
    ];
    assert_eq!(
        build_transcript(&msgs).unwrap(),
        "Bob: first\nAlice: second\nBob: third"
    );
}

#[test]
fn transcript_drops_bots_and_blank_messages() {
    let msgs = vec![
        msg("HealthyBot", "beep boop", true),
        msg("Alice", "   ", false),
        msg("Bob", "real message", false),
    ];
    assert_eq!(build_transcript(&msgs).unwrap(), "Bob: real message");
}

#[test]
fn transcript_is_none_when_nothing_summarizable() {
    let msgs = vec![msg("HealthyBot", "beep", true), msg("Alice", "", false)];
    assert!(build_transcript(&msgs).is_none());
}

#[test]
fn transcript_is_none_for_empty_input() {
    assert!(build_transcript(&[]).is_none());
}

#[test]
fn transcript_trims_message_whitespace() {
    let msgs = vec![msg("Alice", "  hello  ", false)];
    assert_eq!(build_transcript(&msgs).unwrap(), "Alice: hello");
}

// --- build_chat_messages (chime) ---

#[test]
fn chat_messages_order_chronologically_and_map_roles() {
    // Discord returns newest-first; the prompt must read oldest-first. Unlike the
    // transcript, the bot's own messages are kept (as `assistant`).
    let msgs = vec![
        msg("Bob", "third", false),
        msg("HealthyBot", "second", true),
        msg("Alice", "first", false),
    ];
    let out = build_chat_messages("SYS", &msgs);
    assert_eq!(out.len(), 4);
    assert_eq!(out[0].role, "system");
    assert_eq!(out[0].content.to_string(), "SYS");
    assert_eq!(out[1].role, "user"); // Alice, oldest
    assert_eq!(out[1].content.to_string(), "first");
    assert_eq!(out[2].role, "assistant"); // the bot's own message
    assert_eq!(out[3].role, "user"); // Bob, newest
    assert_eq!(out[3].content.to_string(), "third");
}

#[test]
fn chat_messages_skip_blank_entries() {
    let msgs = vec![msg("Alice", "   ", false), msg("Bob", "hi", false)];
    let out = build_chat_messages("SYS", &msgs);
    assert_eq!(out.len(), 2); // system + Bob only
    assert_eq!(out[1].content.to_string(), "hi");
}
