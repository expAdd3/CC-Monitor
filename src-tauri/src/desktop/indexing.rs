use super::{now_ms, pricing, settings::NotificationPolicy, DesktopState, IndexProgress};
use adapter_claude::{
    indexing,
    pricing::PriceCatalog,
    transcript::{
        discover, ingest_streaming_with_freshness, TranscriptCursor, TranscriptError,
        TranscriptStreamItem,
    },
};
use monitor_engine::Engine;
use monitor_storage::{
    StoredUsage, TranscriptCursorPosition, TranscriptIngestRepository, TranscriptPublish,
};
use sqlx::SqlitePool;
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};
use uuid::Uuid;

const TRANSCRIPT_FULL_ANCHOR_AUDIT_INTERVAL_MS: i64 = 10 * 60 * 1_000;
// Unchanged files take only a freshness check; keep the cap high enough that a
// normal transcript catalog is revisited every 30-second worker interval while
// still bounding pathological directories.
const MAX_INCREMENTAL_FILES_PER_TICK: usize = 256;
const MAX_INCREMENTAL_BYTES_PER_TICK: u64 = 32 * 1024 * 1024;
const MAX_INCREMENTAL_TICK_DURATION: Duration = Duration::from_secs(2);

// A transcript cursor makes file parsing durable. This much smaller in-memory
// cursor only makes discovery fair: a permanently unreadable file must not be
// retried first on every tick and starve every path sorted after it.
static INCREMENTAL_SCAN_AFTER: LazyLock<Mutex<HashMap<PathBuf, PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Copy)]
struct IncrementalTickBudget {
    max_files: usize,
    max_bytes: u64,
    max_elapsed: Duration,
}

