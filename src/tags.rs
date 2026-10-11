use poise::serenity_prelude as serenity;
use rand::Rng;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct TagContext<'a> {
    pub author_mention: String,
    pub author_name: String,
    pub args: &'a [String],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluatedTag {
    pub text: String,
    pub asset_filename: Option<String>,
}

/// Evaluates a tag's template content safely, resolving variables, ranges, and conditionals.
pub fn evaluate_tag(
    raw_content: &str,
    ctx: &TagContext<'_>,
    asset_filename: Option<&str>,
) -> EvaluatedTag {
    let mut text = raw_content.to_string();
    let mut vars: HashMap<String, String> = HashMap::new();

    // Iterate up to 50 passes to prevent infinite loops from any malformed input
    for _ in 0..50 {
        let Some(end) = text.find('}') else {
            break;
        };
        let Some(start) = text[..end].rfind('{') else {
            text.remove(end);
            continue;
        };

        let inner = &text[start + 1..end];
        let replacement = evaluate_inner_block(inner, ctx, &mut vars);

        text.replace_range(start..=end, &replacement);
    }

    EvaluatedTag {
        text: text.trim().to_string(),
        asset_filename: asset_filename.map(|s| s.to_string()),
    }
}

fn evaluate_inner_block(
    inner: &str,
    ctx: &TagContext<'_>,
    vars: &mut HashMap<String, String>,
) -> String {
    if inner == "user" {
        ctx.author_mention.clone()
    } else if inner == "nick" {
        ctx.author_name.clone()
    } else if inner == "args" {
        ctx.args.join(" ")
    } else if inner == "randonline" {
        ctx.author_mention.clone()
    } else if let Some(rest) = inner.strip_prefix("arg:") {
        if let Ok(idx) = rest.parse::<usize>() {
            ctx.args.get(idx).cloned().unwrap_or_default()
        } else {
            String::new()
        }
    } else if let Some(rest) = inner.strip_prefix("range:") {
        let mut parts = rest.split('|');
        if let (Some(min_s), Some(max_s)) = (parts.next(), parts.next()) {
            if let (Ok(min), Ok(max)) = (min_s.parse::<i64>(), max_s.parse::<i64>()) {
                if min <= max {
                    let mut rng = rand::thread_rng();
                    let val = rng.gen_range(min..=max);
                    return val.to_string();
                }
            }
        }
        String::new()
    } else if let Some(rest) = inner.strip_prefix("set:") {
        if let Some((k, v)) = rest.split_once('|') {
            vars.insert(k.to_string(), v.to_string());
        }
        String::new()
    } else if let Some(rest) = inner.strip_prefix("get:") {
        vars.get(rest).cloned().unwrap_or_default()
    } else if let Some(rest) = inner.strip_prefix("if:") {
        evaluate_if_block(rest)
    } else if inner.starts_with("attach:") {
        // {attach:...} tag wrapper is cleaned; assets are handled via asset_filename
        String::new()
    } else {
        // Unknown or unsupported macro block: strip it safely
        String::new()
    }
}

fn evaluate_if_block(body: &str) -> String {
    // Body format: LHS|=|RHS|then:THEN[|else:ELSE]
    let parts: Vec<&str> = body.split('|').collect();
    if parts.len() < 3 {
        return String::new();
    }

    let lhs = parts[0];
    let op = parts[1];
    let rhs = parts[2];

    let mut then_val = "";
    let mut else_val = "";

    for part in &parts[3..] {
        if let Some(val) = part.strip_prefix("then:") {
            then_val = val;
        } else if let Some(val) = part.strip_prefix("else:") {
            else_val = val;
        }
    }

    let is_true = match op {
        "=" => lhs == rhs,
        "!=" => lhs != rhs,
        _ => lhs == rhs,
    };

    if is_true {
        then_val.to_string()
    } else {
        else_val.to_string()
    }
}

/// State tracking for channel tag cooldowns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelCooldownState {
    pub last_success: std::time::Instant,
    pub warned: bool,
}

/// The decision outcome for a tag command invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagCooldownDecision {
    Permitted,
    RejectAndWarn { remaining_secs: u64 },
    RejectSilently,
}

