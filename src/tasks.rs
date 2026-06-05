use crate::db;
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
        .ok_or_else(|| "main_text_channel not set")?;
    let channel_id: u64 = channel_id_str.parse()?;

    let mut embed = create_embed("Reminders");
    for reminder in &reminders {
        embed = embed.field(
            "",
            format!("{} (<@{}>)", reminder.message, reminder.owner_discord_id),
            false,
        );
    }

    let _ = ChannelId::new(channel_id)
        .send_message(http, CreateMessage::new().embed(embed))
        .await;

    for reminder in reminders {
        log::info!("Deleting processed reminder #{}", reminder.id);
        sqlx::query("DELETE FROM reminder WHERE id = ?")
            .bind(reminder.id)
            .execute(pool)
            .await?;
    }

    Ok(())
}

async fn update_reminders_vc(
    pool: &SqlitePool,
    http: &Http,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let category_id_str = db::get_setting(pool, "reminder_category")
        .await
        .ok_or_else(|| "reminder_category not set")?;
    let category_id: u64 = category_id_str.parse()?;
    let guild_id_str = std::env::var("DISCORD_GUILD_ID")?;
    let guild_id: u64 = guild_id_str.parse()?;

    let now_ams = Utc::now().with_timezone(&Amsterdam);
    let today_start = Amsterdam
        .with_ymd_and_hms(now_ams.year(), now_ams.month(), now_ams.day(), 0, 0, 0)
        .unwrap()
        .with_timezone(&Utc);
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

    log::info!(
        "Updating VC reminders (found {} for today)",
        reminders.len()
    );

    let guild = GuildId::new(guild_id);
    let channels = guild.channels(http).await?;

    // Delete existing voice channels in the category
    for (_, channel) in channels {
        if let Some(parent) = channel.parent_id {
            if parent.get() == category_id && channel.kind == ChannelType::Voice {
                log::info!("Deleting old VC reminder: {}", channel.name);
                let _ = channel.delete(http).await;
            }
        }
    }

    // Create new ones
    for reminder in reminders {
        let time_str = reminder
            .date_utc()
            .with_timezone(&Amsterdam)
            .format("%H:%M")
            .to_string();
        let name = format!("{} - {}", time_str, reminder.message);
        log::info!("Creating new VC reminder: {}", name);
        let _ = guild
            .create_channel(
                http,
                serenity::all::CreateChannel::new(name)
                    .kind(serenity::all::ChannelType::Voice)
                    .category(ChannelId::new(category_id)),
            )
            .await;
    }

    Ok(())
}
