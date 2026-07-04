use flate2::read::GzDecoder;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePool;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;

/// Matches Discord mentions (`<@id>`, `<@!id>`, `<@&id>`, `<#id>`). Compiled once
/// and shared, since it's used on every stored message and every generation.
static MENTION_REGEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"<(@[!&]?|#)\d+>").unwrap());

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Markov {
    pub chain: HashMap<String, HashMap<String, i32>>,
}

#[derive(Debug, Clone)]
pub struct MarkovRepository {
    pub pool: SqlitePool,
}

impl MarkovRepository {
    pub async fn new(pool: SqlitePool, path: &str) -> Self {
        let repo = Self { pool };
        repo.migrate_legacy(path).await;
        repo
    }

    async fn migrate_legacy(&self, path: &str) {
        if !Path::new(path).exists() {
            return;
        }

        log::info!(
            "Legacy Markov file found at {}, starting migration...",
            path
        );

        let file = match File::open(path) {
            Ok(f) => f,
            Err(_) => return,
        };
        let mut decoder = GzDecoder::new(file);
        let mut json = String::new();
        if decoder.read_to_string(&mut json).is_err() {
            log::error!("Failed to decode legacy Markov file");
            return;
        }

        let markovs: HashMap<String, Markov> = match serde_json::from_str(&json) {
            Ok(m) => m,
            Err(e) => {
                log::error!("Failed to parse legacy Markov JSON: {}", e);
                return;
            }
        };

        log::info!("Migrating Markov data for {} users...", markovs.len());

        let mut tx = match self.pool.begin().await {
            Ok(t) => t,
            Err(e) => {
                log::error!("Failed to start migration transaction: {}", e);
                return;
            }
        };

        for (user_id, markov) in markovs {
            for (word1, next_map) in markov.chain {
                for (word2, count) in next_map {
                    let _ = sqlx::query(
                        "INSERT INTO markov_model (user_id, word1, word2, count) 
                         VALUES (?, ?, ?, ?)
                         ON CONFLICT(user_id, word1, word2) DO UPDATE SET count = count + excluded.count",
                    )
                    .bind(&user_id)
                    .bind(&word1)
                    .bind(&word2)
                    .bind(count)
                    .execute(&mut *tx)
                    .await;
                }
            }
        }

        if let Err(e) = tx.commit().await {
            log::error!("Failed to commit Markov migration transaction: {}", e);
        } else {
            log::info!("Migration successful. Cleaning up empty or whitespace Markov entries...");
            let res = sqlx::query(
                "DELETE FROM markov_model WHERE word1 = '' OR word2 = '' OR TRIM(word1) = '' OR TRIM(word2) = ''"
            )
            .execute(&self.pool)
            .await;

            if let Ok(r) = res {
                if r.rows_affected() > 0 {
                    log::info!("Cleaned up {} empty Markov entries.", r.rows_affected());
                }
            }
            log::info!("Migration successful. Renaming legacy file.");
            let _ = std::fs::rename(path, format!("{}.migrated", path));
        }
    }

    pub async fn store(&self, user_id: &str, bot_id: &str, content: &str) {
        if user_id == bot_id {
            return;
        }

        let formatted = self.format_content(content);
        let words: Vec<&str> = formatted.split_whitespace().collect();
        if words.len() < 3 {
            return;
        }

        // Tally this message's word pairs in memory first. This collapses any
        // pair repeated within the message into a single row, and lets us write
        // every pair in one transaction instead of one round-trip per pair.
        // The map is scoped to this single message and dropped on return.
        let mut pair_counts: HashMap<(&str, &str), i32> = HashMap::new();
        for (index, &word) in words.iter().enumerate() {
            if word.is_empty() {
                continue;
            }

            let mut entries = Vec::new();
            if index == 0 {
                entries.push(("_start", word));
                if index < words.len() - 1 {
                    entries.push((word, words[index + 1]));
                }
            } else if index == words.len() - 1 {
                entries.push((word, "_end"));
            } else {
                entries.push((word, words[index + 1]));
            }

            for (w1, w2) in entries {
                if w1.is_empty() || w2.is_empty() {
                    continue;
                }
                *pair_counts.entry((w1, w2)).or_insert(0) += 1;
            }
        }

        if pair_counts.is_empty() {
            return;
        }

        // Write everything in a single transaction: one commit / disk flush for
        // the whole message instead of one per pair.
        let mut tx = match self.pool.begin().await {
            Ok(t) => t,
            Err(e) => {
                log::error!("Failed to start Markov store transaction: {}", e);
                return;
            }
        };

        for id in [user_id, bot_id] {
            for (&(w1, w2), &count) in &pair_counts {
                let _ = sqlx::query(
                    "INSERT INTO markov_model (user_id, word1, word2, count)
                     VALUES (?, ?, ?, ?)
                     ON CONFLICT(user_id, word1, word2) DO UPDATE SET count = count + excluded.count",
                )
                .bind(id)
                .bind(w1)
                .bind(w2)
                .bind(count)
                .execute(&mut *tx)
                .await;
            }
        }

        if let Err(e) = tx.commit().await {
            log::error!("Failed to commit Markov store transaction: {}", e);
        }
    }

    pub async fn generate(&self, user_id: &str) -> Option<String> {
        let mut phrase = Vec::new();
        let mut word = self.pick_next(user_id, "_start").await?;
        if word.is_empty() || word == "_end" {
            return None;
        }
        phrase.push(word.clone());

        while !word.is_empty() && !word.ends_with(['.', '?', '!']) && word != "_end" {
            if let Some(next_word) = self.pick_next(user_id, &word).await {
                word = next_word;
                if word == "_end" || word.is_empty() {
                    break;
                }
                phrase.push(word.clone());
                if phrase.len() > 50 {
                    break;
                }
            } else {
                break;
            }
        }

        let result = phrase
            .into_iter()
            .filter(|w| !MENTION_REGEX.is_match(w) && w != "_end" && !w.is_empty())
            .collect::<Vec<_>>()
            .join(" ");

        if result.trim().is_empty() {
            return None;
        }

        Some(result)
    }

    async fn pick_next(&self, user_id: &str, word1: &str) -> Option<String> {
        let rows = sqlx::query_as::<_, (String, i32)>(
            "SELECT word2, count FROM markov_model WHERE user_id = ? AND word1 = ?",
        )
        .bind(user_id)
        .bind(word1)
        .fetch_all(&self.pool)
        .await
        .ok()?;

        if rows.is_empty() {
            return None;
        }

        let total: i32 = rows.iter().map(|(_, c)| c).sum();
        if total <= 0 {
            return None;
        }

        let mut random = rand::thread_rng().gen_range(1..=total);
        for (word, count) in rows {
            random -= count;
            if random <= 0 {
                return Some(word);
            }
        }
        None
    }

    fn format_content(&self, content: &str) -> String {
        let cleaned = MENTION_REGEX.replace_all(content, "").trim().to_string();
        if cleaned.is_empty() {
            return String::new();
        }

        let mut capitalized = cleaned;
        if let Some(first) = capitalized.get_mut(0..1) {
            first.make_ascii_uppercase();
        }

        if !capitalized.ends_with(['.', '?', '!']) {
            capitalized.push('.');
        }
        capitalized
    }
}
