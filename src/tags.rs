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
}
