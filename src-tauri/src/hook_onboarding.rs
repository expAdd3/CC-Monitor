use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};

const SETTINGS_KEY: &str = "hook_onboarding";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HookOnboardingDisposition {
    Deferred,
    DeliberatelyUninstalled,
}

pub(crate) async fn load(
    connection: &mut SqliteConnection,
) -> anyhow::Result<Option<HookOnboardingDisposition>> {
    let value: Option<String> = sqlx::query_scalar("SELECT value_json FROM settings WHERE key=?1")
        .bind(SETTINGS_KEY)
        .fetch_optional(&mut *connection)
        .await?;
    // A corrupted optional preference must not make the Dashboard unreadable.
    Ok(value
        .as_deref()
        .and_then(|value| serde_json::from_str(value).ok()))
}

pub(crate) async fn persist(
    pool: &SqlitePool,
    disposition: HookOnboardingDisposition,
) -> anyhow::Result<()> {
    let value = serde_json::to_string(&disposition)?;
    sqlx::query(
        "INSERT INTO settings(key, value_json, updated_at_ms)
         VALUES(?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET
           value_json=excluded.value_json, updated_at_ms=excluded.updated_at_ms",
    )
    .bind(SETTINGS_KEY)
    .bind(value)
    .bind(adapter_claude::hook::now_ms())
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) async fn clear(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM settings WHERE key=?1")
        .bind(SETTINGS_KEY)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disposition_is_separate_from_hook_ownership_and_persists() {
        let directory = tempfile::tempdir().unwrap();
        let pool = monitor_storage::connect(&directory.path().join("state.db"))
            .await
            .unwrap();
        monitor_storage::migrate(&pool).await.unwrap();

        persist(&pool, HookOnboardingDisposition::Deferred)
            .await
            .unwrap();
        let mut connection = pool.acquire().await.unwrap();
        assert_eq!(
            load(&mut connection).await.unwrap(),
            Some(HookOnboardingDisposition::Deferred)
        );
        let installations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM installation")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(installations, 0);

        persist(&pool, HookOnboardingDisposition::DeliberatelyUninstalled)
            .await
            .unwrap();
        assert_eq!(
            load(&mut connection).await.unwrap(),
            Some(HookOnboardingDisposition::DeliberatelyUninstalled)
        );

        clear(&pool).await.unwrap();
        assert_eq!(load(&mut connection).await.unwrap(), None);
    }

    #[tokio::test]
    async fn unknown_optional_preference_falls_back_to_prompt() {
        let directory = tempfile::tempdir().unwrap();
        let pool = monitor_storage::connect(&directory.path().join("state.db"))
            .await
            .unwrap();
        monitor_storage::migrate(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO settings(key, value_json, updated_at_ms)
             VALUES(?1, '\"unknown\"', 1)",
        )
        .bind(SETTINGS_KEY)
        .execute(&pool)
        .await
        .unwrap();
        let mut connection = pool.acquire().await.unwrap();
        assert_eq!(load(&mut connection).await.unwrap(), None);
    }

    #[tokio::test]
    async fn syntactically_damaged_preference_falls_back_to_prompt() {
        let directory = tempfile::tempdir().unwrap();
        let pool = monitor_storage::connect(&directory.path().join("state.db"))
            .await
            .unwrap();
        monitor_storage::migrate(&pool).await.unwrap();
        // SQLite normally prevents this through settings.json_valid. Enabling
        // this test-only pragma simulates on-disk corruption after creation.
        let mut connection = pool.acquire().await.unwrap();
        sqlx::query("PRAGMA ignore_check_constraints = ON")
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO settings(key, value_json, updated_at_ms)
             VALUES(?1, '{bad', 1)",
        )
        .bind(SETTINGS_KEY)
        .execute(&mut *connection)
        .await
        .unwrap();
        assert_eq!(load(&mut connection).await.unwrap(), None);
    }
}
