use adapter_claude::{
    indexing::start_all_history,
    pricing::PriceCatalog,
    pricing::TokenUsage,
    transcript::{
        aggregate_by_day_model, dedupe_usage, discover, ingest_streaming, DiscoveredTranscript,
        TranscriptCursor, TranscriptStreamItem, UsageRecord, MAX_TRANSCRIPT_LINE_BYTES,
    },
};
use monitor_storage::{connect, load_cursor, migrate, save_cursor, StoredCursor};
use serde::Deserialize;
use sqlx::Row;
use std::{fs, io::Write, path::Path};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tempfile::tempdir;

fn fixture(path: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/transcripts")
        .join(path)
}

fn project_file(
    temp: &tempfile::TempDir,
    bytes: &[u8],
) -> (
    std::path::PathBuf,
    adapter_claude::transcript::DiscoveredTranscript,
) {
    let project = temp.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let path = project.join("session.jsonl");
    fs::write(&path, bytes).unwrap();
    let descriptor = discover(temp.path()).unwrap().remove(0);
    (path, descriptor)
}

struct CollectedScan {
    cursor: TranscriptCursor,
    events: Vec<monitor_domain::AgentEvent>,
    usage: Vec<UsageRecord>,
    parsed_records: usize,
    malformed_records: usize,
    oversized_records: usize,
    reset: bool,
    historical_replay: bool,
    notifications_allowed: bool,
}

fn collect_ingest(
    descriptor: &DiscoveredTranscript,
    prior: Option<&TranscriptCursor>,
    catalog: &PriceCatalog,
    observed_at_ms: i64,
    historical_replay: bool,
) -> Result<CollectedScan, adapter_claude::transcript::TranscriptError> {
    assert!(fs::metadata(descriptor.path()).unwrap().len() <= 40 * 1024 * 1024);
    let mut events = Vec::new();
    let mut usage = Vec::new();
    let mut protocol_historical = false;
    let mut notifications_allowed = false;
    let scan = ingest_streaming(
        descriptor,
        prior,
        catalog,
        observed_at_ms,
        historical_replay,
        &mut |item| {
            match item {
                TranscriptStreamItem::Begin {
                    historical_replay,
                    notifications_allowed: allowed,
                    ..
                } => {
                    protocol_historical = historical_replay;
                    notifications_allowed = allowed;
                }
                TranscriptStreamItem::Chunk {
                    events: chunk_events,
                    usage: chunk_usage,
                    ..
                } => {
                    events.extend(chunk_events);
                    usage.extend(chunk_usage);
                }
                TranscriptStreamItem::Commit { .. } => {}
            }
            Ok(())
        },
    )?;
    Ok(CollectedScan {
        cursor: scan.cursor,
        events,
        usage,
        parsed_records: scan.parsed_records,
        malformed_records: scan.malformed_records,
        oversized_records: scan.oversized_records,
        reset: scan.reset,
        historical_replay: protocol_historical,
        notifications_allowed,
    })
}

#[test]
fn usage_fixture_matches_python_contract_and_never_allows_historical_notifications() {
    let temp = tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir_all(project.join("usage-session/subagents")).unwrap();
    fs::copy(
        fixture("usage-session.jsonl"),
        project.join("usage-session.jsonl"),
    )
    .unwrap();
    fs::copy(
        fixture("usage-session/subagents/agent-fixture.jsonl"),
        project.join("usage-session/subagents/agent-fixture.jsonl"),
    )
    .unwrap();
    let descriptors = discover(temp.path()).unwrap();
    let catalog = PriceCatalog::default();
    let main_batch = collect_ingest(&descriptors[0], None, &catalog, 0, true).unwrap();
    let sub_batch = collect_ingest(&descriptors[1], None, &catalog, 0, true).unwrap();
    assert!(!main_batch.notifications_allowed);
    assert!(!sub_batch.notifications_allowed);
    assert!(main_batch.historical_replay && sub_batch.historical_replay);

    let records = dedupe_usage(main_batch.usage.into_iter().chain(sub_batch.usage));
    let totals = records.iter().fold((0, 0, 0, 0, 0_i64, true), |mut a, r| {
        a.0 += r.usage.input;
        a.1 += r.usage.output;
        a.2 += r.usage.cache_write;
        a.3 += r.usage.cache_read;
        a.4 += r.cost_pico_usd;
        a.5 &= r.cost_known;
        a
    });
    assert_eq!(totals, (317, 83, 25, 80, 2_247_750_000, false));
    let by_day_model = aggregate_by_day_model(&records);
    assert!(!by_day_model.is_empty());
    assert!(records
        .iter()
        .all(|record| !record.source_location.contains("Synthetic")));
}

