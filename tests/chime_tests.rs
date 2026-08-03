use healthy_bot::chime::{is_chime_eligible, should_fire};

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
