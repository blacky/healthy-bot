//! Per-user memory: pure helpers for extracting, formatting, and prompting on
//! durable facts about users. The Discord/OpenAI I/O and persistence live in the
//! background task, event handler, and `db` module.

use crate::openai::{ChatMessage, ChatMessageRequestContent};
use serde::Deserialize;

/// A single fact the extractor produced, keyed by the participant's Discord ID.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ExtractedFact {
    pub user_id: String,
    pub fact: String,
}

/// Parse the extractor's response into facts. Tolerant of models that wrap the
/// JSON in prose or ``` fences: falls back to the substring between the first
/// `[` and the last `]`. Entries with an empty id or fact are dropped. Returns
/// an empty vec on any parse failure (the caller simply stores nothing).
pub fn parse_extracted_facts(content: &str) -> Vec<ExtractedFact> {
    let trimmed = content.trim();

    let parsed = serde_json::from_str::<Vec<ExtractedFact>>(trimmed).or_else(|_| {
        // Salvage a JSON array embedded in surrounding text/fences.
        match (trimmed.find('['), trimmed.rfind(']')) {
            (Some(start), Some(end)) if end > start => {
                serde_json::from_str::<Vec<ExtractedFact>>(&trimmed[start..=end])
            }
            _ => Ok(Vec::new()),
        }
    });

    parsed
        .unwrap_or_default()
        .into_iter()
        .filter_map(|mut f| {
            f.user_id = f.user_id.trim().to_string();
            f.fact = f.fact.trim().to_string();
            if f.user_id.is_empty() || f.fact.is_empty() {
                None
            } else {
                Some(f)
            }
        })
        .collect()
}

/// Build the memory block to prepend to a system prompt, given each participant's
/// display name and their facts. Returns `None` when there is nothing to inject.
pub fn format_memory_context(people: &[(String, Vec<String>)]) -> Option<String> {
    let lines: Vec<String> = people
        .iter()
        .filter(|(_, facts)| !facts.is_empty())
        .map(|(name, facts)| format!("- {}: {}", name, facts.join("; ")))
        .collect();

    if lines.is_empty() {
        return None;
    }

    Some(format!(
        "Here is what you remember about people in this conversation. Use it \
         naturally where relevant; do not recite it verbatim or say that you \
         looked it up.\n{}",
        lines.join("\n")
    ))
}

/// Build the OpenAI messages for a fact-extraction pass. `participants` is a list
/// of `(discord_id, display_name)`; the model is told to key every fact by one of
/// those exact IDs, which is how name→ID mapping is resolved.
pub fn build_extraction_messages(
    participants: &[(String, String)],
    transcript: &str,
) -> Vec<ChatMessage> {
    let roster: String = participants
        .iter()
        .map(|(id, name)| format!("- {} ({})", id, name))
        .collect::<Vec<_>>()
        .join("\n");

    let system = format!(
        "You extract durable, useful facts about people from a chat transcript so \
         a bot can remember them later. Record only stable facts — preferences, \
         interests, ongoing situations, life events, relationships, running jokes \
         — not transient chatter, one-off reactions, or messages' literal wording. \
         Write each fact as a short third-person statement.\n\n\
         Participants (use these exact IDs):\n{}\n\n\
         Respond with ONLY a JSON array of objects like \
         {{\"user_id\": \"<one of the IDs above>\", \"fact\": \"<short fact>\"}}. \
         Use only the listed IDs. If there are no durable facts, respond with [].",
        roster
    );

    vec![
        ChatMessage {
            role: "system".to_string(),
            name: None,
            content: ChatMessageRequestContent::Text(system),
        },
        ChatMessage {
            role: "user".to_string(),
            name: None,
            content: ChatMessageRequestContent::Text(transcript.to_string()),
        },
    ]
}
