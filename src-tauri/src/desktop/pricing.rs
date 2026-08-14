use super::{now_ms, DesktopState};
use adapter_claude::pricing::{decimal_usd_to_pico, Price, PriceCatalog};
use monitor_storage::StoredPrice;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tauri::{AppHandle, State};

const PRICE_STORAGE_FAILED: &str = "price_storage_failed";
const PRICE_MODEL_ID_REQUIRED: &str = "price_model_id_required";
const PRICE_MODEL_ID_TOO_LONG: &str = "price_model_id_too_long";
const PRICE_INPUT_INVALID: &str = "price_input_invalid";
const PRICE_INPUT_OUT_OF_RANGE: &str = "price_input_out_of_range";
const PRICE_OUTPUT_INVALID: &str = "price_output_invalid";
const PRICE_OUTPUT_OUT_OF_RANGE: &str = "price_output_out_of_range";
const PRICE_CACHE_WRITE_INVALID: &str = "price_cache_write_invalid";
const PRICE_CACHE_WRITE_OUT_OF_RANGE: &str = "price_cache_write_out_of_range";
const PRICE_CACHE_READ_INVALID: &str = "price_cache_read_invalid";
const PRICE_CACHE_READ_OUT_OF_RANGE: &str = "price_cache_read_out_of_range";
const MAX_MODEL_ID_LEN: usize = 256;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelPriceDto {
    model_id: String,
    input_cost_per_million: String,
    output_cost_per_million: String,
    cache_write_cost_per_million: String,
    cache_read_cost_per_million: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SaveModelPriceDto {
    model_id: String,
    input_cost_per_million: String,
    output_cost_per_million: String,
    cache_write_cost_per_million: String,
    cache_read_cost_per_million: String,
}

async fn changes(pool: &SqlitePool) -> anyhow::Result<Vec<(String, Option<Price>)>> {
    let stored = monitor_storage::load_price_changes(pool).await?;
    Ok(stored
        .into_iter()
        .map(|change| (change.model_id, change.price.map(stored_price)))
        .collect())
}

pub(super) async fn catalog(pool: &SqlitePool) -> anyhow::Result<PriceCatalog> {
    Ok(PriceCatalog::with_changes(changes(pool).await?))
}

async fn list(pool: &SqlitePool) -> anyhow::Result<Vec<ModelPriceDto>> {
    let stored = monitor_storage::load_price_changes(pool).await?;
    let changes = stored
        .into_iter()
        .map(|change| (change.model_id, change.price.map(stored_price)))
        .collect::<BTreeMap<_, _>>();
    let builtins = PriceCatalog::builtin_prices();
    let model_ids = builtins
        .keys()
        .chain(changes.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    Ok(model_ids
        .into_iter()
        .filter_map(|model_id| {
            let builtin = builtins.get(&model_id).copied();
            let price = match changes.get(&model_id) {
                Some(change) => (*change)?,
                None => builtin?,
            };
            Some(ModelPriceDto {
                model_id,
                input_cost_per_million: pico_to_usd(price.input),
                output_cost_per_million: pico_to_usd(price.output),
                cache_write_cost_per_million: pico_to_usd(price.cache_write),
                cache_read_cost_per_million: pico_to_usd(price.cache_read),
            })
        })
        .collect())
}

#[tauri::command]
pub(crate) async fn list_model_prices(
    state: State<'_, Arc<DesktopState>>,
) -> Result<Vec<ModelPriceDto>, String> {
    list(&state.pool)
        .await
        .map_err(|_| PRICE_STORAGE_FAILED.to_owned())
}

#[tauri::command]
pub(crate) async fn save_model_price(
    app: AppHandle,
    price: SaveModelPriceDto,
    state: State<'_, Arc<DesktopState>>,
) -> Result<ModelPriceDto, String> {
    let saved = save(&state.pool, price, now_ms()).await?;
    state.invalidate_tray(&app);
    Ok(saved)
}

async fn save(
    pool: &SqlitePool,
    value: SaveModelPriceDto,
    updated_at_ms: i64,
) -> Result<ModelPriceDto, String> {
    let (model_id, price) = validate(value)?;
    monitor_storage::upsert_price(
        pool,
        &model_id,
        StoredPrice {
            input: price.input,
            output: price.output,
            cache_write: price.cache_write,
            cache_read: price.cache_read,
        },
        updated_at_ms,
    )
    .await
    .map_err(|_| PRICE_STORAGE_FAILED.to_owned())?;
    Ok(ModelPriceDto {
        model_id,
        input_cost_per_million: pico_to_usd(price.input),
        output_cost_per_million: pico_to_usd(price.output),
        cache_write_cost_per_million: pico_to_usd(price.cache_write),
        cache_read_cost_per_million: pico_to_usd(price.cache_read),
    })
}

#[tauri::command]
pub(crate) async fn delete_model_price(
    app: AppHandle,
    model_id: String,
    state: State<'_, Arc<DesktopState>>,
) -> Result<String, String> {
    let deleted = delete(&state.pool, &model_id, now_ms())
        .await
        .map_err(|error| error.to_owned())?;
    state.invalidate_tray(&app);
    Ok(deleted)
}

async fn delete(
    pool: &SqlitePool,
    model_id: &str,
    updated_at_ms: i64,
) -> Result<String, &'static str> {
    let model_id = canonical_model_id(model_id)?;
    let current =
        PriceCatalog::with_changes(changes(pool).await.map_err(|_| PRICE_STORAGE_FAILED)?);
    if current.deletion_requires_tombstone(&model_id) {
        monitor_storage::disable_price(pool, &model_id, updated_at_ms)
            .await
            .map_err(|_| PRICE_STORAGE_FAILED)?;
    } else {
        monitor_storage::delete_price(pool, &model_id)
            .await
            .map_err(|_| PRICE_STORAGE_FAILED)?;
    }
    Ok(model_id)
}

fn stored_price(price: StoredPrice) -> Price {
    Price {
        input: price.input,
        output: price.output,
        cache_write: price.cache_write,
        cache_read: price.cache_read,
    }
}

fn canonical_model_id(value: &str) -> Result<String, &'static str> {
    let model_id = PriceCatalog::canonical_model_id(value);
    if model_id.is_empty() {
        return Err(PRICE_MODEL_ID_REQUIRED);
    }
    if model_id.len() > MAX_MODEL_ID_LEN {
        return Err(PRICE_MODEL_ID_TOO_LONG);
    }
    Ok(model_id)
}

