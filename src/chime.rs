//! Logic for spontaneous conversational interjections ("random chimes").
//!
//! The bot occasionally joins an ongoing conversation on its own, without being
//! mentioned or replied to. This module holds the pure, testable decision logic;
//! the recent-message prompt is built via [`crate::recent`], and the actual
//! Discord/OpenAI I/O lives in the event handler.

/// Whether a message is eligible for a spontaneous chime, *before* the
/// probability roll and cooldown are applied.
///
/// Chimes only happen in the main channel, on non-empty human messages that
/// aren't commands, and never when the bot was already addressed directly
/// (mention or reply) — that case is handled by the normal AI reply path.
pub fn is_chime_eligible(
    content: &str,
    prefix: &str,
    in_main_channel: bool,
    mentioned: bool,
    replied: bool,
) -> bool {
    let trimmed = content.trim();
    in_main_channel && !mentioned && !replied && !trimmed.is_empty() && !trimmed.starts_with(prefix)
}

/// Whether a chime should fire, given a percent `chance_percent` in `[0, 100]`
/// and a random `roll` in `[0.0, 1.0)`. A chance of 0 (or less) never fires; a
/// chance of 100 (or more) always fires for any valid roll.
pub fn should_fire(chance_percent: f64, roll: f64) -> bool {
    roll < chance_percent / 100.0
}
