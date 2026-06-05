pub mod commands;
pub mod db;
pub mod markov;
pub mod openai;
pub mod tasks;

use crate::markov::MarkovRepository;
use crate::openai::OpenAIClient;
use sqlx::sqlite::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug)]
pub struct Data {
    pub db_pool: SqlitePool,
    pub markov_repo: MarkovRepository,
    pub openai_client: OpenAIClient,
    pub last_markov: tokio::sync::Mutex<std::time::Instant>,
    pub last_openai: tokio::sync::Mutex<std::time::Instant>,
    pub settings_cache: Arc<RwLock<HashMap<String, String>>>,
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;
