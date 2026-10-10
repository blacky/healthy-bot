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

pub async fn set_setting(pool: &SqlitePool, key: &str, value: &str) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO setting (k, v) VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v = ?")
        .bind(key)
        .bind(value)
        .bind(value)
        .execute(pool)
        .await?;
    Ok(())
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
/// Returns true if a new fact was inserted, false if it already existed.
pub async fn add_fact(
    pool: &SqlitePool,
    discord_id: &str,
    fact: &str,
    created_at: i64,
) -> sqlx::Result<bool> {
    let res = sqlx::query(
        "INSERT OR IGNORE INTO user_fact (discord_id, fact, created_at) VALUES (?, ?, ?)",
    )
    .bind(discord_id)
    .bind(fact)
    .bind(created_at)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() > 0)
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

/// Delete multiple facts owned by `discord_id`. Returns how many rows were removed.
pub async fn delete_facts_by_ids(
    pool: &SqlitePool,
    discord_id: &str,
    fact_ids: &[i64],
) -> sqlx::Result<u64> {
    if fact_ids.is_empty() {
        return Ok(0);
    }
    let mut query = String::from("DELETE FROM user_fact WHERE discord_id = ? AND id IN (");
    for (i, _) in fact_ids.iter().enumerate() {
        if i > 0 {
            query.push_str(", ");
        }
        query.push('?');
    }
    query.push(')');
    let mut q = sqlx::query(&query).bind(discord_id);
    for &id in fact_ids {
        q = q.bind(id);
    }
    let res = q.execute(pool).await?;
    Ok(res.rows_affected())
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

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Tag {
    pub id: String,
    pub server_id: String,
    pub name: String,
    pub name_normalized: String,
    pub content: String,
    pub asset_filename: Option<String>,
    pub author_id: String,
    pub author_name: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub uses: i64,
}

pub async fn get_tag(pool: &SqlitePool, server_id: &str, name: &str) -> Option<Tag> {
    let name_norm = name.trim().to_lowercase();
    sqlx::query_as::<_, Tag>(
        "SELECT id, server_id, name, name_normalized, content, asset_filename, author_id, author_name, created_at, last_used_at, uses \
         FROM tags WHERE server_id = ? AND name_normalized = ?",
    )
    .bind(server_id)
    .bind(name_norm)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
}

pub async fn search_tag_names(
    pool: &SqlitePool,
    server_id: &str,
    query: &str,
    limit: u32,
) -> Vec<String> {
    let query_norm = format!("{}%", query.trim().to_lowercase());
    sqlx::query_as::<_, (String,)>(
        "SELECT name FROM tags WHERE server_id = ? AND name_normalized LIKE ? ORDER BY uses DESC LIMIT ?",
    )
    .bind(server_id)
    .bind(query_norm)
    .bind(limit as i64)
    .fetch_all(pool)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(n,)| n)
    .collect()
}

