use healthy_bot::{truncate_str, user_error, UserError};

#[test]
fn user_error_is_downcastable_and_keeps_message() {
    // This is the exact check the on_error handler uses to decide whether a
    // message is safe to show the user.
    let e = user_error("You are not authorized.");
    assert!(e.downcast_ref::<UserError>().is_some());
    assert_eq!(e.to_string(), "You are not authorized.");
}

#[test]
fn plain_boxed_error_is_not_a_user_error() {
    // Internal errors (e.g. a sqlx error propagated via `?`) box to a plain
    // error and must NOT be recognized as user-facing.
    let e: healthy_bot::Error = "no such table: users".into();
    assert!(e.downcast_ref::<UserError>().is_none());
}

#[test]
fn leaves_short_strings_unchanged() {
    assert_eq!(truncate_str("hello", 10), "hello");
}

#[test]
fn leaves_exact_length_unchanged() {
    assert_eq!(truncate_str("hello", 5), "hello");
}

#[test]
fn truncates_and_appends_ellipsis() {
    let out = truncate_str("hello world", 5);
    assert_eq!(out, "hell…");
    // Result (including the ellipsis) must fit within the limit.
    assert_eq!(out.chars().count(), 5);
}

#[test]
fn counts_unicode_by_char_not_byte() {
    // 6 multi-byte chars, limit 6 -> unchanged (byte length would wrongly trip).
    let s = "ñññññ"; // 5 chars, 10 bytes
    assert_eq!(truncate_str(s, 5), s);

    // Limit below length -> truncates by chars, never splits a codepoint.
    let out = truncate_str("ñññññ", 3);
    assert_eq!(out.chars().count(), 3);
    assert!(out.ends_with('…'));
}