#[test]
fn cursor_resumes_partial_line_and_only_emits_new_complete_record() {
    let temp = tempdir().unwrap();
    let (path, descriptor) = project_file(
        &temp,
        b"{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\"}]}}\n\
          {\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"incomplete",
    );
    let catalog = PriceCatalog::default();
    let first = collect_ingest(&descriptor, None, &catalog, 1, true).unwrap();
    assert_eq!(first.parsed_records, 1);
    assert_eq!(first.events.len(), 1);
    assert!(first.cursor.byte_offset < fs::metadata(&path).unwrap().len());

    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b" line\"}]}}\n").unwrap();
    let second = collect_ingest(&descriptor, Some(&first.cursor), &catalog, 2, false).unwrap();
    assert_eq!(second.parsed_records, 1);
    assert_eq!(second.events.len(), 1);
    assert_eq!(second.events[0].source_event, "TranscriptAssistantText");
    assert_eq!(
        second.cursor.byte_offset,
        fs::metadata(&path).unwrap().len()
    );

    let third = collect_ingest(&descriptor, Some(&second.cursor), &catalog, 3, false).unwrap();
    assert_eq!(third.parsed_records, 0);
    assert!(third.events.is_empty());
}

#[test]
fn truncation_and_replacement_reset_cursor_and_reindex_is_deterministic() {
    let temp = tempdir().unwrap();
    let (path, descriptor) = project_file(
        &temp,
        &fs::read(fixture("truncation-before.jsonl")).unwrap(),
    );
    let catalog = PriceCatalog::default();
    let before = collect_ingest(&descriptor, None, &catalog, 1, true).unwrap();
    fs::copy(fixture("truncation-after.jsonl"), &path).unwrap();
    let after = collect_ingest(&descriptor, Some(&before.cursor), &catalog, 2, true).unwrap();
    assert!(after.reset);
    assert_eq!(after.events[0].source_event, "TranscriptAssistantText");

    let reindexed = collect_ingest(&descriptor, None, &catalog, 2, true).unwrap();
    assert_eq!(after.events, reindexed.events);
    assert_eq!(after.usage, reindexed.usage);
}

#[test]
fn same_inode_rewrite_that_grows_is_detected_by_content_anchor() {
    let temp = tempdir().unwrap();
    let (path, descriptor) =
        project_file(&temp, &fs::read(fixture("truncation-after.jsonl")).unwrap());
    let catalog = PriceCatalog::default();
    let before = collect_ingest(&descriptor, None, &catalog, 1, true).unwrap();
    let replacement = fs::read(fixture("truncation-before.jsonl")).unwrap();
    fs::write(&path, replacement).unwrap();
    let after = collect_ingest(&descriptor, Some(&before.cursor), &catalog, 2, true).unwrap();
    assert!(after.reset);
    assert_eq!(after.cursor.byte_offset, fs::metadata(path).unwrap().len());
}

#[test]
fn discovery_excludes_internal_projects_and_includes_subagents() {
    let temp = tempdir().unwrap();
    let normal = temp.path().join("project");
    let internal = temp.path().join("project--internal");
    fs::create_dir_all(normal.join("session/subagents")).unwrap();
    fs::create_dir_all(&internal).unwrap();
    fs::write(normal.join("session.jsonl"), b"").unwrap();
    fs::write(normal.join("session/subagents/agent.jsonl"), b"").unwrap();
    fs::write(internal.join("hidden.jsonl"), b"").unwrap();
    let paths = discover(temp.path()).unwrap();
    assert_eq!(paths.len(), 2);
    assert!(paths
        .iter()
        .all(|path| !path.path().to_string_lossy().contains("--")));
}

#[derive(Deserialize)]
struct IdentityCase {
    records: Vec<IdentityRecord>,
    expected_locations: Vec<String>,
}

#[derive(Deserialize)]
struct IdentityRecord {
    session_id: String,
    message_id: String,
    request_id: String,
    observed_at_ms: i64,
    source_location: String,
}

