//! Logic for spontaneous conversational interjections ("random chimes").
//!
//! The bot occasionally joins an ongoing conversation on its own, without being
//! mentioned or replied to. This module holds the pure, testable decision logic;
//! the recent-message prompt is built via [`crate::recent`], and the actual
//! Discord/OpenAI I/O lives in the event handler.

use chrono::NaiveDate;
use std::time::Duration;

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

/// Parsed settings that gate a spontaneous interjection.
#[derive(Debug, Clone, PartialEq)]
pub struct ChimeSettings {
    /// Percent chance per eligible message, in `[0, 100]`. 0 disables chiming.
    pub chance: f64,
    /// Minimum seconds between actual chimes.
    pub cooldown_secs: u64,
    /// Maximum chimes per day; 0 means unlimited.
    pub daily_cap: u32,
}

/// Per-message inputs to the chime decision.
pub struct ChimeInputs<'a> {
    pub content: &'a str,
    pub prefix: &'a str,
    pub in_main_channel: bool,
    pub mentioned: bool,
    pub replied: bool,
    pub elapsed_since_last: Duration,
    pub chimes_today: u32,
    pub roll: f64,
}

/// Why a potential chime was skipped (surfaced for logging and tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    Disabled,
    Ineligible,
    DailyCapReached,
    OnCooldown,
    RollMissed,
}

/// The outcome of the chime gate chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChimeDecision {
    Skip(SkipReason),
    Fire,
}

/// Pure decision for whether to spontaneously chime, evaluated cheapest-check
/// first. All I/O (settings reads, cooldown state, the roll) is resolved by the
/// caller and passed in, so the whole gate chain is unit-testable.
pub fn evaluate(settings: &ChimeSettings, inputs: &ChimeInputs) -> ChimeDecision {
    if settings.chance <= 0.0 {
        return ChimeDecision::Skip(SkipReason::Disabled);
    }
    if !is_chime_eligible(
        inputs.content,
        inputs.prefix,
        inputs.in_main_channel,
        inputs.mentioned,
        inputs.replied,
    ) {
        return ChimeDecision::Skip(SkipReason::Ineligible);
    }
    if settings.daily_cap > 0 && inputs.chimes_today >= settings.daily_cap {
        return ChimeDecision::Skip(SkipReason::DailyCapReached);
    }
    if inputs.elapsed_since_last.as_secs() < settings.cooldown_secs {
        return ChimeDecision::Skip(SkipReason::OnCooldown);
    }
    if !should_fire(settings.chance, inputs.roll) {
        return ChimeDecision::Skip(SkipReason::RollMissed);
    }
    ChimeDecision::Fire
}

/// Tracks how many chimes have happened on the current day, resetting the count
/// when the day changes. Enforces `random_chime_daily_cap`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChimeTally {
    day: NaiveDate,
    count: u32,
}

impl ChimeTally {
    pub fn new(day: NaiveDate) -> Self {
        Self { day, count: 0 }
    }

    /// Chimes recorded for `today`, resetting to 0 first if the stored day is
    /// stale.
    pub fn count_for(&mut self, today: NaiveDate) -> u32 {
        if self.day != today {
            self.day = today;
            self.count = 0;
        }
        self.count
    }

    /// Record one chime on `today` (rolling the day over first if needed).
    pub fn record(&mut self, today: NaiveDate) {
        self.count_for(today);
        self.count += 1;
    }
}

/// Whether an LLM yes/no answer is affirmative, used by the relevance pre-check.
/// Anything starting with "yes" (case-insensitive, ignoring leading punctuation
/// or whitespace) is treated as yes; everything else is no. Callers fail closed,
/// so a malformed or empty answer keeps the bot quiet.
pub fn is_affirmative(response: &str) -> bool {
    response
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
        .starts_with("yes")
}
