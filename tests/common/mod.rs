use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};
use std::str::FromStr;
use tempfile::NamedTempFile;

/// Creates a temporary SQLite database backed by a `NamedTempFile` and runs
/// all production migrations on it.
///
/// Returns the connection pool and the temp file handle (which keeps the file alive
/// until dropped).
pub async fn setup_test_db() -> (SqlitePool, NamedTempFile) {
    let temp_file = NamedTempFile::new().expect("Failed to create temporary file for test db");
    let db_path = temp_file
        .path()
        .to_str()
        .expect("Invalid UTF-8 in temp file path");

    let options = SqliteConnectOptions::from_str(&format!("sqlite:{}", db_path))
        .expect("Invalid SQLite connection string")
        .create_if_missing(true);

    let pool = SqlitePool::connect_with(options)
        .await
        .expect("Failed to connect to test SQLite database");

    // Use the production migrations so tests exercise the real schema.
    sqlx::migrate!()
        .run(&pool)
        .await
        .expect("Failed to run migrations on test db");

    (pool, temp_file)
}
