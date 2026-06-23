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

    // Use the production schema initializer so tests exercise the real schema.
    db::init_schema(&pool).await.unwrap();

    (pool, temp_file)
}

#[tokio::test]
async fn test_db_create_and_get_user() {
    let (pool, _temp) = setup_test_db().await;
    let discord_id = "123456789";

    // Create user
    db::create_user_if_not_exists(&pool, discord_id)
        .await
        .unwrap();

    // Verify user exists
    let user = db::get_user(&pool, discord_id).await.unwrap();
    assert_eq!(user.discord_id, discord_id);
    assert!(!user.authorized);
    assert_eq!(user.role, "USER");
}

#[tokio::test]
async fn test_db_update_last_message() {
    let (pool, _temp) = setup_test_db().await;
    let discord_id = "123456789";
    db::create_user_if_not_exists(&pool, discord_id)
        .await
        .unwrap();

    let now = chrono::Utc::now();
    db::update_last_message(&pool, discord_id, now)
        .await
        .unwrap();

    let user = db::get_user(&pool, discord_id).await.unwrap();
    assert!(user.last_message.is_some());
    // Compare timestamps in milliseconds to avoid small discrepancies in resolution
    assert_eq!(
        user.last_message_utc().unwrap().timestamp_millis(),
        now.timestamp_millis()
    );
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

#[tokio::test]
async fn test_init_schema_creates_core_tables_idempotently() {
    let temp_file = NamedTempFile::new().unwrap();
    let db_path = temp_file.path().to_str().unwrap();
    let options = SqliteConnectOptions::from_str(&format!("sqlite:{}", db_path))
        .unwrap()
        .create_if_missing(true);
    let pool = SqlitePool::connect_with(options).await.unwrap();

    // Fresh database: running it twice must not error (idempotent).
    db::init_schema(&pool).await.unwrap();
    db::init_schema(&pool).await.unwrap();

    // users table works.
    db::create_user_if_not_exists(&pool, "42").await.unwrap();
    assert!(db::get_user(&pool, "42").await.is_some());

    // reminder table works, including the auto-increment id (the production
    // INSERT omits id) that a missing schema previously broke.
    sqlx::query("INSERT INTO reminder (message, date, owner_discord_id) VALUES ('hi', 1, '42')")
        .execute(&pool)
        .await
        .unwrap();
    let id: i64 = sqlx::query_scalar("SELECT id FROM reminder WHERE owner_discord_id = '42'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(id > 0);

    // setting table works.
    sqlx::query("INSERT INTO setting (k, v) VALUES ('x', 'y')")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(db::get_setting(&pool, "x").await, Some("y".to_string()));
}

#[test]
fn test_to_utc_seconds() {
    let ts = 1672531200; // 2023-01-01 00:00:00 UTC
    let dt = db::to_utc(ts);
    assert_eq!(dt.timestamp(), ts);
    assert_eq!(dt.timestamp_subsec_millis(), 0);
}

#[test]
fn test_to_utc_milliseconds() {
    let ts = 1672531200123; // 2023-01-01 00:00:00.123 UTC
    let dt = db::to_utc(ts);
    assert_eq!(dt.timestamp(), 1672531200);
    assert_eq!(dt.timestamp_subsec_millis(), 123);
}

#[test]
fn test_to_utc_boundary() {
    // Just below 10_000_000_000 should be treated as seconds
    let ts = 9_999_999_999;
    let dt = db::to_utc(ts);
    assert_eq!(dt.timestamp(), ts);

    // Exactly 10_000_000_001 should be treated as milliseconds
    let ts_ms = 10_000_000_001;
    let dt_ms = db::to_utc(ts_ms);
    assert_eq!(dt_ms.timestamp(), 10_000_000);
    assert_eq!(dt_ms.timestamp_subsec_millis(), 1);
}