#[test]
fn usage_identity_fixture_obeys_all_stable_and_anonymous_rules() {
    let cases: Vec<IdentityCase> =
        serde_json::from_slice(&fs::read(fixture("usage_identity_cases.json")).unwrap()).unwrap();
    for case in cases {
        let records = case.records.into_iter().map(|record| UsageRecord {
            session_id: record.session_id,
            transcript_path: "fixture.jsonl".into(),
            source_location: record.source_location,
            request_id: (!record.request_id.is_empty()).then_some(record.request_id),
            message_id: (!record.message_id.is_empty()).then_some(record.message_id),
            model_id: "fixture".into(),
            local_day: "2026-01-01".into(),
            usage: TokenUsage {
                input: 1,
                output: 0,
                cache_write: 0,
                cache_read: 0,
            },
            cost_pico_usd: 0,
            cost_known: false,
            observed_at_ms: record.observed_at_ms,
            is_sidechain: false,
            final_message: false,
        });
        let mut actual: Vec<_> = dedupe_usage(records)
            .into_iter()
            .map(|record| record.source_location)
            .collect();
        let mut expected = case.expected_locations;
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected);
    }
}

#[tokio::test]
async fn all_history_index_reports_progress_off_thread_and_disables_notifications() {
    let temp = tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir_all(&project).unwrap();
    fs::copy(
        fixture("usage-session.jsonl"),
        project.join("session.jsonl"),
    )
    .unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let sink_capture = Arc::clone(&captured);
    let finished = Arc::new(AtomicUsize::new(0));
    let progress_finished = Arc::clone(&finished);
    let task = start_all_history(
        temp.path().to_path_buf(),
        i64::MAX / 2,
        move |batch| {
            sink_capture.lock().unwrap().push(batch);
            Ok(())
        },
        move |progress| {
            if progress.finished {
                progress_finished.fetch_add(1, Ordering::Relaxed);
            }
        },
    );
    let summary = task.result.await.unwrap().unwrap();
    let indexed = captured.lock().unwrap();
    assert_eq!(finished.load(Ordering::Relaxed), 1);
    assert_eq!(summary.completed, 1);
    assert!(indexed.iter().any(|item| matches!(
        item.item,
        TranscriptStreamItem::Begin {
            historical_replay: true,
            notifications_allowed: false,
            ..
        }
    )));
}

#[test]
fn oversized_complete_and_incomplete_lines_use_bounded_state() {
    let temp = tempdir().unwrap();
    let mut bytes = vec![b'x'; MAX_TRANSCRIPT_LINE_BYTES + 1];
    bytes.push(b'\n');
    bytes.extend_from_slice(b"{\"type\":\"assistant\",\"message\":{\"content\":[]}}\n");
    let (path, descriptor) = project_file(&temp, &bytes);
    let first = collect_ingest(&descriptor, None, &PriceCatalog::default(), 1, true).unwrap();
    assert_eq!(first.oversized_records, 1);
    assert_eq!(first.parsed_records, 1);
    assert_eq!(first.cursor.byte_offset, fs::metadata(&path).unwrap().len());

    fs::write(&path, vec![b'y'; MAX_TRANSCRIPT_LINE_BYTES + 1]).unwrap();
    let second = collect_ingest(&descriptor, None, &PriceCatalog::default(), 2, true).unwrap();
    assert_eq!(second.oversized_records, 0);
    assert_eq!(second.cursor.byte_offset, 0);
}

#[test]
fn dedupe_hashes_are_collision_safe_for_delimiter_content() {
    let make = |session: &str, message: Option<&str>, request: Option<&str>| UsageRecord {
        session_id: session.into(),
        transcript_path: String::new(),
        source_location: String::new(),
        request_id: request.map(str::to_owned),
        message_id: message.map(str::to_owned),
        model_id: String::new(),
        local_day: String::new(),
        usage: TokenUsage {
            input: 0,
            output: 0,
            cache_write: 0,
            cache_read: 0,
        },
        cost_pico_usd: 0,
        cost_known: false,
        observed_at_ms: 0,
        is_sidechain: false,
        final_message: false,
    };
    for (a, b) in [
        (
            make("a", Some("b:c"), Some("d")),
            make("a", Some("b"), Some("c:d")),
        ),
        (
            make("a", Some("b:m:c"), None),
            make("a:m:b", Some("c"), None),
        ),
        (
            make("a", None, Some("b:r:c")),
            make("a:r:b", None, Some("c")),
        ),
        (make("a:b", None, None), make("a", None, None)),
        (make("a", Some("same"), None), make("a", None, Some("same"))),
    ] {
        assert_ne!(a.dedupe_key(), b.dedupe_key());
    }
}

