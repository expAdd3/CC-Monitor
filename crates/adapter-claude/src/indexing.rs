use crate::{
    pricing::PriceCatalog,
    transcript::{discover, ingest_streaming, TranscriptError, TranscriptStreamItem},
};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const ACTIVE_VISIBILITY_MS: i64 = 24 * 60 * 60 * 1_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexProgress {
    pub completed: usize,
    pub total: usize,
    pub path: Option<PathBuf>,
    pub error: Option<String>,
    pub finished: bool,
}

#[derive(Debug)]
pub struct IndexedTranscript {
    pub active_visible: bool,
    pub item: TranscriptStreamItem,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IndexSummary {
    pub completed: usize,
    pub failed: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error(transparent)]
    Discovery(#[from] TranscriptError),
    #[error("index sink failed: {0}")]
    Sink(String),
}

pub struct IndexTask {
    pub result: tokio::task::JoinHandle<Result<IndexSummary, IndexError>>,
}

/// Runs whole-history file I/O on Tokio's blocking pool and streams each batch
/// to a consumer. Neither result nor progress channels can block the worker.
pub fn start_all_history(
    projects_dir: PathBuf,
    now_ms: i64,
    mut sink: impl FnMut(IndexedTranscript) -> Result<(), String> + Send + 'static,
    mut progress: impl FnMut(IndexProgress) + Send + 'static,
) -> IndexTask {
    let result = tokio::task::spawn_blocking(move || {
        let descriptors = match discover(&projects_dir) {
            Ok(value) => value,
            Err(error) => {
                progress(IndexProgress {
                    completed: 0,
                    total: 0,
                    path: Some(projects_dir),
                    error: Some(error.to_string()),
                    finished: true,
                });
                return Err(IndexError::Discovery(error));
            }
        };
        let total = descriptors.len();
        let catalog = PriceCatalog::default();
        let mut summary = IndexSummary::default();
        let mut sink_failure = None;
        for descriptor in descriptors {
            let path = descriptor.path().to_path_buf();
            let active_visible = is_active(&path, now_ms);
            let mut file_sink = |item: TranscriptStreamItem| {
                sink(IndexedTranscript {
                    active_visible,
                    item,
                })
            };
            let attempt =
                ingest_streaming(&descriptor, None, &catalog, now_ms, true, &mut file_sink);
            let error = match attempt {
                Ok(_) => None,
                Err(error) => {
                    summary.failed += 1;
                    if let TranscriptError::Sink(message) = &error {
                        sink_failure.get_or_insert_with(|| message.clone());
                    }
                    Some(error.to_string())
                }
            };
            summary.completed += 1;
            progress(IndexProgress {
                completed: summary.completed,
                total,
                path: Some(path),
                error,
                finished: summary.completed == total,
            });
        }
        if total == 0 {
            progress(IndexProgress {
                completed: 0,
                total: 0,
                path: None,
                error: None,
                finished: true,
            });
        }
        if let Some(error) = sink_failure {
            Err(IndexError::Sink(error))
        } else {
            Ok(summary)
        }
    });
    IndexTask { result }
}

pub fn is_active(path: &Path, now_ms: i64) -> bool {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .is_some_and(|value| {
            now_ms.saturating_sub(value) <= ACTIVE_VISIBILITY_MS && value <= now_ms
        })
}

pub fn unix_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
