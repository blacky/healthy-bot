use super::{is_server_admin, Context, Error};
use crate::pagination::paginate;
use crate::{create_embed, db, truncate_str, user_error};
use poise::serenity_prelude as serenity;

/// View or modify bot settings
#[poise::command(slash_command, prefix_command)]
pub async fn settings(
    ctx: Context<'_>,
    #[description = "Action (get, set)"]
    #[autocomplete = "autocomplete_settings_action"]
    action: Option<String>,
    #[description = "Setting key"]
    #[autocomplete = "autocomplete_settings_key"]
    key: Option<String>,
    #[rest]
    #[description = "New value"]
    #[autocomplete = "autocomplete_settings_value"]
    value: Option<String>,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let user_id = ctx.author().id.to_string();
    db::create_user_if_not_exists(pool, &user_id).await?;

    if action.as_deref() == Some("set") {
        if !is_server_admin(ctx).await {
            return Err(user_error("You are not authorized to change settings."));
        }
        let key = key.ok_or_else(|| user_error("No key supplied"))?;
        let value = value.ok_or_else(|| user_error("No value supplied"))?;

        // Setting validation
        if let Err(e) = crate::settings::validate_setting(&key, &value) {
            return Err(user_error(e));
        }

        if key == "main_text_channel" || key == "reminder_category" {
            let id_u64 = value.parse::<u64>().map_err(|_| {
                user_error(format!(
                    "ID '{}' must be a valid numeric Discord ID.",
                    value
                ))
            })?;
            let channel_id = serenity::all::ChannelId::new(id_u64);
            if channel_id.to_channel(&ctx).await.is_err() {
                return Err(user_error(format!(
                    "ID '{}' is invalid or not accessible by the bot.",
                    value
                )));
            }
        }

        ctx.data().set_setting(&key, &value).await?;

        ctx.send(
            poise::CreateReply::default()
                .content(format!("Setting {} set to {}", key, value))
                .ephemeral(true),
        )
        .await?;
    } else {
        let filter_key = match (action.as_deref(), key.as_deref()) {
            (Some("set"), _) => None,
            (Some("get"), k) => k,
            (Some(act), _) if act != "get" && act != "set" => Some(act),
            (_, k) => k,
        };

        let db_settings: Vec<db::Setting> =
            sqlx::query_as::<_, db::Setting>("SELECT * FROM setting")
                .fetch_all(pool)
                .await?;
        let db_map: std::collections::HashMap<String, String> =
            db_settings.into_iter().map(|s| (s.k, s.v)).collect();

        let mut settings_list: Vec<(String, String)> = Vec::new();
        let mut processed_keys = std::collections::HashSet::new();
        let is_admin = is_server_admin(ctx).await;

        for def in crate::settings::KNOWN_SETTINGS {
            let v = db_map
                .get(def.key)
                .cloned()
                .unwrap_or_else(|| def.default_value.to_string());
            settings_list.push((def.key.to_string(), v));
            processed_keys.insert(def.key.to_string());
        }

        for (k, v) in db_map {
            if !k.starts_with('_') && !processed_keys.contains(&k) {
                settings_list.push((k, v));
            }
        }

        if !is_admin {
            for (k, v) in &mut settings_list {
                if k.contains("prompt") {
                    *v = "[Redacted - Admin Only]".to_string();
                }
            }
        }

        if let Some(fk) = filter_key {
            let fk_lower = fk.to_lowercase();
            settings_list.retain(|(k, _)| k.to_lowercase().contains(&fk_lower));
        }

        if settings_list.is_empty() {
            let desc = if let Some(fk) = filter_key {
                format!("No settings found matching '{}'.", fk)
            } else {
                "No settings found.".to_string()
            };
            let embed = create_embed("HealthyBot Settings").description(desc);
            ctx.send(poise::CreateReply::default().embed(embed).ephemeral(true))
                .await?;
            return Ok(());
        }

        let page_size = 10;
        let total_pages = settings_list.len().div_ceil(page_size);

        paginate(ctx, total_pages, 1, true, |p| {
            let start_idx = (p - 1) * page_size;
            let end_idx = std::cmp::min(start_idx + page_size, settings_list.len());

            let title = if total_pages > 1 {
                format!("HealthyBot Settings (Page {} of {})", p, total_pages)
            } else {
                "HealthyBot Settings".to_string()
            };

            let mut embed = create_embed(&title);
            for (k, v) in &settings_list[start_idx..end_idx] {
                embed = embed.field(truncate_str(k, 256), truncate_str(v, 1024), false);
            }
            embed
        })
        .await?;
    }
    Ok(())
}

