use crate::db;
use crate::Data;
use chrono::{NaiveDateTime, TimeZone, Utc};
use chrono_tz::Europe::Amsterdam;
use poise::serenity_prelude as serenity;
use serenity::builder::{CreateEmbed, CreateEmbedFooter};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

const HOF_ROLE_ID: u64 = 446675275179098113;

#[derive(Debug, PartialEq)]
pub struct ParsedReminder {
    pub message: String,
    pub datetime_utc: chrono::DateTime<Utc>,
}

pub fn parse_reminder_input(input: &str) -> Vec<ParsedReminder> {
    input
        .split([';', '\n'])
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .filter_map(|msg| {
            let actions: Vec<&str> = msg.split_whitespace().collect();
            if actions.len() < 3 {
                return None;
            }

            let datetime_str = format!("{} {}", actions[0], actions[1]);
            if let Ok(datetime) = NaiveDateTime::parse_from_str(&datetime_str, "%d-%m-%Y %H:%M") {
                if let Some(datetime_tz) = Amsterdam.from_local_datetime(&datetime).single() {
                    return Some(ParsedReminder {
                        message: actions[2..].join(" "),
                        datetime_utc: datetime_tz.with_timezone(&Utc),
                    });
                }
            }
            None
        })
        .collect()
}

fn create_embed(title: &str) -> CreateEmbed {
    CreateEmbed::new()
        .title(title)
        .color(0xFFFF00) // Color.YELLOW equivalent
        .footer(CreateEmbedFooter::new("Healthy Bot"))
}