impl Default for IncrementalTickBudget {
    fn default() -> Self {
        Self {
            max_files: MAX_INCREMENTAL_FILES_PER_TICK,
            max_bytes: MAX_INCREMENTAL_BYTES_PER_TICK,
            max_elapsed: MAX_INCREMENTAL_TICK_DURATION,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct IncrementalFileResult {
    changed: bool,
    bytes_read: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct IncrementalFileError {
    code: &'static str,
    // A commit can publish the cursor/evidence before projection processing
    // fails. Preserve invalidation in that case; an earlier staging-only
    // failure may produce a harmless extra refresh but never a stale UI.
    may_have_changed: bool,
}

#[derive(Debug, Default, Eq, PartialEq)]
struct IncrementalTickResult {
    changed: bool,
    attempted_files: usize,
    failed_files: usize,
    bytes_read: u64,
    last_attempted_path: Option<PathBuf>,
    first_error_code: Option<&'static str>,
}

pub(super) struct IncrementalScanOutcome {
    pub(super) changed: bool,
    pub(super) error_code: Option<&'static str>,
}

fn rotate_after<T>(items: &mut [T], after: Option<&Path>, path: impl Fn(&T) -> &Path) {
    let Some(after) = after else {
        return;
    };
    let split = items.partition_point(|item| path(item) <= after);
    items.rotate_left(split);
}

fn run_incremental_tick<T>(
    work: impl IntoIterator<Item = (PathBuf, T)>,
    budget: IncrementalTickBudget,
    mut process: impl FnMut(T) -> Result<IncrementalFileResult, IncrementalFileError>,
    mut elapsed: impl FnMut() -> Duration,
) -> IncrementalTickResult {
    let mut result = IncrementalTickResult::default();
    for (path, item) in work {
        // Always attempt one file so an expensive discovery pass cannot prevent
        // durable forward progress. Begin/chunk/commit is atomic per file, so
        // byte and time budgets are checked between files and may exceed their
        // target by at most the current file. The file-count limit stays hard.
        if result.attempted_files > 0
            && (result.attempted_files >= budget.max_files
                || result.bytes_read >= budget.max_bytes
                || elapsed() >= budget.max_elapsed)
        {
            break;
        }
        result.attempted_files = result.attempted_files.saturating_add(1);
        result.last_attempted_path = Some(path);
        match process(item) {
            Ok(file) => {
                result.changed |= file.changed;
                result.bytes_read = result.bytes_read.saturating_add(file.bytes_read);
            }
            Err(error) => {
                result.changed |= error.may_have_changed;
                result.failed_files = result.failed_files.saturating_add(1);
                result.first_error_code.get_or_insert(error.code);
            }
        }
    }
    result
}

fn incremental_error_code(error: &TranscriptError) -> &'static str {
    match error {
        TranscriptError::Io(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            "index_permission_denied"
        }
        TranscriptError::Io(_) => "index_io_failed",
        TranscriptError::EscapedRoot | TranscriptError::Symlink | TranscriptError::NonRegular => {
            "index_invalid_transcript"
        }
        TranscriptError::Sink(_) => "index_storage_failed",
    }
}

fn full_anchor_audit_due(last_scanned_at_ms: i64, observed_at_ms: i64) -> bool {
    observed_at_ms < last_scanned_at_ms
        || observed_at_ms.saturating_sub(last_scanned_at_ms)
            >= TRANSCRIPT_FULL_ANCHOR_AUDIT_INTERVAL_MS
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReindexStarted {
    run_id: String,
}

fn try_indexing_guard(
    indexing: Arc<tokio::sync::Mutex<()>>,
) -> Result<tokio::sync::OwnedMutexGuard<()>, tokio::sync::TryLockError> {
    indexing.try_lock_owned()
}

fn index_completed_cleanly(interrupted: bool, failed_files: i64, quarantined: i64) -> bool {
    !interrupted && failed_files == 0 && quarantined == 0
}

pub(super) async fn scan_incremental(
    pool: SqlitePool,
    projects: PathBuf,
    providers: Arc<tokio::sync::RwLock<NotificationPolicy>>,
    indexing: Arc<tokio::sync::Mutex<()>>,
) -> anyhow::Result<Option<IncrementalScanOutcome>> {
    let Ok(_guard) = try_indexing_guard(indexing) else {
        return Ok(None);
    };
    let catalog = pricing::catalog(&pool).await?;
    let tick = tauri::async_runtime::spawn_blocking(move || {
        let started = Instant::now();
        let mut descriptors =
            discover(&projects).map_err(|error| anyhow::anyhow!(incremental_error_code(&error)))?;
        let after = INCREMENTAL_SCAN_AFTER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&projects)
            .cloned();
        rotate_after(&mut descriptors, after.as_deref(), |descriptor| {
            descriptor.path()
        });
        let work = descriptors
            .into_iter()
            .map(|descriptor| (descriptor.path().to_path_buf(), descriptor));
        let result = run_incremental_tick(
            work,
            IncrementalTickBudget::default(),
            |descriptor| {
                let observed_at_ms = now_ms();
                let path = descriptor.path().to_string_lossy().into_owned();
                let stored =
                    tauri::async_runtime::block_on(monitor_storage::load_cursor(&pool, &path))
                        .map_err(|_| IncrementalFileError {
                            code: "index_storage_failed",
                            may_have_changed: false,
                        })?;
                let full_anchor_audit = stored.as_ref().is_none_or(|cursor| {
                    full_anchor_audit_due(cursor.last_scanned_at_ms, observed_at_ms)
                });
                let prior = stored.as_ref().map(adapter_cursor);
                let historical_replay = stored.is_none();
                let mut persistence =
                    TranscriptStreamPersistence::new(pool.clone(), providers.clone());
                let mut file_changed = false;
                let mut sink = |item: TranscriptStreamItem| {
                    match &item {
                        TranscriptStreamItem::Begin { reset: true, .. } => file_changed = true,
                        TranscriptStreamItem::Chunk { events, usage, .. }
                            if !events.is_empty() || !usage.is_empty() =>
                        {
                            file_changed = true;
                        }
                        _ => {}
                    }
                    tauri::async_runtime::block_on(persistence.persist(item))
                        .map_err(|_| "index_storage_failed".to_owned())
                };
                let attempt = ingest_streaming_with_freshness(
                    &descriptor,
                    prior.as_ref(),
                    &catalog,
                    observed_at_ms,
                    historical_replay,
                    full_anchor_audit,
                    &mut sink,
                );
                let scan = attempt.map_err(|error| IncrementalFileError {
                    code: incremental_error_code(&error),
                    may_have_changed: file_changed,
                })?;
                Ok(IncrementalFileResult {
                    changed: file_changed,
                    bytes_read: scan.bytes_read,
                })
            },
            || started.elapsed(),
        );
        if let Some(path) = &result.last_attempted_path {
            INCREMENTAL_SCAN_AFTER
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(projects, path.clone());
        }
        Ok::<_, anyhow::Error>(result)
    })
    .await??;
    Ok(Some(IncrementalScanOutcome {
        changed: tick.changed,
        error_code: tick.first_error_code,
    }))
}

pub(super) async fn reindex_transcripts(
    app: AppHandle,
    state: tauri::State<'_, Arc<DesktopState>>,
) -> Result<ReindexStarted, String> {
    begin_reindex(app, state.inner().clone()).await
}

pub(super) async fn begin_reindex(
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
    let guard = try_indexing_guard(state.indexing.clone()).map_err(super::super::fixed_error(
        super::super::IpcError::ReindexAlreadyRunning,
    ))?;
    // Resolve fallible inputs asynchronously before publishing `running`.
    let catalog = pricing::catalog(&state.pool)
        .await
        .map_err(super::super::fixed_error(
            super::super::IpcError::ReindexStart,
        ))?;
    start_reindex(app, state, projects, guard, catalog)
}

fn start_reindex(
    app: AppHandle,
    state: Arc<DesktopState>,
    projects: PathBuf,
    guard: tokio::sync::OwnedMutexGuard<()>,
    catalog: PriceCatalog,
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
    let quarantined_sessions = Arc::new(Mutex::new(BTreeSet::new()));
    let persistence_quarantined = quarantined_sessions.clone();
    let mut persistence = TranscriptStreamPersistence::with_quarantine_counter(
        state.pool.clone(),
        state.providers.clone(),
        persistence_quarantined.clone(),
    );
    let task = indexing::start_all_history_with_catalog(
        projects,
        now_ms(),
        catalog,
        move |indexed| {
            tauri::async_runtime::block_on(persistence.persist(indexed.item))
                .map_err(|_| "index_storage_failed".to_owned())
        },
        move |paths| {
            present_sink.lock().unwrap().extend(paths);
            Ok(())
        },
        move |value| {
            let mut progress = progress_state.index.lock().unwrap();
            progress.run_id = Some(progress_run_id.clone());
            progress.state = "running".into();
            progress.completed = i64::try_from(value.completed).unwrap_or(i64::MAX);
            progress.total = i64::try_from(value.total).unwrap_or(i64::MAX);
            if value.error.is_some() {
                progress.failed_files = progress.failed_files.saturating_add(1);
            }
            progress.quarantined_sessions = quarantined_count(&persistence_quarantined);
            progress_state.invalidate(&progress_app);
        },
    );
    let terminal_run_id = run_id.clone();
    tauri::async_runtime::spawn(async move {
        let _guard = guard;
        let result = task.result.await;
        let mut interrupted = !matches!(&result, Ok(Ok(_)));
        let failed_files = match result {
            Ok(Ok(summary)) => i64::try_from(summary.failed).unwrap_or(i64::MAX),
            _ => state.index.lock().unwrap().failed_files,
        };
        if !interrupted {
            let paths = present_paths.lock().unwrap().clone();
            match monitor_storage::remove_missing_transcripts(&state.pool, &paths).await {
                Ok(sessions) if !sessions.is_empty() => {
                    match process_transcript_sessions(
                        &state.providers,
                        &state.engine(),
                        sessions,
                        now_ms(),
                    )
                    .await
                    {
                        Ok(batch) => {
                            add_quarantined(&quarantined_sessions, batch.quarantined_session_ids())
                        }
                        Err(_) => interrupted = true,
                    }
                }
                Ok(_) => {}
                Err(_) => interrupted = true,
            }
        }
        let quarantined = quarantined_count(&quarantined_sessions);
        let success = index_completed_cleanly(interrupted, failed_files, quarantined);
        let mut progress = state.index.lock().unwrap();
        progress.run_id = Some(terminal_run_id);
        progress.state = if success { "complete" } else { "failed" }.into();
        progress.failed_files = failed_files;
        progress.quarantined_sessions = quarantined;
        progress.interrupted = interrupted;
        drop(progress);
        state.invalidate_tray(&app);
    });
    Ok(ReindexStarted { run_id })
}

struct TranscriptStreamPersistence {
    repository: TranscriptIngestRepository,
    providers: Arc<tokio::sync::RwLock<NotificationPolicy>>,
    quarantined_sessions: Option<Arc<Mutex<BTreeSet<String>>>>,
}

impl TranscriptStreamPersistence {
    fn new(pool: SqlitePool, providers: Arc<tokio::sync::RwLock<NotificationPolicy>>) -> Self {
        Self {
            repository: TranscriptIngestRepository::new(pool),
            providers,
            quarantined_sessions: None,
        }
    }

    fn with_quarantine_counter(
        pool: SqlitePool,
        providers: Arc<tokio::sync::RwLock<NotificationPolicy>>,
        quarantined_sessions: Arc<Mutex<BTreeSet<String>>>,
    ) -> Self {
        Self {
            repository: TranscriptIngestRepository::new(pool),
            providers,
            quarantined_sessions: Some(quarantined_sessions),
        }
    }

    async fn persist(&mut self, item: TranscriptStreamItem) -> anyhow::Result<()> {
        match item {
            TranscriptStreamItem::Begin {
                descriptor,
                reset,
                notifications_allowed,
                ..
            } => {
                self.repository
                    .begin(
                        descriptor.path().to_string_lossy().into_owned(),
                        descriptor.session_id(),
                        reset,
                        notifications_allowed,
                        now_ms(),
                    )
                    .await?;
            }
            TranscriptStreamItem::Chunk { events, usage, .. } => {
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
                self.repository.chunk(events, records, now_ms()).await?;
            }
            TranscriptStreamItem::Commit { final_cursor } => {
                let publish = self
                    .repository
                    .commit(&cursor_position(&final_cursor), now_ms())
                    .await?;
                if let Some(sessions) = published_sessions(publish) {
                    let batch = process_transcript_sessions(
                        &self.providers,
                        &Engine::new(self.repository.pool().clone()),
                        sessions,
                        now_ms(),
                    )
                    .await?;
                    if let Some(sessions) = &self.quarantined_sessions {
                        add_quarantined(sessions, batch.quarantined_session_ids());
                    }
                }
            }
        }
        Ok(())
    }
}

fn add_quarantined<I, S>(sessions: &Mutex<BTreeSet<String>>, values: I)
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    sessions
        .lock()
        .unwrap()
        .extend(values.into_iter().map(Into::into));
}

fn quarantined_count(sessions: &Mutex<BTreeSet<String>>) -> i64 {
    i64::try_from(sessions.lock().unwrap().len()).unwrap_or(i64::MAX)
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
    fn full_and_incremental_scans_cannot_own_the_guard_together() {
        let indexing = Arc::new(tokio::sync::Mutex::new(()));
        let full_scan = try_indexing_guard(indexing.clone()).expect("first scan owns the guard");
        assert!(try_indexing_guard(indexing.clone()).is_err());
        drop(full_scan);
        assert!(try_indexing_guard(indexing).is_ok());
    }

    #[test]
    fn partial_failures_are_not_reported_as_clean_completion() {
        assert!(index_completed_cleanly(false, 0, 0));
        assert!(!index_completed_cleanly(false, 1, 0));
        assert!(!index_completed_cleanly(false, 0, 1));
        assert!(!index_completed_cleanly(true, 0, 0));
    }

    #[test]
    fn quarantine_progress_counts_one_session_once_across_transcript_commits() {
        let sessions = Mutex::new(BTreeSet::new());
        add_quarantined(&sessions, ["shared-session"]);
        add_quarantined(&sessions, ["shared-session"]);
        add_quarantined(&sessions, ["another-session"]);
        assert_eq!(quarantined_count(&sessions), 2);
    }

    #[test]
    fn full_anchor_audit_is_periodic_and_handles_clock_rollback() {
        assert!(!full_anchor_audit_due(1_000, 1_001));
        assert!(full_anchor_audit_due(
            1_000,
            1_000 + TRANSCRIPT_FULL_ANCHOR_AUDIT_INTERVAL_MS
        ));
        assert!(full_anchor_audit_due(1_000, 999));
    }

    #[test]
    fn incremental_tick_isolates_a_file_failure_and_advances_past_it() {
        let work = [
            (PathBuf::from("a.jsonl"), "bad"),
            (PathBuf::from("b.jsonl"), "changed"),
            (PathBuf::from("c.jsonl"), "unchanged"),
        ];
        let mut visited = Vec::new();
        let result = run_incremental_tick(
            work,
            IncrementalTickBudget {
                max_files: 8,
                max_bytes: 1_000,
                max_elapsed: Duration::from_secs(1),
            },
            |value| {
                visited.push(value);
                match value {
                    "bad" => Err(IncrementalFileError {
                        code: "index_io_failed",
                        may_have_changed: false,
                    }),
                    "changed" => Ok(IncrementalFileResult {
                        changed: true,
                        bytes_read: 20,
                    }),
                    _ => Ok(IncrementalFileResult {
                        changed: false,
                        bytes_read: 10,
                    }),
                }
            },
            || Duration::ZERO,
        );

        assert_eq!(visited, ["bad", "changed", "unchanged"]);
        assert!(result.changed);
        assert_eq!(result.failed_files, 1);
        assert_eq!(result.first_error_code, Some("index_io_failed"));
        assert_eq!(result.last_attempted_path, Some(PathBuf::from("c.jsonl")));
    }

    #[test]
    fn failed_file_preserves_possible_publication_for_invalidation() {
        let result = run_incremental_tick(
            [(PathBuf::from("published.jsonl"), ())],
            IncrementalTickBudget {
                max_files: 1,
                max_bytes: u64::MAX,
                max_elapsed: Duration::MAX,
            },
            |_| {
                Err(IncrementalFileError {
                    code: "index_storage_failed",
                    may_have_changed: true,
                })
            },
            || Duration::ZERO,
        );

        assert!(result.changed);
        assert_eq!(result.first_error_code, Some("index_storage_failed"));
    }

    #[test]
    fn incremental_tick_stops_at_each_budget_and_can_resume_after_last_attempt() {
        let work = [
            (PathBuf::from("a.jsonl"), 6_u64),
            (PathBuf::from("b.jsonl"), 6_u64),
            (PathBuf::from("c.jsonl"), 6_u64),
        ];
        let elapsed = std::cell::Cell::new(Duration::ZERO);
        let result = run_incremental_tick(
            work,
            IncrementalTickBudget {
                max_files: 8,
                max_bytes: 10,
                max_elapsed: Duration::from_secs(1),
            },
            |bytes| {
                elapsed.set(elapsed.get() + Duration::from_millis(10));
                Ok(IncrementalFileResult {
                    changed: false,
                    bytes_read: bytes,
                })
            },
            || elapsed.get(),
        );
        assert_eq!(result.attempted_files, 2);
        assert_eq!(result.bytes_read, 12);
        assert_eq!(result.last_attempted_path, Some(PathBuf::from("b.jsonl")));

        let mut paths = vec![
            PathBuf::from("a.jsonl"),
            PathBuf::from("b.jsonl"),
            PathBuf::from("c.jsonl"),
        ];
        rotate_after(
            &mut paths,
            result.last_attempted_path.as_deref(),
            PathBuf::as_path,
        );
        assert_eq!(paths[0], PathBuf::from("c.jsonl"));
    }

    #[test]
    fn incremental_tick_file_limit_is_hard() {
        let work = (0..5).map(|index| (PathBuf::from(format!("{index}.jsonl")), index));
        let result = run_incremental_tick(
            work,
            IncrementalTickBudget {
                max_files: 2,
                max_bytes: u64::MAX,
                max_elapsed: Duration::MAX,
            },
            |_| Ok(IncrementalFileResult::default()),
            || Duration::ZERO,
        );

        assert_eq!(result.attempted_files, 2);
        assert_eq!(result.last_attempted_path, Some(PathBuf::from("1.jsonl")));
    }

    #[test]
    fn incremental_tick_stops_after_current_atomic_file_when_time_expires() {
        let work = [
            (PathBuf::from("a.jsonl"), ()),
            (PathBuf::from("b.jsonl"), ()),
        ];
        let elapsed = std::cell::Cell::new(Duration::ZERO);
        let result = run_incremental_tick(
            work,
            IncrementalTickBudget {
                max_files: 8,
                max_bytes: u64::MAX,
                max_elapsed: Duration::from_millis(5),
            },
            |_| {
                elapsed.set(Duration::from_millis(6));
                Ok(IncrementalFileResult::default())
            },
            || elapsed.get(),
        );

        assert_eq!(result.attempted_files, 1);
        assert_eq!(result.last_attempted_path, Some(PathBuf::from("a.jsonl")));
    }
}
