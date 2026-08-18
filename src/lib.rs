pub mod commands;
pub mod db;
pub mod markov;
pub mod openai;
pub mod tasks;

use crate::markov::MarkovRepository;
use crate::openai::OpenAIClient;
use serenity::all::ChannelId;
use sqlx::sqlite::SqlitePool;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug)]
pub struct Data {
    pub db_pool: SqlitePool,
    pub markov_repo: MarkovRepository,
    pub openai_client: OpenAIClient,
    pub last_markov: tokio::sync::Mutex<std::time::Instant>,
    pub last_openai: tokio::sync::Mutex<std::time::Instant>,
    pub settings_cache: Arc<RwLock<HashMap<String, String>>>,
    /// Threads the bot is actively chatting in: once a thread's conversation is
    /// kicked off (via @mention or a reply), every subsequent message in that
    /// thread is treated as a continuation, without needing another mention.
    pub active_ai_threads: RwLock<HashSet<ChannelId>>,
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// An error whose message is intended to be shown to the end user verbatim
/// (e.g. "You are not authorized"). The error handler shows a generic message
/// for any error that is *not* a `UserError`, so internal failures (database
/// errors, parse errors, …) never leak their raw text into chat.
#[derive(Debug)]
pub struct UserError(pub String);

impl std::fmt::Display for UserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UserError {}

/// Build a user-facing [`UserError`] as a boxed [`Error`].
pub fn user_error(msg: impl Into<String>) -> Error {
    Box::new(UserError(msg.into()))
}

/// Truncate `s` to at most `max` characters (Unicode scalar values, which is how
/// Discord counts), appending '…' if it was shortened. Discord rejects an entire
/// embed/message when any field exceeds its limit (field name 256, field value
/// 1024, channel name 100, …), so user-controlled strings must pass through here
/// before being placed into one. Borrows when no truncation is needed.
pub fn truncate_str(s: &str, max: usize) -> std::borrow::Cow<'_, str> {
    if s.chars().count() <= max {
        std::borrow::Cow::Borrowed(s)
    } else {
        // Reserve one character for the ellipsis so the result fits within `max`.
        let kept: String = s.chars().take(max.saturating_sub(1)).collect();
        std::borrow::Cow::Owned(format!("{}…", kept))
    }
}
