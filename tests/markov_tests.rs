use healthy_bot::markov::MarkovRepository;
use sqlx::sqlite::SqlitePool;

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
