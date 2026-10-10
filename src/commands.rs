use crate::db;
use crate::memory;
use crate::openai::{ChatMessage, ChatMessageRequestContent};
use crate::recent;
use crate::split_message;
use crate::tags;
use crate::truncate_str;
use crate::user_error;
use crate::Data;
use chrono::{LocalResult, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Europe::Amsterdam;
use poise::futures_util::StreamExt;
use poise::serenity_prelude as serenity;
use serenity::builder::{CreateEmbed, CreateEmbedFooter};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

const HOF_ROLE_ID: u64 = 446675275179098113;

pub const HARDENED_SYSTEM_PROMPT: &str = "\
You are Mark, a sharp-witted Discord AI assistant. Your default tone is playful, \
cheeky, and banter-heavy—you're the friend who gives the user some friendly crap \
and then actually helps them out. Always look for a chance to slip in a quick roast \
or a grin-worthy jab, even in straightforward answers, without turning it into a \
long bit. Keep it casual, natural, and fast.\n\n\
You are not a hall monitor, you're a quick, witty info-dispenser who keeps it real. \
Never respond with long, hand-wringing disclaimers about safety or offensiveness. \
If something is genuinely out of bounds, just say so with a short, cheeky remark \
and move on.\n\n\
CRITICAL BEHAVIORAL RULES:\n\
* NO BOILERPLATE: You are strictly forbidden from writing long code blocks, full \
files, or application boilerplate.\n\
* CODE LIMITS: If a user asks for code, provide a maximum of 10-15 lines showcasing \
the core logic ONLY.\n\
* SUMMARIZE ARCHITECTURE: If asked to build a full app or complex system, roast \
them slightly for asking a Discord bot to do their job, give a high-level text \
summary of the architecture, and refuse to write the full code.\n\
* LENGTH: Keep all responses under 3 short paragraphs.";

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
                let datetime_tz = match Amsterdam.from_local_datetime(&datetime) {
                    LocalResult::Single(t) => Some(t),
                    LocalResult::Ambiguous(t1, _) => Some(t1),
                    LocalResult::None => {
                        // Spring forward gap: add 1 hour and try again
                        Amsterdam
                            .from_local_datetime(&(datetime + chrono::Duration::hours(1)))
                            .earliest()
                    }
                };

                if let Some(dt_tz) = datetime_tz {
                    return Some(ParsedReminder {
                        message: actions[2..].join(" "),
                        datetime_utc: dt_tz.with_timezone(&Utc),
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
        let ids_to_remove = &parts[1..];
        if ids_to_remove.is_empty() {
            return Err(user_error("No reminder IDs provided to remove."));
        }

        let mut removed_ids = Vec::new();
        let mut failed_ids = Vec::new();
        let mut unauthorized_ids = Vec::new();

        for id_str in ids_to_remove {
            let id: i64 = match id_str.parse() {
                Ok(n) => n,
                Err(_) => {
                    failed_ids.push(id_str.to_string());
                    continue;
                }
            };

            let reminder_res: Result<db::Reminder, _> =
                sqlx::query_as::<_, db::Reminder>("SELECT * FROM reminder WHERE id = ?")
                    .bind(id)
                    .fetch_one(pool)
                    .await;

            match reminder_res {
                Ok(reminder) => {
                    if reminder.owner_discord_id != user_id {
                        unauthorized_ids.push(id.to_string());
                        continue;
                    }

                    if sqlx::query("DELETE FROM reminder WHERE id = ?")
                        .bind(id)
                        .execute(pool)
                        .await
                        .is_ok()
                    {
                        removed_ids.push(id.to_string());
                    } else {
                        failed_ids.push(id.to_string());
                    }
                }
                Err(_) => {
                    failed_ids.push(id.to_string());
                }
            }
        }

        let mut response = String::new();
        if !removed_ids.is_empty() {
            response.push_str(&format!(
                "Successfully removed {} reminder(s): #{}.\n",
                removed_ids.len(),
                removed_ids.join(", #")
            ));
        }
        if !unauthorized_ids.is_empty() {
            response.push_str(&format!(
                "You do not own these reminder(s): #{}.\n",
                unauthorized_ids.join(", #")
            ));
        }
        if !failed_ids.is_empty() {
            response.push_str(&format!(
                "Could not find or parse these ID(s): {}.",
                failed_ids.join(", ")
            ));
        }

        ctx.say(response.trim()).await?;
        return Ok(());
    }

    // Check for listing actions
    let mut page: usize = 1;
    let mut filter = None;

    for part in &parts {
        if ["today", "tomorrow", "week", "month"].contains(part) {
            filter = Some(*part);
        } else if let Ok(p) = part.parse::<usize>() {
            if p > 0 {
                page = p;
            }
        }
    }

    let invoked_name = ctx.invoked_command_name();
    let is_listing = invoked_name == "reminders"
        || invoked_name == "reminder"
        || input_val.is_empty()
        || filter.is_some()
        || (parts.len() <= 2
            && parts
                .first()
                .map(|p| p.parse::<usize>().is_ok())
                .unwrap_or(false));

    if is_listing {
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
        if let Some(f) = filter {
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

        if reminders.is_empty() {
            let mut embed = create_embed("Reminders").footer(CreateEmbedFooter::new(format!(
                "Reminder example: !remind {} f1 time turds",
                now_ams.format("%d-%m-%Y %H:%M")
            )));
            embed = embed.description("No reminders found.");
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
            return Ok(());
        }

        reminders.sort_by_key(|r| r.date);

        // Paginate: 10 reminders per page
        let page_size = 10;
        let total_pages = reminders.len().div_ceil(page_size);

        if page > total_pages {
            let mut embed = create_embed("Reminders").footer(CreateEmbedFooter::new(format!(
                "Reminder example: !remind {} f1 time turds",
                now_ams.format("%d-%m-%Y %H:%M")
            )));
            embed = embed.description(format!(
                "Page {} does not exist. Total pages: {}.",
                page, total_pages
            ));
            ctx.send(poise::CreateReply::default().embed(embed)).await?;
            return Ok(());
        }

        let build_page_data =
            |page: usize,
             total_pages: usize,
             reminders: &[db::Reminder],
             now_ams: chrono::DateTime<chrono_tz::Tz>| {
                let start_idx = (page - 1) * page_size;
                let end_idx = std::cmp::min(start_idx + page_size, reminders.len());

                let title = if total_pages > 1 {
                    format!("Reminders (Page {} of {})", page, total_pages)
                } else {
                    "Reminders".to_string()
                };

                let mut embed = create_embed(&title).footer(CreateEmbedFooter::new(format!(
                    "Reminder example: !remind {} f1 time turds",
                    now_ams.format("%d-%m-%Y %H:%M")
                )));

                for r in &reminders[start_idx..end_idx] {
                    embed = embed.field(
                        truncate_str(&format!("#{}. {}", r.id, r.message.trim()), 256),
                        format!("<t:{}>", r.date_utc().timestamp()),
                        false,
                    );
                }

                let components = if total_pages > 1 {
                    vec![serenity::CreateActionRow::Buttons(vec![
                        serenity::CreateButton::new("prev_page")
                            .label("◀")
                            .style(serenity::ButtonStyle::Secondary)
                            .disabled(page == 1),
                        serenity::CreateButton::new("next_page")
                            .label("▶")
                            .style(serenity::ButtonStyle::Secondary)
                            .disabled(page == total_pages),
                        serenity::CreateButton::new("cancel")
                            .label("❌")
                            .style(serenity::ButtonStyle::Danger),
                    ])]
                } else {
                    vec![]
                };

                (embed, components)
            };

        let (embed, components) = build_page_data(page, total_pages, &reminders, now_ams);
        let reply = ctx
            .send(
                poise::CreateReply::default()
                    .embed(embed)
                    .components(components),
            )
            .await?;

        if total_pages > 1 {
            let mut collector = reply
                .message()
                .await?
                .await_component_interactions(ctx)
                .author_id(ctx.author().id)
                .timeout(std::time::Duration::from_secs(60))
                .stream();

            let mut cancelled = false;
            while let Some(interaction) = collector.next().await {
                if interaction.data.custom_id == "cancel" {
                    cancelled = true;
                    let (current_embed, _) =
                        build_page_data(page, total_pages, &reminders, now_ams);
                    let _ = interaction
                        .create_response(
                            ctx,
                            serenity::CreateInteractionResponse::UpdateMessage(
                                serenity::CreateInteractionResponseMessage::new()
                                    .embed(current_embed)
                                    .components(vec![]),
                            ),
                        )
                        .await;
                    break;
                }

                match interaction.data.custom_id.as_str() {
                    "prev_page" => page = page.saturating_sub(1),
                    "next_page" => page = std::cmp::min(page + 1, total_pages),
                    _ => continue,
                }

                let (new_embed, new_components) =
                    build_page_data(page, total_pages, &reminders, now_ams);
                let _ = interaction
                    .create_response(
                        ctx,
                        serenity::CreateInteractionResponse::UpdateMessage(
                            serenity::CreateInteractionResponseMessage::new()
                                .embed(new_embed)
                                .components(new_components),
                        ),
                    )
                    .await;
            }

            if !cancelled {
                let (final_embed, _) = build_page_data(page, total_pages, &reminders, now_ams);
                let _ = reply
                    .edit(
                        ctx,
                        poise::CreateReply::default()
                            .embed(final_embed)
                            .components(vec![]),
                    )
                    .await;
            }
        }

        return Ok(());
    }

    // Add reminder logic
    let is_authorized = {
        let has_hof = match ctx.author_member().await {
            Some(member) => member.roles.iter().any(|r| r.get() == HOF_ROLE_ID),
            None => false,
        };
        is_server_admin(ctx).await || user.role == "ADMIN" || user.authorized || has_hof
    };

    if is_authorized {
        let reminders = parse_reminder_input(&input_val);
        let mut created = 0;

        for r in reminders {
            sqlx::query("INSERT INTO reminder (message, date, owner_discord_id) VALUES (?, ?, ?)")
                .bind(r.message)
                .bind(r.datetime_utc.timestamp_millis())
                .bind(&user_id)
                .execute(pool)
                .await?;
            created += 1;
        }

        if created == 0 {
            return Err(user_error("Failed to parse reminder. Make sure the formatting is correct (eg. 30-09-2022 19:24 message)."));
        }

        ctx.say(format!("Created {} reminders.", created)).await?;
        return Ok(());
    }

    Err(user_error("You are not authorized to use this command."))
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
    if let Some(generated) = ctx.data().markov_repo.generate(&target_id).await {
        log::info!("Markov generated for {}: {}", target_id, generated);
        ctx.say(generated).await?;
    } else {
        log::warn!("Markov generation failed or no data for {}", target_id);
        ctx.say("Crack cocaine").await?;
    }
    Ok(())
}

/// Summarize the recent conversation in the current channel
#[poise::command(slash_command, prefix_command)]
pub async fn tldr(
    ctx: Context<'_>,
    #[description = "How many recent messages to summarize (default 50, max 100)"] count: Option<
        i64,
    >,
) -> Result<(), Error> {
    // Cooldown check, mirroring the markov / AI throttling pattern.
    let cooldown_secs: u64 = {
        let cache = ctx.data().settings_cache.read().await;
        cache
            .get("tldr_cooldown_seconds")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    {
        let mut last_inv = ctx.data().last_tldr.lock().await;
        if last_inv.elapsed().as_secs() < cooldown_secs {
            return Ok(());
        }
        *last_inv = std::time::Instant::now();
    }

    // 100 is Discord's single-fetch cap, so we never paginate.
    let limit = count.unwrap_or(50).clamp(1, 100) as u8;

    let messages = ctx
        .channel_id()
        .messages(
            ctx.http(),
            serenity::builder::GetMessages::new().limit(limit),
        )
        .await?;

    let entries: Vec<recent::RecentMessage> = messages
        .iter()
        .map(|m| recent::RecentMessage {
            author_name: m.author.name.clone(),
            // content_safe renders mentions as names instead of raw <@id> tokens.
            content: m.content_safe(ctx.cache()),
            is_bot: m.author.bot,
        })
        .collect();

    let transcript = recent::build_transcript(&entries)
        .ok_or_else(|| user_error("There's nothing recent to summarize here."))?;

    let chat_model = {
        let cache = ctx.data().settings_cache.read().await;
        cache
            .get("ai_chat_model")
            .cloned()
            .unwrap_or_else(|| "gpt-4o-mini".to_string())
    };

    let request = vec![
        ChatMessage {
            role: "system".to_string(),
            name: None,
            content: ChatMessageRequestContent::Text(
                "You are a summarizer for a Discord channel. You are given a chronological \
                 transcript of messages formatted as `Name: message`. Produce a concise \
                 summary of the discussion as a few short bullet points, focusing on the key \
                 topics, decisions, and open questions. Do not invent information that is not \
                 present in the transcript."
                    .to_string(),
            ),
        },
        ChatMessage {
            role: "user".to_string(),
            name: None,
            content: ChatMessageRequestContent::Text(transcript),
        },
    ];

    let _typing = ctx.channel_id().start_typing(&ctx.serenity_context().http);

    let response = ctx
        .data()
        .openai_client
        .create_chat(&chat_model, request)
        .await?;

    let summary = response
        .choices
        .first()
        .map(|c| c.message.content_text().to_string())
        .unwrap_or_default();

    if summary.trim().is_empty() {
        return Err(user_error("The summarizer returned an empty response."));
    }

    // Discord caps a single message at 2000 chars; send the summary in pieces.
    for chunk in split_message(&summary, 2000) {
        ctx.say(chunk).await?;
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

        // Setting validation dry-runs
        match key.as_str() {
            "main_text_channel" | "reminder_category" => {
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
            "markov_cooldown_seconds"
            | "ai_cooldown_seconds"
            | "tldr_cooldown_seconds"
            | "random_chime_cooldown_seconds"
            | "memory_extraction_interval_seconds" => {
                let _ = value.parse::<u64>().map_err(|_| {
                    user_error(format!(
                        "Value '{}' must be a non-negative integer (seconds).",
                        value
                    ))
                })?;
            }
            "memory_max_facts_per_user" => {
                let _ = value.parse::<u32>().map_err(|_| {
                    user_error(format!("Value '{}' must be a non-negative integer.", value))
                })?;
            }
            "random_chime_chance" => {
                let pct = value.parse::<f64>().map_err(|_| {
                    user_error(format!(
                        "Chance '{}' must be a number between 0 and 100.",
                        value
                    ))
                })?;
                if !(0.0..=100.0).contains(&pct) {
                    return Err(user_error(format!(
                        "Chance '{}' must be between 0 and 100.",
                        value
                    )));
                }
            }
            "random_chime_daily_cap" => {
                let _ = value.parse::<u32>().map_err(|_| {
                    user_error(format!(
                        "Daily cap '{}' must be a non-negative integer (0 = unlimited).",
                        value
                    ))
                })?;
            }
            "ai_chat_model" | "random_chime_model" | "memory_model" => {
                let val_lower = value.to_lowercase();
                if !(val_lower.starts_with("gpt-")
                    || val_lower.starts_with("o1-")
                    || val_lower.starts_with("o3-")
                    || val_lower.starts_with("claude-")
                    || val_lower == "o1")
                {
                    return Err(user_error(format!(
                        "Model '{}' is not recognized as a supported model name.",
                        value
                    )));
                }
            }
            "reminder_pings" | "ai_debug" | "random_chime_relevance_check" | "memory_enabled" => {
                let val_lower = value.to_lowercase();
                if val_lower != "true" && val_lower != "false" {
                    return Err(user_error(format!(
                        "Value '{}' must be 'true' or 'false'.",
                        value
                    )));
                }
            }
            "bot_status_type" => {
                let val_lower = value.to_lowercase();
                if !["playing", "watching", "listening", "competing", "custom"]
                    .contains(&val_lower.as_str())
                {
                    return Err(user_error(format!(
                        "Status type '{}' must be one of: playing, watching, listening, competing, custom.",
                        value
                    )));
                }
            }
            "allowed_bot_id" => {
                let _ = value.parse::<u64>().map_err(|_| {
                    user_error(format!(
                        "Bot ID '{}' must be a valid numeric user ID.",
                        value
                    ))
                })?;
            }
            _ => {}
        }

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

        let known_defaults: &[(&str, &str)] = &[
            ("command_prefix", "!"),
            ("main_text_channel", "(not set)"),
            ("reminder_category", "(not set)"),
            ("markov_cooldown_seconds", "60"),
            ("ai_cooldown_seconds", "60"),
            ("tldr_cooldown_seconds", "60"),
            ("ai_initial_prompt", HARDENED_SYSTEM_PROMPT),
            ("ai_chat_model", "gpt-4o"),
            ("bot_status_type", "playing"),
            ("bot_status_message", "(not set)"),
            ("allowed_bot_id", "(not set)"),
            ("ai_debug", "false"),
            ("reminder_pings", "true"),
            ("random_chime_chance", "0"),
            ("random_chime_cooldown_seconds", "1800"),
            ("random_chime_daily_cap", "3"),
            ("random_chime_prompt", "(fallback to ai_initial_prompt)"),
            ("random_chime_model", "(fallback to ai_chat_model)"),
            ("random_chime_relevance_check", "true"),
            ("ai_max_tokens", "300"),
            ("memory_enabled", "true"),
            ("memory_extraction_interval_seconds", "3600"),
            ("memory_max_facts_per_user", "20"),
            ("memory_model", "(fallback to ai_chat_model)"),
            ("memory_max_tokens", "500"),
        ];

        let mut settings_list: Vec<(String, String)> = Vec::new();
        let mut processed_keys = std::collections::HashSet::new();
        let is_admin = is_server_admin(ctx).await;

        for &(k, default_v) in known_defaults {
            let v = db_map
                .get(k)
                .cloned()
                .unwrap_or_else(|| default_v.to_string());
            settings_list.push((k.to_string(), v));
            processed_keys.insert(k.to_string());
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
        let mut page = 1;

        let build_page_data = |page: usize,
                               total_pages: usize,
                               settings: &[(String, String)]|
         -> (CreateEmbed, Vec<serenity::CreateActionRow>) {
            let start_idx = (page - 1) * page_size;
            let end_idx = std::cmp::min(start_idx + page_size, settings.len());

            let title = if total_pages > 1 {
                format!("HealthyBot Settings (Page {} of {})", page, total_pages)
            } else {
                "HealthyBot Settings".to_string()
            };

            let mut embed = create_embed(&title);
            for (k, v) in &settings[start_idx..end_idx] {
                embed = embed.field(truncate_str(k, 256), truncate_str(v, 1024), false);
            }

            let components = if total_pages > 1 {
                vec![serenity::CreateActionRow::Buttons(vec![
                    serenity::CreateButton::new("prev_page")
                        .label("◀")
                        .style(serenity::ButtonStyle::Secondary)
                        .disabled(page == 1),
                    serenity::CreateButton::new("next_page")
                        .label("▶")
                        .style(serenity::ButtonStyle::Secondary)
                        .disabled(page == total_pages),
                    serenity::CreateButton::new("cancel")
                        .label("❌")
                        .style(serenity::ButtonStyle::Danger),
                ])]
            } else {
                vec![]
            };

            (embed, components)
        };

        let (embed, components) = build_page_data(page, total_pages, &settings_list);
        let reply = ctx
            .send(
                poise::CreateReply::default()
                    .embed(embed)
                    .components(components)
                    .ephemeral(true),
            )
            .await?;

        if total_pages > 1 {
            let mut collector = reply
                .message()
                .await?
                .await_component_interactions(ctx)
                .author_id(ctx.author().id)
                .timeout(std::time::Duration::from_secs(60))
                .stream();

            let mut cancelled = false;
            while let Some(interaction) = collector.next().await {
                if interaction.data.custom_id == "cancel" {
                    cancelled = true;
                    let (current_embed, _) = build_page_data(page, total_pages, &settings_list);
                    let _ = interaction
                        .create_response(
                            ctx,
                            serenity::CreateInteractionResponse::UpdateMessage(
                                serenity::CreateInteractionResponseMessage::new()
                                    .embed(current_embed)
                                    .components(vec![]),
                            ),
                        )
                        .await;
                    break;
                }

                match interaction.data.custom_id.as_str() {
                    "prev_page" => page = page.saturating_sub(1),
                    "next_page" => page = std::cmp::min(page + 1, total_pages),
                    _ => continue,
                }
                let (new_embed, new_components) =
                    build_page_data(page, total_pages, &settings_list);
                let _ = interaction
                    .create_response(
                        ctx,
                        serenity::CreateInteractionResponse::UpdateMessage(
                            serenity::CreateInteractionResponseMessage::new()
                                .embed(new_embed)
                                .components(new_components),
                        ),
                    )
                    .await;
            }

            if !cancelled {
                let (final_embed, _) = build_page_data(page, total_pages, &settings_list);
                let _ = reply
                    .edit(
                        ctx,
                        poise::CreateReply::default()
                            .embed(final_embed)
                            .components(vec![]),
                    )
                    .await;
            }
        }
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

/// Whether the invoking user is a server admin: the guild owner, or a member
/// with the Administrator permission.
///
/// Permissions are computed from the member's roles via `member_permissions`,
/// which works for both prefix and slash commands — unlike the `Member`
/// `permissions` field, which Discord only populates for slash interactions.
async fn is_server_admin(ctx: Context<'_>) -> bool {
    // Server owner is always an admin.
    if ctx
        .guild()
        .map(|g| g.owner_id == ctx.author().id)
        .unwrap_or(false)
    {
        return true;
    }

    match ctx.author_member().await {
        Some(member) => ctx
            .guild()
            .map(|guild| guild.member_permissions(&member).administrator())
            .unwrap_or(false),
        None => false,
    }
}

/// Whether the invoking user may run the privileged `user` subcommands: a server
/// admin, or a user with the ADMIN role in the database.
async fn is_user_admin(ctx: Context<'_>) -> bool {
    if is_server_admin(ctx).await {
        return true;
    }
    db::get_user(&ctx.data().db_pool, &ctx.author().id.to_string())
        .await
        .map(|u| u.role == "ADMIN")
        .unwrap_or(false)
}

/// User and database management commands
#[poise::command(
    slash_command,
    prefix_command,
    rename = "user",
    aliases("users"),
    subcommands("authorize", "sync", "latest_message", "inthards")
)]
pub async fn user_cmd(ctx: Context<'_>) -> Result<(), Error> {
    ctx.say("Available subcommands: `authorize`, `sync`, `latest_message`, `inthards`.")
        .await?;
    Ok(())
}

/// Toggle a user's authorized status (admin only)
#[poise::command(slash_command, prefix_command)]
pub async fn authorize(
    ctx: Context<'_>,
    #[description = "The user to toggle authorization for"] user: serenity::User,
) -> Result<(), Error> {
    if !is_user_admin(ctx).await {
        return Err(user_error("Unauthorized"));
    }

    let pool = &ctx.data().db_pool;
    let target_id = user.id.to_string();
    db::create_user_if_not_exists(pool, &target_id).await?;
    sqlx::query("UPDATE users SET authorized = NOT authorized WHERE discord_id = ?")
        .bind(&target_id)
        .execute(pool)
        .await?;

    let target = db::get_user(pool, &target_id)
        .await
        .ok_or("Failed to load user after update")?;
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
    Ok(())
}

/// Sync guild members with the database (admin only)
#[poise::command(slash_command, prefix_command)]
pub async fn sync(ctx: Context<'_>) -> Result<(), Error> {
    if !is_user_admin(ctx).await {
        return Err(user_error("Unauthorized"));
    }

    let pool = &ctx.data().db_pool;
    let guild_id = ctx
        .guild_id()
        .ok_or_else(|| user_error("Must be run in a guild"))?;
    let mut synced_count = 0;

    let allowed_bot_id = {
        let cache = ctx.data().settings_cache.read().await;
        cache.get("allowed_bot_id").cloned()
    };

    let members = guild_id.members(ctx.http(), None, None).await?;
    for member in members {
        let is_allowed_bot = allowed_bot_id.as_deref() == Some(&member.user.id.to_string());

        if !member.user.bot || is_allowed_bot {
            let _ = db::create_user_if_not_exists(pool, &member.user.id.to_string()).await;
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
    Ok(())
}

/// Show a user's most recent message in the main channel (admin only)
#[poise::command(slash_command, prefix_command)]
pub async fn latest_message(
    ctx: Context<'_>,
    #[description = "The user to look up"] user: serenity::User,
) -> Result<(), Error> {
    if !is_user_admin(ctx).await {
        return Err(user_error("Unauthorized"));
    }

    let target_id = user.id.get();

    let channel_id_str = {
        let cache = ctx.data().settings_cache.read().await;
        cache
            .get("main_text_channel")
            .cloned()
            .ok_or_else(|| user_error("main_text_channel setting not found"))?
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
    Ok(())
}

/// Show the most inactive users
#[poise::command(slash_command, prefix_command)]
pub async fn inthards(
    ctx: Context<'_>,
    #[description = "How many users to show (default 5)"] count: Option<i64>,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let top_x = count.unwrap_or(5).unsigned_abs() as i64;

    let users: Vec<db::User> =
        sqlx::query_as::<_, db::User>("SELECT * FROM users ORDER BY last_message ASC LIMIT ?")
            .bind(top_x)
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
    Ok(())
}

/// Render a user's stored facts as an ephemeral embed.
async fn reply_facts(ctx: Context<'_>, discord_id: &str, self_view: bool) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let opted_out = db::is_opted_out(pool, discord_id).await;
    let facts = db::get_facts(pool, discord_id).await;

    let mut embed = create_embed("Memory");
    if opted_out {
        embed = embed.description(if self_view {
            "You are opted out of memory — nothing is stored about you. Use `optin` to re-enable."
        } else {
            "This user is opted out of memory."
        });
    } else if facts.is_empty() {
        embed = embed.description("No memories stored.");
    } else {
        let body = facts
            .iter()
            .enumerate()
            .map(|(i, f)| format!("{}. {}", i + 1, f.fact))
            .collect::<Vec<_>>()
            .join("\n");
        embed = embed.description(truncate_str(&body, 4000).into_owned());
        if self_view {
            embed = embed.footer(CreateEmbedFooter::new(
                "forget <number> deletes one · forget all clears them",
            ));
        }
    }
    ctx.send(poise::CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}

/// View or manage what the bot remembers about you
#[poise::command(
    slash_command,
    prefix_command,
    subcommands(
        "memory_view",
        "memory_forget_slash",
        "memory_forget_prefix",
        "memory_clear",
        "memory_optout",
        "memory_optin",
        "memory_insert",
        "memory_add"
    )
)]
pub async fn memory(ctx: Context<'_>) -> Result<(), Error> {
    reply_facts(ctx, &ctx.author().id.to_string(), true).await
}

/// View stored facts (yours, or another user's if you're an admin)
#[poise::command(slash_command, prefix_command, rename = "view")]
pub async fn memory_view(
    ctx: Context<'_>,
    #[description = "User to view (admin only)"] user: Option<serenity::User>,
) -> Result<(), Error> {
    match user {
        Some(u) => {
            if !is_user_admin(ctx).await {
                return Err(user_error(
                    "You are not authorized to view others' memories.",
                ));
            }
            reply_facts(ctx, &u.id.to_string(), false).await
        }
        None => reply_facts(ctx, &ctx.author().id.to_string(), true).await,
    }
}

async fn execute_forget(
    ctx: Context<'_>,
    target_user_id: &str,
    target_user_name: &str,
    target_is_self: bool,
    target_spec: &str,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let target_spec = target_spec.trim();

    if target_spec.eq_ignore_ascii_case("all") {
        let n = db::clear_facts(pool, target_user_id).await?;
        let msg = if target_is_self {
            format!("Cleared {} memory item(s).", n)
        } else {
            format!("Cleared {} memory item(s) for {}.", n, target_user_name)
        };
        ctx.send(
            poise::CreateReply::default()
                .content(msg)
                .ephemeral(true)
                .allowed_mentions(serenity::builder::CreateAllowedMentions::new().empty_users()),
        )
        .await?;
        return Ok(());
    }

    let facts = db::get_facts(pool, target_user_id).await;
    if facts.is_empty() {
        let msg = if target_is_self {
            "You have no memories stored."
        } else {
            "This user has no memories stored."
        };
        return Err(user_error(msg));
    }

    let indices = memory::parse_fact_indices(target_spec, facts.len()).map_err(user_error)?;

    let to_delete: Vec<&db::UserFact> = indices.iter().map(|&idx| &facts[idx - 1]).collect();
    let ids: Vec<i64> = to_delete.iter().map(|f| f.id).collect();
    let fact_texts: Vec<String> = to_delete.iter().map(|f| f.fact.clone()).collect();

    let _ = db::delete_facts_by_ids(pool, target_user_id, &ids).await?;

    let msg = if target_is_self {
        if fact_texts.len() == 1 {
            format!("Forgot: {}", fact_texts[0])
        } else {
            let list = fact_texts
                .iter()
                .map(|t| format!("• {}", t))
                .collect::<Vec<_>>()
                .join("\n");
            format!("Forgot {} items:\n{}", fact_texts.len(), list)
        }
    } else {
        if fact_texts.len() == 1 {
            format!("Forgot for {}: \"{}\"", target_user_name, fact_texts[0])
        } else {
            let list = fact_texts
                .iter()
                .map(|t| format!("• {}", t))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "Forgot {} item(s) for {}:\n{}",
                fact_texts.len(),
                target_user_name,
                list
            )
        }
    };

    ctx.send(
        poise::CreateReply::default()
            .content(msg)
            .ephemeral(true)
            .allowed_mentions(serenity::builder::CreateAllowedMentions::new().empty_users()),
    )
    .await?;

    Ok(())
}

/// Forget stored facts by number (e.g. 1, 1 2, 2-4), or all of them
#[poise::command(slash_command, rename = "forget")]
pub async fn memory_forget_slash(
    ctx: Context<'_>,
    #[description = "'all', or numbers from view (e.g. '1', '1, 2', '2-4')"] target: String,
    #[description = "User whose facts to forget (admin only)"] user: Option<serenity::User>,
) -> Result<(), Error> {
    let (target_id, target_name, target_is_self) = match user {
        Some(u) => {
            if !is_user_admin(ctx).await {
                return Err(user_error(
                    "You are not authorized to forget others' memories.",
                ));
            }
            let is_self = u.id == ctx.author().id;
            (u.id.to_string(), u.name, is_self)
        }
        None => (ctx.author().id.to_string(), ctx.author().name.clone(), true),
    };

    execute_forget(ctx, &target_id, &target_name, target_is_self, &target).await
}

/// Forget stored facts by number (e.g. 1, 1 2, 2-4), or all of them
#[poise::command(prefix_command, rename = "forget")]
pub async fn memory_forget_prefix(
    ctx: Context<'_>,
    #[rest]
    #[description = "'all', or numbers like '1', '1, 2', '1 2 3', optionally with user if admin"]
    input: Option<String>,
) -> Result<(), Error> {
    let raw = input.unwrap_or_default();
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(user_error(
            "Usage: `forget <number(s)>` (e.g. `forget 1` or `forget 1, 2`) or `forget all`.",
        ));
    }

    let is_admin = is_user_admin(ctx).await;
    let tokens: Vec<&str> = raw.split_whitespace().collect();

    // Check if the first or last token is a user specification (admin only)
    if is_admin && tokens.len() >= 2 {
        // Try first token as user
        if let Ok((uid, name)) = resolve_user(ctx, tokens[0]).await {
            let spec = tokens[1..].join(" ");
            let is_self = uid == ctx.author().id.to_string();
            return execute_forget(ctx, &uid, &name, is_self, &spec).await;
        }
        // Try last token as user
        let last_idx = tokens.len() - 1;
        if let Ok((uid, name)) = resolve_user(ctx, tokens[last_idx]).await {
            let spec = tokens[..last_idx].join(" ");
            let is_self = uid == ctx.author().id.to_string();
            return execute_forget(ctx, &uid, &name, is_self, &spec).await;
        }
    }

    let self_id = ctx.author().id.to_string();
    let self_name = ctx.author().name.clone();
    execute_forget(ctx, &self_id, &self_name, true, raw).await
}

/// Clear all of a user's stored facts (admin only)
#[poise::command(slash_command, prefix_command, rename = "clear")]
pub async fn memory_clear(
    ctx: Context<'_>,
    #[description = "The user whose memory to clear"] user: serenity::User,
) -> Result<(), Error> {
    if !is_user_admin(ctx).await {
        return Err(user_error("Unauthorized"));
    }
    let n = db::clear_facts(&ctx.data().db_pool, &user.id.to_string()).await?;
    ctx.say(format!("Cleared {} memory item(s) for <@{}>.", n, user.id))
        .await?;
    Ok(())
}

/// Opt out of memory: clear your facts and stop remembering anything about you
#[poise::command(slash_command, prefix_command, rename = "optout")]
pub async fn memory_optout(ctx: Context<'_>) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let user_id = ctx.author().id.to_string();
    db::create_user_if_not_exists(pool, &user_id).await?;
    db::set_opt_out(pool, &user_id, true).await?;
    let n = db::clear_facts(pool, &user_id).await?;
    ctx.say(format!(
        "You are now opted out of memory. Cleared {} stored item(s); nothing new will be remembered.",
        n
    ))
    .await?;
    Ok(())
}

/// Opt back in to memory
#[poise::command(slash_command, prefix_command, rename = "optin")]
pub async fn memory_optin(ctx: Context<'_>) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let user_id = ctx.author().id.to_string();
    db::create_user_if_not_exists(pool, &user_id).await?;
    db::set_opt_out(pool, &user_id, false).await?;
    ctx.say("You are opted back in to memory.").await?;
    Ok(())
}

async fn resolve_user(ctx: Context<'_>, query: &str) -> Result<(String, String), Error> {
    let query = query.trim();
    // 1. Check if it's a mention <@!12345> or <@12345>
    if let Some(id_str) = query.strip_prefix("<@").and_then(|s| s.strip_suffix('>')) {
        let id_str = id_str.strip_prefix('!').unwrap_or(id_str);
        if let Ok(id_u64) = id_str.parse::<u64>() {
            let user_id = serenity::UserId::new(id_u64);
            if let Ok(u) = user_id.to_user(&ctx).await {
                return Ok((u.id.to_string(), u.name));
            }
        }
    }
    // 2. Check if it's a numeric ID
    if let Ok(id_u64) = query.parse::<u64>() {
        let user_id = serenity::UserId::new(id_u64);
        if let Ok(u) = user_id.to_user(&ctx).await {
            return Ok((u.id.to_string(), u.name));
        }
    }
    // 3. Search members in guild cache by username, global name, or nickname
    if let Some(guild) = ctx.guild() {
        let q_lower = query.to_lowercase();
        for member in guild.members.values() {
            let name_match = member.user.name.to_lowercase() == q_lower;
            let nick_match = member
                .nick
                .as_deref()
                .map(|n| n.to_lowercase() == q_lower)
                .unwrap_or(false);
            let global_match = member
                .user
                .global_name
                .as_deref()
                .map(|g| g.to_lowercase() == q_lower)
                .unwrap_or(false);
            if name_match || nick_match || global_match {
                return Ok((member.user.id.to_string(), member.user.name.clone()));
            }
        }
        for member in guild.members.values() {
            let name_match = member.user.name.to_lowercase().contains(&q_lower);
            let nick_match = member
                .nick
                .as_deref()
                .map(|n| n.to_lowercase().contains(&q_lower))
                .unwrap_or(false);
            let global_match = member
                .user
                .global_name
                .as_deref()
                .map(|g| g.to_lowercase().contains(&q_lower))
                .unwrap_or(false);
            if name_match || nick_match || global_match {
                return Ok((member.user.id.to_string(), member.user.name.clone()));
            }
        }
    }
    // 4. Search via HTTP
    if let Some(guild_id) = ctx.guild_id() {
        if let Ok(members) = guild_id.search_members(ctx.http(), query, Some(5)).await {
            if let Some(m) = members.into_iter().next() {
                return Ok((m.user.id.to_string(), m.user.name));
            }
        }
    }

    Err(user_error(format!(
        "Could not find user matching '{}'.",
        query
    )))
}

async fn handle_memory_insert(
    ctx: Context<'_>,
    user_id: &str,
    user_name: &str,
    is_bot: bool,
    fact: &str,
) -> Result<(), Error> {
    if !is_user_admin(ctx).await {
        return Err(user_error("Unauthorized"));
    }
    if is_bot {
        return Err(user_error("Cannot add memories for bot accounts."));
    }

    let pool = &ctx.data().db_pool;

    if db::is_opted_out(pool, user_id).await {
        return Err(user_error(format!("{} is opted out of memory.", user_name)));
    }

    let fact_trimmed = fact.trim();
    if fact_trimmed.is_empty() {
        return Err(user_error("Fact cannot be empty."));
    }
    if fact_trimmed.len() > 1000 {
        return Err(user_error("Fact must be under 1000 characters."));
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    db::create_user_if_not_exists(pool, user_id).await?;
    let inserted = db::add_fact(pool, user_id, fact_trimmed, now_ms).await?;

    let max_facts = db::get_setting(pool, "memory_max_facts_per_user")
        .await
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(20);
    let _ = db::prune_facts(pool, user_id, max_facts).await;

    let content = if inserted {
        format!("Remembered for {}: \"{}\"", user_name, fact_trimmed)
    } else {
        format!("That fact is already remembered for {}.", user_name)
    };

    ctx.send(
        poise::CreateReply::default()
            .content(content)
            .ephemeral(true)
            .allowed_mentions(serenity::builder::CreateAllowedMentions::new().empty_users()),
    )
    .await?;

    Ok(())
}

/// Insert a fact for a user without notifying them (admin only)
#[poise::command(prefix_command, hide_in_help, rename = "insert")]
pub async fn memory_insert(
    ctx: Context<'_>,
    #[description = "The user (name, nickname, mention, or ID)"] target: String,
    #[rest]
    #[description = "The fact to remember about the user"]
    fact: String,
) -> Result<(), Error> {
    let (uid, name) = resolve_user(ctx, &target).await?;
    let is_bot = ctx
        .http()
        .get_user(serenity::UserId::new(uid.parse().unwrap_or(0)))
        .await
        .map(|u| u.bot)
        .unwrap_or(false);
    handle_memory_insert(ctx, &uid, &name, is_bot, &fact).await
}

/// Add a fact for a user without notifying them (admin only)
#[poise::command(prefix_command, hide_in_help, rename = "add")]
pub async fn memory_add(
    ctx: Context<'_>,
    #[description = "The user (name, nickname, mention, or ID)"] target: String,
    #[rest]
    #[description = "The fact to remember about the user"]
    fact: String,
) -> Result<(), Error> {
    let (uid, name) = resolve_user(ctx, &target).await?;
    let is_bot = ctx
        .http()
        .get_user(serenity::UserId::new(uid.parse().unwrap_or(0)))
        .await
        .map(|u| u.bot)
        .unwrap_or(false);
    handle_memory_insert(ctx, &uid, &name, is_bot, &fact).await
}

/// Remember a fact about a user without notifying them (admin only)
#[poise::command(
    slash_command,
    default_member_permissions = "ADMINISTRATOR",
    hide_in_help,
    rename = "remember"
)]
pub async fn remember_cmd(
    ctx: Context<'_>,
    #[description = "The user to remember a fact about"] user: serenity::User,
    #[rest]
    #[description = "The fact to remember about the user"]
    fact: String,
) -> Result<(), Error> {
    handle_memory_insert(ctx, &user.id.to_string(), &user.name, user.bot, &fact).await
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
        return Err(user_error(
            "Only the server owner or authorized administrators can register commands.",
        ));
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
        "tldr_cooldown_seconds",
        "ai_initial_prompt",
        "ai_chat_model",
        "bot_status_type",
        "bot_status_message",
        "allowed_bot_id",
        "ai_debug",
        "reminder_pings",
        "random_chime_chance",
        "random_chime_cooldown_seconds",
        "random_chime_daily_cap",
        "random_chime_prompt",
        "random_chime_model",
        "random_chime_relevance_check",
        "memory_enabled",
        "memory_extraction_interval_seconds",
        "memory_max_facts_per_user",
        "memory_model",
    ];
    keys.into_iter()
        .filter(move |name| name.to_lowercase().starts_with(&partial.to_lowercase()))
        .map(|name| name.to_string())
        .collect()
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

async fn autocomplete_settings_value(ctx: Context<'_>, partial: &str) -> Vec<String> {
    let mut key = None;
    if let poise::Context::Application(app_ctx) = ctx {
        for option in &app_ctx.interaction.data.options {
            // In slash subcommands, options might be nested under the subcommand
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

    let recommendations = match key.as_deref() {
        Some("reminder_pings") | Some("ai_debug") => vec!["true", "false"],
        Some("bot_status_type") => vec!["playing", "watching", "listening", "competing", "custom"],
        Some("ai_chat_model") => vec![
            "gpt-4o-mini",
            "gpt-4o",
            "gpt-5.1",
            "o1",
            "o1-mini",
            "o3-mini",
        ],
        Some("ai_max_tokens") | Some("memory_max_tokens") => {
            vec!["150", "300", "500", "1000", "2000"]
        }
        _ => vec![],
    };

    recommendations
        .into_iter()
        .filter(move |val| val.to_lowercase().starts_with(&partial.to_lowercase()))
        .map(|val| val.to_string())
        .collect()
}

fn format_tokens(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

/// View API token usage leaderboard.
#[poise::command(
    slash_command,
    prefix_command,
    rename = "usage_leaderboard",
    guild_only
)]
pub async fn usage_leaderboard(ctx: Context<'_>) -> Result<(), Error> {
    if !is_server_admin(ctx).await {
        return Err(user_error(
            "Only server administrators can view the usage leaderboard.",
        ));
    }

    let usage_list = db::get_usage_leaderboard(&ctx.data().db_pool, 10).await?;

    if usage_list.is_empty() {
        let embed = create_embed("API Usage Leaderboard").description("No API usage recorded yet.");
        ctx.send(poise::CreateReply::default().embed(embed)).await?;
        return Ok(());
    }

    let mut lines = Vec::new();
    for (i, row) in usage_list.iter().enumerate() {
        let total = row.chat_tokens + row.memory_tokens;
        let formatted_total = format_tokens(total);
        let formatted_chat = format_tokens(row.chat_tokens);
        let formatted_mem = format_tokens(row.memory_tokens);

        let restricted_flag = if row.is_restricted {
            " 🚫 [Restricted]"
        } else {
            ""
        };

        lines.push(format!(
            "{}. <@{}> - Total: {} (Chat: {} | Mem: {}){}",
            i + 1,
            row.user_id,
            formatted_total,
            formatted_chat,
            formatted_mem,
            restricted_flag
        ));
    }

    let embed = create_embed("API Usage Leaderboard").description(lines.join("\n"));
    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

/// Restrict or unrestrict a user from accessing AI features.
#[poise::command(slash_command, prefix_command, rename = "restrict", guild_only)]
pub async fn restrict(
    ctx: Context<'_>,
    #[description = "The target user to restrict or unrestrict"] target: serenity::User,
    #[description = "Set to true to restrict, false to unrestrict"] restricted: bool,
) -> Result<(), Error> {
    if !is_server_admin(ctx).await {
        return Err(user_error(
            "Only server administrators can restrict users from using AI.",
        ));
    }

    let user_id = target.id.to_string();
    db::set_user_restricted(&ctx.data().db_pool, &user_id, restricted).await?;

    let action_str = if restricted {
        "restricted from"
    } else {
        "unrestricted for"
    };
    ctx.send(
        poise::CreateReply::default()
            .content(format!(
                "User <@{}> has been {} AI access.",
                user_id, action_str
            ))
            .ephemeral(true),
    )
    .await?;

    Ok(())
}

async fn autocomplete_tag_name(ctx: Context<'_>, partial: &str) -> Vec<String> {
    let server_id = ctx
        .guild_id()
        .map(|g| g.to_string())
        .unwrap_or_else(|| "182937695515967488".to_string());
    db::search_tag_names(&ctx.data().db_pool, &server_id, partial, 25).await
}

pub fn get_tag_assets_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("TAG_ASSETS_DIR") {
        std::path::PathBuf::from(dir)
    } else if std::path::Path::new("/healthybot/data").exists() {
        std::path::PathBuf::from("/healthybot/data/tag_assets")
    } else {
        std::path::PathBuf::from("data/tag_assets")
    }
}

async fn download_discord_cdn_url(url: &str, tag_id: &str) -> Option<String> {
    let client = reqwest::Client::new();
    let resp = client
        .get(url)
        .header(
            reqwest::header::USER_AGENT,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64)",
        )
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let bytes = resp.bytes().await.ok()?;

    let path_part = url.split('?').next().unwrap_or(url);
    let ext = path_part
        .split('.')
        .next_back()
        .unwrap_or("png")
        .split('/')
        .next()
        .unwrap_or("png");
    let ext = if ext.len() > 5 || ext.is_empty() {
        "png"
    } else {
        ext
    };

    let filename = format!("{}.{}", tag_id, ext);
    let target_path = get_tag_assets_dir().join(&filename);

    tokio::fs::create_dir_all(get_tag_assets_dir()).await.ok()?;
    tokio::fs::write(&target_path, &bytes).await.ok()?;

    Some(filename)
}

async fn extract_and_download_discord_cdn_asset(
    content: &str,
    tag_id: &str,
) -> (String, Option<String>) {
    // 1. Check for {attach:https://cdn.discordapp.com/attachments/...} or media.discordapp.net
    if let Ok(attach_re) = regex::Regex::new(
        r"\{attach:(https?://(?:cdn\.discordapp\.com|media\.discordapp\.net)/attachments/[^}]+)\}",
    ) {
        if let Some(caps) = attach_re.captures(content) {
            if let Some(m) = caps.get(1) {
                if let Some(filename) = download_discord_cdn_url(m.as_str(), tag_id).await {
                    let cleaned = attach_re.replace(content, "").trim().to_string();
                    return (cleaned, Some(filename));
                }
            }
        }
    }

    // 2. Check for bare Discord CDN attachment URL in content
    if let Ok(url_re) = regex::Regex::new(
        r"https?://(?:cdn\.discordapp\.com|media\.discordapp\.net)/attachments/[^\s<>]+",
    ) {
        if let Some(m) = url_re.find(content) {
            if let Some(filename) = download_discord_cdn_url(m.as_str(), tag_id).await {
                let cleaned = url_re.replace(content, "").trim().to_string();
                return (cleaned, Some(filename));
            }
        }
    }

    (content.to_string(), None)
}

async fn send_tag(
    ctx: Context<'_>,
    tag: &db::Tag,
    args: &[String],
    is_random: bool,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let _ = db::increment_tag_uses(pool, &tag.id, Utc::now().timestamp_millis()).await;

    let tag_ctx = tags::TagContext {
        author_mention: format!("<@{}>", ctx.author().id),
        author_name: ctx.author().name.clone(),
        args,
    };
    let evaluated = tags::evaluate_tag(&tag.content, &tag_ctx, tag.asset_filename.as_deref());

    let attachment = if let Some(filename) = &evaluated.asset_filename {
        let asset_path = get_tag_assets_dir().join(filename);
        if asset_path.exists() {
            match serenity::all::CreateAttachment::path(&asset_path).await {
                Ok(att) => Some(att),
                Err(e) => {
                    log::error!("Failed to attach file {:?}: {}", asset_path, e);
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    let text = if is_random {
        if evaluated.text.is_empty() {
            format!("Showing tag: {}", tag.name)
        } else {
            format!("Showing tag: {}\n\n{}", tag.name, evaluated.text)
        }
    } else {
        evaluated.text
    };

    if text.len() <= 2000 {
        let mut reply = poise::CreateReply::default();
        if !text.is_empty() {
            reply = reply.content(&text);
        }
        if let Some(att) = attachment {
            reply = reply.attachment(att);
        }
        ctx.send(reply).await?;
    } else {
        let chunks = split_message(&text, 2000);
        let mut first = true;
        for chunk in chunks {
            let mut reply = poise::CreateReply::default().content(chunk);
            if first {
                if let Some(att) = attachment.clone() {
                    reply = reply.attachment(att);
                }
                first = false;
            }
            ctx.send(reply).await?;
        }
    }

    Ok(())
}

async fn handle_tag_create_prefix(
    ctx: Context<'_>,
    server_id: &str,
    args: &[String],
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let Some(tag_name) = args.first() else {
        return Err(user_error(
            "Usage: `!t add <name> [content]` (or attach a file)",
        ));
    };

    if tag_name.len() > 100 {
        return Err(user_error("Tag name is too long (max 100 characters)."));
    }

    if tag_name.chars().any(char::is_whitespace) {
        return Err(user_error(
            "Tag names must be a single word without spaces.",
        ));
    }

    if db::get_tag(pool, server_id, tag_name).await.is_some() {
        return Err(user_error(format!(
            "A tag named '{}' already exists.",
            tag_name
        )));
    }

    let raw_content = if args.len() > 1 {
        args[1..].join(" ")
    } else {
        String::new()
    };

    let tag_id = Utc::now().timestamp_nanos_opt().unwrap_or(0).to_string();
    let mut asset_filename = None;

    if let poise::Context::Prefix(p_ctx) = ctx {
        if let Some(attachment) = p_ctx.msg.attachments.first() {
            let ext = attachment.filename.split('.').next_back().unwrap_or("png");
            let filename = format!("{}.{}", tag_id, ext);
            let target_path = get_tag_assets_dir().join(&filename);

            if let Ok(bytes) = attachment.download().await {
                tokio::fs::create_dir_all(get_tag_assets_dir()).await.ok();
                if tokio::fs::write(&target_path, &bytes).await.is_ok() {
                    asset_filename = Some(filename);
                }
            }
        }
    }

    let (content, cdn_asset) = if asset_filename.is_none() {
        extract_and_download_discord_cdn_asset(&raw_content, &tag_id).await
    } else {
        (raw_content, None)
    };
    if let Some(a) = cdn_asset {
        asset_filename = Some(a);
    }

    if content.is_empty() && asset_filename.is_none() {
        return Err(user_error("Tag must have content or an attachment."));
    }

    let new_tag = db::Tag {
        id: tag_id,
        server_id: server_id.to_string(),
        name: tag_name.clone(),
        name_normalized: tag_name.trim().to_lowercase(),
        content,
        asset_filename,
        author_id: ctx.author().id.to_string(),
        author_name: ctx.author().name.clone(),
        created_at: Utc::now().timestamp_millis(),
        last_used_at: None,
        uses: 0,
    };

    db::create_tag(pool, &new_tag).await?;
    ctx.say(format!("Tag '{}' created!", tag_name)).await?;
    Ok(())
}

async fn handle_tag_edit_prefix(
    ctx: Context<'_>,
    server_id: &str,
    args: &[String],
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let Some(tag_name) = args.first() else {
        return Err(user_error(
            "Usage: `!t edit <name> [new_content]` (or attach a file)",
        ));
    };

    let Some(tag) = db::get_tag(pool, server_id, tag_name).await else {
        return Err(user_error(format!("Tag '{}' not found.", tag_name)));
    };

    let is_admin = is_server_admin(ctx).await;
    let is_author = tag.author_id == ctx.author().id.to_string();
    if !is_admin && !is_author {
        return Err(user_error("You do not have permission to edit this tag (only the author or a server admin can edit it)."));
    }

    let mut new_asset_filename = tag.asset_filename.clone();
    let mut uploaded_file = false;

    if let poise::Context::Prefix(p_ctx) = ctx {
        if let Some(attachment) = p_ctx.msg.attachments.first() {
            let ext = attachment.filename.split('.').next_back().unwrap_or("png");
            let filename = format!("{}_{}.{}", tag.id, Utc::now().timestamp_millis(), ext);
            let target_path = get_tag_assets_dir().join(&filename);

            if let Ok(bytes) = attachment.download().await {
                tokio::fs::create_dir_all(get_tag_assets_dir()).await.ok();
                if tokio::fs::write(&target_path, &bytes).await.is_ok() {
                    new_asset_filename = Some(filename);
                    uploaded_file = true;
                }
            }
        }
    }

    let raw_content = if args.len() > 1 {
        args[1..].join(" ")
    } else {
        tag.content.clone()
    };

    let (new_content, cdn_asset) = if !uploaded_file {
        extract_and_download_discord_cdn_asset(&raw_content, &tag.id).await
    } else {
        (raw_content, None)
    };
    if let Some(a) = cdn_asset {
        new_asset_filename = Some(a);
    }

    if new_content.is_empty() && new_asset_filename.is_none() {
        return Err(user_error("Tag must have content or an attachment."));
    }

    db::update_tag(pool, &tag.id, &new_content, new_asset_filename.as_deref()).await?;
    ctx.say(format!("Tag '{}' updated!", tag.name)).await?;
    Ok(())
}

async fn handle_tag_rename(
    ctx: Context<'_>,
    server_id: &str,
    args: &[String],
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    if args.len() < 2 {
        return Err(user_error("Usage: `!t rename <old_name> <new_name>`"));
    }
    let old_name = &args[0];
    let new_name = &args[1];

    if new_name.len() > 100 {
        return Err(user_error("Tag name is too long (max 100 characters)."));
    }

    if new_name.chars().any(char::is_whitespace) {
        return Err(user_error(
            "Tag names must be a single word without spaces.",
        ));
    }

    let Some(tag) = db::get_tag(pool, server_id, old_name).await else {
        return Err(user_error(format!("Tag '{}' not found.", old_name)));
    };

    let is_admin = is_server_admin(ctx).await;
    let is_author = tag.author_id == ctx.author().id.to_string();
    if !is_admin && !is_author {
        return Err(user_error("You do not have permission to rename this tag (only the author or a server admin can rename it)."));
    }

    if old_name.trim().to_lowercase() != new_name.trim().to_lowercase()
        && db::get_tag(pool, server_id, new_name).await.is_some()
    {
        return Err(user_error(format!(
            "A tag named '{}' already exists.",
            new_name
        )));
    }

    db::rename_tag(pool, &tag.id, new_name).await?;
    ctx.say(format!("Tag '{}' renamed to '{}'.", tag.name, new_name))
        .await?;
    Ok(())
}

async fn handle_tag_alias(ctx: Context<'_>, server_id: &str, args: &[String]) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    if args.len() < 2 {
        return Err(user_error("Usage: `!t alias <existing_name> <alias_name>`"));
    }
    let existing_name = &args[0];
    let alias_name = &args[1];

    if alias_name.len() > 100 {
        return Err(user_error(
            "Alias tag name is too long (max 100 characters).",
        ));
    }

    if alias_name.chars().any(char::is_whitespace) {
        return Err(user_error(
            "Tag names must be a single word without spaces.",
        ));
    }

    let Some(existing_tag) = db::get_tag(pool, server_id, existing_name).await else {
        return Err(user_error(format!("Tag '{}' not found.", existing_name)));
    };

    if db::get_tag(pool, server_id, alias_name).await.is_some() {
        return Err(user_error(format!(
            "A tag named '{}' already exists.",
            alias_name
        )));
    }

    let tag_id = Utc::now().timestamp_nanos_opt().unwrap_or(0).to_string();
    let alias_tag = db::Tag {
        id: tag_id,
        server_id: server_id.to_string(),
        name: alias_name.to_string(),
        name_normalized: alias_name.trim().to_lowercase(),
        content: existing_tag.content.clone(),
        asset_filename: existing_tag.asset_filename.clone(),
        author_id: ctx.author().id.to_string(),
        author_name: ctx.author().name.clone(),
        created_at: Utc::now().timestamp_millis(),
        last_used_at: None,
        uses: 0,
    };

    db::create_tag(pool, &alias_tag).await?;
    ctx.say(format!(
        "Created alias '{}' for tag '{}'.",
        alias_name, existing_tag.name
    ))
    .await?;
    Ok(())
}

async fn handle_tag_delete(ctx: Context<'_>, server_id: &str, tag_name: &str) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let Some(tag) = db::get_tag(pool, server_id, tag_name).await else {
        return Err(user_error(format!("Tag '{}' not found.", tag_name)));
    };

    let is_admin = is_server_admin(ctx).await;
    let is_author = tag.author_id == ctx.author().id.to_string();

    if !is_admin && !is_author {
        return Err(user_error(
            "You do not have permission to delete this tag (only the author or a server admin can delete it).",
        ));
    }

    // If deleting someone else's tag, require admin confirmation via buttons
    if !is_author {
        let embed = CreateEmbed::new()
            .title("⚠️ Warning: Force Delete Tag")
            .description(format!(
                "Are you sure you want to forcefully delete tag **{}**?\n\n**Owner:** <@{}> ({})\n**Uses:** {}",
                tag.name, tag.author_id, tag.author_name, tag.uses
            ))
            .color(0xFF4444)
            .footer(CreateEmbedFooter::new("Healthy Bot"));

        let components = vec![serenity::CreateActionRow::Buttons(vec![
            serenity::CreateButton::new("confirm_force_delete")
                .label("Force Delete")
                .style(serenity::ButtonStyle::Danger),
            serenity::CreateButton::new("cancel_force_delete")
                .label("Cancel")
                .style(serenity::ButtonStyle::Secondary),
        ])];

        let reply = ctx
            .send(
                poise::CreateReply::default()
                    .embed(embed)
                    .components(components),
            )
            .await?;

        let mut collector = reply
            .message()
            .await?
            .await_component_interactions(ctx)
            .author_id(ctx.author().id)
            .timeout(std::time::Duration::from_secs(60))
            .stream();

        if let Some(interaction) = collector.next().await {
            if interaction.data.custom_id == "confirm_force_delete" {
                if let Some(filename) = &tag.asset_filename {
                    if !db::is_asset_used_elsewhere(pool, filename, &tag.id)
                        .await
                        .unwrap_or(true)
                    {
                        let p = get_tag_assets_dir().join(filename);
                        let _ = tokio::fs::remove_file(p).await;
                    }
                }
                db::delete_tag(pool, &tag.id).await?;
                let _ = interaction
                    .create_response(
                        ctx,
                        serenity::CreateInteractionResponse::UpdateMessage(
                            serenity::CreateInteractionResponseMessage::new()
                                .content(format!("Tag '{}' was forcefully deleted.", tag.name))
                                .embeds(vec![])
                                .components(vec![]),
                        ),
                    )
                    .await;
            } else {
                let _ = interaction
                    .create_response(
                        ctx,
                        serenity::CreateInteractionResponse::UpdateMessage(
                            serenity::CreateInteractionResponseMessage::new()
                                .content(format!("Deletion of tag '{}' cancelled.", tag.name))
                                .embeds(vec![])
                                .components(vec![]),
                        ),
                    )
                    .await;
            }
        } else {
            let _ = reply
                .edit(
                    ctx,
                    poise::CreateReply::default()
                        .content(format!("Deletion of tag '{}' timed out.", tag.name))
                        .components(vec![]),
                )
                .await;
        }

        return Ok(());
    }

    if let Some(filename) = &tag.asset_filename {
        if !db::is_asset_used_elsewhere(pool, filename, &tag.id)
            .await
            .unwrap_or(true)
        {
            let p = get_tag_assets_dir().join(filename);
            let _ = tokio::fs::remove_file(p).await;
        }
    }

    db::delete_tag(pool, &tag.id).await?;
    ctx.say(format!("Tag '{}' deleted.", tag.name)).await?;
    Ok(())
}

async fn handle_tag_info(ctx: Context<'_>, server_id: &str, tag_name: &str) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let Some(tag) = db::get_tag(pool, server_id, tag_name).await else {
        return Err(user_error(format!("Tag '{}' not found.", tag_name)));
    };

    let created_dt = chrono::DateTime::from_timestamp_millis(tag.created_at)
        .map(|dt| {
            dt.with_timezone(&Amsterdam)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "Unknown".to_string());

    let last_used_dt = tag
        .last_used_at
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|dt| {
            dt.with_timezone(&Amsterdam)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "Never".to_string());

    let embed = CreateEmbed::new()
        .title(format!("Tag: {}", tag.name))
        .field(
            "Author",
            format!("<@{}> ({})", tag.author_id, tag.author_name),
            true,
        )
        .field("Uses", tag.uses.to_string(), true)
        .field(
            "Has Local Media",
            if tag.asset_filename.is_some() {
                "Yes"
            } else {
                "No"
            },
            true,
        )
        .field("Created", created_dt, true)
        .field("Last Used", last_used_dt, true)
        .field("Content Preview", truncate_str(&tag.content, 200), false)
        .color(0x00FF88)
        .footer(CreateEmbedFooter::new("Healthy Bot"));

    ctx.send(poise::CreateReply::default().embed(embed)).await?;
    Ok(())
}

async fn fetch_tag_list_page(
    pool: &sqlx::sqlite::SqlitePool,
    server_id: &str,
    author_id: &str,
    page: u32,
    page_size: u32,
) -> sqlx::Result<Vec<db::Tag>> {
    let offset = (page - 1) * page_size;
    db::list_tags_by_author(pool, server_id, author_id, offset, page_size).await
}

async fn handle_tag_list(
    ctx: Context<'_>,
    server_id: &str,
    target_user: Option<(String, String)>,
    mut page: u32,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let page_size: u32 = 15;
    page = if page == 0 { 1 } else { page };

    let (target_id, target_name) = match target_user {
        Some((uid, uname)) => (uid, uname),
        None => (ctx.author().id.to_string(), ctx.author().name.clone()),
    };

    let total = db::count_tags_by_author(pool, server_id, &target_id)
        .await
        .unwrap_or(0);

    if total == 0 {
        let msg = if target_id == ctx.author().id.to_string() {
            "You haven't created any tags on this server yet.".to_string()
        } else {
            format!("No tags found for {}.", target_name)
        };
        ctx.say(msg).await?;
        return Ok(());
    }

    let total_pages = ((total + page_size as i64 - 1) / page_size as i64).max(1) as u32;

    if page > total_pages {
        page = total_pages;
    }

    let build_page_data = |p: u32, total_pages: u32, total: i64, tags: &[db::Tag]| {
        let title = if total_pages > 1 {
            format!("Tags by {} (Page {}/{})", target_name, p, total_pages)
        } else {
            format!("Tags by {}", target_name)
        };

        let mut desc = format!("**Total Tags:** {}\n\n", total);
        let offset = (p - 1) * page_size;
        for (i, t) in tags.iter().enumerate() {
            desc.push_str(&format!(
                "{}. **{}** — {} uses\n",
                offset + i as u32 + 1,
                t.name,
                t.uses
            ));
        }

        let embed = CreateEmbed::new()
            .title(title)
            .description(desc)
            .color(0x00FF88)
            .footer(CreateEmbedFooter::new("Healthy Bot"));

        let components = if total_pages > 1 {
            vec![serenity::CreateActionRow::Buttons(vec![
                serenity::CreateButton::new("prev_page")
                    .label("◀")
                    .style(serenity::ButtonStyle::Secondary)
                    .disabled(p <= 1),
                serenity::CreateButton::new("next_page")
                    .label("▶")
                    .style(serenity::ButtonStyle::Secondary)
                    .disabled(p >= total_pages),
                serenity::CreateButton::new("cancel")
                    .label("❌")
                    .style(serenity::ButtonStyle::Danger),
            ])]
        } else {
            vec![]
        };

        (embed, components)
    };

    let initial_tags = fetch_tag_list_page(pool, server_id, &target_id, page, page_size).await?;
    let (embed, components) = build_page_data(page, total_pages, total, &initial_tags);

    let reply = ctx
        .send(
            poise::CreateReply::default()
                .embed(embed)
                .components(components),
        )
        .await?;

    if total_pages > 1 {
        let mut collector = reply
            .message()
            .await?
            .await_component_interactions(ctx)
            .author_id(ctx.author().id)
            .timeout(std::time::Duration::from_secs(60))
            .stream();

        let mut cancelled = false;
        while let Some(interaction) = collector.next().await {
            if interaction.data.custom_id == "cancel" {
                cancelled = true;
                let tags =
                    fetch_tag_list_page(pool, server_id, &target_id, page, page_size).await?;
                let (current_embed, _) = build_page_data(page, total_pages, total, &tags);
                let _ = interaction
                    .create_response(
                        ctx,
                        serenity::CreateInteractionResponse::UpdateMessage(
                            serenity::CreateInteractionResponseMessage::new()
                                .embed(current_embed)
                                .components(vec![]),
                        ),
                    )
                    .await;
                break;
            }

            match interaction.data.custom_id.as_str() {
                "prev_page" => page = page.saturating_sub(1).max(1),
                "next_page" => page = std::cmp::min(page + 1, total_pages),
                _ => continue,
            }

            let tags = fetch_tag_list_page(pool, server_id, &target_id, page, page_size).await?;
            let (new_embed, new_components) = build_page_data(page, total_pages, total, &tags);
            let _ = interaction
                .create_response(
                    ctx,
                    serenity::CreateInteractionResponse::UpdateMessage(
                        serenity::CreateInteractionResponseMessage::new()
                            .embed(new_embed)
                            .components(new_components),
                    ),
                )
                .await;
        }

        if !cancelled {
            if let Ok(tags) =
                fetch_tag_list_page(pool, server_id, &target_id, page, page_size).await
            {
                let (final_embed, _) = build_page_data(page, total_pages, total, &tags);
                let _ = reply
                    .edit(
                        ctx,
                        poise::CreateReply::default()
                            .embed(final_embed)
                            .components(vec![]),
                    )
                    .await;
            }
        }
    }

    Ok(())
}

async fn handle_tag_list_prefix(
    ctx: Context<'_>,
    server_id: &str,
    args: &[String],
) -> Result<(), Error> {
    let mut target_user: Option<(String, String)> = None;
    let mut page: u32 = 1;

    for arg in args {
        if let Ok(p) = arg.parse::<u32>() {
            page = p;
        } else if target_user.is_none() {
            if let Ok(u) = resolve_user(ctx, arg).await {
                target_user = Some(u);
            }
        }
    }

    handle_tag_list(ctx, server_id, target_user, page).await
}

async fn handle_tag_search(
    ctx: Context<'_>,
    server_id: &str,
    query: &str,
    target_user: Option<(String, String)>,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let author_id = target_user.as_ref().map(|(id, _)| id.as_str());
    let page_size = 15;
    let results = db::search_tags(pool, server_id, query, author_id, 150).await?;

    if results.is_empty() {
        let msg = match &target_user {
            Some((_, uname)) => format!("No tags matching '{}' found by {}.", query, uname),
            None => format!("No tags found matching '{}'.", query),
        };
        ctx.say(msg).await?;
        return Ok(());
    }

    let total_results = results.len();
    let total_pages = total_results.div_ceil(page_size) as u32;
    let mut page: u32 = 1;

    let build_page_data = |p: u32, total_pages: u32, results: &[db::Tag]| {
        let title = match &target_user {
            Some((_, uname)) if total_pages > 1 => {
                format!(
                    "Tag Search: '{}' (by {}) (Page {}/{})",
                    query, uname, p, total_pages
                )
            }
            Some((_, uname)) => format!("Tag Search: '{}' (by {})", query, uname),
            None if total_pages > 1 => {
                format!("Tag Search: '{}' (Page {}/{})", query, p, total_pages)
            }
            None => format!("Tag Search: '{}'", query),
        };

        let start_idx = (p as usize - 1) * page_size;
        let end_idx = std::cmp::min(start_idx + page_size, results.len());

        let mut desc = format!(
            "Found {} matching tag{}:\n\n",
            results.len(),
            if results.len() == 1 { "" } else { "s" }
        );
        for (i, t) in results[start_idx..end_idx].iter().enumerate() {
            desc.push_str(&format!(
                "{}. **{}** — {} uses (by {})\n",
                start_idx + i + 1,
                t.name,
                t.uses,
                t.author_name
            ));
        }

        let embed = CreateEmbed::new()
            .title(title)
            .description(desc)
            .color(0x00FF88)
            .footer(CreateEmbedFooter::new("Healthy Bot"));

        let components = if total_pages > 1 {
            vec![serenity::CreateActionRow::Buttons(vec![
                serenity::CreateButton::new("prev_page")
                    .label("◀")
                    .style(serenity::ButtonStyle::Secondary)
                    .disabled(p <= 1),
                serenity::CreateButton::new("next_page")
                    .label("▶")
                    .style(serenity::ButtonStyle::Secondary)
                    .disabled(p >= total_pages),
                serenity::CreateButton::new("cancel")
                    .label("❌")
                    .style(serenity::ButtonStyle::Danger),
            ])]
        } else {
            vec![]
        };

        (embed, components)
    };

    let (embed, components) = build_page_data(page, total_pages, &results);
    let reply = ctx
        .send(
            poise::CreateReply::default()
                .embed(embed)
                .components(components),
        )
        .await?;

    if total_pages > 1 {
        let mut collector = reply
            .message()
            .await?
            .await_component_interactions(ctx)
            .author_id(ctx.author().id)
            .timeout(std::time::Duration::from_secs(60))
            .stream();

        let mut cancelled = false;
        while let Some(interaction) = collector.next().await {
            if interaction.data.custom_id == "cancel" {
                cancelled = true;
                let (current_embed, _) = build_page_data(page, total_pages, &results);
                let _ = interaction
                    .create_response(
                        ctx,
                        serenity::CreateInteractionResponse::UpdateMessage(
                            serenity::CreateInteractionResponseMessage::new()
                                .embed(current_embed)
                                .components(vec![]),
                        ),
                    )
                    .await;
                break;
            }

            match interaction.data.custom_id.as_str() {
                "prev_page" => page = page.saturating_sub(1).max(1),
                "next_page" => page = std::cmp::min(page + 1, total_pages),
                _ => continue,
            }

            let (new_embed, new_components) = build_page_data(page, total_pages, &results);
            let _ = interaction
                .create_response(
                    ctx,
                    serenity::CreateInteractionResponse::UpdateMessage(
                        serenity::CreateInteractionResponseMessage::new()
                            .embed(new_embed)
                            .components(new_components),
                    ),
                )
                .await;
        }

        if !cancelled {
            let (final_embed, _) = build_page_data(page, total_pages, &results);
            let _ = reply
                .edit(
                    ctx,
                    poise::CreateReply::default()
                        .embed(final_embed)
                        .components(vec![]),
                )
                .await;
        }
    }

    Ok(())
}

async fn handle_tag_search_prefix(
    ctx: Context<'_>,
    server_id: &str,
    args: &[String],
) -> Result<(), Error> {
    let mut query_words = Vec::new();
    let mut target_user: Option<(String, String)> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-user" && i + 1 < args.len() {
            if let Ok(u) = resolve_user(ctx, &args[i + 1]).await {
                target_user = Some(u);
            }
            i += 2;
        } else {
            query_words.push(args[i].clone());
            i += 1;
        }
    }

    let query = query_words.join(" ");
    if query.trim().is_empty() && target_user.is_none() {
        return Err(user_error("Usage: `!t search <query> [-user <user>]`"));
    }

    handle_tag_search(ctx, server_id, &query, target_user).await
}

async fn handle_tag_export(
    ctx: Context<'_>,
    server_id: &str,
    target_user: Option<(String, String)>,
) -> Result<(), Error> {
    if !is_server_admin(ctx).await {
        return Err(user_error("Only server administrators can export tags."));
    }

    let pool = &ctx.data().db_pool;
    let author_id = target_user.as_ref().map(|(id, _)| id.as_str());
    let tags = db::export_all_tags(pool, server_id, author_id).await?;

    if tags.is_empty() {
        ctx.say("No tags found to export.").await?;
        return Ok(());
    }

    let json_bytes = serde_json::to_vec_pretty(&tags)?;
    let filename = match &target_user {
        Some((uid, _)) => format!("tags-export-{}-{}.json", server_id, uid),
        None => format!("tags-export-{}.json", server_id),
    };

    let attachment = serenity::all::CreateAttachment::bytes(json_bytes, filename);
    let reply = poise::CreateReply::default()
        .content(format!("Exported {} tags.", tags.len()))
        .attachment(attachment);

    ctx.send(reply).await?;
    Ok(())
}

async fn handle_tag_export_prefix(
    ctx: Context<'_>,
    server_id: &str,
    args: &[String],
) -> Result<(), Error> {
    if !is_server_admin(ctx).await {
        return Err(user_error("Only server administrators can export tags."));
    }

    let mut target_user: Option<(String, String)> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-user" && i + 1 < args.len() {
            if let Ok(u) = resolve_user(ctx, &args[i + 1]).await {
                target_user = Some(u);
            }
            i += 2;
        } else if target_user.is_none() {
            if let Ok(u) = resolve_user(ctx, &args[i]).await {
                target_user = Some(u);
            }
            i += 1;
        } else {
            i += 1;
        }
    }

    handle_tag_export(ctx, server_id, target_user).await
}

async fn handle_tag_remove_all(
    ctx: Context<'_>,
    server_id: &str,
    target_user: Option<(String, String)>,
) -> Result<(), Error> {
    if !is_server_admin(ctx).await {
        return Err(user_error(
            "Only server administrators can remove all tags.",
        ));
    }

    let pool = &ctx.data().db_pool;
    let author_id = target_user.as_ref().map(|(id, _)| id.as_str());
    let (deleted_count, asset_files) = db::delete_all_tags(pool, server_id, author_id).await?;

    for f in asset_files.into_iter().flatten() {
        if !db::is_asset_used_elsewhere(pool, &f, "")
            .await
            .unwrap_or(true)
        {
            let path = get_tag_assets_dir().join(&f);
            let _ = tokio::fs::remove_file(path).await;
        }
    }

    if let Some((_, name)) = target_user {
        ctx.say(format!(
            "Deleted {} tags authored by {}.",
            deleted_count, name
        ))
        .await?;
    } else {
        ctx.say(format!(
            "Deleted all {} tags on this server.",
            deleted_count
        ))
        .await?;
    }
    Ok(())
}

async fn handle_tag_remove_all_prefix(
    ctx: Context<'_>,
    server_id: &str,
    args: &[String],
) -> Result<(), Error> {
    if !is_server_admin(ctx).await {
        return Err(user_error(
            "Only server administrators can remove all tags.",
        ));
    }

    let mut target_user: Option<(String, String)> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-user" && i + 1 < args.len() {
            if let Ok(u) = resolve_user(ctx, &args[i + 1]).await {
                target_user = Some(u);
            }
            i += 2;
        } else if target_user.is_none() {
            if let Ok(u) = resolve_user(ctx, &args[i]).await {
                target_user = Some(u);
            }
            i += 1;
        } else {
            i += 1;
        }
    }

    handle_tag_remove_all(ctx, server_id, target_user).await
}

async fn handle_tag_cleanup(ctx: Context<'_>) -> Result<(), Error> {
    if !is_server_admin(ctx).await {
        return Err(user_error(
            "Only server administrators can clean up tag assets.",
        ));
    }

    let pool = &ctx.data().db_pool;
    let active_assets = db::get_all_asset_filenames(pool).await?;

    let assets_dir = get_tag_assets_dir();
    if !assets_dir.exists() {
        ctx.say("Asset directory does not exist. No files to clean up.")
            .await?;
        return Ok(());
    }

    let mut dir_entries = tokio::fs::read_dir(&assets_dir).await?;
    let mut removed_count = 0;
    let mut reclaimed_bytes: u64 = 0;

    while let Ok(Some(entry)) = dir_entries.next_entry().await {
        let file_name = entry.file_name().to_string_lossy().to_string();
        if !active_assets.contains(&file_name) {
            if let Ok(metadata) = entry.metadata().await {
                reclaimed_bytes += metadata.len();
            }
            if tokio::fs::remove_file(entry.path()).await.is_ok() {
                removed_count += 1;
            }
        }
    }

    let reclaimed_mb = reclaimed_bytes as f64 / 1_048_576.0;
    ctx.say(format!(
        "Asset cleanup complete. Removed {} orphaned file(s), reclaiming {:.2} MB.",
        removed_count, reclaimed_mb
    ))
    .await?;
    Ok(())
}

async fn handle_tag_seed(ctx: Context<'_>) -> Result<(), Error> {
    if !is_server_admin(ctx).await {
        return Err(user_error("Only server administrators can run seed."));
    }
    let pool = &ctx.data().db_pool;
    match db::seed_tags_from_file_if_present(pool).await {
        Ok(Some(path)) => {
            ctx.say(format!(
                "Successfully seeded tags from `{}` and removed the seed file.",
                path.display()
            ))
            .await?;
            Ok(())
        }
        Ok(None) => {
            ctx.say("No seed file found. Place `tags_seed.sql` in `/healthybot/data/tags_seed.sql` or `data/tags_seed.sql`.").await?;
            Ok(())
        }
        Err(e) => Err(user_error(format!("Failed to seed tags: {e}"))),
    }
}

/// Tag command: invoke, create, delete, or inspect tags
#[poise::command(prefix_command, rename = "t", aliases("tag"))]
pub async fn t_cmd(ctx: Context<'_>, #[rest] input: Option<String>) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let server_id = ctx
        .guild_id()
        .map(|g| g.to_string())
        .unwrap_or_else(|| "182937695515967488".to_string());

    let input_str = input.unwrap_or_default();
    let trimmed = input_str.trim();
    if trimmed.is_empty() {
        ctx.say("Usage: `!t <tagname> [args]` | `!t random` | `!t add <name> <content>` | `!t edit <name> <content>` | `!t rename <old> <new>` | `!t alias <tag> <alias>` | `!t delete <name>` | `!t info <name>` (or `!t raw` / `!t owner <name>`) | `!t list [user] [page]` | `!t search <query>` | `!t export` | `!t remove all` | `!t cleanup` | `!t seed`").await?;
        return Ok(());
    }

    let mut parts = trimmed.split_whitespace();
    let first = parts.next().unwrap_or("");
    let rest_vec: Vec<String> = parts.map(|s| s.to_string()).collect();

    match first.to_lowercase().as_str() {
        "random" | "rand" => {
            if let Some(tag) = db::get_random_tag(pool, &server_id).await {
                send_tag(ctx, &tag, &rest_vec, true).await
            } else {
                Err(user_error("No tags found on this server."))
            }
        }
        "add" | "create" => handle_tag_create_prefix(ctx, &server_id, &rest_vec).await,
        "edit" => handle_tag_edit_prefix(ctx, &server_id, &rest_vec).await,
        "rename" => handle_tag_rename(ctx, &server_id, &rest_vec).await,
        "alias" => handle_tag_alias(ctx, &server_id, &rest_vec).await,
        "cleanup" => handle_tag_cleanup(ctx).await,
        "seed" => handle_tag_seed(ctx).await,
        "delete" | "del" | "remove" | "rm" => {
            if rest_vec.first().map(|s| s.as_str()) == Some("all") {
                handle_tag_remove_all_prefix(ctx, &server_id, &rest_vec[1..]).await
            } else {
                let clean_args: Vec<String> = rest_vec
                    .into_iter()
                    .filter(|s| s != "tag" && s != "-force")
                    .collect();
                let Some(tag_name) = clean_args.first() else {
                    return Err(user_error("Please specify a tag name to delete."));
                };
                handle_tag_delete(ctx, &server_id, tag_name).await
            }
        }
        "info" | "raw" | "owner" => {
            let Some(tag_name) = rest_vec.first() else {
                return Err(user_error("Please specify a tag name."));
            };
            handle_tag_info(ctx, &server_id, tag_name).await
        }
        "list" => handle_tag_list_prefix(ctx, &server_id, &rest_vec).await,
        "search" => handle_tag_search_prefix(ctx, &server_id, &rest_vec).await,
        "export" => handle_tag_export_prefix(ctx, &server_id, &rest_vec).await,
        _ => {
            if let Some(tag) = db::get_tag(pool, &server_id, first).await {
                send_tag(ctx, &tag, &rest_vec, false).await
            } else if let Some(tag) = db::get_tag(pool, &server_id, trimmed).await {
                send_tag(ctx, &tag, &[], false).await
            } else {
                Err(user_error(format!(
                    "Tag '{}' not found.",
                    truncate_str(first, 50)
                )))
            }
        }
    }
}

/// Tag management commands
#[poise::command(
    slash_command,
    rename = "tag",
    subcommands(
        "tag_get_slash",
        "tag_random_slash",
        "tag_create_slash",
        "tag_edit_slash",
        "tag_rename_slash",
        "tag_alias_slash",
        "tag_delete_slash",
        "tag_info_slash",
        "tag_raw_slash",
        "tag_owner_slash",
        "tag_list_slash",
        "tag_search_slash",
        "tag_export_slash",
        "tag_remove_all_slash",
        "tag_cleanup_slash",
        "tag_seed_slash"
    )
)]
pub async fn tag_slash(_: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Retrieve and send a tag
#[poise::command(slash_command, rename = "get")]
pub async fn tag_get_slash(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_tag_name"]
    #[description = "Tag name"]
    name: String,
    #[description = "Arguments to pass to the tag"] args: Option<String>,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let server_id = ctx
        .guild_id()
        .map(|g| g.to_string())
        .unwrap_or_else(|| "182937695515967488".to_string());

    let Some(tag) = db::get_tag(pool, &server_id, &name).await else {
        return Err(user_error(format!(
            "Tag '{}' not found.",
            truncate_str(&name, 50)
        )));
    };

    let args_vec: Vec<String> = args
        .map(|s| s.split_whitespace().map(|w| w.to_string()).collect())
        .unwrap_or_default();

    send_tag(ctx, &tag, &args_vec, false).await
}

/// Retrieve and send a random tag
#[poise::command(slash_command, rename = "random")]
pub async fn tag_random_slash(
    ctx: Context<'_>,
    #[description = "Arguments to pass to the tag"] args: Option<String>,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let server_id = ctx
        .guild_id()
        .map(|g| g.to_string())
        .unwrap_or_else(|| "182937695515967488".to_string());

    if let Some(tag) = db::get_random_tag(pool, &server_id).await {
        let args_vec: Vec<String> = args
            .map(|s| s.split_whitespace().map(|w| w.to_string()).collect())
            .unwrap_or_default();
        send_tag(ctx, &tag, &args_vec, true).await
    } else {
        Err(user_error("No tags found on this server."))
    }
}

/// Create a new tag
#[poise::command(slash_command, rename = "create")]
pub async fn tag_create_slash(
    ctx: Context<'_>,
    #[description = "Tag name"] name: String,
    #[description = "Tag text content"] content: Option<String>,
    #[description = "Optional file attachment"] attachment: Option<serenity::all::Attachment>,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let server_id = ctx
        .guild_id()
        .map(|g| g.to_string())
        .unwrap_or_else(|| "182937695515967488".to_string());

    if name.len() > 100 {
        return Err(user_error("Tag name is too long (max 100 characters)."));
    }

    if name.chars().any(char::is_whitespace) {
        return Err(user_error(
            "Tag names must be a single word without spaces.",
        ));
    }

    if db::get_tag(pool, &server_id, &name).await.is_some() {
        return Err(user_error(format!(
            "A tag named '{}' already exists.",
            name
        )));
    }

    let mut asset_filename = None;
    let tag_id = Utc::now().timestamp_nanos_opt().unwrap_or(0).to_string();
    if let Some(att) = &attachment {
        let ext = att.filename.split('.').next_back().unwrap_or("png");
        let filename = format!("{}.{}", tag_id, ext);
        let target_path = get_tag_assets_dir().join(&filename);

        if let Ok(bytes) = att.download().await {
            tokio::fs::create_dir_all(get_tag_assets_dir()).await.ok();
            if tokio::fs::write(&target_path, &bytes).await.is_ok() {
                asset_filename = Some(filename);
            }
        }
    }

    let raw_content = content.unwrap_or_default();
    let (content_val, cdn_asset) = if asset_filename.is_none() {
        extract_and_download_discord_cdn_asset(&raw_content, &tag_id).await
    } else {
        (raw_content, None)
    };
    if let Some(a) = cdn_asset {
        asset_filename = Some(a);
    }

    if content_val.is_empty() && asset_filename.is_none() {
        return Err(user_error("Tag must have content or an attachment."));
    }

    let new_tag = db::Tag {
        id: tag_id,
        server_id,
        name: name.clone(),
        name_normalized: name.trim().to_lowercase(),
        content: content_val,
        asset_filename,
        author_id: ctx.author().id.to_string(),
        author_name: ctx.author().name.clone(),
        created_at: Utc::now().timestamp_millis(),
        last_used_at: None,
        uses: 0,
    };

    db::create_tag(pool, &new_tag).await?;
    ctx.say(format!("Tag '{}' created!", name)).await?;
    Ok(())
}

/// Edit an existing tag
#[poise::command(slash_command, rename = "edit")]
pub async fn tag_edit_slash(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_tag_name"]
    #[description = "Tag name to edit"]
    name: String,
    #[description = "New text content"] content: Option<String>,
    #[description = "New file attachment"] attachment: Option<serenity::all::Attachment>,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let server_id = ctx
        .guild_id()
        .map(|g| g.to_string())
        .unwrap_or_else(|| "182937695515967488".to_string());

    if name.chars().any(char::is_whitespace) {
        return Err(user_error(
            "Tag names must be a single word without spaces.",
        ));
    }

    let Some(tag) = db::get_tag(pool, &server_id, &name).await else {
        return Err(user_error(format!("Tag '{}' not found.", name)));
    };

    let is_admin = is_server_admin(ctx).await;
    let is_author = tag.author_id == ctx.author().id.to_string();
    if !is_admin && !is_author {
        return Err(user_error("You do not have permission to edit this tag (only the author or a server admin can edit it)."));
    }

    let mut new_asset_filename = tag.asset_filename.clone();
    let mut uploaded_file = false;
    if let Some(att) = &attachment {
        let ext = att.filename.split('.').next_back().unwrap_or("png");
        let filename = format!("{}_{}.{}", tag.id, Utc::now().timestamp_millis(), ext);
        let target_path = get_tag_assets_dir().join(&filename);

        if let Ok(bytes) = att.download().await {
            tokio::fs::create_dir_all(get_tag_assets_dir()).await.ok();
            if tokio::fs::write(&target_path, &bytes).await.is_ok() {
                new_asset_filename = Some(filename);
                uploaded_file = true;
            }
        }
    }

    let raw_content = content.unwrap_or(tag.content);
    let (new_content, cdn_asset) = if !uploaded_file {
        extract_and_download_discord_cdn_asset(&raw_content, &tag.id).await
    } else {
        (raw_content, None)
    };
    if let Some(a) = cdn_asset {
        new_asset_filename = Some(a);
    }

    if new_content.is_empty() && new_asset_filename.is_none() {
        return Err(user_error("Tag must have content or an attachment."));
    }

    db::update_tag(pool, &tag.id, &new_content, new_asset_filename.as_deref()).await?;
    ctx.say(format!("Tag '{}' updated!", tag.name)).await?;
    Ok(())
}

/// Rename an existing tag
#[poise::command(slash_command, rename = "rename")]
pub async fn tag_rename_slash(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_tag_name"]
    #[description = "Current tag name"]
    old_name: String,
    #[description = "New tag name"] new_name: String,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let server_id = ctx
        .guild_id()
        .map(|g| g.to_string())
        .unwrap_or_else(|| "182937695515967488".to_string());

    if new_name.len() > 100 {
        return Err(user_error("Tag name is too long (max 100 characters)."));
    }

    if new_name.chars().any(char::is_whitespace) {
        return Err(user_error(
            "Tag names must be a single word without spaces.",
        ));
    }

    let Some(tag) = db::get_tag(pool, &server_id, &old_name).await else {
        return Err(user_error(format!("Tag '{}' not found.", old_name)));
    };

    let is_admin = is_server_admin(ctx).await;
    let is_author = tag.author_id == ctx.author().id.to_string();
    if !is_admin && !is_author {
        return Err(user_error("You do not have permission to rename this tag (only the author or a server admin can rename it)."));
    }

    if old_name.trim().to_lowercase() != new_name.trim().to_lowercase()
        && db::get_tag(pool, &server_id, &new_name).await.is_some()
    {
        return Err(user_error(format!(
            "A tag named '{}' already exists.",
            new_name
        )));
    }

    db::rename_tag(pool, &tag.id, &new_name).await?;
    ctx.say(format!("Tag '{}' renamed to '{}'.", tag.name, new_name))
        .await?;
    Ok(())
}

/// Create a tag alias
#[poise::command(slash_command, rename = "alias")]
pub async fn tag_alias_slash(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_tag_name"]
    #[description = "Existing tag name"]
    target: String,
    #[description = "New alias name"] alias_name: String,
) -> Result<(), Error> {
    let pool = &ctx.data().db_pool;
    let server_id = ctx
        .guild_id()
        .map(|g| g.to_string())
        .unwrap_or_else(|| "182937695515967488".to_string());

    if alias_name.len() > 100 {
        return Err(user_error(
            "Alias tag name is too long (max 100 characters).",
        ));
    }

    if alias_name.chars().any(char::is_whitespace) {
        return Err(user_error(
            "Tag names must be a single word without spaces.",
        ));
    }

    let Some(existing_tag) = db::get_tag(pool, &server_id, &target).await else {
        return Err(user_error(format!("Tag '{}' not found.", target)));
    };

    if db::get_tag(pool, &server_id, &alias_name).await.is_some() {
        return Err(user_error(format!(
            "A tag named '{}' already exists.",
            alias_name
        )));
    }

    let tag_id = Utc::now().timestamp_nanos_opt().unwrap_or(0).to_string();
    let alias_tag = db::Tag {
        id: tag_id,
        server_id,
        name: alias_name.clone(),
        name_normalized: alias_name.trim().to_lowercase(),
        content: existing_tag.content.clone(),
        asset_filename: existing_tag.asset_filename.clone(),
        author_id: ctx.author().id.to_string(),
        author_name: ctx.author().name.clone(),
        created_at: Utc::now().timestamp_millis(),
        last_used_at: None,
        uses: 0,
    };

    db::create_tag(pool, &alias_tag).await?;
    ctx.say(format!(
        "Created alias '{}' for tag '{}'.",
        alias_name, existing_tag.name
    ))
    .await?;
    Ok(())
}

/// Delete an existing tag
#[poise::command(slash_command, rename = "delete")]
pub async fn tag_delete_slash(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_tag_name"]
    #[description = "Tag name to delete"]
    name: String,
) -> Result<(), Error> {
    let server_id = ctx
        .guild_id()
        .map(|g| g.to_string())
        .unwrap_or_else(|| "182937695515967488".to_string());
    handle_tag_delete(ctx, &server_id, &name).await
}

/// Show statistics and info for a tag
#[poise::command(slash_command, rename = "info")]
pub async fn tag_info_slash(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_tag_name"]
    #[description = "Tag name to inspect"]
    name: String,
) -> Result<(), Error> {
    let server_id = ctx
        .guild_id()
        .map(|g| g.to_string())
        .unwrap_or_else(|| "182937695515967488".to_string());
    handle_tag_info(ctx, &server_id, &name).await
}

/// View a tag's raw content and metadata
#[poise::command(slash_command, rename = "raw")]
pub async fn tag_raw_slash(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_tag_name"]
    #[description = "Tag name to inspect"]
    name: String,
) -> Result<(), Error> {
    let server_id = ctx
        .guild_id()
        .map(|g| g.to_string())
        .unwrap_or_else(|| "182937695515967488".to_string());
    handle_tag_info(ctx, &server_id, &name).await
}

/// View a tag's owner and metadata
#[poise::command(slash_command, rename = "owner")]
pub async fn tag_owner_slash(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_tag_name"]
    #[description = "Tag name to inspect"]
    name: String,
) -> Result<(), Error> {
    let server_id = ctx
        .guild_id()
        .map(|g| g.to_string())
        .unwrap_or_else(|| "182937695515967488".to_string());
    handle_tag_info(ctx, &server_id, &name).await
}

/// List tags ordered by popularity
#[poise::command(slash_command, rename = "list")]
pub async fn tag_list_slash(
    ctx: Context<'_>,
    #[description = "Filter by author"] user: Option<serenity::User>,
    #[description = "Page number (defaults to 1)"] page: Option<u32>,
) -> Result<(), Error> {
    let server_id = ctx
        .guild_id()
        .map(|g| g.to_string())
        .unwrap_or_else(|| "182937695515967488".to_string());
    let target_user = user.map(|u| (u.id.to_string(), u.name));
    handle_tag_list(ctx, &server_id, target_user, page.unwrap_or(1)).await
}

/// Search tags by name or content
#[poise::command(slash_command, rename = "search")]
pub async fn tag_search_slash(
    ctx: Context<'_>,
    #[description = "Search query"] query: String,
    #[description = "Filter by author"] user: Option<serenity::User>,
) -> Result<(), Error> {
    let server_id = ctx
        .guild_id()
        .map(|g| g.to_string())
        .unwrap_or_else(|| "182937695515967488".to_string());
    let target_user = user.map(|u| (u.id.to_string(), u.name));
    handle_tag_search(ctx, &server_id, &query, target_user).await
}

/// Export server tags as JSON (Admin only)
#[poise::command(
    slash_command,
    rename = "export",
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn tag_export_slash(
    ctx: Context<'_>,
    #[description = "Filter by author"] user: Option<serenity::User>,
) -> Result<(), Error> {
    let server_id = ctx
        .guild_id()
        .map(|g| g.to_string())
        .unwrap_or_else(|| "182937695515967488".to_string());
    let target_user = user.map(|u| (u.id.to_string(), u.name));
    handle_tag_export(ctx, &server_id, target_user).await
}

/// Delete all tags on the server (Admin only)
#[poise::command(
    slash_command,
    rename = "remove_all",
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn tag_remove_all_slash(
    ctx: Context<'_>,
    #[description = "Filter by author"] user: Option<serenity::User>,
    #[description = "Confirm deletion (must be True)"] confirm: bool,
) -> Result<(), Error> {
    if !confirm {
        return Err(user_error("Please set confirm to True to delete all tags."));
    }
    let server_id = ctx
        .guild_id()
        .map(|g| g.to_string())
        .unwrap_or_else(|| "182937695515967488".to_string());
    let target_user = user.map(|u| (u.id.to_string(), u.name));
    handle_tag_remove_all(ctx, &server_id, target_user).await
}

/// Clean up unreferenced media files from disk (Admin only)
#[poise::command(
    slash_command,
    rename = "cleanup",
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn tag_cleanup_slash(ctx: Context<'_>) -> Result<(), Error> {
    handle_tag_cleanup(ctx).await
}

/// Seed tags from SQL file on the bot host/volume (Admin only)
#[poise::command(
    slash_command,
    rename = "seed",
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn tag_seed_slash(ctx: Context<'_>) -> Result<(), Error> {
    handle_tag_seed(ctx).await
}