pub async fn create_tag(pool: &SqlitePool, tag: &Tag) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO tags (id, server_id, name, name_normalized, content, asset_filename, author_id, author_name, created_at, last_used_at, uses) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&tag.id)
    .bind(&tag.server_id)
    .bind(&tag.name)
    .bind(&tag.name_normalized)
    .bind(&tag.content)
    .bind(&tag.asset_filename)
    .bind(&tag.author_id)
    .bind(&tag.author_name)
    .bind(tag.created_at)
    .bind(tag.last_used_at)
    .bind(tag.uses)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn update_tag(
    pool: &SqlitePool,
    tag_id: &str,
    new_content: &str,
    new_asset: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE tags SET content = ?, asset_filename = ? WHERE id = ?")
        .bind(new_content)
        .bind(new_asset)
        .bind(tag_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_tag(pool: &SqlitePool, tag_id: &str) -> sqlx::Result<bool> {
    let r = sqlx::query("DELETE FROM tags WHERE id = ?")
        .bind(tag_id)
        .execute(pool)
        .await?;
    Ok(r.rows_affected() > 0)
}

pub async fn increment_tag_uses(pool: &SqlitePool, tag_id: &str, used_at: i64) -> sqlx::Result<()> {
    sqlx::query("UPDATE tags SET uses = uses + 1, last_used_at = ? WHERE id = ?")
        .bind(used_at)
        .bind(tag_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_tags(
    pool: &SqlitePool,
    server_id: &str,
    offset: u32,
    limit: u32,
) -> sqlx::Result<Vec<Tag>> {
    sqlx::query_as::<_, Tag>(
        "SELECT id, server_id, name, name_normalized, content, asset_filename, author_id, author_name, created_at, last_used_at, uses \
         FROM tags WHERE server_id = ? ORDER BY uses DESC, name_normalized ASC LIMIT ? OFFSET ?",
    )
    .bind(server_id)
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(pool)
    .await
}

pub async fn count_tags(pool: &SqlitePool, server_id: &str) -> sqlx::Result<i64> {
    let row = sqlx::query_as::<_, (i64,)>("SELECT COUNT(*) FROM tags WHERE server_id = ?")
        .bind(server_id)
        .fetch_one(pool)
        .await?;
    Ok(row.0)
}

pub async fn get_random_tag(pool: &SqlitePool, server_id: &str) -> Option<Tag> {
    sqlx::query_as::<_, Tag>(
        "SELECT id, server_id, name, name_normalized, content, asset_filename, author_id, author_name, created_at, last_used_at, uses \
         FROM tags WHERE server_id = ? ORDER BY RANDOM() LIMIT 1",
    )
    .bind(server_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
}

pub async fn rename_tag(pool: &SqlitePool, tag_id: &str, new_name: &str) -> sqlx::Result<()> {
    let new_name_norm = new_name.trim().to_lowercase();
    sqlx::query("UPDATE tags SET name = ?, name_normalized = ? WHERE id = ?")
        .bind(new_name)
        .bind(new_name_norm)
        .bind(tag_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list_tags_by_author(
    pool: &SqlitePool,
    server_id: &str,
    author_id: &str,
    offset: u32,
    limit: u32,
) -> sqlx::Result<Vec<Tag>> {
    sqlx::query_as::<_, Tag>(
        "SELECT id, server_id, name, name_normalized, content, asset_filename, author_id, author_name, created_at, last_used_at, uses \
         FROM tags WHERE server_id = ? AND author_id = ? ORDER BY uses DESC, name_normalized ASC LIMIT ? OFFSET ?",
    )
    .bind(server_id)
    .bind(author_id)
    .bind(limit as i64)
    .bind(offset as i64)
    .fetch_all(pool)
    .await
}

pub async fn count_tags_by_author(
    pool: &SqlitePool,
    server_id: &str,
    author_id: &str,
) -> sqlx::Result<i64> {
    let row = sqlx::query_as::<_, (i64,)>(
        "SELECT COUNT(*) FROM tags WHERE server_id = ? AND author_id = ?",
    )
    .bind(server_id)
    .bind(author_id)
    .fetch_one(pool)
    .await?;
    Ok(row.0)
}

pub async fn search_tags(
    pool: &SqlitePool,
    server_id: &str,
    query: &str,
    author_id: Option<&str>,
    limit: u32,
) -> sqlx::Result<Vec<Tag>> {
    let pattern = format!("%{}%", query.trim().to_lowercase());
    match author_id {
        Some(aid) => {
            sqlx::query_as::<_, Tag>(
                "SELECT id, server_id, name, name_normalized, content, asset_filename, author_id, author_name, created_at, last_used_at, uses \
                 FROM tags WHERE server_id = ? AND author_id = ? AND (name_normalized LIKE ? OR LOWER(content) LIKE ?) \
                 ORDER BY uses DESC LIMIT ?",
            )
            .bind(server_id)
            .bind(aid)
            .bind(&pattern)
            .bind(&pattern)
            .bind(limit as i64)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, Tag>(
                "SELECT id, server_id, name, name_normalized, content, asset_filename, author_id, author_name, created_at, last_used_at, uses \
                 FROM tags WHERE server_id = ? AND (name_normalized LIKE ? OR LOWER(content) LIKE ?) \
                 ORDER BY uses DESC LIMIT ?",
            )
            .bind(server_id)
            .bind(&pattern)
            .bind(&pattern)
            .bind(limit as i64)
            .fetch_all(pool)
            .await
        }
    }
}

pub async fn is_asset_used_elsewhere(
    pool: &SqlitePool,
    asset_filename: &str,
    excluded_tag_id: &str,
) -> sqlx::Result<bool> {
    let row = sqlx::query_as::<_, (i64,)>(
        "SELECT COUNT(*) FROM tags WHERE asset_filename = ? AND id != ?",
    )
    .bind(asset_filename)
    .bind(excluded_tag_id)
    .fetch_one(pool)
    .await?;
    Ok(row.0 > 0)
}

pub async fn export_all_tags(
    pool: &SqlitePool,
    server_id: &str,
    author_id: Option<&str>,
) -> sqlx::Result<Vec<Tag>> {
    match author_id {
        Some(aid) => {
            sqlx::query_as::<_, Tag>(
                "SELECT id, server_id, name, name_normalized, content, asset_filename, author_id, author_name, created_at, last_used_at, uses \
                 FROM tags WHERE server_id = ? AND author_id = ? ORDER BY name_normalized ASC",
            )
            .bind(server_id)
            .bind(aid)
            .fetch_all(pool)
            .await
        }
        None => {
            sqlx::query_as::<_, Tag>(
                "SELECT id, server_id, name, name_normalized, content, asset_filename, author_id, author_name, created_at, last_used_at, uses \
                 FROM tags WHERE server_id = ? ORDER BY name_normalized ASC",
            )
            .bind(server_id)
            .fetch_all(pool)
            .await
        }
    }
}

pub async fn delete_all_tags(
    pool: &SqlitePool,
    server_id: &str,
    author_id: Option<&str>,
) -> sqlx::Result<(u64, Vec<Option<String>>)> {
    let asset_files: Vec<(Option<String>,)> = match author_id {
        Some(aid) => {
            sqlx::query_as::<_, (Option<String>,)>(
                "SELECT asset_filename FROM tags WHERE server_id = ? AND author_id = ? AND asset_filename IS NOT NULL",
            )
            .bind(server_id)
            .bind(aid)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, (Option<String>,)>(
                "SELECT asset_filename FROM tags WHERE server_id = ? AND asset_filename IS NOT NULL",
            )
            .bind(server_id)
            .fetch_all(pool)
            .await?
        }
    };

    let count = match author_id {
        Some(aid) => sqlx::query("DELETE FROM tags WHERE server_id = ? AND author_id = ?")
            .bind(server_id)
            .bind(aid)
            .execute(pool)
            .await?
            .rows_affected(),
        None => sqlx::query("DELETE FROM tags WHERE server_id = ?")
            .bind(server_id)
            .execute(pool)
            .await?
            .rows_affected(),
    };

    Ok((count, asset_files.into_iter().map(|(f,)| f).collect()))
}

pub async fn get_all_asset_filenames(
    pool: &SqlitePool,
) -> sqlx::Result<std::collections::HashSet<String>> {
    let rows = sqlx::query_as::<_, (String,)>(
        "SELECT DISTINCT asset_filename FROM tags WHERE asset_filename IS NOT NULL",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(f,)| f).collect())
}

pub async fn seed_tags_from_file_if_present(
    pool: &SqlitePool,
) -> Result<Option<std::path::PathBuf>, String> {
    let candidate_paths = if let Ok(custom) = std::env::var("TAG_SEED_FILE") {
        vec![std::path::PathBuf::from(custom)]
    } else {
        vec![
            std::path::PathBuf::from("/healthybot/data/tags_seed.sql"),
            std::path::PathBuf::from("/healthybot/data/seed.sql"),
            std::path::PathBuf::from("data/tags_seed.sql"),
            std::path::PathBuf::from("data/seed.sql"),
        ]
    };

    for path in candidate_paths {
        if path.is_file() {
            log::info!("Found tag seed SQL at {}; applying...", path.display());
            let sql_content = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| e.to_string())?;
            sqlx::raw_sql(&sql_content)
                .execute(pool)
                .await
                .map_err(|e| e.to_string())?;
            log::info!(
                "Successfully seeded tags from {}. Removing seed file...",
                path.display()
            );
            if let Err(e) = tokio::fs::remove_file(&path).await {
                log::warn!("Could not remove seed file {}: {}", path.display(), e);
            }
            return Ok(Some(path));
        }
    }
    Ok(None)
}
