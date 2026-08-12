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

pub fn to_utc(ts: i64) -> DateTime<Utc> {
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

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct UserFact {
    pub id: i64,
    pub discord_id: String,
    pub fact: String,
    pub created_at: i64,
}

/// A user's stored facts, oldest first (creation order).
pub async fn get_facts(pool: &SqlitePool, discord_id: &str) -> Vec<UserFact> {
    sqlx::query_as::<_, UserFact>(
        "SELECT id, discord_id, fact, created_at FROM user_fact WHERE discord_id = ? ORDER BY id ASC",
    )
    .bind(discord_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default()
}

/// Insert a fact, ignoring exact duplicates (enforced by the unique index).
pub async fn add_fact(
    pool: &SqlitePool,
    discord_id: &str,
    fact: &str,
    created_at: i64,
) -> sqlx::Result<()> {
    sqlx::query("INSERT OR IGNORE INTO user_fact (discord_id, fact, created_at) VALUES (?, ?, ?)")
        .bind(discord_id)
        .bind(fact)
        .bind(created_at)
        .execute(pool)
        .await?;
    Ok(())
}

/// Delete a single fact owned by `discord_id`. Returns true if a row was removed
/// (the ownership check means another user's id can never delete it).
pub async fn delete_fact(pool: &SqlitePool, discord_id: &str, fact_id: i64) -> bool {
    sqlx::query("DELETE FROM user_fact WHERE id = ? AND discord_id = ?")
        .bind(fact_id)
        .bind(discord_id)
        .execute(pool)
        .await
        .map(|r| r.rows_affected() > 0)
        .unwrap_or(false)
}

/// Delete all of a user's facts, returning how many were removed.
pub async fn clear_facts(pool: &SqlitePool, discord_id: &str) -> sqlx::Result<u64> {
    let r = sqlx::query("DELETE FROM user_fact WHERE discord_id = ?")
        .bind(discord_id)
        .execute(pool)
        .await?;
    Ok(r.rows_affected())
}

/// Keep only the `max` newest facts for a user, deleting any older overflow.
pub async fn prune_facts(pool: &SqlitePool, discord_id: &str, max: u32) -> sqlx::Result<()> {
    sqlx::query(
        "DELETE FROM user_fact WHERE discord_id = ? AND id NOT IN \
         (SELECT id FROM user_fact WHERE discord_id = ? ORDER BY id DESC LIMIT ?)",
    )
    .bind(discord_id)
    .bind(discord_id)
    .bind(max as i64)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_opt_out(pool: &SqlitePool, discord_id: &str, opt_out: bool) -> sqlx::Result<()> {
    sqlx::query("UPDATE users SET memory_opt_out = ? WHERE discord_id = ?")
        .bind(opt_out)
        .bind(discord_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn is_opted_out(pool: &SqlitePool, discord_id: &str) -> bool {
    sqlx::query_as::<_, (bool,)>("SELECT memory_opt_out FROM users WHERE discord_id = ?")
        .bind(discord_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .map(|(v,)| v)
        .unwrap_or(false)
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ApiUsage {
    pub user_id: String,
    pub chat_tokens: i64,
    pub memory_tokens: i64,
    pub is_restricted: bool,
}

pub async fn is_user_restricted(pool: &SqlitePool, user_id: &str) -> bool {
    sqlx::query_as::<_, (bool,)>("SELECT is_restricted FROM api_usage WHERE user_id = ?")
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .map(|(v,)| v)
        .unwrap_or(false)
}

pub async fn record_chat_tokens(pool: &SqlitePool, user_id: &str, tokens: u32) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO api_usage (user_id, chat_tokens, is_restricted) VALUES (?, ?, false) \
         ON CONFLICT(user_id) DO UPDATE SET chat_tokens = chat_tokens + excluded.chat_tokens",
    )
    .bind(user_id)
    .bind(tokens as i64)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn record_memory_tokens(
    pool: &SqlitePool,
    user_id: &str,
    tokens: u32,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO api_usage (user_id, memory_tokens, is_restricted) VALUES (?, ?, false) \
         ON CONFLICT(user_id) DO UPDATE SET memory_tokens = memory_tokens + excluded.memory_tokens",
    )
    .bind(user_id)
    .bind(tokens as i64)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn set_user_restricted(
    pool: &SqlitePool,
    user_id: &str,
    restricted: bool,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO api_usage (user_id, is_restricted) VALUES (?, ?) \
         ON CONFLICT(user_id) DO UPDATE SET is_restricted = excluded.is_restricted",
    )
    .bind(user_id)
    .bind(restricted)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_usage_leaderboard(pool: &SqlitePool, limit: u32) -> sqlx::Result<Vec<ApiUsage>> {
    sqlx::query_as::<_, ApiUsage>(
        "SELECT user_id, chat_tokens, memory_tokens, is_restricted FROM api_usage \
         ORDER BY (chat_tokens + memory_tokens) DESC LIMIT ?",
    )
    .bind(limit as i64)
    .fetch_all(pool)
    .await
}
