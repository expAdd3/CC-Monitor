use super::{now_ms, settings::NotificationPolicy, DesktopState, IndexProgress};
use adapter_claude::{
    indexing,
    pricing::PriceCatalog,
    transcript::{
        discover, ingest_streaming_with_freshness, TranscriptCursor, TranscriptStreamItem,
    },
};
use monitor_engine::Engine;
use monitor_storage::{
    StoredUsage, TranscriptCursorPosition, TranscriptIngestRepository, TranscriptPublish,
};
use sqlx::SqlitePool;
use std::{collections::BTreeSet, path::PathBuf, sync::Arc};
use tauri::{AppHandle, Manager};
use uuid::Uuid;

const TRANSCRIPT_FULL_ANCHOR_AUDIT_INTERVAL_MS: i64 = 10 * 60 * 1_000;

fn full_anchor_audit_due(last_scanned_at_ms: i64, observed_at_ms: i64) -> bool {
    observed_at_ms < last_scanned_at_ms
        || observed_at_ms.saturating_sub(last_scanned_at_ms)
            >= TRANSCRIPT_FULL_ANCHOR_AUDIT_INTERVAL_MS
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReindexStarted {
    run_id: String,
}

pub(super) async fn scan_incremental(
    pool: SqlitePool,
    projects: PathBuf,
    providers: Arc<tokio::sync::RwLock<NotificationPolicy>>,
) -> anyhow::Result<bool> {
    tauri::async_runtime::spawn_blocking(move || {
        let descriptors = discover(&projects)?;
        let catalog = PriceCatalog::default();
        let mut changed = false;
        for descriptor in descriptors {
            let observed_at_ms = now_ms();
            let path = descriptor.path().to_string_lossy().into_owned();
            let stored =
                tauri::async_runtime::block_on(monitor_storage::load_cursor(&pool, &path))?;
            let full_anchor_audit = stored.as_ref().is_none_or(|cursor| {
                full_anchor_audit_due(cursor.last_scanned_at_ms, observed_at_ms)
            });
            let prior = stored.as_ref().map(adapter_cursor);
            let historical_replay = stored.is_none();
            let mut persistence = TranscriptStreamPersistence::new(pool.clone(), providers.clone());
            let mut sink = |item: TranscriptStreamItem| {
                match &item {
                    TranscriptStreamItem::Begin { reset: true, .. } => changed = true,
                    TranscriptStreamItem::Chunk { events, usage, .. }
                        if !events.is_empty() || !usage.is_empty() =>
                    {
                        changed = true;
                    }
                    _ => {}
                }
                tauri::async_runtime::block_on(persistence.persist(item))
                    .map_err(|_| "index_storage_failed".to_owned())
            };
            ingest_streaming_with_freshness(
                &descriptor,
                prior.as_ref(),
                &catalog,
                observed_at_ms,
                historical_replay,
                full_anchor_audit,
                &mut sink,
            )?;
        }
        Ok::<_, anyhow::Error>(changed)
    })
    .await?
}

pub(super) async fn reindex_transcripts(
    app: AppHandle,
    state: tauri::State<'_, Arc<DesktopState>>,
) -> Result<ReindexStarted, String> {
    begin_reindex(app, state.inner().clone())
}

pub(super) fn begin_reindex(
    app: AppHandle,
    state: Arc<DesktopState>,
) -> Result<ReindexStarted, String> {
    let projects = app
        .path()
        .home_dir()
        .map_err(super::super::fixed_error(
            super::super::IpcError::ReindexStart,
        ))?
        .join(".claude/projects");
    let guard = state
        .indexing
        .clone()
        .try_lock_owned()
        .map_err(super::super::fixed_error(
            super::super::IpcError::ReindexAlreadyRunning,
        ))?;
    start_reindex(app, state, projects, guard)
}

fn start_reindex(
    app: AppHandle,
    state: Arc<DesktopState>,
    projects: PathBuf,
    guard: tokio::sync::OwnedMutexGuard<()>,
) -> Result<ReindexStarted, String> {
    let run_id = Uuid::now_v7().to_string();
    {
        let mut progress = state.index.lock().unwrap();
        if progress.state == "running" {
            return Err(super::super::IpcError::ReindexAlreadyRunning
                .code()
                .to_owned());
        }
        *progress = IndexProgress {
            run_id: Some(run_id.clone()),
            state: "running".into(),
            ..IndexProgress::default()
        };
    }
    state.invalidate(&app);
    let present_paths = Arc::new(std::sync::Mutex::new(BTreeSet::new()));
    let present_sink = present_paths.clone();
    let progress_state = state.clone();
    let progress_app = app.clone();
    let progress_run_id = run_id.clone();
    let mut persistence =
        TranscriptStreamPersistence::new(state.pool.clone(), state.providers.clone());
    let task = indexing::start_all_history(
        projects,
        now_ms(),
        move |indexed| {
            tauri::async_runtime::block_on(persistence.persist(indexed.item))
                .map_err(|_| "index_storage_failed".to_owned())
        },
        move |paths| {
            present_sink.lock().unwrap().extend(paths);
            Ok(())
        },
        move |value| {
            *progress_state.index.lock().unwrap() = IndexProgress {
                run_id: Some(progress_run_id.clone()),
                state: "running".into(),
                completed: i64::try_from(value.completed).unwrap_or(i64::MAX),
                total: i64::try_from(value.total).unwrap_or(i64::MAX),
                ..IndexProgress::default()
            };
            progress_state.invalidate(&progress_app);
        },
    );
    let terminal_run_id = run_id.clone();
    tauri::async_runtime::spawn(async move {
        let _guard = guard;
        let mut success = matches!(task.result.await, Ok(Ok(summary)) if summary.failed == 0);
        if success {
            let paths = present_paths.lock().unwrap().clone();
            match monitor_storage::remove_missing_transcripts(&state.pool, &paths).await {
                Ok(sessions) if !sessions.is_empty() => {
                    success = process_transcript_sessions(
                        &state.providers,
                        &state.engine(),
                        sessions,
                        now_ms(),
                    )
                    .await
                    .is_ok();
                }
                Ok(_) => {}
                Err(_) => success = false,
            }
        }
        let mut progress = state.index.lock().unwrap();
        progress.run_id = Some(terminal_run_id);
        progress.state = if success { "complete" } else { "failed" }.into();
        progress.interrupted = !success;
        drop(progress);
        state.invalidate_tray(&app);
    });
    Ok(ReindexStarted { run_id })
}

struct TranscriptStreamPersistence {
    repository: TranscriptIngestRepository,
    providers: Arc<tokio::sync::RwLock<NotificationPolicy>>,
}

impl TranscriptStreamPersistence {
    fn new(pool: SqlitePool, providers: Arc<tokio::sync::RwLock<NotificationPolicy>>) -> Self {
        Self {
            repository: TranscriptIngestRepository::new(pool),
            providers,
        }
    }

    async fn persist(&mut self, item: TranscriptStreamItem) -> anyhow::Result<()> {
        match item {
            TranscriptStreamItem::Begin {
                descriptor,
                reset,
                start_cursor,
                notifications_allowed,
                ..
            } => {
                self.repository
                    .begin(
                        descriptor.path().to_string_lossy().into_owned(),
                        descriptor.session_id(),
                        reset,
                        notifications_allowed,
                        &cursor_position(&start_cursor),
                        now_ms(),
                    )
                    .await?;
            }
            TranscriptStreamItem::Chunk {
                events,
                usage,
                cursor_after,
            } => {
                let records = usage
                    .into_iter()
                    .map(|usage| {
                        let dedupe_key = usage.dedupe_key();
                        StoredUsage {
                            id: Uuid::now_v7().to_string(),
                            session_id: usage.session_id,
                            transcript_path: usage.transcript_path,
                            source_location: usage.source_location,
                            request_id: usage.request_id,
                            message_id: usage.message_id,
                            model_id: usage.model_id,
                            local_day: usage.local_day,
                            input_tokens: i64::try_from(usage.usage.input).unwrap_or(i64::MAX),
                            output_tokens: i64::try_from(usage.usage.output).unwrap_or(i64::MAX),
                            cache_write_tokens: i64::try_from(usage.usage.cache_write)
                                .unwrap_or(i64::MAX),
                            cache_read_tokens: i64::try_from(usage.usage.cache_read)
                                .unwrap_or(i64::MAX),
                            cost_pico_usd: usage.cost_pico_usd,
                            cost_known: usage.cost_known,
                            dedupe_key,
                            observed_at_ms: usage.observed_at_ms,
                            is_sidechain: usage.is_sidechain,
                            final_message: usage.final_message,
                        }
                    })
                    .collect();
                self.repository
                    .chunk(events, records, &cursor_position(&cursor_after), now_ms())
                    .await?;
            }
            TranscriptStreamItem::Commit { final_cursor } => {
                let publish = self
                    .repository
                    .commit(&cursor_position(&final_cursor), now_ms())
                    .await?;
                if let Some(sessions) = published_sessions(publish) {
                    process_transcript_sessions(
                        &self.providers,
                        &Engine::new(self.repository.pool().clone()),
                        sessions,
                        now_ms(),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }
}

async fn process_transcript_sessions(
    providers: &Arc<tokio::sync::RwLock<NotificationPolicy>>,
    engine: &Engine,
    sessions: Vec<String>,
    processed_at_ms: i64,
) -> Result<monitor_engine::BatchResult, monitor_engine::BatchError> {
    let sessions = sessions.into_iter().map(|session_id| {
        (
            monitor_engine::domain::AgentKind::claude(),
            monitor_engine::domain::SessionId(session_id),
        )
    });
    let policy = providers.read().await;
    engine
        .process_sessions(sessions, policy.providers(), processed_at_ms)
        .await
}

fn published_sessions(publish: TranscriptPublish) -> Option<Vec<String>> {
    publish.published.then_some(publish.sessions)
}

fn cursor_position(cursor: &TranscriptCursor) -> TranscriptCursorPosition {
    TranscriptCursorPosition {
        file_identity: cursor.file_identity.clone(),
        byte_offset: i64::try_from(cursor.byte_offset).unwrap_or(i64::MAX),
        file_size: i64::try_from(cursor.file_size).unwrap_or(i64::MAX),
        modified_at_ms: cursor.modified_at_ms,
        modified_at_ns: cursor.modified_at_ns,
        content_anchor: cursor.content_anchor.clone(),
        hash_checkpoint: cursor.hash_checkpoint.clone(),
    }
}

fn adapter_cursor(cursor: &monitor_storage::StoredTranscriptCursor) -> TranscriptCursor {
    TranscriptCursor {
        file_identity: cursor.file_identity.clone(),
        byte_offset: u64::try_from(cursor.byte_offset).unwrap_or_default(),
        file_size: u64::try_from(cursor.file_size).unwrap_or_default(),
        modified_at_ms: cursor.modified_at_ms,
        modified_at_ns: cursor.modified_at_ns,
        content_anchor: cursor.content_anchor.clone(),
        hash_checkpoint: cursor.hash_checkpoint.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_anchor_audit_is_periodic_and_handles_clock_rollback() {
        assert!(!full_anchor_audit_due(1_000, 1_001));
        assert!(full_anchor_audit_due(
            1_000,
            1_000 + TRANSCRIPT_FULL_ANCHOR_AUDIT_INTERVAL_MS
        ));
        assert!(full_anchor_audit_due(1_000, 999));
    }
}
