use crate::openai::OpenAIClient;
use crate::truncate_str;
use crate::{db, memory, recent};
use chrono::{Datelike, TimeZone, Utc};
use chrono_tz::Europe::Amsterdam;
use serenity::all::{ChannelId, ChannelType, GetMessages, GuildId, Http};
use serenity::builder::{CreateEmbed, CreateEmbedFooter, CreateMessage};
use sqlx::SqlitePool;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::time::{interval, Duration};

fn create_embed(title: &str) -> CreateEmbed {
    CreateEmbed::new()
        .title(title)
        .color(0xFFFF00) // Color.YELLOW equivalent
        .footer(CreateEmbedFooter::new("Healthy Bot"))
}

pub async fn start_tasks(pool: SqlitePool, http: Arc<Http>, openai: OpenAIClient) {
    let pool_clone = pool.clone();
    let http_clone = http.clone();
    tokio::spawn(async move {
        let mut interval = interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            if let Err(e) = announce_reminders(&pool_clone, &http_clone).await {
                log::error!("Error announcing reminders: {:?}", e);
            }
        }
    });

    let pool_clone = pool.clone();
    let http_clone = http.clone();
    tokio::spawn(async move {
        let mut interval = interval(Duration::from_secs(600));
        loop {
            interval.tick().await;
            if let Err(e) = update_reminders_vc(&pool_clone, &http_clone).await {
                log::error!("Error updating reminders VC: {:?}", e);
            }
        }
    });

    // Memory extraction. The interval is read each cycle so it can be tuned at
    // runtime; sleeping first means a restart doesn't immediately trigger a pass.
    let pool_clone = pool.clone();
    let http_clone = http.clone();
    tokio::spawn(async move {
        loop {
            let interval_secs = db::get_setting(&pool_clone, "memory_extraction_interval_seconds")
                .await
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(3600)
                .max(60); // never hammer the API with a tiny/zero interval
            tokio::time::sleep(Duration::from_secs(interval_secs)).await;
            if let Err(e) = extract_memory(&pool_clone, &http_clone, &openai).await {
                log::error!("Error extracting memory: {:?}", e);
            }
        }
    });
}

/// Read recent main-channel messages and extract durable per-user facts into the
/// database. No-op when memory is disabled or no main channel is configured.
async fn extract_memory(
    pool: &SqlitePool,
    http: &Http,
    openai: &OpenAIClient,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let enabled = db::get_setting(pool, "memory_enabled")
        .await
        .map(|v| v.trim().to_lowercase() != "false")
        .unwrap_or(true); // on by default
    if !enabled {
        return Ok(());
    }

    let Some(channel_id_str) = db::get_setting(pool, "main_text_channel").await else {
        return Ok(());
    };
    let channel_id: u64 = channel_id_str.parse()?;

    let messages = ChannelId::new(channel_id)
        .messages(http, GetMessages::new().limit(100))
        .await?;

    // Collect eligible messages (newest-first; build_transcript reverses them) and
    // the unique participant roster, skipping bots and opted-out users.
    let mut entries: Vec<recent::RecentMessage> = Vec::new();
    let mut participants: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for m in &messages {
        if m.author.bot || m.content.trim().is_empty() {
            continue;
        }
        let id = m.author.id.to_string();
        if db::is_opted_out(pool, &id).await {
            continue;
        }
        if seen.insert(id.clone()) {
            participants.push((id, m.author.name.clone()));
        }
        entries.push(recent::RecentMessage {
            author_name: m.author.name.clone(),
            content: m.content.clone(),
            is_bot: false,
        });
    }

    if participants.is_empty() {
        return Ok(());
    }
    let Some(transcript) = recent::build_transcript(&entries) else {
        return Ok(());
    };

    let model = match db::get_setting(pool, "memory_model")
        .await
        .filter(|v| !v.trim().is_empty())
    {
        Some(m) => m,
        None => db::get_setting(pool, "ai_chat_model")
            .await
            .unwrap_or_else(|| "gpt-4o-mini".to_string()),
    };
    let max_facts = db::get_setting(pool, "memory_max_facts_per_user")
        .await
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(20);

    let memory_max_tokens = db::get_setting(pool, "memory_max_tokens")
        .await
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(500);

    let request = memory::build_extraction_messages(&participants, &transcript);
    let response = openai
        .create_chat_with_max_tokens(&model, request, Some(memory_max_tokens))
        .await?;

    if let Some(usage) = &response.usage {
        if !participants.is_empty() {
            let tokens_per_user = usage.total_tokens / (participants.len() as u32);
            for (user_id, _) in &participants {
                let _ = db::record_memory_tokens(pool, user_id, tokens_per_user).await;
            }
        }
    }

    let Some(choice) = response.choices.first() else {
        return Ok(());
    };

    let facts = memory::parse_extracted_facts(choice.message.content_text());
    if facts.is_empty() {
        return Ok(());
    }

    // Only accept facts keyed to a real participant from this batch.
    let valid_ids: HashSet<&str> = participants.iter().map(|(id, _)| id.as_str()).collect();
    let now_ms = Utc::now().timestamp_millis();
    let mut touched: HashSet<String> = HashSet::new();

    for f in facts {
        if !valid_ids.contains(f.user_id.as_str()) {
            continue;
        }
        if db::add_fact(pool, &f.user_id, &f.fact, now_ms)
            .await
            .is_ok()
        {
            touched.insert(f.user_id);
        }
    }

    for id in &touched {
        let _ = db::prune_facts(pool, id, max_facts).await;
    }

    log::info!(
        "Memory extraction: {} participants, {} users updated",
        participants.len(),
        touched.len()
    );
    Ok(())
}

