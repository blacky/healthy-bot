use healthy_bot::db::{self, Tag};
use healthy_bot::tags::{evaluate_tag, TagContext};
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
async fn test_create_and_get_tag() {
    let (pool, _temp) = setup_test_db().await;
    let server_id = "182937695515967488";

    let tag = Tag {
        id: "1001".into(),
        server_id: server_id.into(),
        name: "didanyoneask?".into(),
        name_normalized: "didanyoneask?".into(),
        content: "No.".into(),
        asset_filename: None,
        author_id: "155763335369981953".into(),
        author_name: "java11".into(),
        created_at: 1774388140305,
        last_used_at: None,
        uses: 0,
    };

    db::create_tag(&pool, &tag).await.unwrap();

    // Exact lowercase lookup
    let fetched = db::get_tag(&pool, server_id, "didanyoneask?")
        .await
        .unwrap();
    assert_eq!(fetched.id, "1001");
    assert_eq!(fetched.content, "No.");

    // Case-insensitive lookup
    let fetched_caps = db::get_tag(&pool, server_id, "DidAnyoneAsk?")
        .await
        .unwrap();
    assert_eq!(fetched_caps.id, "1001");

    // Missing punctuation does not match
    let missing_q = db::get_tag(&pool, server_id, "didanyoneask").await;
    assert!(missing_q.is_none());
}

#[tokio::test]
async fn test_special_character_and_emoji_tags() {
    let (pool, _temp) = setup_test_db().await;
    let server_id = "182937695515967488";

    let emoji_tag = Tag {
        id: "1002".into(),
        server_id: server_id.into(),
        name: "🥸".into(),
        name_normalized: "🥸".into(),
        content: "disguise face".into(),
        asset_filename: None,
        author_id: "999".into(),
        author_name: "tester".into(),
        created_at: 1000,
        last_used_at: None,
        uses: 0,
    };
    db::create_tag(&pool, &emoji_tag).await.unwrap();

    let fetched = db::get_tag(&pool, server_id, "🥸").await.unwrap();
    assert_eq!(fetched.content, "disguise face");
}

#[tokio::test]
async fn test_tag_uses_and_update_and_delete() {
    let (pool, _temp) = setup_test_db().await;
    let server_id = "182937695515967488";

    let tag = Tag {
        id: "1003".into(),
        server_id: server_id.into(),
        name: "easter".into(),
        name_normalized: "easter".into(),
        content: "".into(),
        asset_filename: Some("easter.png".into()),
        author_id: "123".into(),
        author_name: "em64t".into(),
        created_at: 2000,
        last_used_at: None,
        uses: 10,
    };
    db::create_tag(&pool, &tag).await.unwrap();

    // Increment uses
    db::increment_tag_uses(&pool, "1003", 5000).await.unwrap();
    let updated = db::get_tag(&pool, server_id, "easter").await.unwrap();
    assert_eq!(updated.uses, 11);
    assert_eq!(updated.last_used_at, Some(5000));

    // Update content
    db::update_tag(&pool, "1003", "new easter text", Some("easter_v2.png"))
        .await
        .unwrap();
    let updated2 = db::get_tag(&pool, server_id, "easter").await.unwrap();
    assert_eq!(updated2.content, "new easter text");
    assert_eq!(updated2.asset_filename, Some("easter_v2.png".into()));

    // Delete tag
    let deleted = db::delete_tag(&pool, "1003").await.unwrap();
    assert!(deleted);
    assert!(db::get_tag(&pool, server_id, "easter").await.is_none());
}

#[tokio::test]
async fn test_tag_autocomplete_search() {
    let (pool, _temp) = setup_test_db().await;
    let server_id = "182937695515967488";

    let t1 = Tag {
        id: "1".into(),
        server_id: server_id.into(),
        name: "kika".into(),
        name_normalized: "kika".into(),
        content: "pic 1".into(),
        asset_filename: None,
        author_id: "1".into(),
        author_name: "user".into(),
        created_at: 1,
        last_used_at: None,
        uses: 50,
    };
    let t2 = Tag {
        id: "2".into(),
        server_id: server_id.into(),
        name: "kika2".into(),
        name_normalized: "kika2".into(),
        content: "pic 2".into(),
        asset_filename: None,
        author_id: "1".into(),
        author_name: "user".into(),
        created_at: 2,
        last_used_at: None,
        uses: 100, // higher uses
    };
    let t3 = Tag {
        id: "3".into(),
        server_id: server_id.into(),
        name: "other".into(),
        name_normalized: "other".into(),
        content: "other".into(),
        asset_filename: None,
        author_id: "1".into(),
        author_name: "user".into(),
        created_at: 3,
        last_used_at: None,
        uses: 10,
    };

    db::create_tag(&pool, &t1).await.unwrap();
    db::create_tag(&pool, &t2).await.unwrap();
    db::create_tag(&pool, &t3).await.unwrap();

    let suggestions = db::search_tag_names(&pool, server_id, "ki", 10).await;
    assert_eq!(suggestions.len(), 2);
    // Ordered by uses DESC: kika2 (100) first, kika (50) second
    assert_eq!(suggestions[0], "kika2");
    assert_eq!(suggestions[1], "kika");
}

