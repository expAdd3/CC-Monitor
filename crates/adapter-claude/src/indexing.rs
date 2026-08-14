use crate::{
    pricing::PriceCatalog,
    transcript::{ingest_streaming, visit_discovered, TranscriptError, TranscriptStreamItem},
};
use std::path::PathBuf;

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

/// Runs whole-history file I/O on Tokio's blocking pool, using the supplied
/// catalog and streaming each bounded batch to a consumer. Neither result nor
/// progress channels can block the worker.
pub fn start_all_history_with_catalog(
    projects_dir: PathBuf,
    now_ms: i64,
    catalog: PriceCatalog,
    mut sink: impl FnMut(IndexedTranscript) -> Result<(), String> + Send + 'static,
    mut present_page_sink: impl FnMut(Vec<String>) -> Result<(), String> + Send + 'static,
    mut progress: impl FnMut(IndexProgress) + Send + 'static,
) -> IndexTask {
    let result = tokio::task::spawn_blocking(move || {
        let mut total = 0_usize;
        match visit_discovered(&projects_dir, |_| {
            total = total.saturating_add(1);
            Ok(())
        }) {
            Ok(()) => {}
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
        }
        let mut summary = IndexSummary::default();
        let mut sink_failure = None;
        let mut present_page = Vec::with_capacity(64);
        let visit = visit_discovered(&projects_dir, |descriptor| {
            present_page.push(descriptor.path().to_string_lossy().into_owned());
            if present_page.len() == 64 {
                present_page_sink(std::mem::take(&mut present_page))
                    .map_err(TranscriptError::Sink)?;
                std::thread::yield_now();
            }
            let path = descriptor.path().to_path_buf();
            let mut file_sink = |item: TranscriptStreamItem| {
                let item = match item {
                    TranscriptStreamItem::Begin {
                        descriptor,
                        start_cursor,
                        historical_replay,
                        notifications_allowed,
                        ..
                    } => TranscriptStreamItem::Begin {
                        descriptor,
                        reset: true,
                        start_cursor,
                        historical_replay,
                        notifications_allowed,
                    },
                    item => item,
                };
                sink(IndexedTranscript { item })
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
            Ok(())
        });
        if let Err(error) = visit {
            if let TranscriptError::Sink(message) = error {
                sink_failure.get_or_insert(message);
            } else {
                return Err(IndexError::Discovery(error));
            }
        }
        if !present_page.is_empty() {
            if let Err(error) = present_page_sink(present_page) {
                sink_failure.get_or_insert(error);
            }
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
