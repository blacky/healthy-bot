use chrono::NaiveDate;
use healthy_bot::chime::{
    evaluate, is_affirmative, is_chime_eligible, should_fire, ChimeDecision, ChimeInputs,
    ChimeSettings, ChimeTally, SkipReason,
};
use std::time::Duration;

fn settings(chance: f64, cooldown_secs: u64, daily_cap: u32) -> ChimeSettings {
    ChimeSettings {
        chance,
        cooldown_secs,
        daily_cap,
    }
}

/// Inputs that pass every gate, so individual tests can perturb one field.
fn passing_inputs() -> ChimeInputs<'static> {
    ChimeInputs {
        content: "hey what's up",
        prefix: "!",
        in_main_channel: true,
        mentioned: false,
        replied: false,
        elapsed_since_last: Duration::from_secs(10_000),
        chimes_today: 0,
        roll: 0.0, // always under any positive chance
    }
}

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 8, 3).unwrap()
}

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

#[test]
fn evaluate_fires_when_all_gates_pass() {
    assert_eq!(
        evaluate(&settings(5.0, 600, 0), &passing_inputs()),
        ChimeDecision::Fire
    );
}

#[test]
fn evaluate_skips_when_disabled() {
    assert_eq!(
        evaluate(&settings(0.0, 600, 0), &passing_inputs()),
        ChimeDecision::Skip(SkipReason::Disabled)
    );
}

#[test]
fn evaluate_skips_when_ineligible() {
    let mut inputs = passing_inputs();
    inputs.in_main_channel = false;
    assert_eq!(
        evaluate(&settings(5.0, 600, 0), &inputs),
        ChimeDecision::Skip(SkipReason::Ineligible)
    );
}

#[test]
fn evaluate_skips_when_daily_cap_reached() {
    let mut inputs = passing_inputs();
    inputs.chimes_today = 3;
    assert_eq!(
        evaluate(&settings(5.0, 600, 3), &inputs),
        ChimeDecision::Skip(SkipReason::DailyCapReached)
    );
    // A cap of 0 means unlimited, so the same count fires.
    assert_eq!(
        evaluate(&settings(5.0, 600, 0), &inputs),
        ChimeDecision::Fire
    );
}

#[test]
fn evaluate_skips_when_on_cooldown() {
    let mut inputs = passing_inputs();
    inputs.elapsed_since_last = Duration::from_secs(30);
    assert_eq!(
        evaluate(&settings(5.0, 600, 0), &inputs),
        ChimeDecision::Skip(SkipReason::OnCooldown)
    );
}

#[test]
fn evaluate_skips_when_roll_misses() {
    let mut inputs = passing_inputs();
    inputs.roll = 0.99; // above a 5% threshold
    assert_eq!(
        evaluate(&settings(5.0, 600, 0), &inputs),
        ChimeDecision::Skip(SkipReason::RollMissed)
    );
}

#[test]
fn tally_counts_and_rolls_over_by_day() {
    let mut tally = ChimeTally::new(day());
    assert_eq!(tally.count_for(day()), 0);
    tally.record(day());
    tally.record(day());
    assert_eq!(tally.count_for(day()), 2);

    // A new day resets the count.
    let next = day().succ_opt().unwrap();
    assert_eq!(tally.count_for(next), 0);
    tally.record(next);
    assert_eq!(tally.count_for(next), 1);
}

#[test]
fn is_affirmative_recognizes_yes() {
    assert!(is_affirmative("YES"));
    assert!(is_affirmative("yes, definitely"));
    assert!(is_affirmative("  Yes."));
    assert!(is_affirmative("\"YES\""));
    assert!(!is_affirmative("NO"));
    assert!(!is_affirmative("no thanks"));
    assert!(!is_affirmative("")); // empty → not affirmative (fail closed)
    assert!(!is_affirmative("maybe"));
}