/// Check if a tag command invocation is permitted by the channel cooldown.
///
/// Returns:
/// - `TagCooldownDecision::Permitted` if allowed (and updates the channel timestamp if not an edit).
/// - `TagCooldownDecision::RejectAndWarn` the first time an invocation is blocked during this cooldown window.
/// - `TagCooldownDecision::RejectSilently` if an invocation is blocked and the warning was already sent in this cooldown window.
pub fn check_and_update_tag_cooldown(
    channels: &mut HashMap<serenity::ChannelId, ChannelCooldownState>,
    channel_id: serenity::ChannelId,
    cooldown_secs: u64,
    is_edit: bool,
    now: std::time::Instant,
) -> TagCooldownDecision {
    if cooldown_secs == 0 || is_edit {
        return TagCooldownDecision::Permitted;
    }

    // Prune entries older than 3600 seconds to prevent unbounded memory growth
    channels.retain(|_, state| now.saturating_duration_since(state.last_success).as_secs() < 3600);

    if let Some(state) = channels.get_mut(&channel_id) {
        let elapsed = now.saturating_duration_since(state.last_success).as_secs();
        if elapsed < cooldown_secs {
            let remaining_secs = cooldown_secs.saturating_sub(elapsed).max(1);
            if !state.warned {
                state.warned = true;
                return TagCooldownDecision::RejectAndWarn { remaining_secs };
            } else {
                return TagCooldownDecision::RejectSilently;
            }
        }
    }

    channels.insert(
        channel_id,
        ChannelCooldownState {
            last_success: now,
            warned: false,
        },
    );
    TagCooldownDecision::Permitted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plain_text() {
        let ctx = TagContext {
            author_mention: "<@123>".into(),
            author_name: "Alice".into(),
            args: &[],
        };
        let eval = evaluate_tag("Hello world!", &ctx, None);
        assert_eq!(eval.text, "Hello world!");
        assert_eq!(eval.asset_filename, None);
    }

    #[test]
    fn test_user_and_nick() {
        let ctx = TagContext {
            author_mention: "<@123>".into(),
            author_name: "Alice".into(),
            args: &[],
        };
        let eval = evaluate_tag("Hello {user}, your name is {nick}!", &ctx, None);
        assert_eq!(eval.text, "Hello <@123>, your name is Alice!");
    }

    #[test]
    fn test_args_and_arg_index() {
        let args = vec!["apple".into(), "banana".into()];
        let ctx = TagContext {
            author_mention: "<@123>".into(),
            author_name: "Alice".into(),
            args: &args,
        };
        let eval = evaluate_tag(
            "First: {arg:0}, Second: {arg:1}, Third: '{arg:2}', All: {args}",
            &ctx,
            None,
        );
        assert_eq!(
            eval.text,
            "First: apple, Second: banana, Third: '', All: apple banana"
        );
    }

    #[test]
    fn test_range() {
        let ctx = TagContext {
            author_mention: "<@123>".into(),
            author_name: "Alice".into(),
            args: &[],
        };
        let eval = evaluate_tag("Value: {range:5|10}", &ctx, None);
        assert!(eval.text.starts_with("Value: "));
        let num: i64 = eval.text.strip_prefix("Value: ").unwrap().parse().unwrap();
        assert!((5..=10).contains(&num));
    }

    #[test]
    fn test_meds_conditional() {
        let template = "{if:{arg:0}|=||then:blm take your meds|else:{arg:0} take your meds}";
        let ctx_empty = TagContext {
            author_mention: "<@123>".into(),
            author_name: "Alice".into(),
            args: &[],
        };
        let eval_empty = evaluate_tag(template, &ctx_empty, None);
        assert_eq!(eval_empty.text, "blm take your meds");

        let args_bob = vec!["Bob".into()];
        let ctx_bob = TagContext {
            author_mention: "<@123>".into(),
            author_name: "Alice".into(),
            args: &args_bob,
        };
        let eval_bob = evaluate_tag(template, &ctx_bob, None);
        assert_eq!(eval_bob.text, "Bob take your meds");
    }

    #[test]
    fn test_set_get_and_eightball() {
        let template =
            "{set:a|2}{if:{get:a}|=|0|then:No}{if:{get:a}|=|1|then:Maybe}{if:{get:a}|=|2|then:Yes}";
        let ctx = TagContext {
            author_mention: "<@123>".into(),
            author_name: "Alice".into(),
            args: &[],
        };
        let eval = evaluate_tag(template, &ctx, None);
        assert_eq!(eval.text, "Yes");
    }

    #[test]
    fn test_attach_stripped_when_asset_filename_present() {
        let template = "{attach:https://example.com/pic.png}";
        let ctx = TagContext {
            author_mention: "<@123>".into(),
            author_name: "Alice".into(),
            args: &[],
        };
        let eval = evaluate_tag(template, &ctx, Some("12345.png"));
        assert_eq!(eval.text, "");
        assert_eq!(eval.asset_filename, Some("12345.png".to_string()));
    }

    #[test]
    fn test_tag_cooldown_behavior() {
        let mut channels = HashMap::new();
        let ch1 = serenity::ChannelId::new(100);
        let ch2 = serenity::ChannelId::new(200);
        let t0 = std::time::Instant::now();

        // When cooldown_secs is 0, always permitted and doesn't record
        assert_eq!(
            check_and_update_tag_cooldown(&mut channels, ch1, 0, false, t0),
            TagCooldownDecision::Permitted
        );
        assert!(channels.is_empty());

        // First call with 5s cooldown succeeds
        assert_eq!(
            check_and_update_tag_cooldown(&mut channels, ch1, 5, false, t0),
            TagCooldownDecision::Permitted
        );
        assert!(channels.contains_key(&ch1));

        // First attempt during cooldown at t0 + 2s triggers RejectAndWarn (remaining: 3s)
        let t2 = t0 + std::time::Duration::from_secs(2);
        assert_eq!(
            check_and_update_tag_cooldown(&mut channels, ch1, 5, false, t2),
            TagCooldownDecision::RejectAndWarn { remaining_secs: 3 }
        );

        // Immediate subsequent attempt during the same cooldown at t0 + 3s triggers RejectSilently
        let t3 = t0 + std::time::Duration::from_secs(3);
        assert_eq!(
            check_and_update_tag_cooldown(&mut channels, ch1, 5, false, t3),
            TagCooldownDecision::RejectSilently
        );

        // Third attempt during same cooldown at t0 + 4s still triggers RejectSilently
        let t4 = t0 + std::time::Duration::from_secs(4);
        assert_eq!(
            check_and_update_tag_cooldown(&mut channels, ch1, 5, false, t4),
            TagCooldownDecision::RejectSilently
        );

        // Call at t0 + 3s on a different channel succeeds
        assert_eq!(
            check_and_update_tag_cooldown(&mut channels, ch2, 5, false, t3),
            TagCooldownDecision::Permitted
        );

        // Message edit at t0 + 3s on ch1 succeeds without blocking
        assert_eq!(
            check_and_update_tag_cooldown(&mut channels, ch1, 5, true, t3),
            TagCooldownDecision::Permitted
        );

        // Call at t0 + 6s on ch1 succeeds (5s elapsed)
        let t6 = t0 + std::time::Duration::from_secs(6);
        assert_eq!(
            check_and_update_tag_cooldown(&mut channels, ch1, 5, false, t6),
            TagCooldownDecision::Permitted
        );

        // Subsequent call in new cooldown window at t6 + 1s warns again
        let t7 = t6 + std::time::Duration::from_secs(1);
        assert_eq!(
            check_and_update_tag_cooldown(&mut channels, ch1, 5, false, t7),
            TagCooldownDecision::RejectAndWarn { remaining_secs: 4 }
        );

        // Another immediate try in this new window is silent again
        let t8 = t6 + std::time::Duration::from_secs(2);
        assert_eq!(
            check_and_update_tag_cooldown(&mut channels, ch1, 5, false, t8),
            TagCooldownDecision::RejectSilently
        );

        // Stale entries (>3600s) are pruned
        let t4000 = t0 + std::time::Duration::from_secs(4000);
        let ch3 = serenity::ChannelId::new(300);
        assert_eq!(
            check_and_update_tag_cooldown(&mut channels, ch3, 5, false, t4000),
            TagCooldownDecision::Permitted
        );
        assert!(!channels.contains_key(&ch1)); // ch1 was at t6, >3600s ago relative to t4000
        assert!(channels.contains_key(&ch3));
    }
}