/// Manage reminders
#[poise::command(
    slash_command,
    prefix_command,
    rename = "remind",
    aliases("reminder", "reminders")
)]
pub async fn remind_cmd(
    ctx: Context<'_>,
    #[rest]
    #[description = "Reminder message or action"]
    input: Option<String>,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let user_id = ctx.author().id.to_string();
    db::create_user_if_not_exists(pool, &user_id).await?;
    let user = db::get_user(pool, &user_id).await.unwrap();

    let input_val = input.unwrap_or_default();
    let parts: Vec<&str> = input_val.split_whitespace().collect();
    let action = parts.first().copied().unwrap_or("");

    if action == "remove" {
        let id_str = parts.get(1).ok_or("No id provided")?;
        let id: i64 = id_str.parse()?;
        let reminder: db::Reminder =
            sqlx::query_as::<_, db::Reminder>("SELECT * FROM reminder WHERE id = ?")
                .bind(id)
                .fetch_one(pool)
                .await?;

        if reminder.owner_discord_id != user_id {
            return Err("You do not own this reminder.".into());
        }

        sqlx::query("DELETE FROM reminder WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await?;
        ctx.say(format!("Successfully removed reminder #{}.", id))
            .await?;
        return Ok(());
    }

    // Check for listing actions
    let filter = parts
        .iter()
        .find(|&&p| ["today", "tomorrow", "week", "month"].contains(&p));

    let invoked_name = ctx.invoked_command_name();
    if input_val.is_empty() || filter.is_some() {
        let mut reminders: Vec<db::Reminder> = if invoked_name == "reminders" {
            sqlx::query_as::<_, db::Reminder>("SELECT * FROM reminder")
                .fetch_all(pool)
                .await?
        } else {
            sqlx::query_as::<_, db::Reminder>("SELECT * FROM reminder WHERE owner_discord_id = ?")
                .bind(&user_id)
                .fetch_all(pool)
                .await?
        };

        let now_ams = Utc::now().with_timezone(&Amsterdam);
        if let Some(&f) = filter {
            reminders.retain(|r| {
                let r_date_ams = r.date_utc().with_timezone(&Amsterdam).date_naive();
                match f {
                    "today" => r_date_ams == now_ams.date_naive(),
                    "tomorrow" => r_date_ams == now_ams.date_naive() + chrono::Duration::days(1),
                    "week" => {
                        r_date_ams >= now_ams.date_naive()
                            && r_date_ams <= now_ams.date_naive() + chrono::Duration::days(7)
                    }
                    "month" => {
                        r_date_ams >= now_ams.date_naive()
                            && r_date_ams <= now_ams.date_naive() + chrono::Duration::days(30)
                    }
                    _ => true,
                }
            });
        }

        let mut embed = create_embed("Reminders").footer(CreateEmbedFooter::new(format!(
            "Reminder example: !remind {} f1 time turds",
            now_ams.format("%d-%m-%Y %H:%M")
        )));

        if reminders.is_empty() {
            embed = embed.description("No reminders found.");
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
            return Ok(());
        }

        reminders.sort_by_key(|r| r.date);
        for r in reminders.iter().take(25) {
            embed = embed.field(
                format!("#{}. {}", r.id, r.message.trim()),
                format!("<t:{}>", r.date_utc().timestamp()),
                false,
            );
        }
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
        return Ok(());
    }

    // Add reminder logic
    let is_authorized = {
        let is_owner = ctx
            .guild()
            .map(|g| g.owner_id == ctx.author().id)
            .unwrap_or(false);
        let has_admin = match ctx.author_member().await {
            Some(member) => member
                .permissions
                .map(|p| p.administrator())
                .unwrap_or(false),
            None => false,
        };
        let has_hof = match ctx.author_member().await {
            Some(member) => member.roles.iter().any(|r| r.get() == HOF_ROLE_ID),
            None => false,
        };
        is_owner || has_admin || user.role == "ADMIN" || user.authorized || has_hof
    };

    if is_authorized {
        let reminders = parse_reminder_input(&input_val);
        let mut created = 0;

        for r in reminders {
            sqlx::query(
                "INSERT INTO reminder (message, date, owner_discord_id) VALUES (?, ?, ?)",
            )
            .bind(r.message)
            .bind(r.datetime_utc.timestamp_millis())
            .bind(&user_id)
            .execute(pool)
            .await?;
            created += 1;
        }

        if created == 0 {
            return Err("Failed to parse reminder. Make sure the formatting is correct (eg. 30-09-2022 19:24 message).".into());
        }

        ctx.say(format!("Created {} reminders.", created)).await?;
        return Ok(());
    }

    Err("You are not authorized to use this command.".into())
}

/// Generate a markov chain message for a user
#[poise::command(slash_command, prefix_command)]
pub async fn markov(ctx: Context<'_>, user: Option<serenity::User>) -> Result<(), Error> {
    // Check cooldown from cache
    let cooldown_secs: u64 = {
        let cache = ctx.data().settings_cache.read().await;
        cache
            .get("markov_cooldown_seconds")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };

    let mut last_inv = ctx.data().last_markov.lock().await;
    if last_inv.elapsed().as_secs() < cooldown_secs {
        return Ok(());
    }
    *last_inv = std::time::Instant::now();

    let target_id = user
        .map(|u| u.id.to_string())
        .unwrap_or_else(|| ctx.author().id.to_string());
    let markovs = ctx.data().markov_repo.markovs.read().await;

    if let Some(markov) = markovs.get(&target_id) {
        if let Some(generated) = markov.generate() {
            log::info!("Markov generated for {}: {}", target_id, generated);
            ctx.say(generated).await?;
        } else {
            log::warn!("Markov generation failed for {}", target_id);
            ctx.say("Crack cocaine").await?;
        }
    } else {
        log::warn!("No Markov data found for {}", target_id);
        ctx.say("Crack cocaine").await?;
    }
    Ok(())
}

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
    value: Option<String>,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let user_id = ctx.author().id.to_string();
    db::create_user_if_not_exists(pool, &user_id).await?;

    if action.as_deref() == Some("set") {
        let is_admin = {
            let is_owner = ctx
                .guild()
                .map(|g| g.owner_id == ctx.author().id)
                .unwrap_or(false);
            let has_admin = match ctx.author_member().await {
                Some(member) => member
                    .permissions
                    .map(|p| p.administrator())
                    .unwrap_or(false),
                None => false,
            };
            is_owner || has_admin
        };

        if !is_admin {
            return Err("You are not authorized to change settings.".into());
        }
        let key = key.ok_or("No key supplied")?;
        let value = value.ok_or("No value supplied")?;

        sqlx::query("INSERT INTO setting (k, v) VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v = ?")
            .bind(&key)
            .bind(&value)
            .bind(&value)
            .execute(pool)
            .await?;

        // Update cache
        {
            let mut cache = ctx.data().settings_cache.write().await;
            cache.insert(key.clone(), value.clone());
        }

        ctx.say(format!("Setting {} set to {}", key, value)).await?;
    } else {
        let settings: Vec<db::Setting> = sqlx::query_as::<_, db::Setting>("SELECT * FROM setting")
            .fetch_all(pool)
            .await?;

        let mut embed = create_embed("HealthyBot Settings");
        for s in settings {
            embed = embed.field(s.k, s.v, false);
        }
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
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
    let pool = &ctx.data().db_pool;

    // Check permissions
    let is_admin = {
        let is_owner = ctx
            .guild()
            .map(|g| g.owner_id == ctx.author().id)
            .unwrap_or(false);
        let has_admin = match ctx.author_member().await {
            Some(member) => member
                .permissions
                .map(|p| p.administrator())
                .unwrap_or(false),
            None => false,
        };
        is_owner || has_admin
    };
    if !is_admin {
        return Err("You are not authorized to change status.".into());
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

    // Save to DB
    sqlx::query("INSERT INTO setting (k, v) VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v = ?")
        .bind("bot_status_type")
        .bind(&activity_type)
        .bind(&activity_type)
        .execute(pool)
        .await?;

    sqlx::query("INSERT INTO setting (k, v) VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v = ?")
        .bind("bot_status_message")
        .bind(&message)
        .bind(&message)
        .execute(pool)
        .await?;

    // Update Cache
    {
        let mut cache = ctx.data().settings_cache.write().await;
        cache.insert("bot_status_type".to_string(), activity_type.clone());
        cache.insert("bot_status_message".to_string(), message.clone());
    }

    ctx.say(format!("Status updated to: {} {}", activity_type, message))
        .await?;
    Ok(())
}

/// User and database management commands
#[poise::command(
    slash_command,
    prefix_command,
    rename = "user",
    aliases("users", "inthards")
)]
pub async fn user_cmd(
    ctx: Context<'_>,
    #[description = "Action (authorize, inthards, sync, latest_message)"]
    #[autocomplete = "autocomplete_user_action"]
    action: Option<String>,
    #[rest]
    #[description = "User or ID"]
    arg: Option<String>,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let cmd_name = ctx.command().name.as_str();

    if cmd_name == "inthards" || action.as_deref() == Some("inthards") {
        let top_x = action
            .as_deref()
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(5)
            .unsigned_abs() as usize;
        let users: Vec<db::User> =
            sqlx::query_as::<_, db::User>("SELECT * FROM users ORDER BY last_message ASC LIMIT ?")
                .bind(top_x as i32)
                .fetch_all(pool)
                .await?;

        let mut embed =
            create_embed("Inthards Top List").description(format!("Top {} inactive inters", top_x));

        for (idx, user) in users.iter().enumerate() {
            let ts = user.last_message_utc().map(|m| m.timestamp()).unwrap_or(0);
            embed = embed.field(
                format!("{}.", idx + 1),
                format!("<@{}>: <t:{}:R>", user.discord_id, ts),
                false,
            );
        }
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
        return Ok(());
    }

    match action.as_deref() {
        Some("authorize") | Some("sync") | Some("latest_message") => {
            let is_admin = {
                let is_owner = ctx
                    .guild()
                    .map(|g| g.owner_id == ctx.author().id)
                    .unwrap_or(false);
                let has_admin = match ctx.author_member().await {
                    Some(member) => member
                        .permissions
                        .map(|p| p.administrator())
                        .unwrap_or(false),
                    None => false,
                };
                let db_admin = db::get_user(pool, &ctx.author().id.to_string())
                    .await
                    .map(|u| u.role == "ADMIN")
                    .unwrap_or(false);
                is_owner || has_admin || db_admin
            };

            if !is_admin {
                return Err("Unauthorized".into());
            }

            if action.as_deref() == Some("authorize") {
                let target_id = arg.ok_or("No user id specified")?;
                db::create_user_if_not_exists(pool, &target_id).await?;
                sqlx::query("UPDATE users SET authorized = NOT authorized WHERE discord_id = ?")
                    .bind(&target_id)
                    .execute(pool)
                    .await?;
                let target = db::get_user(pool, &target_id).await.unwrap();
                ctx.say(format!(
                    "<@{}> is {} authorized.",
                    target_id,
                    if target.authorized {
                        "now"
                    } else {
                        "no longer"
                    }
                ))
                .await?;
            } else if action.as_deref() == Some("sync") {
                let guild_id = ctx.guild_id().ok_or("Must be run in a guild")?;
                let mut synced_count = 0;

                let members = guild_id.members(ctx.http(), None, None).await?;
                for member in members {
                    if !member.user.bot {
                        let _ =
                            db::create_user_if_not_exists(pool, &member.user.id.to_string()).await;
                        synced_count += 1;
                    } else {
                        let _ = sqlx::query("DELETE FROM users WHERE discord_id = ?")
                            .bind(member.user.id.to_string())
                            .execute(pool)
                            .await;
                    }
                }

                ctx.say(format!("Synced {} members with database.", synced_count))
                    .await?;
            } else if action.as_deref() == Some("latest_message") {
                let target_id_str = arg.ok_or("No user id specified")?;
                let target_id: u64 = target_id_str
                    .replace("<@", "")
                    .replace(">", "")
                    .replace("!", "")
                    .parse()?;

                let channel_id_str = {
                    let cache = ctx.data().settings_cache.read().await;
                    cache
                        .get("main_text_channel")
                        .cloned()
                        .ok_or("main_text_channel setting not found")?
                };
                let channel_id: u64 = channel_id_str.parse()?;

                let messages = serenity::ChannelId::new(channel_id)
                    .messages(ctx.http(), serenity::builder::GetMessages::new().limit(100))
                    .await?;

                if let Some(msg) = messages.iter().find(|m| m.author.id.get() == target_id) {
                    ctx.say(format!(
                        "<@{}>'s latest message was: {} at <t:{}>",
                        target_id,
                        msg.content_safe(ctx.cache()),
                        msg.timestamp.unix_timestamp()
                    ))
                    .await?;
                } else {
                    ctx.say(format!(
                        "No message found for member <@{}> in the last 100 messages.",
                        target_id
                    ))
                    .await?;
                }
            }
        }
        _ => {
            ctx.say("Invalid action. Available: authorize, inthards, sync, latest_message")
                .await?;
        }
    }

    Ok(())
}

