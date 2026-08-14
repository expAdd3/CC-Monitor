use sqlx::{Row, SqlitePool};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoredPrice {
    pub input: i64,
    pub output: i64,
    pub cache_write: i64,
    pub cache_read: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredPriceChange {
    pub model_id: String,
    pub price: Option<StoredPrice>,
}

pub async fn load_price_changes(pool: &SqlitePool) -> Result<Vec<StoredPriceChange>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT model_id,input_pico_usd_per_million,output_pico_usd_per_million,
                cache_write_pico_usd_per_million,cache_read_pico_usd_per_million,disabled
           FROM price_overrides
          ORDER BY model_id COLLATE NOCASE, model_id COLLATE BINARY",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let disabled = row.get::<i64, _>("disabled") != 0;
            let complete_price = match (
                row.get::<Option<i64>, _>("input_pico_usd_per_million"),
                row.get::<Option<i64>, _>("output_pico_usd_per_million"),
                row.get::<Option<i64>, _>("cache_write_pico_usd_per_million"),
                row.get::<Option<i64>, _>("cache_read_pico_usd_per_million"),
            ) {
                (Some(input), Some(output), Some(cache_write), Some(cache_read)) => {
                    Some(StoredPrice {
                        input,
                        output,
                        cache_write,
                        cache_read,
                    })
                }
                _ => None,
            };
            if !disabled && complete_price.is_none() {
                return None;
            }
            Some(StoredPriceChange {
                model_id: row.get("model_id"),
                price: (!disabled).then_some(complete_price).flatten(),
            })
        })
        .collect())
}

pub async fn upsert_price(
    pool: &SqlitePool,
    model_id: &str,
    price: StoredPrice,
    updated_at_ms: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO price_overrides(model_id,input_pico_usd_per_million,output_pico_usd_per_million,
             cache_write_pico_usd_per_million,cache_read_pico_usd_per_million,updated_at_ms,disabled)
         VALUES(?1,?2,?3,?4,?5,?6,0)
         ON CONFLICT(model_id) DO UPDATE SET input_pico_usd_per_million=excluded.input_pico_usd_per_million,
             output_pico_usd_per_million=excluded.output_pico_usd_per_million,
             cache_write_pico_usd_per_million=excluded.cache_write_pico_usd_per_million,
             cache_read_pico_usd_per_million=excluded.cache_read_pico_usd_per_million,
             updated_at_ms=excluded.updated_at_ms,disabled=0",
    )
    .bind(model_id)
    .bind(price.input)
    .bind(price.output)
    .bind(price.cache_write)
    .bind(price.cache_read)
    .bind(updated_at_ms)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn disable_price(
    pool: &SqlitePool,
    model_id: &str,
    updated_at_ms: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO price_overrides(model_id,updated_at_ms,disabled) VALUES(?1,?2,1)
         ON CONFLICT(model_id) DO UPDATE SET disabled=1,updated_at_ms=excluded.updated_at_ms",
    )
    .bind(model_id)
    .bind(updated_at_ms)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete_price(pool: &SqlitePool, model_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM price_overrides WHERE model_id=?1")
        .bind(model_id)
        .execute(pool)
        .await?;
    Ok(())
}