/// Set the bot's status activity
#[poise::command(slash_command, prefix_command)]
pub async fn status(
    ctx: Context<'_>,
    #[description = "Activity type"]
    #[autocomplete = "autocomplete_status"]
    activity_type: String,
    #[rest]
    #[description = "Status message"]
    message: String,
) -> Result<(), Error> {
    // Check permissions
    if !is_server_admin(ctx).await {
        return Err(user_error("You are not authorized to change status."));
    }

    let (activity_type, message) = match activity_type.to_lowercase().as_str() {
        "playing" | "watching" | "listening" | "competing" | "custom" => {
            (activity_type.to_lowercase(), message)
        }
        _ => {
            // Default behavior: if the first arg is not a type, treat everything as a custom status
            let full_msg = if message.is_empty() {
                activity_type
            } else {
                format!("{} {}", activity_type, message)
            };
            ("custom".to_string(), full_msg)
        }
    };

    let activity = match activity_type.as_str() {
        "playing" => Some(serenity::ActivityData::playing(&message)),
        "watching" => Some(serenity::ActivityData::watching(&message)),
        "listening" => Some(serenity::ActivityData::listening(&message)),
        "competing" => Some(serenity::ActivityData::competing(&message)),
        "custom" => Some(serenity::ActivityData::custom(&message)),
        _ => None,
    };

    ctx.serenity_context().set_activity(activity);

    // Save to DB and Cache atomically via Data helper
    ctx.data()
        .set_setting("bot_status_type", &activity_type)
        .await?;
    ctx.data()
        .set_setting("bot_status_message", &message)
        .await?;

    ctx.say(format!("Status updated to: {} {}", activity_type, message))
        .await?;
    Ok(())
}

pub async fn autocomplete_settings_key(_ctx: Context<'_>, partial: &str) -> Vec<String> {
    crate::settings::KNOWN_SETTINGS
        .iter()
        .map(|s| s.key)
        .filter(move |name| name.to_lowercase().starts_with(&partial.to_lowercase()))
        .map(|name| name.to_string())
        .collect()
}

pub async fn autocomplete_settings_action(_ctx: Context<'_>, partial: &str) -> Vec<String> {
    vec!["get", "set"]
        .into_iter()
        .filter(move |name| name.starts_with(partial))
        .map(|name| name.to_string())
        .collect()
}

pub async fn autocomplete_status(_ctx: Context<'_>, partial: &str) -> Vec<String> {
    vec!["playing", "watching", "listening", "competing", "custom"]
        .into_iter()
        .filter(move |name| name.to_lowercase().starts_with(&partial.to_lowercase()))
        .map(|name| name.to_string())
        .collect()
}

pub async fn autocomplete_settings_value(ctx: Context<'_>, partial: &str) -> Vec<String> {
    let mut key = None;
    if let poise::Context::Application(app_ctx) = ctx {
        for option in &app_ctx.interaction.data.options {
            if option.name == "set" {
                if let serenity::all::CommandDataOptionValue::SubCommand(sub_options) =
                    &option.value
                {
                    for sub_opt in sub_options {
                        if sub_opt.name == "key" {
                            if let serenity::all::CommandDataOptionValue::String(val) =
                                &sub_opt.value
                            {
                                key = Some(val.clone());
                            }
                        }
                    }
                }
            } else if option.name == "key" {
                if let serenity::all::CommandDataOptionValue::String(val) = &option.value {
                    key = Some(val.clone());
                }
            }
        }
    }

    let recommendations: &[&str] = key
        .as_deref()
        .and_then(crate::settings::get_setting_def)
        .map(|def| def.recommendations)
        .unwrap_or(&[]);

    recommendations
        .iter()
        .filter(move |val| val.to_lowercase().starts_with(&partial.to_lowercase()))
        .map(|val| val.to_string())
        .collect()
}
