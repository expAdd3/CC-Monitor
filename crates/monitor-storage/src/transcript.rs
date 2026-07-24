use sqlx::{Row, SqlitePool};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StoredCursor {
    pub transcript_path: String,
    pub file_identity: Option<String>,
    pub byte_offset: i64,
    pub file_size: i64,
    pub modified_at_ms: Option<i64>,
    pub content_anchor: Option<String>,
    pub last_scanned_at_ms: i64,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredUsage {
    pub id: String,
    pub session_id: String,
    pub transcript_path: String,
    pub source_location: String,
    pub request_id: Option<String>,
    pub message_id: Option<String>,
    pub model_id: String,
    pub local_day: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_write_tokens: i64,
    pub cache_read_tokens: i64,
    pub cost_pico_usd: i64,
    pub cost_known: bool,
    pub dedupe_key: String,
    pub observed_at_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceOverride {
    pub model_id: String,
    pub input: i64,
    pub output: i64,
    pub cache_write: i64,
    pub cache_read: i64,
    pub updated_at_ms: i64,
}

pub async fn load_cursor(
    pool: &SqlitePool,
    path: &str,
) -> Result<Option<StoredCursor>, sqlx::Error> {
    let row = sqlx::query("SELECT transcript_path,file_identity,byte_offset,file_size,modified_at_ms,partial_line,content_anchor,last_scanned_at_ms,last_error FROM transcript_cursors WHERE transcript_path=?")
        .bind(path).fetch_optional(pool).await?;
    Ok(row.map(|row| StoredCursor {
        transcript_path: row.get(0),
        file_identity: row.get(1),
        byte_offset: row.get(2),
        file_size: row.get(3),
        modified_at_ms: row.get(4),
        content_anchor: row.get(6),
        last_scanned_at_ms: row.get(7),
        last_error: row.get(8),
    }))
}

pub async fn save_cursor(pool: &SqlitePool, cursor: &StoredCursor) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO transcript_cursors(transcript_path,file_identity,byte_offset,file_size,modified_at_ms,partial_line,content_anchor,last_scanned_at_ms,last_error) VALUES(?,?,?,?,?,?,?,?,?) ON CONFLICT(transcript_path) DO UPDATE SET file_identity=excluded.file_identity,byte_offset=excluded.byte_offset,file_size=excluded.file_size,modified_at_ms=excluded.modified_at_ms,partial_line=excluded.partial_line,content_anchor=excluded.content_anchor,last_scanned_at_ms=excluded.last_scanned_at_ms,last_error=excluded.last_error")
        .bind(&cursor.transcript_path).bind(&cursor.file_identity).bind(cursor.byte_offset)
        .bind(cursor.file_size).bind(cursor.modified_at_ms).bind(Option::<Vec<u8>>::None)
        .bind(&cursor.content_anchor)
        .bind(cursor.last_scanned_at_ms).bind(&cursor.last_error).execute(pool).await?;
    Ok(())
}

/// Replaces all derived usage for one session deterministically. Used for
/// explicit reindex and file-replacement recovery.
pub async fn replace_session_usage(
    pool: &SqlitePool,
    session_id: &str,
    records: &[StoredUsage],
    updated_at_ms: i64,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM usage_records WHERE agent_kind='claude' AND session_id=?")
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM daily_usage WHERE agent_kind='claude' AND session_id=?")
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    for r in records {
        sqlx::query("INSERT INTO usage_records(id,agent_kind,session_id,transcript_path,source_location,request_id,message_id,model_id,local_day,input_tokens,output_tokens,cache_write_tokens,cache_read_tokens,cost_pico_usd,cost_known,dedupe_key,observed_at_ms) VALUES(?,'claude',?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .bind(&r.id).bind(&r.session_id).bind(&r.transcript_path).bind(&r.source_location)
            .bind(&r.request_id).bind(&r.message_id).bind(&r.model_id).bind(&r.local_day)
            .bind(r.input_tokens).bind(r.output_tokens).bind(r.cache_write_tokens)
            .bind(r.cache_read_tokens).bind(r.cost_pico_usd).bind(r.cost_known)
            .bind(&r.dedupe_key).bind(r.observed_at_ms).execute(&mut *tx).await?;
    }
    sqlx::query("INSERT INTO daily_usage(local_day,agent_kind,session_id,model_id,input_tokens,output_tokens,cache_write_tokens,cache_read_tokens,cost_pico_usd,cost_known,updated_at_ms) SELECT local_day,'claude',session_id,model_id,SUM(input_tokens),SUM(output_tokens),SUM(cache_write_tokens),SUM(cache_read_tokens),SUM(cost_pico_usd),MIN(cost_known),? FROM usage_records WHERE agent_kind='claude' AND session_id=? GROUP BY local_day,session_id,model_id")
        .bind(updated_at_ms).bind(session_id).execute(&mut *tx).await?;
    tx.commit().await
}

pub async fn list_price_overrides(pool: &SqlitePool) -> Result<Vec<PriceOverride>, sqlx::Error> {
    let rows = sqlx::query("SELECT model_id,input_pico_usd_per_million,output_pico_usd_per_million,cache_write_pico_usd_per_million,cache_read_pico_usd_per_million,updated_at_ms FROM price_overrides ORDER BY model_id").fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            Some(PriceOverride {
                model_id: row.get(0),
                input: row.get::<Option<i64>, _>(1)?,
                output: row.get::<Option<i64>, _>(2)?,
                cache_write: row.get::<Option<i64>, _>(3)?,
                cache_read: row.get::<Option<i64>, _>(4)?,
                updated_at_ms: row.get(5),
            })
        })
        .collect())
}

pub async fn put_price_override(
    pool: &SqlitePool,
    value: &PriceOverride,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO price_overrides(model_id,input_pico_usd_per_million,output_pico_usd_per_million,cache_write_pico_usd_per_million,cache_read_pico_usd_per_million,updated_at_ms) VALUES(?,?,?,?,?,?) ON CONFLICT(model_id) DO UPDATE SET input_pico_usd_per_million=excluded.input_pico_usd_per_million,output_pico_usd_per_million=excluded.output_pico_usd_per_million,cache_write_pico_usd_per_million=excluded.cache_write_pico_usd_per_million,cache_read_pico_usd_per_million=excluded.cache_read_pico_usd_per_million,updated_at_ms=excluded.updated_at_ms")
        .bind(&value.model_id).bind(value.input).bind(value.output).bind(value.cache_write)
        .bind(value.cache_read).bind(value.updated_at_ms).execute(pool).await?;
    Ok(())
}

pub async fn delete_price_override(pool: &SqlitePool, model: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM price_overrides WHERE model_id=?")
        .bind(model)
        .execute(pool)
        .await?;
    Ok(())
}
