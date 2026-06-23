use crate::db;
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
        let total_pages = (reminders.len() + page_size - 1) / page_size;

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
            let mut m = reply.into_message().await?;
            let mut collector = serenity::collector::ComponentInteractionCollector::new(ctx)
                .filter(move |int| int.message.id == m.id)
                .timeout(std::time::Duration::from_secs(60))
                .stream();

            let mut canceled = false;
            while let Some(interaction) = collector.next().await {
                if interaction.user.id != ctx.author().id {
                    let _ = interaction
                        .create_response(
                            ctx.http(),
                            serenity::CreateInteractionResponse::Message(
                                serenity::CreateInteractionResponseMessage::new()
                                    .content("You cannot control this pagination.")
                                    .ephemeral(true),
                            ),
                        )
                        .await;
                    continue;
                }

                let action = interaction.data.custom_id.as_str();
                match action {
                    "prev_page" => {
                        if page > 1 {
                            page -= 1;
                        }
                    }
                    "next_page" => {
                        if page < total_pages {
                            page += 1;
                        }
                    }
                    "cancel" => {
                        let (final_embed, _) =
                            build_page_data(page, total_pages, &reminders, now_ams);
                        let _ = interaction
                            .create_response(
                                ctx.http(),
                                serenity::CreateInteractionResponse::UpdateMessage(
                                    serenity::CreateInteractionResponseMessage::new()
                                        .embed(final_embed)
                                        .components(vec![]),
                                ),
                            )
                            .await;
                        canceled = true;
                        break;
                    }
                    _ => {}
                }

                let (new_embed, new_components) =
                    build_page_data(page, total_pages, &reminders, now_ams);
                let _ = interaction
                    .create_response(
                        ctx.http(),
                        serenity::CreateInteractionResponse::UpdateMessage(
                            serenity::CreateInteractionResponseMessage::new()
                                .embed(new_embed)
                                .components(new_components),
                        ),
                    )
                    .await;
            }

            if !canceled {
                let _ = m
                    .edit(ctx, serenity::EditMessage::new().components(vec![]))
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
        if !is_server_admin(ctx).await {
            return Err(user_error("You are not authorized to change settings."));
        }
        let key = key.ok_or_else(|| user_error("No key supplied"))?;
        let value = value.ok_or_else(|| user_error("No value supplied"))?;

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
            embed = embed.field(truncate_str(&s.k, 256), truncate_str(&s.v, 1024), false);
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
        "ai_initial_prompt",
        "ai_chat_model",
        "bot_status_type",
        "bot_status_message",
        "allowed_bot_id",
        "ai_debug",
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
