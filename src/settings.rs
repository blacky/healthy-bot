//! Bot configuration and setting definitions.
//!
//! Provides a single source of truth for all known setting keys, their
//! default values, redaction rules, validation, and autocomplete suggestions.

/// Metadata definition for a known setting key.
#[derive(Debug, Clone, Copy)]
pub struct SettingDef {
    pub key: &'static str,
    pub default_value: &'static str,
    pub is_secret: bool,
    pub recommendations: &'static [&'static str],
}

pub const KNOWN_SETTINGS: &[SettingDef] = &[
    SettingDef {
        key: "command_prefix",
        default_value: "!",
        is_secret: false,
        recommendations: &["!", "?", ".", "-"],
    },
    SettingDef {
        key: "main_text_channel",
        default_value: "(not set)",
        is_secret: false,
        recommendations: &[],
    },
    SettingDef {
        key: "reminder_category",
        default_value: "(not set)",
        is_secret: false,
        recommendations: &[],
    },
    SettingDef {
        key: "markov_cooldown_seconds",
        default_value: "60",
        is_secret: false,
        recommendations: &["0", "30", "60", "120"],
    },
    SettingDef {
        key: "ai_cooldown_seconds",
        default_value: "60",
        is_secret: false,
        recommendations: &["0", "30", "60", "120"],
    },
    SettingDef {
        key: "tldr_cooldown_seconds",
        default_value: "60",
        is_secret: false,
        recommendations: &["0", "30", "60", "120"],
    },
    SettingDef {
        key: "tag_cooldown_seconds",
        default_value: "5",
        is_secret: false,
        recommendations: &["0", "5", "10", "30"],
    },
    SettingDef {
        key: "ai_initial_prompt",
        default_value: crate::commands::HARDENED_SYSTEM_PROMPT,
        is_secret: true,
        recommendations: &[],
    },
    SettingDef {
        key: "ai_chat_model",
        default_value: "gpt-4o",
        is_secret: false,
        recommendations: &[
            "gpt-4o",
            "gpt-4o-mini",
            "gpt-5.1",
            "o1",
            "o1-mini",
            "o3-mini",
        ],
    },
    SettingDef {
        key: "bot_status_type",
        default_value: "playing",
        is_secret: false,
        recommendations: &["playing", "watching", "listening", "competing", "custom"],
    },
    SettingDef {
        key: "bot_status_message",
        default_value: "(not set)",
        is_secret: false,
        recommendations: &[],
    },
    SettingDef {
        key: "allowed_bot_id",
        default_value: "(not set)",
        is_secret: false,
        recommendations: &[],
    },
    SettingDef {
        key: "ai_debug",
        default_value: "false",
        is_secret: false,
        recommendations: &["true", "false"],
    },
    SettingDef {
        key: "reminder_pings",
        default_value: "true",
        is_secret: false,
        recommendations: &["true", "false"],
    },
    SettingDef {
        key: "random_chime_chance",
        default_value: "0",
        is_secret: false,
        recommendations: &["0", "5", "10", "25"],
    },
    SettingDef {
        key: "random_chime_cooldown_seconds",
        default_value: "1800",
        is_secret: false,
        recommendations: &["600", "1800", "3600"],
    },
    SettingDef {
        key: "random_chime_daily_cap",
        default_value: "3",
        is_secret: false,
        recommendations: &["0", "3", "5", "10"],
    },
    SettingDef {
        key: "random_chime_prompt",
        default_value: "(fallback to ai_initial_prompt)",
        is_secret: true,
        recommendations: &[],
    },
    SettingDef {
        key: "random_chime_model",
        default_value: "(fallback to ai_chat_model)",
        is_secret: false,
        recommendations: &["gpt-3.5-turbo", "gpt-4o-mini", "gpt-4o"],
    },
    SettingDef {
        key: "random_chime_relevance_check",
        default_value: "true",
        is_secret: false,
        recommendations: &["true", "false"],
    },
    SettingDef {
        key: "ai_max_tokens",
        default_value: "300",
        is_secret: false,
        recommendations: &["150", "300", "500", "1000", "2000"],
    },
    SettingDef {
        key: "memory_enabled",
        default_value: "true",
        is_secret: false,
        recommendations: &["true", "false"],
    },
    SettingDef {
        key: "memory_extraction_interval_seconds",
        default_value: "3600",
        is_secret: false,
        recommendations: &["600", "1800", "3600", "7200"],
    },
    SettingDef {
        key: "memory_max_facts_per_user",
        default_value: "20",
        is_secret: false,
        recommendations: &["10", "20", "50"],
    },
    SettingDef {
        key: "memory_model",
        default_value: "(fallback to ai_chat_model)",
        is_secret: false,
        recommendations: &["gpt-4o-mini", "gpt-4o"],
    },
    SettingDef {
        key: "memory_max_tokens",
        default_value: "500",
        is_secret: false,
        recommendations: &["150", "300", "500", "1000"],
    },
];