async fn announce_reminders(
    pool: &SqlitePool,
    http: &Http,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let now = chrono::Utc::now();
    let all_reminders: Vec<db::Reminder> =
        sqlx::query_as::<_, db::Reminder>("SELECT * FROM reminder")
            .fetch_all(pool)
            .await?;

    let reminders: Vec<db::Reminder> = all_reminders
        .into_iter()
        .filter(|r| r.date_utc() <= now)
        .collect();

    if reminders.is_empty() {
        return Ok(());
    }

    log::info!("Announcing {} expired reminders", reminders.len());

    let channel_id_str = db::get_setting(pool, "main_text_channel")
        .await
        .ok_or("main_text_channel not set")?;
    let channel_id: u64 = channel_id_str.parse()?;

    let mut embed = create_embed("Reminders");
    let mut pings = Vec::new();
    for reminder in &reminders {
        embed = embed.field("", truncate_str(&reminder.message, 1024), false);
        let ping = format!("<@{}>", reminder.owner_discord_id);
        if !pings.contains(&ping) {
            pings.push(ping);
        }
    }

    let reminder_pings = db::get_setting(pool, "reminder_pings")
        .await
        .map(|v| v.trim().to_lowercase() == "true")
        .unwrap_or(false);

    let mut message = CreateMessage::new().embed(embed);
    if reminder_pings && !pings.is_empty() {
        message = message.content(pings.join(" "));
    }

    // Only delete the reminders once we've confirmed the announcement was sent.
    // If the send fails (permissions, 5xx, embed limits, …), leave them in place
    // so the next tick retries instead of silently dropping them.
    match ChannelId::new(channel_id).send_message(http, message).await {
        Ok(_) => {
            for reminder in reminders {
                log::info!("Deleting processed reminder #{}", reminder.id);
                sqlx::query("DELETE FROM reminder WHERE id = ?")
                    .bind(reminder.id)
                    .execute(pool)
                    .await?;
            }
        }
        Err(e) => {
            log::error!(
                "Failed to announce {} reminder(s); leaving them for the next tick: {:?}",
                reminders.len(),
                e
            );
        }
    }

    Ok(())
}

