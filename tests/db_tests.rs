use healthy_bot::db;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};
use std::str::FromStr;
use tempfile::NamedTempFile;

async fn setup_test_db() -> (SqlitePool, NamedTempFile) {
    let temp_file = NamedTempFile::new().unwrap();
    let db_path = temp_file.path().to_str().unwrap();
    
    let options = SqliteConnectOptions::from_str(&format!("sqlite:{}", db_path))
        .unwrap()
        .create_if_missing(true);
        
    let pool = SqlitePool::connect_with(options).await.unwrap();

    // Initialize schema
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS users (discord_id TEXT PRIMARY KEY, authorized BOOLEAN, role TEXT, last_message INTEGER);"
    ).execute(&pool).await.unwrap();
    
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS reminder (id INTEGER PRIMARY KEY, message TEXT, date INTEGER, owner_discord_id TEXT);"
    ).execute(&pool).await.unwrap();
    
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS setting (k TEXT PRIMARY KEY, v TEXT);"
    ).execute(&pool).await.unwrap();

    (pool, temp_file)
}

#[tokio::test]
async fn test_db_create_and_get_user() {
    let (pool, _temp) = setup_test_db().await;
    let discord_id = "123456789";

    // Create user
    db::create_user_if_not_exists(&pool, discord_id).await.unwrap();

    // Verify user exists
    let user = db::get_user(&pool, discord_id).await.unwrap();
    assert_eq!(user.discord_id, discord_id);
    assert_eq!(user.authorized, false);
    assert_eq!(user.role, "USER");
}

#[tokio::test]
async fn test_db_update_last_message() {
    let (pool, _temp) = setup_test_db().await;
    let discord_id = "123456789";
    db::create_user_if_not_exists(&pool, discord_id).await.unwrap();

    let now = chrono::Utc::now();
    db::update_last_message(&pool, discord_id, now).await.unwrap();

    let user = db::get_user(&pool, discord_id).await.unwrap();
    assert!(user.last_message.is_some());
    // Compare timestamps in milliseconds to avoid small discrepancies in resolution
    assert_eq!(user.last_message_utc().unwrap().timestamp_millis(), now.timestamp_millis());
}

#[tokio::test]
async fn test_db_settings() {
    let (pool, _temp) = setup_test_db().await;
    
    sqlx::query("INSERT INTO setting (k, v) VALUES (?, ?)")
        .bind("test_key")
        .bind("test_value")
        .execute(&pool)
        .await
        .unwrap();

    let val = db::get_setting(&pool, "test_key").await;
    assert_eq!(val, Some("test_value".to_string()));

    let non_existent = db::get_setting(&pool, "none").await;
    assert_eq!(non_existent, None);
}
