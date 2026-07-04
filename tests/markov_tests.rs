use healthy_bot::markov::MarkovRepository;
use sqlx::sqlite::SqlitePool;

async fn setup_test_repo() -> MarkovRepository {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();
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

// A pair repeated within a single message must be tallied (count = occurrences),
// not written once. "go go go go stop" produces the pair ("go","go") twice.
#[tokio::test]
async fn test_markov_store_dedup_counts_repeated_pairs() {
    let repo = setup_test_repo().await;
    repo.store("user1", "bot", "go go go go stop").await;

    let count: i32 = sqlx::query_scalar(
        "SELECT count FROM markov_model WHERE user_id = 'user1' AND word1 = 'go' AND word2 = 'go'",
    )
    .fetch_one(&repo.pool)
    .await
    .unwrap();
    assert_eq!(count, 2);
}

// Storing the same message twice must accumulate counts via ON CONFLICT, proving
// the transaction commits and the upsert adds the new tally to the existing row.
#[tokio::test]
async fn test_markov_store_accumulates_across_messages() {
    let repo = setup_test_repo().await;
    repo.store("user1", "bot", "hello world foo").await;
    repo.store("user1", "bot", "hello world foo").await;

    let count: i32 = sqlx::query_scalar(
        "SELECT count FROM markov_model WHERE user_id = 'user1' AND word1 = '_start' AND word2 = 'Hello'",
    )
    .fetch_one(&repo.pool)
    .await
    .unwrap();
    assert_eq!(count, 2);
}

// Every pair is written for both the author and the bot id, with identical counts.
#[tokio::test]
async fn test_markov_store_writes_for_both_ids() {
    let repo = setup_test_repo().await;
    repo.store("user1", "botid", "alpha beta gamma").await;

    let user_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM markov_model WHERE user_id = 'user1'")
            .fetch_one(&repo.pool)
            .await
            .unwrap();
    let bot_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM markov_model WHERE user_id = 'botid'")
            .fetch_one(&repo.pool)
            .await
            .unwrap();

    assert!(user_rows > 0);
    assert_eq!(user_rows, bot_rows);
}

// The bot must never train on its own id; storing with user_id == bot_id is a no-op.
#[tokio::test]
async fn test_markov_store_skips_when_user_is_bot() {
    let repo = setup_test_repo().await;
    repo.store("same", "same", "alpha beta gamma").await;

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM markov_model")
        .fetch_one(&repo.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}
