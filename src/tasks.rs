use crate::db;
use crate::truncate_str;
use chrono::{Datelike, TimeZone, Utc};
use chrono_tz::Europe::Amsterdam;
use serenity::all::{ChannelId, ChannelType, GuildId, Http};
use serenity::builder::{CreateEmbed, CreateEmbedFooter, CreateMessage};
use sqlx::SqlitePool;
use std::sync::Arc;
use tokio::time::{interval, Duration};

fn create_embed(title: &str) -> CreateEmbed {
    CreateEmbed::new()
        .title(title)
        .color(0xFFFF00) // Color.YELLOW equivalent
        .footer(CreateEmbedFooter::new("Healthy Bot"))
}

pub async fn start_tasks(pool: SqlitePool, http: Arc<Http>) {
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
    for reminder in &reminders {
        embed = embed.field(
            "",
            truncate_str(
                &format!("{} (<@{}>)", reminder.message, reminder.owner_discord_id),
                1024,
            ),
            false,
        );
    }

    // Only delete the reminders once we've confirmed the announcement was sent.
    // If the send fails (permissions, 5xx, embed limits, …), leave them in place
    // so the next tick retries instead of silently dropping them.
    match ChannelId::new(channel_id)
        .send_message(http, CreateMessage::new().embed(embed))
        .await
    {
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