#[tokio::test]
async fn test_get_random_tag() {
    let (pool, _temp) = setup_test_db().await;
    let server_id = "182937695515967488";

    // When empty
    assert!(db::get_random_tag(&pool, server_id).await.is_none());

    let tag = Tag {
        id: "1".into(),
        server_id: server_id.into(),
        name: "my_tag".into(),
        name_normalized: "my_tag".into(),
        content: "content".into(),
        asset_filename: None,
        author_id: "1".into(),
        author_name: "user".into(),
        created_at: 1,
        last_used_at: None,
        uses: 0,
    };
    db::create_tag(&pool, &tag).await.unwrap();

    let random_tag = db::get_random_tag(&pool, server_id).await.unwrap();
    assert_eq!(random_tag.name, "my_tag");
}

#[test]
fn test_template_evaluator_with_arguments() {
    let args = vec!["target_user".to_string()];
    let ctx = TagContext {
        author_mention: "<@123>".into(),
        author_name: "Sender".into(),
        args: &args,
    };

    let raw = "{if:{arg:0}|=||then:Take your meds!|else:{arg:0} take your meds!}";
    let evaluated = evaluate_tag(raw, &ctx, None);
    assert_eq!(evaluated.text, "target_user take your meds!");
}

#[tokio::test]
async fn test_rename_tag() {
    let (pool, _temp) = setup_test_db().await;
    let server_id = "182937695515967488";

    let tag = Tag {
        id: "1004".into(),
        server_id: server_id.into(),
        name: "oldname".into(),
        name_normalized: "oldname".into(),
        content: "hello world".into(),
        asset_filename: None,
        author_id: "111".into(),
        author_name: "author1".into(),
        created_at: 100,
        last_used_at: None,
        uses: 5,
    };
    db::create_tag(&pool, &tag).await.unwrap();

    // Verify initial tag
    assert!(db::get_tag(&pool, server_id, "oldname").await.is_some());

    // Rename
    db::rename_tag(&pool, "1004", "NewName").await.unwrap();

    // Old name no longer found
    assert!(db::get_tag(&pool, server_id, "oldname").await.is_none());

    // New name found (case-insensitive)
    let fetched = db::get_tag(&pool, server_id, "newname").await.unwrap();
    assert_eq!(fetched.name, "NewName");
    assert_eq!(fetched.name_normalized, "newname");
    assert_eq!(fetched.content, "hello world");
}

#[tokio::test]
async fn test_search_and_list_by_author() {
    let (pool, _temp) = setup_test_db().await;
    let server_id = "182937695515967488";

    let t1 = Tag {
        id: "1".into(),
        server_id: server_id.into(),
        name: "cottoncat".into(),
        name_normalized: "cottoncat".into(),
        content: "cute white cat".into(),
        asset_filename: None,
        author_id: "user_a".into(),
        author_name: "User A".into(),
        created_at: 10,
        last_used_at: None,
        uses: 50,
    };
    let t2 = Tag {
        id: "2".into(),
        server_id: server_id.into(),
        name: "cottondog".into(),
        name_normalized: "cottondog".into(),
        content: "woof".into(),
        asset_filename: None,
        author_id: "user_b".into(),
        author_name: "User B".into(),
        created_at: 20,
        last_used_at: None,
        uses: 30,
    };
    let t3 = Tag {
        id: "3".into(),
        server_id: server_id.into(),
        name: "blackcat".into(),
        name_normalized: "blackcat".into(),
        content: "purr".into(),
        asset_filename: None,
        author_id: "user_a".into(),
        author_name: "User A".into(),
        created_at: 30,
        last_used_at: None,
        uses: 10,
    };

    db::create_tag(&pool, &t1).await.unwrap();
    db::create_tag(&pool, &t2).await.unwrap();
    db::create_tag(&pool, &t3).await.unwrap();

    // Search query "cotton" without author filter
    let results = db::search_tags(&pool, server_id, "cotton", None, 10)
        .await
        .unwrap();
    assert_eq!(results.len(), 2);

    // Search query "cotton" filtered to user_a
    let results_a = db::search_tags(&pool, server_id, "cotton", Some("user_a"), 10)
        .await
        .unwrap();
    assert_eq!(results_a.len(), 1);
    assert_eq!(results_a[0].name, "cottoncat");

    // List and count by author
    let count_a = db::count_tags_by_author(&pool, server_id, "user_a")
        .await
        .unwrap();
    assert_eq!(count_a, 2);
    let list_a = db::list_tags_by_author(&pool, server_id, "user_a", 0, 10)
        .await
        .unwrap();
    assert_eq!(list_a.len(), 2);
    assert_eq!(list_a[0].name, "cottoncat"); // sorted by uses DESC
    assert_eq!(list_a[1].name, "blackcat");
}

