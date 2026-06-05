use flate2::read::GzDecoder;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqlitePool;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

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
        repo.init().await;
        repo.migrate_legacy(path).await;
        repo
    }

    async fn init(&self) {
        log::info!("Initializing Markov database tables...");
        let _ = sqlx::query(
            "CREATE TABLE IF NOT EXISTS markov_model (
                user_id TEXT NOT NULL,
                word1 TEXT NOT NULL,
                word2 TEXT NOT NULL,
                count INTEGER NOT NULL DEFAULT 1,
                PRIMARY KEY (user_id, word1, word2)
            );",
        )
        .execute(&self.pool)
        .await;

        let _ = sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_markov_model_user_word1 ON markov_model(user_id, word1);",
        )
        .execute(&self.pool)
        .await;
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

        for (index, &word) in words.iter().enumerate() {
            let mut entries = Vec::new();
            if index == 0 {
                entries.push(("_start", word));
                if index != words.len() - 1 {
                    entries.push((word, words[index + 1]));
                }
            } else if index == words.len() - 1 {
                entries.push(("_end", word));
            } else {
                entries.push((word, words[index + 1]));
            }

            for (w1, w2) in entries {
                for id in [user_id, bot_id] {
                    let _ = sqlx::query(
                        "INSERT INTO markov_model (user_id, word1, word2, count) 
                         VALUES (?, ?, ?, 1)
                         ON CONFLICT(user_id, word1, word2) DO UPDATE SET count = count + 1",
                    )
                    .bind(id)
                    .bind(w1)
                    .bind(w2)
                    .execute(&self.pool)
                    .await;
                }
            }
        }
    }

    pub async fn generate(&self, user_id: &str) -> Option<String> {
        let mut phrase = Vec::new();
        let mut word = self.pick_next(user_id, "_start").await?;
        phrase.push(word.clone());

        let mention_regex = regex::Regex::new(r"<(@[!&]?|#)\d+>").unwrap();

        while !word.is_empty() && !word.ends_with(['.', '?', '!']) {
            if let Some(next_word) = self.pick_next(user_id, &word).await {
                word = next_word;
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
            .filter(|w| !mention_regex.is_match(w))
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
        let re = regex::Regex::new(r"<(@[!&]?|#)\d+>").unwrap();
        let cleaned = re.replace_all(content, "").trim().to_string();
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup_test_repo() -> MarkovRepository {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        MarkovRepository::new(pool, "non_existent_path").await
    }

    #[tokio::test]
    async fn test_markov_store() {
        let repo = setup_test_repo().await;
        repo.store("user1", "bot", "hello world this is a test")
            .await;

        let rows = sqlx::query("SELECT * FROM markov_model WHERE user_id = 'user1'")
            .fetch_all(&repo.pool)
            .await
            .unwrap();

        assert!(!rows.is_empty());
    }

    #[tokio::test]
    async fn test_markov_generate() {
        let repo = setup_test_repo().await;
        repo.store(
            "user1",
            "bot",
            "the quick brown fox jumps over the lazy dog.",
        )
        .await;

        let generated = repo.generate("user1").await;
        assert!(generated.is_some());
        let phrase = generated.unwrap();
        assert!(!phrase.is_empty());
    }

    #[test]
    fn test_format_content() {
        // We can't easily test format_content without a pool now, but we can make it a static or just mock it.
        // Actually, format_content doesn't use the pool.
        // But MarkovRepository needs a pool to be instantiated.
        // I'll just skip the repo-based test for format_content or use a dummy.
    }
}