/// Show help for all commands
#[poise::command(slash_command, prefix_command)]
pub async fn help(
    ctx: Context<'_>,
    #[description = "Specific command to show help for"]
    #[autocomplete = "poise::builtins::autocomplete_command"]
    command: Option<String>,
) -> Result<(), Error> {
    poise::builtins::help(
        ctx,
        command.as_deref(),
        poise::builtins::HelpConfiguration {
            extra_text_at_bottom: "Type !help <command> for more info on a specific command.",
            ..Default::default()
        },
    )
    .await?;
    Ok(())
}

/// Register slash commands (Owner only)
#[poise::command(prefix_command, hide_in_help)]
pub async fn register(ctx: Context<'_>) -> Result<(), Error> {
    let is_owner = ctx
        .guild()
        .map(|g| g.owner_id == ctx.author().id)
        .unwrap_or(false);
    let is_authorized_user = ctx.author().id.get() == 210531463932674050;

    if !is_owner && !is_authorized_user {
        return Err(
            "Only the server owner or authorized administrators can register commands.".into(),
        );
    }

    poise::builtins::register_application_commands_buttons(ctx).await?;
    Ok(())
}

async fn autocomplete_settings_key(_ctx: Context<'_>, partial: &str) -> Vec<String> {
    let keys = vec![
        "command_prefix",
        "main_text_channel",
        "reminder_category",
        "markov_cooldown_seconds",
        "ai_cooldown_seconds",
        "ai_initial_prompt",
        "ai_chat_model",
        "bot_status_type",
        "bot_status_message",
    ];
    keys.into_iter()
        .filter(move |name| name.to_lowercase().starts_with(&partial.to_lowercase()))
        .map(|name| name.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_reminder_input_single() {
        let input = "01-01-2023 12:00 do something";
        let parsed = parse_reminder_input(input);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].message, "do something");
        // 12:00 Amsterdam is 11:00 UTC (Jan 1st)
        assert_eq!(parsed[0].datetime_utc.timestamp(), 1672570800);
    }

    #[test]
    fn test_parse_reminder_input_multiple() {
        let input = "01-01-2023 12:00 first; 02-01-2023 13:00 second";
        let parsed = parse_reminder_input(input);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].message, "first");
        assert_eq!(parsed[1].message, "second");
    }

    #[test]
    fn test_parse_reminder_input_invalid() {
        let input = "invalid input";
        let parsed = parse_reminder_input(input);
        assert!(parsed.is_empty());
    }
}

async fn autocomplete_settings_action(_ctx: Context<'_>, partial: &str) -> Vec<String> {
    vec!["get", "set"]
        .into_iter()
        .filter(move |name| name.starts_with(partial))
        .map(|name| name.to_string())
        .collect()
}

async fn autocomplete_status(_ctx: Context<'_>, partial: &str) -> Vec<String> {
    vec!["playing", "watching", "listening", "competing", "custom"]
        .into_iter()
        .filter(move |name| name.to_lowercase().starts_with(&partial.to_lowercase()))
        .map(|name| name.to_string())
        .collect()
}

async fn autocomplete_user_action(_ctx: Context<'_>, partial: &str) -> Vec<String> {
    vec!["authorize", "inthards", "sync", "latest_message"]
        .into_iter()
        .filter(move |name| name.to_lowercase().starts_with(&partial.to_lowercase()))
        .map(|name| name.to_string())
        .collect()
}
