mod common;
use common::setup_test_db;
use healthy_bot::db;

#[tokio::test]
async fn test_db_settings_read() {
    let (pool, _temp) = setup_test_db().await;

    let db_settings: Vec<db::Setting> = sqlx::query_as::<_, db::Setting>("SELECT * FROM setting")
        .fetch_all(&pool)
        .await
        .unwrap();

    assert!(db_settings.is_empty());
}