/// Find the metadata definition for a known setting key.
pub fn get_setting_def(key: &str) -> Option<&'static SettingDef> {
    KNOWN_SETTINGS.iter().find(|def| def.key == key)
}

/// Validate a setting value string against validation rules.
pub fn validate_setting(key: &str, value: &str) -> Result<(), String> {
    match key {
        "markov_cooldown_seconds"
        | "ai_cooldown_seconds"
        | "tldr_cooldown_seconds"
        | "random_chime_cooldown_seconds"
        | "tag_cooldown_seconds"
        | "memory_extraction_interval_seconds" => {
            value.parse::<u64>().map_err(|_| {
                format!(
                    "Value '{}' must be a non-negative integer (seconds).",
                    value
                )
            })?;
        }
        "memory_max_facts_per_user" => {
            value
                .parse::<u32>()
                .map_err(|_| format!("Value '{}' must be a non-negative integer.", value))?;
        }
        "random_chime_chance" => {
            let pct = value
                .parse::<f64>()
                .map_err(|_| format!("Chance '{}' must be a number between 0 and 100.", value))?;
            if !(0.0..=100.0).contains(&pct) {
                return Err(format!("Chance '{}' must be between 0 and 100.", value));
            }
        }
        "random_chime_daily_cap" => {
            value.parse::<u32>().map_err(|_| {
                format!(
                    "Daily cap '{}' must be a non-negative integer (0 = unlimited).",
                    value
                )
            })?;
        }
        "ai_max_tokens" | "memory_max_tokens" => {
            value
                .parse::<u32>()
                .map_err(|_| format!("Value '{}' must be a non-negative integer.", value))?;
        }
        "ai_chat_model" | "random_chime_model" | "memory_model" => {
            let val_lower = value.to_lowercase();
            if !(val_lower.starts_with("gpt-")
                || val_lower.starts_with("o1-")
                || val_lower.starts_with("o3-")
                || val_lower.starts_with("claude-")
                || val_lower == "o1")
            {
                return Err(format!(
                    "Model '{}' is not recognized as a supported model name.",
                    value
                ));
            }
        }
        "reminder_pings" | "ai_debug" | "random_chime_relevance_check" | "memory_enabled" => {
            let val_lower = value.to_lowercase();
            if val_lower != "true" && val_lower != "false" {
                return Err(format!("Value '{}' must be 'true' or 'false'.", value));
            }
        }
        "bot_status_type" => {
            let val_lower = value.to_lowercase();
            if !["playing", "watching", "listening", "competing", "custom"]
                .contains(&val_lower.as_str())
            {
                return Err(format!(
                    "Status type '{}' must be one of: playing, watching, listening, competing, custom.",
                    value
                ));
            }
        }
        "allowed_bot_id" => {
            value
                .parse::<u64>()
                .map_err(|_| format!("Bot ID '{}' must be a valid numeric user ID.", value))?;
        }
        _ => {}
    }
    Ok(())
}