#[test]
fn extreme_transcript_timestamps_do_not_overflow() {
    let temp = tempdir().unwrap();
    let bytes = format!(
        "{{\"type\":\"assistant\",\"timestamp\":{},\"message\":{{\"content\":[]}}}}\n\
         {{\"type\":\"assistant\",\"timestamp\":{},\"message\":{{\"content\":[]}}}}\n",
        i64::MIN,
        i64::MAX
    );
    let (_, descriptor) = project_file(&temp, bytes.as_bytes());
    let batch = collect_ingest(&descriptor, None, &PriceCatalog::default(), 7, true).unwrap();
    assert_eq!(batch.events.len(), 2);
    assert_eq!(batch.events[0].occurred_at_ms, i64::MIN);
    assert_eq!(batch.events[1].occurred_at_ms, i64::MAX);
}

#[test]
fn full_prefix_anchor_detects_early_change_even_when_tail_is_preserved() {
    let temp = tempdir().unwrap();
    let mut bytes = vec![b'a'; 256];
    bytes.push(b'\n');
    bytes.extend_from_slice(b"{\"type\":\"assistant\",\"message\":{\"content\":[]}}\n");
    let (path, descriptor) = project_file(&temp, &bytes);
    let first = collect_ingest(&descriptor, None, &PriceCatalog::default(), 1, true).unwrap();
    let mut changed = bytes;
    changed[0] = b'b';
    changed.extend_from_slice(b"{\"type\":\"assistant\",\"message\":{\"content\":[]}}\n");
    fs::write(path, changed).unwrap();
    let second = collect_ingest(
        &descriptor,
        Some(&first.cursor),
        &PriceCatalog::default(),
        2,
        true,
    )
    .unwrap();
    assert!(second.reset);
}

#[cfg(unix)]
#[test]
fn discovery_and_ingest_reject_symlink_escape() {
    use std::os::unix::fs::symlink;
    let temp = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir_all(project.join("session/subagents")).unwrap();
    fs::write(project.join("session.jsonl"), b"").unwrap();
    fs::write(outside.path().join("outside.jsonl"), b"").unwrap();
    symlink(
        outside.path().join("outside.jsonl"),
        project.join("session/subagents/escape.jsonl"),
    )
    .unwrap();
    let descriptors = discover(temp.path()).unwrap();
    assert_eq!(descriptors.len(), 1);
    let descriptor = descriptors[0].clone();
    fs::remove_file(descriptor.path()).unwrap();
    symlink(outside.path().join("outside.jsonl"), descriptor.path()).unwrap();
    assert!(collect_ingest(&descriptor, None, &PriceCatalog::default(), 0, true).is_err());
}

#[tokio::test]
async fn indexing_completes_more_than_sixteen_files_through_synchronous_sink() {
    let temp = tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir_all(&project).unwrap();
    for index in 0..20 {
        fs::write(project.join(format!("{index}.jsonl")), b"").unwrap();
    }
    let count = Arc::new(AtomicUsize::new(0));
    let sink_count = Arc::clone(&count);
    let progress_count = Arc::new(AtomicUsize::new(0));
    let progress_sink_count = Arc::clone(&progress_count);
    let task = start_all_history(
        temp.path().to_path_buf(),
        0,
        move |_| {
            sink_count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        },
        move |_| {
            progress_sink_count.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(1));
        },
    );
    let summary = task.result.await.unwrap().unwrap();
    assert_eq!(summary.completed, 20);
    assert_eq!(count.load(Ordering::Relaxed), 40);
    assert_eq!(progress_count.load(Ordering::Relaxed), 20);
}

#[tokio::test]
async fn discovery_errors_reach_index_result_and_progress() {
    let temp = tempdir().unwrap();
    let missing = temp.path().join("missing");
    let progress = Arc::new(Mutex::new(Vec::new()));
    let progress_sink = Arc::clone(&progress);
    let task = start_all_history(
        missing,
        0,
        |_| Ok(()),
        move |item| {
            progress_sink.lock().unwrap().push(item);
        },
    );
    assert!(task.result.await.unwrap().is_err());
    let progress = progress.lock().unwrap();
    let progress = &progress[0];
    assert!(progress.finished);
    assert!(progress.error.is_some());
}

