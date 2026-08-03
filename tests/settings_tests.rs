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

    sqlx::migrate!().run(&pool).await.unwrap();

    (pool, temp_file)
}

#[tokio::test]
async fn test_db_settings_read() {
    let (pool, _temp) = setup_test_db().await;

    let db_settings: Vec<db::Setting> = sqlx::query_as::<_, db::Setting>("SELECT * FROM setting")
        .fetch_all(&pool)
        .await
        .unwrap();

    assert!(db_settings.is_empty());
}