async fn update_reminders_vc(
    pool: &SqlitePool,
    http: &Http,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let category_id_str = db::get_setting(pool, "reminder_category")
        .await
        .ok_or("reminder_category not set")?;
    let category_id: u64 = category_id_str.parse()?;
    let guild_id_str = std::env::var("DISCORD_GUILD_ID")?;
    let guild_id: u64 = guild_id_str.parse()?;

    let now_ams = Utc::now().with_timezone(&Amsterdam);
    let Some(today_start) = Amsterdam
        .with_ymd_and_hms(now_ams.year(), now_ams.month(), now_ams.day(), 0, 0, 0)
        .earliest()
    else {
        // Local midnight doesn't exist (a DST spring-forward gap); skip this
        // tick rather than panicking in the background task.
        log::warn!("Could not resolve local midnight (DST gap); skipping VC update this tick.");
        return Ok(());
    };
    let today_start = today_start.with_timezone(&Utc);
    let today_end = today_start + chrono::Duration::days(1);

    let all_reminders: Vec<db::Reminder> =
        sqlx::query_as::<_, db::Reminder>("SELECT * FROM reminder ORDER BY date ASC")
            .fetch_all(pool)
            .await?;

    let reminders: Vec<db::Reminder> = all_reminders
        .into_iter()
        .filter(|r| {
            let utc = r.date_utc();
            utc >= today_start && utc < today_end
        })
        .take(3)
        .collect();

    let guild = GuildId::new(guild_id);
    let channels = guild.channels(http).await?;

    let mut existing_vcs = Vec::new();
    for channel in channels.values() {
        if let Some(parent) = channel.parent_id {
            if parent.get() == category_id && channel.kind == ChannelType::Voice {
                existing_vcs.push(channel.clone());
            }
        }
    }
    existing_vcs.sort_by_key(|c| c.position);

    let desired_names: Vec<String> = reminders
        .iter()
        .map(|r| {
            let time_str = r
                .date_utc()
                .with_timezone(&Amsterdam)
                .format("%H:%M")
                .to_string();
            // Discord caps channel names at 100 characters.
            truncate_str(&format!("{} - {}", time_str, r.message), 100).into_owned()
        })
        .collect();

    let current_names: Vec<String> = existing_vcs.iter().map(|c| c.name.clone()).collect();

    if desired_names == current_names {
        return Ok(());
    }

    log::info!(
        "Updating VC reminders (found {} for today, reconciling channels)",
        reminders.len()
    );

    // Reconcile in place instead of deleting and recreating everything: channel
    // create/delete are heavily rate-limited, so we only touch what changed.
    let common = existing_vcs.len().min(desired_names.len());

    // 1. Rename channels whose name no longer matches (one edit per changed slot).
    for i in 0..common {
        if existing_vcs[i].name != desired_names[i] {
            log::info!(
                "Renaming VC reminder '{}' -> '{}'",
                existing_vcs[i].name,
                desired_names[i]
            );
            if let Err(e) = existing_vcs[i]
                .edit(
                    http,
                    serenity::all::EditChannel::new().name(&desired_names[i]),
                )
                .await
            {
                log::error!("Failed to rename VC reminder: {:?}", e);
            }
        }
    }

    // 2. Delete any surplus channels (more exist than are needed today).
    for channel in existing_vcs.iter().skip(desired_names.len()) {
        log::info!("Deleting surplus VC reminder: {}", channel.name);
        if let Err(e) = channel.delete(http).await {
            log::error!("Failed to delete VC reminder: {:?}", e);
        }
    }

    // 3. Create channels for any remaining desired names (fewer exist than needed).
    for name in desired_names.iter().skip(existing_vcs.len()) {
        log::info!("Creating new VC reminder: {}", name);
        if let Err(e) = guild
            .create_channel(
                http,
                serenity::all::CreateChannel::new(name)
                    .kind(ChannelType::Voice)
                    .category(ChannelId::new(category_id)),
            )
            .await
        {
            log::error!("Failed to create VC reminder: {:?}", e);
        }
    }

    Ok(())
}
