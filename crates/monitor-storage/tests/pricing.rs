use monitor_storage::{
    delete_price, disable_price, load_price_changes, migrate, upsert_price, StoredPrice,
};

#[tokio::test]
async fn price_changes_round_trip_through_override_disable_restore_and_delete() {
    let directory = tempfile::tempdir().unwrap();
    let pool = monitor_storage::connect(&directory.path().join("state.db"))
        .await
        .unwrap();
    migrate(&pool).await.unwrap();
    let first = StoredPrice {
        input: 1,
        output: 2,
        cache_write: 3,
        cache_read: 4,
    };

    upsert_price(&pool, "model-a", first, 10).await.unwrap();
    assert_eq!(
        load_price_changes(&pool).await.unwrap(),
        vec![monitor_storage::StoredPriceChange {
            model_id: "model-a".to_owned(),
            price: Some(first),
        }]
    );

    disable_price(&pool, "model-a", 11).await.unwrap();
    assert_eq!(load_price_changes(&pool).await.unwrap()[0].price, None);

    upsert_price(&pool, "model-a", first, 12).await.unwrap();
    assert_eq!(
        load_price_changes(&pool).await.unwrap()[0].price,
        Some(first)
    );

    delete_price(&pool, "model-a").await.unwrap();
    assert!(load_price_changes(&pool).await.unwrap().is_empty());
}

#[tokio::test]
async fn incomplete_enabled_rates_are_ignored_instead_of_becoming_tombstones() {
    let directory = tempfile::tempdir().unwrap();
    let pool = monitor_storage::connect(&directory.path().join("state.db"))
        .await
        .unwrap();
    migrate(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO price_overrides(model_id,input_pico_usd_per_million,updated_at_ms,disabled)
         VALUES('legacy-model',1,10,0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    assert!(load_price_changes(&pool).await.unwrap().is_empty());
}

#[tokio::test]
async fn writes_and_deletes_target_one_canonical_identity() {
    let directory = tempfile::tempdir().unwrap();
    let pool = monitor_storage::connect(&directory.path().join("state.db"))
        .await
        .unwrap();
    migrate(&pool).await.unwrap();
    let price = StoredPrice {
        input: 1,
        output: 2,
        cache_write: 3,
        cache_read: 4,
    };
    upsert_price(&pool, "custom-model", price, 11)
        .await
        .unwrap();
    assert_eq!(
        load_price_changes(&pool).await.unwrap()[0].model_id,
        "custom-model"
    );

    delete_price(&pool, "custom-model").await.unwrap();
    assert!(load_price_changes(&pool).await.unwrap().is_empty());
}