#[tokio::test]
async fn huge_file_obeys_chunk_bound_and_slow_sink_backpressure() {
    let temp = tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let line = b"{\"type\":\"assistant\",\"message\":{\"model\":\"fixture\",\"usage\":{\"input_tokens\":1},\"content\":[]}}\n";
    let mut file = fs::File::create(project.join("huge.jsonl")).unwrap();
    for _ in 0..2_000 {
        file.write_all(line).unwrap();
    }
    let max_seen = Arc::new(AtomicUsize::new(0));
    let chunks = Arc::new(AtomicUsize::new(0));
    let max_sink = Arc::clone(&max_seen);
    let chunk_sink = Arc::clone(&chunks);
    let task = start_all_history(
        temp.path().to_path_buf(),
        0,
        move |batch| {
            if let TranscriptStreamItem::Chunk { events, usage, .. } = batch.item {
                max_sink.fetch_max(events.len() + usage.len(), Ordering::Relaxed);
                chunk_sink.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(())
        },
        |_| {},
    );
    let summary = task.result.await.unwrap().unwrap();
    assert_eq!(summary.completed, 1);
    assert!(chunks.load(Ordering::Relaxed) > 1);
    assert!(max_seen.load(Ordering::Relaxed) <= adapter_claude::transcript::MAX_BATCH_RECORDS);
}

#[tokio::test]
async fn last_file_sink_failure_has_one_terminal_error_progress() {
    let temp = tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let line = b"{\"type\":\"assistant\",\"message\":{\"content\":[]}}\n";
    fs::write(project.join("a.jsonl"), line).unwrap();
    fs::write(project.join("b.jsonl"), line).unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let sink_attempts = Arc::clone(&attempts);
    let progress = Arc::new(Mutex::new(Vec::new()));
    let progress_sink = Arc::clone(&progress);
    let task = start_all_history(
        temp.path().to_path_buf(),
        0,
        move |_| {
            if sink_attempts.fetch_add(1, Ordering::Relaxed) == 4 {
                Err("fixture sink failure".into())
            } else {
                Ok(())
            }
        },
        move |item| progress_sink.lock().unwrap().push(item),
    );
    assert!(task.result.await.unwrap().is_err());
    let progress = progress.lock().unwrap();
    assert_eq!(progress.len(), 2);
    assert_eq!(progress[0].completed, 1);
    assert!(!progress[0].finished);
    assert!(progress[0].error.is_none());
    assert_eq!(progress[1].completed, 2);
    assert!(progress[1].finished);
    assert!(progress[1].error.is_some());
}

#[test]
fn sink_failure_exposes_only_the_last_successfully_delivered_cursor() {
    let temp = tempdir().unwrap();
    let line = b"{\"type\":\"assistant\",\"message\":{\"content\":[]}}\n";
    let mut bytes = Vec::new();
    for _ in 0..300 {
        bytes.extend_from_slice(line);
    }
    let (path, descriptor) = project_file(&temp, &bytes);
    let committed = Arc::new(Mutex::new(None));
    let sink_committed = Arc::clone(&committed);
    let calls = AtomicUsize::new(0);
    let error = ingest_streaming(
        &descriptor,
        None,
        &PriceCatalog::default(),
        0,
        true,
        &mut |item| {
            if let TranscriptStreamItem::Chunk { cursor_after, .. } = item {
                if calls.fetch_add(1, Ordering::Relaxed) == 0 {
                    *sink_committed.lock().unwrap() = Some(cursor_after);
                } else {
                    return Err("stop".into());
                }
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("sink failed"));
    let cursor = committed.lock().unwrap().clone().unwrap();
    assert!(cursor.byte_offset < fs::metadata(path).unwrap().len());
}

#[test]
fn stream_protocol_always_begins_and_commits_zero_payload_files() {
    let cases: Vec<Vec<u8>> = vec![
        Vec::new(),
        b"malformed only\n".to_vec(),
        vec![b'x'; MAX_TRANSCRIPT_LINE_BYTES + 1],
        b"{\"type\":\"user\",\"message\":{\"content\":\"irrelevant\"}}\n".to_vec(),
        b"{\"type\":\"assistant\",\"message\":{\"content\":[]}}\nBAD-TAIL".to_vec(),
    ];
    for bytes in cases {
        let temp = tempdir().unwrap();
        let (_, descriptor) = project_file(&temp, &bytes);
        let items = Arc::new(Mutex::new(Vec::new()));
        let sink_items = Arc::clone(&items);
        ingest_streaming(
            &descriptor,
            None,
            &PriceCatalog::default(),
            0,
            true,
            &mut |item| {
                sink_items.lock().unwrap().push(item);
                Ok(())
            },
        )
        .unwrap();
        let items = items.lock().unwrap();
        assert!(matches!(
            items.first(),
            Some(TranscriptStreamItem::Begin { .. })
        ));
        assert!(matches!(
            items.last(),
            Some(TranscriptStreamItem::Commit { .. })
        ));
        assert_eq!(
            items
                .iter()
                .filter(|item| matches!(item, TranscriptStreamItem::Commit { .. }))
                .count(),
            1
        );
    }
}

#[test]
fn reset_with_zero_payload_is_explicitly_begun_then_committed() {
    let temp = tempdir().unwrap();
    let (path, descriptor) = project_file(
        &temp,
        b"{\"type\":\"assistant\",\"message\":{\"content\":[]}}\n",
    );
    let first = collect_ingest(&descriptor, None, &PriceCatalog::default(), 0, true).unwrap();
    fs::write(path, b"irrelevant\n").unwrap();
    let items = Arc::new(Mutex::new(Vec::new()));
    let sink_items = Arc::clone(&items);
    ingest_streaming(
        &descriptor,
        Some(&first.cursor),
        &PriceCatalog::default(),
        0,
        true,
        &mut |item| {
            sink_items.lock().unwrap().push(item);
            Ok(())
        },
    )
    .unwrap();
    let items = items.lock().unwrap();
    assert!(matches!(
        items.first(),
        Some(TranscriptStreamItem::Begin { reset: true, .. })
    ));
    assert!(matches!(
        items.last(),
        Some(TranscriptStreamItem::Commit { .. })
    ));
}

#[test]
fn sink_failures_on_begin_chunk_and_commit_all_propagate() {
    for fail_at in 0..3 {
        let temp = tempdir().unwrap();
        let (_, descriptor) = project_file(
            &temp,
            b"{\"type\":\"assistant\",\"message\":{\"content\":[]}}\n",
        );
        let calls = AtomicUsize::new(0);
        let result = ingest_streaming(
            &descriptor,
            None,
            &PriceCatalog::default(),
            0,
            true,
            &mut |_| {
                if calls.fetch_add(1, Ordering::Relaxed) == fail_at {
                    Err("protocol failure".into())
                } else {
                    Ok(())
                }
            },
        );
        assert!(result.unwrap_err().to_string().contains("sink failed"));
        assert_eq!(calls.load(Ordering::Relaxed), fail_at + 1);
    }
}

#[tokio::test]
async fn persisted_cursor_round_trip_resumes_without_storing_partial_content() {
    let temp = tempdir().unwrap();
    let (path, descriptor) = project_file(
        &temp,
        b"{\"type\":\"assistant\",\"message\":{\"content\":[]}}\nSECRET-PROMPT",
    );
    let first = collect_ingest(&descriptor, None, &PriceCatalog::default(), 1, true).unwrap();
    let database = temp.path().join("state.db");
    let pool = connect(&database).await.unwrap();
    migrate(&pool).await.unwrap();
    save_cursor(
        &pool,
        &StoredCursor {
            transcript_path: path.to_string_lossy().into_owned(),
            file_identity: first.cursor.file_identity.clone(),
            byte_offset: first.cursor.byte_offset as i64,
            file_size: first.cursor.file_size as i64,
            modified_at_ms: first.cursor.modified_at_ms,
            content_anchor: first.cursor.content_anchor.clone(),
            last_scanned_at_ms: 1,
            last_error: None,
        },
    )
    .await
    .unwrap();
    let partial: Option<Vec<u8>> = sqlx::query("SELECT partial_line FROM transcript_cursors")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
    assert!(partial.is_none() || partial.as_deref() == Some(&[]));
    let stored = load_cursor(&pool, &path.to_string_lossy())
        .await
        .unwrap()
        .unwrap();
    let cursor = TranscriptCursor {
        file_identity: stored.file_identity,
        byte_offset: stored.byte_offset as u64,
        file_size: stored.file_size as u64,
        modified_at_ms: stored.modified_at_ms,
        content_anchor: stored.content_anchor,
    };
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    let resumed = collect_ingest(
        &descriptor,
        Some(&cursor),
        &PriceCatalog::default(),
        2,
        false,
    )
    .unwrap();
    assert_eq!(resumed.malformed_records, 1);
    let database_bytes = fs::read(database).unwrap();
    assert!(!database_bytes
        .windows(b"SECRET-PROMPT".len())
        .any(|window| window == b"SECRET-PROMPT"));
}
