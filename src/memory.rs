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

/// Given message IDs (ordered newest-first) and an optional cursor of the last
/// scanned message ID, determine whether chat has moved and return the new
/// message IDs along with the updated cursor ID.
///
/// If `message_ids` is empty, returns `(vec![], last_scanned_id)`.
/// If the newest message ID is `<= last_scanned_id`, chat hasn't moved: returns `(vec![], last_scanned_id)`.
pub fn filter_unscanned_message_ids(
    message_ids: &[u64],
    last_scanned_id: Option<u64>,
) -> (Vec<u64>, Option<u64>) {
    let Some(&newest_id) = message_ids.first() else {
        return (Vec::new(), last_scanned_id);
    };

    if let Some(last_id) = last_scanned_id {
        if newest_id <= last_id {
            return (Vec::new(), Some(last_id));
        }
        let new_ids: Vec<u64> = message_ids
            .iter()
            .copied()
            .filter(|&id| id > last_id)
            .collect();
        (new_ids, Some(newest_id))
    } else {
        (message_ids.to_vec(), Some(newest_id))
    }
}

/// Parse fact indices from an input string like "1", "1, 2, 3", "1 2 3", "2-4".
/// Deduplicates numbers and keeps them in sorted order.
/// Validates that all indices are >= 1 and <= `max_facts`.
/// Returns Ok(Vec<usize>) (1-based indices), or Err(user-facing error message).
pub fn parse_fact_indices(input: &str, max_facts: usize) -> Result<Vec<usize>, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(
            "Please specify which memory number(s) to forget (e.g. `forget 1` or `forget 1, 2`)."
                .to_string(),
        );
    }

    let normalized = trimmed.replace(',', " ");
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    if tokens.is_empty() {
        return Err(
            "Please specify which memory number(s) to forget (e.g. `forget 1` or `forget 1, 2`)."
                .to_string(),
        );
    }

    let mut indices: Vec<usize> = Vec::new();

    for token in tokens {
        if let Some((start_str, end_str)) = token.split_once('-') {
            if !start_str.is_empty() && !end_str.is_empty() {
                let start = start_str
                    .parse::<usize>()
                    .map_err(|_| format!("Invalid number '{}' in range.", start_str))?;
                let end = end_str
                    .parse::<usize>()
                    .map_err(|_| format!("Invalid number '{}' in range.", end_str))?;
                if start == 0 || end == 0 {
                    return Err("Numbers start at 1. Use `view` to see them.".to_string());
                }
                if start > end {
                    return Err(format!("Invalid range '{}-{}'.", start, end));
                }
                for i in start..=end {
                    indices.push(i);
                }
                continue;
            }
        }

        let idx = token.parse::<usize>().map_err(|_| {
            format!(
                "Invalid number '{}'. Use numbers from `view` (e.g. `forget 1, 2`) or `forget all`.",
                token
            )
        })?;
        if idx == 0 {
            return Err("Numbers start at 1. Use `view` to see them.".to_string());
        }
        indices.push(idx);
    }

    indices.sort_unstable();
    indices.dedup();

    for &idx in &indices {
        if idx > max_facts {
            if max_facts == 0 {
                return Err("No memories are currently stored.".to_string());
            } else if max_facts == 1 {
                return Err("Only 1 memory item is stored.".to_string());
            } else {
                return Err(format!(
                    "Number {} is out of range. There are only {} memory item(s).",
                    idx, max_facts
                ));
            }
        }
    }

    Ok(indices)
}
