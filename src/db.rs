use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePool;

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct User {
    pub discord_id: String,
    pub authorized: bool,
    pub role: String,
    pub last_message: Option<i64>,
}

impl User {
    pub fn last_message_utc(&self) -> Option<DateTime<Utc>> {
        self.last_message.map(to_utc)
    }
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct Reminder {
    pub id: i64,
    pub message: String,
    pub date: i64,
    pub owner_discord_id: String,
}

impl Reminder {
    pub fn date_utc(&self) -> DateTime<Utc> {
        to_utc(self.date)
    }
}

fn to_utc(ts: i64) -> DateTime<Utc> {
    if ts > 10_000_000_000 {
        // Assume milliseconds
        DateTime::from_timestamp(ts / 1000, (ts % 1000) as u32 * 1_000_000).unwrap_or_default()
    } else {
        // Assume seconds
        DateTime::from_timestamp(ts, 0).unwrap_or_default()
    }
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct Setting {
    pub k: String,
    pub v: String,
}

pub async fn get_setting(pool: &SqlitePool, key: &str) -> Option<String> {
    sqlx::query_as::<_, Setting>("SELECT k, v FROM setting WHERE k = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .map(|s| s.v)
}

pub async fn get_user(pool: &SqlitePool, discord_id: &str) -> Option<User> {
    sqlx::query_as::<_, User>("SELECT * FROM users WHERE discord_id = ?")
        .bind(discord_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
}

pub async fn create_user_if_not_exists(pool: &SqlitePool, discord_id: &str) -> sqlx::Result<()> {
    sqlx::query("INSERT OR IGNORE INTO users (discord_id, authorized, role) VALUES (?, ?, ?)")
        .bind(discord_id)
        .bind(false)
        .bind("USER")
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn update_last_message(
    pool: &SqlitePool,
    discord_id: &str,
    time: DateTime<Utc>,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE users SET last_message = ? WHERE discord_id = ?")
        .bind(time.timestamp_millis())
        .bind(discord_id)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_utc_seconds() {
        let ts = 1672531200; // 2023-01-01 00:00:00 UTC
        let dt = to_utc(ts);
        assert_eq!(dt.timestamp(), ts);
        assert_eq!(dt.timestamp_subsec_millis(), 0);
    }

    #[test]
    fn test_to_utc_milliseconds() {
        let ts = 1672531200123; // 2023-01-01 00:00:00.123 UTC
        let dt = to_utc(ts);
        assert_eq!(dt.timestamp(), 1672531200);
        assert_eq!(dt.timestamp_subsec_millis(), 123);
    }

    #[test]
    fn test_to_utc_boundary() {
        // Just below 10_000_000_000 should be treated as seconds
        let ts = 9_999_999_999;
        let dt = to_utc(ts);
        assert_eq!(dt.timestamp(), ts);

        // Exactly 10_000_000_001 should be treated as milliseconds
        let ts_ms = 10_000_000_001;
        let dt_ms = to_utc(ts_ms);
        assert_eq!(dt_ms.timestamp(), 10_000_000);
        assert_eq!(dt_ms.timestamp_subsec_millis(), 1);
    }
}
