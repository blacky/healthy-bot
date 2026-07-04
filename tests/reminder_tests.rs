use healthy_bot::commands::parse_reminder_input;
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

#[test]
fn test_parse_reminder_input_single() {
    let input = "01-01-2023 12:00 do something";
    let parsed = parse_reminder_input(input);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].message, "do something");
    // 12:00 Amsterdam is 11:00 UTC (Jan 1st)
    assert_eq!(parsed[0].datetime_utc.timestamp(), 1672570800);
}

#[test]
fn test_parse_reminder_input_multiple() {
    let input = "01-01-2023 12:00 first; 02-01-2023 13:00 second";
    let parsed = parse_reminder_input(input);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].message, "first");
    assert_eq!(parsed[1].message, "second");
}

#[test]
fn test_parse_reminder_input_invalid() {
    let input = "invalid input";
    let parsed = parse_reminder_input(input);
    assert!(parsed.is_empty());
}

#[test]
fn test_parse_reminder_input_dst_spring_forward() {
    // In 2024, Amsterdam clocks spring forward on March 31 at 02:00.
    // 02:30:00 does not exist.
    let input = "31-03-2024 02:30 Spring Forward";
    let parsed = parse_reminder_input(input);
    assert_eq!(parsed.len(), 1);
    // Should be shifted to 03:30 local, which is 01:30 UTC
    assert_eq!(
        parsed[0].datetime_utc.format("%Y-%m-%d %H:%M").to_string(),
        "2024-03-31 01:30"
    );
}

#[test]
fn test_parse_reminder_input_dst_fall_backward() {
    // In 2024, Amsterdam clocks fall back on October 27 at 03:00.
    // 02:30:00 happens twice.
    let input = "27-10-2024 02:30 Fall Backward";
    let parsed = parse_reminder_input(input);
    assert_eq!(parsed.len(), 1);
    // Should pick first occurrence (CEST), which is 00:30 UTC
    assert_eq!(
        parsed[0].datetime_utc.format("%Y-%m-%d %H:%M").to_string(),
        "2024-10-27 00:30"
    );
}

#[tokio::test]
async fn test_db_remove_multiple_reminders() {
    let (pool, _temp) = setup_test_db().await;
    let user_id = "user1";

    // Insert some reminders
    sqlx::query(
        "INSERT INTO reminder (id, message, date, owner_discord_id) VALUES (1, 'msg1', 100, ?)",
    )
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO reminder (id, message, date, owner_discord_id) VALUES (2, 'msg2', 200, ?)",
    )
    .bind(user_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO reminder (id, message, date, owner_discord_id) VALUES (3, 'msg3', 300, ?)",
    )
    .bind("other_user")
    .execute(&pool)
    .await
    .unwrap();

    // Verify they exist
    let count: i32 = sqlx::query_scalar("SELECT COUNT(*) FROM reminder")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 3);

    // Simulate multi-remove logic (similar to src/commands.rs)
    let ids_to_remove = vec!["1", "2", "3", "not_a_number"];
    let mut removed = 0;

    for id_str in ids_to_remove {
        if let Ok(id) = id_str.parse::<i64>() {
            let reminder: Option<db::Reminder> =
                sqlx::query_as("SELECT * FROM reminder WHERE id = ?")
                    .bind(id)
                    .fetch_optional(&pool)
                    .await
                    .unwrap();

            if let Some(r) = reminder {
                if r.owner_discord_id == user_id {
                    sqlx::query("DELETE FROM reminder WHERE id = ?")
                        .bind(id)
                        .execute(&pool)
                        .await
                        .unwrap();
                    removed += 1;
                }
            }
        }
    }

    assert_eq!(removed, 2); // Should have removed #1 and #2, but not #3 (owned by other) or "not_a_number"

    let final_count: i32 = sqlx::query_scalar("SELECT COUNT(*) FROM reminder")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(final_count, 1); // Only #3 remains
}
