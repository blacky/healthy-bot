use healthy_bot::chime::{build_chime_messages, is_chime_eligible, should_fire, ChimeEntry};

fn entry(name: &str, content: &str, is_bot: bool) -> ChimeEntry {
    ChimeEntry {
        author_name: name.to_string(),
        content: content.to_string(),
        is_bot,
    }
}

#[test]
fn eligible_for_plain_human_message_in_main_channel() {
    assert!(is_chime_eligible("hey what's up", "!", true, false, false));
}

#[test]
fn not_eligible_outside_main_channel() {
    assert!(!is_chime_eligible(
        "hey what's up",
        "!",
        false,
        false,
        false
    ));
}

#[test]
fn not_eligible_when_addressed_directly() {
    // Mention or reply is handled by the normal AI path, not a random chime.
    assert!(!is_chime_eligible("hey", "!", true, true, false));
    assert!(!is_chime_eligible("hey", "!", true, false, true));
}

#[test]
fn not_eligible_for_commands() {
    assert!(!is_chime_eligible("!markov", "!", true, false, false));
    // Honors a custom prefix.
    assert!(!is_chime_eligible("?help", "?", true, false, false));
}

#[test]
fn not_eligible_for_empty_or_whitespace() {
    assert!(!is_chime_eligible("", "!", true, false, false));
    assert!(!is_chime_eligible("   ", "!", true, false, false));
}

#[test]
fn should_fire_respects_bounds() {
    assert!(!should_fire(0.0, 0.0)); // 0% never fires
    assert!(should_fire(100.0, 0.999)); // 100% always fires for a valid roll
    assert!(should_fire(50.0, 0.49));
    assert!(!should_fire(50.0, 0.50)); // roll == threshold does not fire
    assert!(!should_fire(50.0, 0.51));
}

#[test]
fn build_messages_orders_chronologically_and_maps_roles() {
    // Discord returns newest-first; the prompt must read oldest-first.
    let recent = vec![
        entry("Bob", "third", false),
        entry("HealthyBot", "second", true),
        entry("Alice", "first", false),
    ];
    let msgs = build_chime_messages("SYS", &recent);
    assert_eq!(msgs.len(), 4);
    assert_eq!(msgs[0].role, "system");
    assert_eq!(msgs[0].content.to_string(), "SYS");
    assert_eq!(msgs[1].role, "user"); // Alice, oldest
    assert_eq!(msgs[1].content.to_string(), "first");
    assert_eq!(msgs[2].role, "assistant"); // the bot's own message
    assert_eq!(msgs[3].role, "user"); // Bob, newest
    assert_eq!(msgs[3].content.to_string(), "third");
}

#[test]
fn build_messages_skips_blank_entries() {
    let recent = vec![entry("Alice", "   ", false), entry("Bob", "hi", false)];
    let msgs = build_chime_messages("SYS", &recent);
    assert_eq!(msgs.len(), 2); // system + Bob only
    assert_eq!(msgs[1].content.to_string(), "hi");
}
