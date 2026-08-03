//! Logic for spontaneous conversational interjections ("random chimes").
//!
//! The bot occasionally joins an ongoing conversation on its own, without being
//! mentioned or replied to. This module holds the pure, testable decision and
//! prompt-building logic; the actual Discord/OpenAI I/O lives in the event
//! handler.

use crate::openai::{sanitize_name, ChatMessage, ChatMessageRequestContent};

/// A recent channel message reduced to what the interjection prompt needs.
#[derive(Debug, Clone, PartialEq)]
pub struct ChimeEntry {
    pub author_name: String,
    pub content: String,
    /// Whether this message was sent by the bot itself.
    pub is_bot: bool,
}

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

/// Build the OpenAI message list for a spontaneous interjection.
///
/// `recent` is the channel's recent messages as Discord returns them
/// (newest-first); they are reversed into chronological order. The bot's own
/// messages map to the `assistant` role, everyone else to `user`. Blank entries
/// are skipped. `system_prompt` is placed first as the `system` message.
pub fn build_chime_messages(system_prompt: &str, recent: &[ChimeEntry]) -> Vec<ChatMessage> {
    let mut messages = Vec::with_capacity(recent.len() + 1);
    messages.push(ChatMessage {
        role: "system".to_string(),
        name: None,
        content: ChatMessageRequestContent::Text(system_prompt.to_string()),
    });

    for entry in recent.iter().rev() {
        let trimmed = entry.content.trim();
        if trimmed.is_empty() {
            continue;
        }
        let role = if entry.is_bot { "assistant" } else { "user" };
        messages.push(ChatMessage {
            role: role.to_string(),
            name: Some(sanitize_name(&entry.author_name)),
            content: ChatMessageRequestContent::Text(trimmed.to_string()),
        });
    }

    messages
}
