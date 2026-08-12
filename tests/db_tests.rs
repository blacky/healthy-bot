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

    // Use the production migrations so tests exercise the real schema.
    sqlx::migrate!().run(&pool).await.unwrap();

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
async fn test_migrations_run_idempotently() {
    let temp_file = NamedTempFile::new().unwrap();
    let db_path = temp_file.path().to_str().unwrap();
    let options = SqliteConnectOptions::from_str(&format!("sqlite:{}", db_path))
        .unwrap()
        .create_if_missing(true);
    let pool = SqlitePool::connect_with(options).await.unwrap();

    // Fresh database: running it twice must not error (idempotent).
    sqlx::migrate!().run(&pool).await.unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();

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

#[tokio::test]
async fn test_facts_add_dedup_and_get() {
    let (pool, _temp) = setup_test_db().await;
    let uid = "111";

    db::add_fact(&pool, uid, "likes hiking", 1).await.unwrap();
    db::add_fact(&pool, uid, "has a dog", 2).await.unwrap();
    // Exact duplicate is ignored by the unique index.
    db::add_fact(&pool, uid, "likes hiking", 3).await.unwrap();

    let facts = db::get_facts(&pool, uid).await;
    assert_eq!(facts.len(), 2);
    // Oldest first (creation order).
    assert_eq!(facts[0].fact, "likes hiking");
    assert_eq!(facts[1].fact, "has a dog");
}

#[tokio::test]
async fn test_delete_fact_is_scoped_to_owner() {
    let (pool, _temp) = setup_test_db().await;
    db::add_fact(&pool, "111", "secret", 1).await.unwrap();
    let id = db::get_facts(&pool, "111").await[0].id;

    // A different user's id cannot delete it.
    assert!(!db::delete_fact(&pool, "222", id).await);
    assert_eq!(db::get_facts(&pool, "111").await.len(), 1);

    // The owner can.
    assert!(db::delete_fact(&pool, "111", id).await);
    assert!(db::get_facts(&pool, "111").await.is_empty());
}

#[tokio::test]
async fn test_prune_keeps_newest() {
    let (pool, _temp) = setup_test_db().await;
    let uid = "111";
    for i in 0..5 {
        db::add_fact(&pool, uid, &format!("fact {}", i), i)
            .await
            .unwrap();
    }

    db::prune_facts(&pool, uid, 2).await.unwrap();

    let facts = db::get_facts(&pool, uid).await;
    assert_eq!(facts.len(), 2);
    // The two newest (highest ids) survive.
    assert_eq!(facts[0].fact, "fact 3");
    assert_eq!(facts[1].fact, "fact 4");
}

#[tokio::test]
async fn test_opt_out_toggle_and_default() {
    let (pool, _temp) = setup_test_db().await;
    let uid = "111";
    db::create_user_if_not_exists(&pool, uid).await.unwrap();

    // Default is opted in.
    assert!(!db::is_opted_out(&pool, uid).await);
    // Unknown user also reports opted-in.
    assert!(!db::is_opted_out(&pool, "999").await);

    db::set_opt_out(&pool, uid, true).await.unwrap();
    assert!(db::is_opted_out(&pool, uid).await);

    db::set_opt_out(&pool, uid, false).await.unwrap();
    assert!(!db::is_opted_out(&pool, uid).await);
}

#[tokio::test]
async fn test_db_api_usage_tracking() {
    let (pool, _temp) = setup_test_db().await;
    let uid = "1001";

    // Initial state: not restricted
    assert!(!db::is_user_restricted(&pool, uid).await);

    // Record chat tokens
    db::record_chat_tokens(&pool, uid, 150).await.unwrap();
    db::record_chat_tokens(&pool, uid, 50).await.unwrap();

    // Record memory tokens
    db::record_memory_tokens(&pool, uid, 100).await.unwrap();

    let leaderboard = db::get_usage_leaderboard(&pool, 10).await.unwrap();
    assert_eq!(leaderboard.len(), 1);
    assert_eq!(leaderboard[0].user_id, uid);
    assert_eq!(leaderboard[0].chat_tokens, 200);
    assert_eq!(leaderboard[0].memory_tokens, 100);
    assert_eq!(
        leaderboard[0].chat_tokens + leaderboard[0].memory_tokens,
        300
    );
}

#[tokio::test]
async fn test_db_user_restriction() {
    let (pool, _temp) = setup_test_db().await;
    let uid = "1002";

    assert!(!db::is_user_restricted(&pool, uid).await);

    db::set_user_restricted(&pool, uid, true).await.unwrap();
    assert!(db::is_user_restricted(&pool, uid).await);

    db::set_user_restricted(&pool, uid, false).await.unwrap();
    assert!(!db::is_user_restricted(&pool, uid).await);
}

#[tokio::test]
async fn test_db_usage_leaderboard() {
    let (pool, _temp) = setup_test_db().await;

    db::record_chat_tokens(&pool, "userA", 100).await.unwrap();
    db::record_memory_tokens(&pool, "userA", 50).await.unwrap(); // Total 150

    db::record_chat_tokens(&pool, "userB", 500).await.unwrap(); // Total 500

    db::record_memory_tokens(&pool, "userC", 50).await.unwrap(); // Total 50

    let leaderboard = db::get_usage_leaderboard(&pool, 2).await.unwrap();
    assert_eq!(leaderboard.len(), 2);
    assert_eq!(leaderboard[0].user_id, "userB");
    assert_eq!(leaderboard[1].user_id, "userA");
}
