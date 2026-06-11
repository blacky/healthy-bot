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