fn validate(value: SaveModelPriceDto) -> Result<(String, Price), String> {
    let model_id = canonical_model_id(&value.model_id).map_err(str::to_owned)?;
    let parse = |value: &str, invalid: &'static str, out_of_range: &'static str| {
        decimal_usd_to_pico(value.trim()).map_err(|error| {
            if error == "price overflow" {
                out_of_range.to_owned()
            } else {
                invalid.to_owned()
            }
        })
    };
    Ok((
        model_id,
        Price {
            input: parse(
                &value.input_cost_per_million,
                PRICE_INPUT_INVALID,
                PRICE_INPUT_OUT_OF_RANGE,
            )?,
            output: parse(
                &value.output_cost_per_million,
                PRICE_OUTPUT_INVALID,
                PRICE_OUTPUT_OUT_OF_RANGE,
            )?,
            cache_write: parse(
                &value.cache_write_cost_per_million,
                PRICE_CACHE_WRITE_INVALID,
                PRICE_CACHE_WRITE_OUT_OF_RANGE,
            )?,
            cache_read: parse(
                &value.cache_read_cost_per_million,
                PRICE_CACHE_READ_INVALID,
                PRICE_CACHE_READ_OUT_OF_RANGE,
            )?,
        },
    ))
}

fn pico_to_usd(value: i64) -> String {
    let whole = value / 1_000_000_000_000;
    let fraction = value % 1_000_000_000_000;
    if fraction == 0 {
        return whole.to_string();
    }
    format!("{whole}.{fraction:012}")
        .trim_end_matches('0')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_and_canonicalizes_exact_non_negative_rates() {
        let price = validate(SaveModelPriceDto {
            model_id: " Claude_Test ".into(),
            input_cost_per_million: "1.25".into(),
            output_cost_per_million: "2".into(),
            cache_write_cost_per_million: "0".into(),
            cache_read_cost_per_million: "0.125".into(),
        })
        .unwrap();
        assert_eq!(price.0, "claude-test");
        assert_eq!(price.1.input, 1_250_000_000_000);
        assert_eq!(pico_to_usd(price.1.cache_read), "0.125");
    }

    #[test]
    fn validation_returns_safe_field_specific_codes() {
        let value = |model_id: String, input: &str| SaveModelPriceDto {
            model_id,
            input_cost_per_million: input.into(),
            output_cost_per_million: "2".into(),
            cache_write_cost_per_million: "3".into(),
            cache_read_cost_per_million: "4".into(),
        };
        assert_eq!(
            validate(value("x".repeat(MAX_MODEL_ID_LEN + 1), "1")),
            Err(PRICE_MODEL_ID_TOO_LONG.to_owned())
        );
        assert_eq!(
            validate(value("model".into(), "9223372036854775808")),
            Err(PRICE_INPUT_OUT_OF_RANGE.to_owned())
        );
        assert_eq!(
            validate(value("model".into(), "not-a-price")),
            Err(PRICE_INPUT_INVALID.to_owned())
        );
    }

    #[tokio::test]
    async fn list_is_a_flat_effective_catalog_and_hides_tombstones() {
        let directory = tempfile::tempdir().unwrap();
        let pool = monitor_storage::connect(&directory.path().join("state.db"))
            .await
            .unwrap();
        monitor_storage::migrate(&pool).await.unwrap();
        let initial = list(&pool).await.unwrap();
        assert_eq!(initial.len(), PriceCatalog::builtin_prices().len());
        assert!(initial
            .iter()
            .any(|price| price.model_id == "claude-fable-5"));

        delete(&pool, "claude-fable-5", 43).await.unwrap();
        let prices = list(&pool).await.unwrap();
        assert_eq!(prices.len() + 1, initial.len());
        assert!(!prices
            .iter()
            .any(|price| price.model_id == "claude-fable-5"));
        assert!(catalog(&pool)
            .await
            .unwrap()
            .price("claude-fable-5")
            .is_none());
    }

    #[tokio::test]
    async fn incomplete_stored_builtin_falls_back_to_the_bundled_price() {
        let directory = tempfile::tempdir().unwrap();
        let pool = monitor_storage::connect(&directory.path().join("state.db"))
            .await
            .unwrap();
        monitor_storage::migrate(&pool).await.unwrap();
        let builtin = "gpt-5-6-sol";
        sqlx::query(
            "INSERT INTO price_overrides (
                model_id,input_pico_usd_per_million,disabled,updated_at_ms
             ) VALUES (?1,1,0,1)",
        )
        .bind(builtin)
        .execute(&pool)
        .await
        .unwrap();

        assert!(catalog(&pool).await.unwrap().price(builtin).is_some());
        assert!(list(&pool)
            .await
            .unwrap()
            .iter()
            .any(|price| price.model_id == builtin));
    }

    #[tokio::test]
    async fn unified_delete_persists_for_defaults_and_physically_removes_custom_prices() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("state.db");
        let pool = monitor_storage::connect(&database).await.unwrap();
        monitor_storage::migrate(&pool).await.unwrap();
        let builtin = "gpt-5-6-sol";
        save(
            &pool,
            SaveModelPriceDto {
                model_id: "GPT_5.6_SOL".into(),
                input_cost_per_million: "7".into(),
                output_cost_per_million: "8".into(),
                cache_write_cost_per_million: "9".into(),
                cache_read_cost_per_million: "10".into(),
            },
            10,
        )
        .await
        .unwrap();
        assert_eq!(
            list(&pool)
                .await
                .unwrap()
                .iter()
                .find(|price| price.model_id == builtin)
                .unwrap()
                .input_cost_per_million,
            "7"
        );

        delete(&pool, "gpt_5.6_sol", 11).await.unwrap();
        assert!(!list(&pool)
            .await
            .unwrap()
            .iter()
            .any(|price| price.model_id == builtin));
        assert!(catalog(&pool).await.unwrap().price(builtin).is_none());
        assert_eq!(
            monitor_storage::load_price_changes(&pool)
                .await
                .unwrap()
                .iter()
                .find(|change| change.model_id == builtin)
                .unwrap()
                .price,
            None
        );

        pool.close().await;
        let pool = monitor_storage::connect(&database).await.unwrap();
        monitor_storage::migrate(&pool).await.unwrap();
        assert!(!list(&pool)
            .await
            .unwrap()
            .iter()
            .any(|price| price.model_id == builtin));

        save(
            &pool,
            SaveModelPriceDto {
                model_id: "custom_model".into(),
                input_cost_per_million: "1".into(),
                output_cost_per_million: "2".into(),
                cache_write_cost_per_million: "3".into(),
                cache_read_cost_per_million: "4".into(),
            },
            12,
        )
        .await
        .unwrap();
        delete(&pool, "custom-model", 13).await.unwrap();
        assert!(monitor_storage::load_price_changes(&pool)
            .await
            .unwrap()
            .iter()
            .all(|change| change.model_id != "custom-model"));
    }

    #[tokio::test]
    async fn saving_a_deleted_default_clears_its_tombstone() {
        let directory = tempfile::tempdir().unwrap();
        let pool = monitor_storage::connect(&directory.path().join("state.db"))
            .await
            .unwrap();
        monitor_storage::migrate(&pool).await.unwrap();
        let builtin = "gpt-5-6-sol";
        delete(&pool, builtin, 10).await.unwrap();
        save(
            &pool,
            SaveModelPriceDto {
                model_id: builtin.into(),
                input_cost_per_million: "9".into(),
                output_cost_per_million: "10".into(),
                cache_write_cost_per_million: "11".into(),
                cache_read_cost_per_million: "12".into(),
            },
            11,
        )
        .await
        .unwrap();

        let visible = list(&pool)
            .await
            .unwrap()
            .into_iter()
            .find(|price| price.model_id == builtin)
            .unwrap();
        assert_eq!(visible.input_cost_per_million, "9");
        assert!(monitor_storage::load_price_changes(&pool)
            .await
            .unwrap()
            .iter()
            .find(|change| change.model_id == builtin)
            .unwrap()
            .price
            .is_some());
    }

    #[tokio::test]
    async fn provider_override_and_dated_default_deletes_preserve_resolver_identity() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("state.db");
        let pool = monitor_storage::connect(&database).await.unwrap();
        monitor_storage::migrate(&pool).await.unwrap();
        let provider = "anthropic/claude-fable-5";
        save(
            &pool,
            SaveModelPriceDto {
                model_id: provider.into(),
                input_cost_per_million: "9".into(),
                output_cost_per_million: "10".into(),
                cache_write_cost_per_million: "11".into(),
                cache_read_cost_per_million: "12".into(),
            },
            20,
        )
        .await
        .unwrap();
        delete(&pool, provider, 21).await.unwrap();
        delete(&pool, "claude-sonnet-4-5-20250929", 22)
            .await
            .unwrap();
        pool.close().await;

        let pool = monitor_storage::connect(&database).await.unwrap();
        monitor_storage::migrate(&pool).await.unwrap();
        let persisted_catalog = catalog(&pool).await.unwrap();
        assert!(persisted_catalog.price(provider).is_none());
        assert!(persisted_catalog.price("claude-fable-5").is_some());
        assert!(persisted_catalog.price("claude-sonnet-4-5").is_none());
        assert!(persisted_catalog
            .price("claude-haiku-4-5-20251001")
            .is_some());

        save(
            &pool,
            SaveModelPriceDto {
                model_id: provider.into(),
                input_cost_per_million: "13".into(),
                output_cost_per_million: "14".into(),
                cache_write_cost_per_million: "15".into(),
                cache_read_cost_per_million: "16".into(),
            },
            23,
        )
        .await
        .unwrap();
        assert_eq!(
            catalog(&pool).await.unwrap().price(provider).unwrap().input,
            13_000_000_000_000
        );
    }
}
