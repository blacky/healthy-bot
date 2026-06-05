use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Markov {
    pub chain: HashMap<String, HashMap<String, i32>>,
}

impl Markov {
    pub fn add_phrase(&mut self, text: &str, min_words: usize) {
        let words: Vec<&str> = text.split_whitespace().collect();
        if words.len() < min_words {
            return;
        }

        for (index, &word) in words.iter().enumerate() {
            if index == 0 {
                let starts = self.chain.entry("_start".to_string()).or_default();
                *starts.entry(word.to_string()).or_insert(0) += 1;

                if index != words.len() - 1 {
                    let suffix = self.chain.entry(word.to_string()).or_default();
                    let next_word = words[index + 1];
                    *suffix.entry(next_word.to_string()).or_insert(0) += 1;
                }
            } else if index == words.len() - 1 {
                let ends = self.chain.entry("_end".to_string()).or_default();
                *ends.entry(word.to_string()).or_insert(0) += 1;
            } else {
                let suffix = self.chain.entry(word.to_string()).or_default();
                let next_word = words[index + 1];
                *suffix.entry(next_word.to_string()).or_insert(0) += 1;
            }
        }
    }

    pub fn generate(&self) -> Option<String> {
        let mut phrase = Vec::new();
        let mut word = self.pick_random(self.chain.get("_start")?)?;
        phrase.push(word.clone());

        let mention_regex = regex::Regex::new(r"<(@[!&]?|#)\d+>").unwrap();

        while !word.is_empty() && !word.ends_with(['.', '?', '!']) {
            if let Some(next_map) = self.chain.get(&word) {
                word = self.pick_random(next_map)?;
                phrase.push(word.clone());
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
            log::warn!("Markov generated an empty phrase after filtering mentions");
            return None;
        }

        Some(result)
    }

    fn pick_random(&self, frequencies: &HashMap<String, i32>) -> Option<String> {
        if frequencies.is_empty() {
            return None;
        }
        let total: i32 = frequencies.values().sum();
        if total <= 0 {
            return None;
        }
        let mut random = rand::thread_rng().gen_range(1..=total);
        for (word, count) in frequencies {
            random -= count;
            if random <= 0 {
                return Some(word.clone());
            }
        }
        frequencies.keys().next().cloned()
    }
}

#[derive(Debug)]
pub struct MarkovRepository {
    pub storage_path: String,
    pub markovs: std::sync::Arc<tokio::sync::RwLock<HashMap<String, Markov>>>,
    pub last_save: std::sync::Arc<tokio::sync::Mutex<std::time::Instant>>,
}

impl MarkovRepository {
    pub fn new(path: &str) -> Self {
        let markovs = Self::load(path).unwrap_or_default();
        Self {
            storage_path: path.to_string(),
            markovs: std::sync::Arc::new(tokio::sync::RwLock::new(markovs)),
            last_save: std::sync::Arc::new(tokio::sync::Mutex::new(std::time::Instant::now())),
        }
    }

    fn load(path: &str) -> Option<HashMap<String, Markov>> {
        let file = File::open(path).ok()?;
        let mut decoder = GzDecoder::new(file);
        let mut json = String::new();
        decoder.read_to_string(&mut json).ok()?;
        serde_json::from_str(&json).ok()
    }

    pub async fn save(&self) {
        let markovs = self.markovs.read().await;
        let json = match serde_json::to_string(&*markovs) {
            Ok(j) => j,
            Err(_) => return,
        };
        drop(markovs);

        let path = self.storage_path.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(file) = File::create(path) {
                let mut encoder = GzEncoder::new(file, Compression::default());
                let _ = encoder.write_all(json.as_bytes());
                let _ = encoder.finish();
            }
        })
        .await;
    }

    pub async fn store(&self, user_id: &str, bot_id: &str, content: &str) {
        if user_id == bot_id {
            return;
        }

        let mut markovs = self.markovs.write().await;
        let formatted = self.format_content(content);

        markovs
            .entry(user_id.to_string())
            .or_default()
            .add_phrase(&formatted, 3);
        markovs
            .entry(bot_id.to_string())
            .or_default()
            .add_phrase(&formatted, 3);

        let mut last_save = self.last_save.lock().await;
        if last_save.elapsed().as_secs() > 15 * 60 {
            let json = serde_json::to_string(&*markovs).unwrap_or_default();
            drop(markovs);
            let path = self.storage_path.clone();
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(file) = File::create(path) {
                    let mut encoder = GzEncoder::new(file, Compression::default());
                    let _ = encoder.write_all(json.as_bytes());
                    let _ = encoder.finish();
                }
            });
            *last_save = std::time::Instant::now();
        }
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
