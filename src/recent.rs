//! Shared helpers for turning a channel's recent messages into prompt inputs.
//!
//! Discord returns messages newest-first. Both the `tldr` summary and the random
//! chime feed recent history to OpenAI, differing only in shape: `tldr` wants a
//! plain-text transcript (and ignores the bot's own messages), while a chime
//! wants a role-tagged [`ChatMessage`] list (keeping the bot's messages as
//! `assistant`). This module holds the one [`RecentMessage`] type and the shared
//! chronological / non-blank pass that both builders sit on top of.

use crate::openai::{sanitize_name, ChatMessage, ChatMessageRequestContent};

/// A channel message reduced to what the prompt builders need. Decoupled from
/// serenity's `Message` so the builders can be unit-tested without Discord types.
#[derive(Debug, Clone, PartialEq)]
pub struct RecentMessage {
    pub author_name: String,
    pub content: String,
    /// Whether the message was sent by the bot itself.
    pub is_bot: bool,
}

/// Iterate `messages` (as Discord returns them, newest-first) in chronological
/// order, skipping entries that are blank after trimming.
fn chronological(messages: &[RecentMessage]) -> impl Iterator<Item = &RecentMessage> {
    messages
        .iter()
        .rev()
        .filter(|m| !m.content.trim().is_empty())
}

/// Build a plain chronological transcript (`Name: message` per line) for
/// summarization. The bot's own messages are dropped. Returns `None` when
/// nothing summarizable remains.
pub fn build_transcript(messages: &[RecentMessage]) -> Option<String> {
    let lines: Vec<String> = chronological(messages)
        .filter(|m| !m.is_bot)
        .map(|m| format!("{}: {}", m.author_name, m.content.trim()))
        .collect();

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

/// Build the OpenAI message list for an interjection. `system_prompt` is placed
/// first; the bot's own messages map to the `assistant` role, everyone else to
/// `user`.
pub fn build_chat_messages(system_prompt: &str, messages: &[RecentMessage]) -> Vec<ChatMessage> {
    let mut out = Vec::with_capacity(messages.len() + 1);
    out.push(ChatMessage {
        role: "system".to_string(),
        name: None,
        content: ChatMessageRequestContent::Text(system_prompt.to_string()),
    });

    for m in chronological(messages) {
        let role = if m.is_bot { "assistant" } else { "user" };
        out.push(ChatMessage {
            role: role.to_string(),
            name: Some(sanitize_name(&m.author_name)),
            content: ChatMessageRequestContent::Text(m.content.trim().to_string()),
        });
    }

    out
}