#[tokio::test]
async fn test_export_and_delete_all_tags() {
    let (pool, _temp) = setup_test_db().await;
    let server_id = "182937695515967488";

    let t1 = Tag {
        id: "1".into(),
        server_id: server_id.into(),
        name: "tag1".into(),
        name_normalized: "tag1".into(),
        content: "content1".into(),
        asset_filename: Some("shared.png".into()),
        author_id: "user1".into(),
        author_name: "User 1".into(),
        created_at: 1,
        last_used_at: None,
        uses: 1,
    };
    let t2 = Tag {
        id: "2".into(),
        server_id: server_id.into(),
        name: "tag2".into(),
        name_normalized: "tag2".into(),
        content: "content2".into(),
        asset_filename: Some("shared.png".into()),
        author_id: "user2".into(),
        author_name: "User 2".into(),
        created_at: 2,
        last_used_at: None,
        uses: 2,
    };

    db::create_tag(&pool, &t1).await.unwrap();
    db::create_tag(&pool, &t2).await.unwrap();

    // Test asset sharing detection
    let shared = db::is_asset_used_elsewhere(&pool, "shared.png", "1")
        .await
        .unwrap();
    assert!(shared); // tag2 also uses it!

    // Export all tags
    let all = db::export_all_tags(&pool, server_id, None).await.unwrap();
    assert_eq!(all.len(), 2);

    // Export tags by user1
    let user1_tags = db::export_all_tags(&pool, server_id, Some("user1"))
        .await
        .unwrap();
    assert_eq!(user1_tags.len(), 1);
    assert_eq!(user1_tags[0].name, "tag1");

    // Delete user1 tags
    let (deleted_user1, assets) = db::delete_all_tags(&pool, server_id, Some("user1"))
        .await
        .unwrap();
    assert_eq!(deleted_user1, 1);
    assert_eq!(assets.len(), 1);

    // Now asset is not used elsewhere for tag2
    let still_shared = db::is_asset_used_elsewhere(&pool, "shared.png", "2")
        .await
        .unwrap();
    assert!(!still_shared);

    // Delete all remaining tags
    let (deleted_all, _) = db::delete_all_tags(&pool, server_id, None).await.unwrap();
    assert_eq!(deleted_all, 1);
    assert_eq!(db::count_tags(&pool, server_id).await.unwrap(), 0);
}

#[tokio::test]
async fn test_get_all_asset_filenames() {
    let (pool, _temp) = setup_test_db().await;
    let server_id = "182937695515967488";

    let t1 = Tag {
        id: "1".into(),
        server_id: server_id.into(),
        name: "tag1".into(),
        name_normalized: "tag1".into(),
        content: "content1".into(),
        asset_filename: Some("asset1.png".into()),
        author_id: "user1".into(),
        author_name: "User 1".into(),
        created_at: 1,
        last_used_at: None,
        uses: 1,
    };
    let t2 = Tag {
        id: "2".into(),
        server_id: server_id.into(),
        name: "tag2".into(),
        name_normalized: "tag2".into(),
        content: "content2".into(),
        asset_filename: None,
        author_id: "user2".into(),
        author_name: "User 2".into(),
        created_at: 2,
        last_used_at: None,
        uses: 2,
    };
    db::create_tag(&pool, &t1).await.unwrap();
    db::create_tag(&pool, &t2).await.unwrap();

    let assets = db::get_all_asset_filenames(&pool).await.unwrap();
    assert_eq!(assets.len(), 1);
    assert!(assets.contains("asset1.png"));
}
