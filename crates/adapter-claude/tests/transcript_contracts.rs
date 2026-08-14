use adapter_claude::{
    indexing::start_all_history_with_catalog,
    pricing::TokenUsage,
    pricing::{Price, PriceCatalog},
    transcript::{
        discover, ingest_streaming, DiscoveredTranscript, TranscriptCursor, TranscriptError,
        TranscriptStreamItem, UsageRecord, MAX_TRANSCRIPT_LINE_BYTES,
    },
};
use monitor_storage::{
    connect, load_cursor, migrate, StoredUsage, TranscriptCursorPosition,
    TranscriptIngestRepository,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
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

fn stored_usage(record: UsageRecord, id: String) -> StoredUsage {
    let dedupe_key = record.dedupe_key();
    StoredUsage {
        id,
        session_id: record.session_id,
        transcript_path: record.transcript_path,
        source_location: record.source_location,
        request_id: record.request_id,
        message_id: record.message_id,
        model_id: record.model_id,
        local_day: record.local_day,
        input_tokens: i64::try_from(record.usage.input).unwrap(),
        output_tokens: i64::try_from(record.usage.output).unwrap(),
        cache_write_tokens: i64::try_from(record.usage.cache_write).unwrap(),
        cache_read_tokens: i64::try_from(record.usage.cache_read).unwrap(),
        cost_pico_usd: record.cost_pico_usd,
        cost_known: record.cost_known,
        dedupe_key,
        observed_at_ms: record.observed_at_ms,
        is_sidechain: record.is_sidechain,
        final_message: record.final_message,
    }
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

#[tokio::test]
async fn incremental_and_full_history_paths_use_the_supplied_price_catalog() {
    let temp = tempdir().unwrap();
    let bytes = b"{\"type\":\"assistant\",\"message\":{\"id\":\"priced\",\"model\":\"custom_model\",\"usage\":{\"input_tokens\":10},\"content\":[]}}\n";
    let (_, descriptor) = project_file(&temp, bytes);
    let catalog = PriceCatalog::with_changes([(
        "custom-model".to_owned(),
        Some(Price {
            input: 7_000_000_000_000,
            output: 0,
            cache_write: 0,
            cache_read: 0,
        }),
    )]);

    let incremental = collect_ingest(&descriptor, None, &catalog, 0, true).unwrap();
    assert_eq!(incremental.usage[0].cost_pico_usd, 70_000_000);
    assert!(incremental.usage[0].cost_known);

    let captured = Arc::new(Mutex::new(Vec::new()));
    let sink_capture = Arc::clone(&captured);
    let task = start_all_history_with_catalog(
        temp.path().to_path_buf(),
        0,
        catalog,
        move |indexed| {
            if let TranscriptStreamItem::Chunk { usage, .. } = indexed.item {
                sink_capture.lock().unwrap().extend(usage);
            }
            Ok(())
        },
        |_| Ok(()),
        |_| {},
    );
    task.result.await.unwrap().unwrap();
    let full = captured.lock().unwrap();
    assert_eq!(full[0].cost_pico_usd, 70_000_000);
    assert!(full[0].cost_known);
}

#[tokio::test]
async fn usage_fixture_matches_contract_and_never_allows_historical_notifications() {
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

    let pool = connect(&temp.path().join("state.db")).await.unwrap();
    migrate(&pool).await.unwrap();
    for (index, (descriptor, batch)) in [main_batch, sub_batch]
        .into_iter()
        .enumerate()
        .zip([&descriptors[0], &descriptors[1]])
        .map(|((index, batch), descriptor)| (index, (descriptor, batch)))
    {
        let cursor = TranscriptCursorPosition {
            file_identity: batch.cursor.file_identity,
            byte_offset: batch.cursor.byte_offset as i64,
            file_size: batch.cursor.file_size as i64,
            modified_at_ms: batch.cursor.modified_at_ms,
            modified_at_ns: batch.cursor.modified_at_ns,
            content_anchor: batch.cursor.content_anchor,
            hash_checkpoint: batch.cursor.hash_checkpoint,
        };
        let mut repository = TranscriptIngestRepository::new(pool.clone());
        repository
            .begin(
                descriptor.path().to_string_lossy().into_owned(),
                descriptor.session_id(),
                true,
                false,
                index as i64,
            )
            .await
            .unwrap();
        let usage = batch
            .usage
            .into_iter()
            .enumerate()
            .map(|(record_index, record)| {
                stored_usage(record, format!("fixture-{index}-{record_index}"))
            })
            .collect();
        repository
            .chunk(Vec::new(), usage, index as i64)
            .await
            .unwrap();
        repository.commit(&cursor, index as i64).await.unwrap();
    }
    let totals: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT SUM(input_tokens),SUM(output_tokens),SUM(cache_write_tokens),
                SUM(cache_read_tokens),SUM(cost_pico_usd),MIN(cost_known)
         FROM usage_records",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(totals, (317, 83, 25, 80, 2_247_750_000, 0));
    assert!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM usage_records")
            .fetch_one(&pool)
            .await
            .unwrap()
            > 0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM usage_records WHERE source_location LIKE '%Synthetic%'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
}

#[tokio::test]
async fn full_history_replacement_rebuilds_cross_source_usage_candidates() {
    let temp = tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir_all(project.join("candidate-session/subagents")).unwrap();
    let record = |input_tokens| {
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"2026-01-10T00:30:00Z\",\
             \"requestId\":\"shared-request\",\"message\":{{\"id\":\"shared-message\",\
             \"model\":\"claude-sonnet-4-5-20250929\",\"stop_reason\":\"end_turn\",\
             \"usage\":{{\"input_tokens\":{input_tokens},\"output_tokens\":1}},\
             \"content\":[]}}}}\n"
        )
    };
    fs::write(project.join("candidate-session.jsonl"), record(10)).unwrap();
    fs::write(
        project.join("candidate-session/subagents/agent.jsonl"),
        record(99),
    )
    .unwrap();
    let descriptors = discover(temp.path()).unwrap();
    assert_eq!(descriptors.len(), 2);
    let pool = connect(&temp.path().join("state.db")).await.unwrap();
    migrate(&pool).await.unwrap();
    let catalog = PriceCatalog::default();

    for (index, descriptor) in descriptors.iter().enumerate() {
        let batch = collect_ingest(descriptor, None, &catalog, index as i64, true).unwrap();
        assert!(!batch.notifications_allowed);
        let cursor = TranscriptCursorPosition {
            file_identity: batch.cursor.file_identity,
            byte_offset: batch.cursor.byte_offset as i64,
            file_size: batch.cursor.file_size as i64,
            modified_at_ms: batch.cursor.modified_at_ms,
            modified_at_ns: batch.cursor.modified_at_ns,
            content_anchor: batch.cursor.content_anchor,
            hash_checkpoint: batch.cursor.hash_checkpoint,
        };
        let usage = batch
            .usage
            .into_iter()
            .enumerate()
            .map(|(record_index, record)| {
                stored_usage(record, format!("repair-{index}-{record_index}"))
            })
            .collect();
        let mut repository = TranscriptIngestRepository::new(pool.clone());
        repository
            .begin(
                descriptor.path().to_string_lossy().into_owned(),
                descriptor.session_id(),
                true,
                false,
                index as i64,
            )
            .await
            .unwrap();
        repository
            .chunk(Vec::new(), usage, index as i64)
            .await
            .unwrap();
        repository.commit(&cursor, index as i64).await.unwrap();
    }

    assert_eq!(
        sqlx::query_as::<_, (i64, i64)>(
            "SELECT
                (SELECT COUNT(*) FROM transcript_usage_stage WHERE staged_at_ms=-1),
                (SELECT COUNT(*) FROM usage_records)"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        (2, 1),
        "full replacement must rebuild the losing source candidate behind one winner"
    );
}

#[tokio::test]
async fn future_transcript_usage_timestamp_is_clamped_before_storage_day_projection() {
    let temp = tempdir().unwrap();
    let (_, descriptor) = project_file(
        &temp,
        br#"{"type":"assistant","timestamp":"2099-01-01T00:00:00Z","message":{"id":"message","model":"claude-sonnet-4-5","usage":{"input_tokens":1,"output_tokens":2}}}
"#,
    );
    let observed_at_ms = 1_700_000_000_000;
    let scan = collect_ingest(
        &descriptor,
        None,
        &PriceCatalog::default(),
        observed_at_ms,
        true,
    )
    .unwrap();
    let expected_day = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(observed_at_ms)
        .unwrap()
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d")
        .to_string();
    assert_eq!(scan.usage.len(), 1);
    assert_eq!(scan.usage[0].local_day, expected_day);

    let pool = connect(&temp.path().join("state.db")).await.unwrap();
    migrate(&pool).await.unwrap();
    let cursor = TranscriptCursorPosition::default();
    let mut repository = TranscriptIngestRepository::new(pool.clone());
    repository
        .begin(
            descriptor.path().to_string_lossy().into_owned(),
            descriptor.session_id(),
            true,
            false,
            observed_at_ms,
        )
        .await
        .unwrap();
    repository
        .chunk(
            Vec::new(),
            scan.usage
                .into_iter()
                .map(|record| stored_usage(record, "future".into()))
                .collect(),
            observed_at_ms,
        )
        .await
        .unwrap();
    repository.commit(&cursor, observed_at_ms).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT local_day FROM usage_records")
            .fetch_one(&pool)
            .await
            .unwrap(),
        expected_day
    );
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
    assert!(!after.notifications_allowed);
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

#[tokio::test]
async fn usage_identity_fixture_obeys_all_stable_and_anonymous_rules() {
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
        let temp = tempdir().unwrap();
        let pool = connect(&temp.path().join("state.db")).await.unwrap();
        migrate(&pool).await.unwrap();
        let cursor = TranscriptCursorPosition::default();
        let mut repository = TranscriptIngestRepository::new(pool.clone());
        repository
            .begin("fixture.jsonl".into(), "fixture".into(), true, false, 1)
            .await
            .unwrap();
        repository
            .chunk(
                Vec::new(),
                records
                    .enumerate()
                    .map(|(index, record)| stored_usage(record, format!("case-{index}")))
                    .collect(),
                1,
            )
            .await
            .unwrap();
        repository.commit(&cursor, 1).await.unwrap();
        let mut actual: Vec<String> =
            sqlx::query_scalar("SELECT source_location FROM usage_records")
                .fetch_all(&pool)
                .await
                .unwrap();
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
    let task = start_all_history_with_catalog(
        temp.path().to_path_buf(),
        i64::MAX / 2,
        PriceCatalog::default(),
        move |batch| {
            sink_capture.lock().unwrap().push(batch);
            Ok(())
        },
        |_| Ok(()),
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
            reset: true,
            historical_replay: true,
            notifications_allowed: false,
            ..
        }
    )));
}

#[tokio::test]
async fn full_history_reindex_forces_replacement_after_shorter_rewrite() {
    let temp = tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let transcript = project.join("session.jsonl");
    fs::write(
        &transcript,
        b"{\"type\":\"assistant\",\"message\":{\"id\":\"first\",\"model\":\"fixture\",\"usage\":{\"input_tokens\":10},\"content\":[]}}\n{\"type\":\"assistant\",\"message\":{\"id\":\"removed\",\"model\":\"fixture\",\"usage\":{\"input_tokens\":20},\"content\":[]}}\n",
    )
    .unwrap();

    for (bytes, expected_messages) in [
        (None, vec!["first", "removed"]),
        (
            Some(
                b"{\"type\":\"assistant\",\"message\":{\"id\":\"replacement\",\"model\":\"fixture\",\"usage\":{\"input_tokens\":3},\"content\":[]}}\n"
                    .as_slice(),
            ),
            vec!["replacement"],
        ),
    ] {
        if let Some(bytes) = bytes {
            fs::write(&transcript, bytes).unwrap();
        }
        let captured_items = Arc::new(Mutex::new((Vec::new(), Vec::new())));
        let captured = Arc::clone(&captured_items);
        let task = start_all_history_with_catalog(
            temp.path().to_path_buf(),
            i64::MAX / 2,
            PriceCatalog::default(),
            move |batch| {
                match batch.item {
                    TranscriptStreamItem::Begin { reset, .. } => {
                        captured.lock().unwrap().0.push(reset);
                    }
                    TranscriptStreamItem::Chunk { usage, .. } => {
                        captured
                            .lock()
                            .unwrap()
                            .1
                            .extend(usage.into_iter().filter_map(|record| record.message_id));
                    }
                    TranscriptStreamItem::Commit { .. } => {}
                }
                Ok(())
            },
            |_| Ok(()),
            |_| {},
        );
        task.result.await.unwrap().unwrap();
        let captured = captured_items.lock().unwrap();
        assert_eq!(captured.0, vec![true]);
        assert_eq!(captured.1, expected_messages);
    }
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
fn transcript_timestamps_use_the_inclusive_receipt_window() {
    let temp = tempdir().unwrap();
    const DAY_MS: i64 = 24 * 60 * 60 * 1_000;
    let received = 1_800_000_000_000_i64;
    let bytes = format!(
        "{{\"type\":\"assistant\",\"timestamp\":{},\"message\":{{\"content\":[]}}}}\n\
         {{\"type\":\"assistant\",\"timestamp\":{},\"message\":{{\"content\":[]}}}}\n\
         {{\"type\":\"assistant\",\"timestamp\":{},\"message\":{{\"content\":[]}}}}\n\
         {{\"type\":\"assistant\",\"timestamp\":{},\"message\":{{\"content\":[]}}}}\n\
         {{\"type\":\"assistant\",\"timestamp\":{},\"message\":{{\"content\":[]}}}}\n\
         {{\"type\":\"assistant\",\"timestamp\":{},\"message\":{{\"content\":[]}}}}\n\
         {{\"type\":\"assistant\",\"timestamp\":{},\"message\":{{\"content\":[]}}}}\n\
         {{\"type\":\"assistant\",\"timestamp\":{},\"message\":{{\"content\":[]}}}}\n",
        i64::MIN,
        0,
        received - DAY_MS - 1,
        received - DAY_MS,
        received,
        received + DAY_MS,
        received + DAY_MS + 1,
        i64::MAX,
    );
    let (_, descriptor) = project_file(&temp, bytes.as_bytes());
    let batch =
        collect_ingest(&descriptor, None, &PriceCatalog::default(), received, false).unwrap();
    assert_eq!(batch.events.len(), 8);
    assert_eq!(
        batch
            .events
            .iter()
            .map(|event| event.occurred_at_ms)
            .collect::<Vec<_>>(),
        vec![
            received,
            received,
            received,
            received - DAY_MS,
            received,
            received + DAY_MS,
            received,
            received,
        ]
    );
}

#[test]
fn historical_transcript_timestamps_survive_a_later_reindex_time() {
    let temp = tempdir().unwrap();
    let received = 1_800_000_000_000_i64;
    let historical = 1_700_000_000_000_i64;
    let (_, descriptor) = project_file(
        &temp,
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":{historical},\"message\":{{\"content\":[]}}}}\n"
        )
        .as_bytes(),
    );
    let batch =
        collect_ingest(&descriptor, None, &PriceCatalog::default(), received, true).unwrap();
    assert!(batch.historical_replay);
    assert!(!batch.notifications_allowed);
    assert_eq!(batch.events.len(), 1);
    assert_eq!(batch.events[0].occurred_at_ms, historical);
    assert_eq!(batch.events[0].received_at_ms, historical);
    assert_eq!(batch.events[0].logical_at_ms(), historical);
}

#[test]
fn implausible_historical_timestamp_still_falls_back_to_scan_time() {
    let temp = tempdir().unwrap();
    let scanned = 1_800_000_000_000_i64;
    let (_, descriptor) = project_file(
        &temp,
        br#"{"type":"assistant","timestamp":1,"message":{"content":[]}}
"#,
    );
    let batch = collect_ingest(&descriptor, None, &PriceCatalog::default(), scanned, true).unwrap();

    assert_eq!(batch.events[0].occurred_at_ms, scanned);
    assert_eq!(batch.events[0].received_at_ms, scanned);
    assert_eq!(batch.events[0].logical_at_ms(), scanned);
}

#[test]
fn historical_main_and_subagent_replay_is_discovery_order_independent() {
    let temp = tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir_all(project.join("shared-session/subagents")).unwrap();
    fs::write(
        project.join("shared-session.jsonl"),
        br#"{"type":"assistant","timestamp":1,"message":{"content":[{"type":"thinking","thinking":"historical"}]}}
"#,
    )
    .unwrap();
    fs::write(
        project.join("shared-session/subagents/agent.jsonl"),
        br#"{"type":"assistant","timestamp":1,"isSidechain":true,"message":{"content":[{"type":"tool_use","name":"Read"}]}}
"#,
    )
    .unwrap();

    let descriptors = discover(temp.path()).unwrap();
    assert_eq!(descriptors.len(), 2);
    assert!(descriptors
        .iter()
        .all(|descriptor| descriptor.session_id() == "shared-session"));

    let received = 1_800_000_000_000_i64;
    let catalog = PriceCatalog::default();
    let replay = |ordered: Vec<&DiscoveredTranscript>| {
        let mut events = Vec::new();
        for descriptor in ordered {
            let batch = collect_ingest(descriptor, None, &catalog, received, true).unwrap();
            assert!(batch.historical_replay);
            assert!(!batch.notifications_allowed);
            assert_eq!(batch.events.len(), 1);
            assert_eq!(batch.events[0].occurred_at_ms, received);
            assert_eq!(batch.events[0].received_at_ms, received);
            assert_eq!(batch.events[0].session_id.0, "shared-session");
            events.extend(batch.events);
        }
        assert_eq!(events.len(), 2);
        monitor_domain::reduce(events)
    };

    let forward = replay(descriptors.iter().collect());
    let reverse = replay(descriptors.iter().rev().collect());
    assert_eq!(forward, reverse);
    assert!(forward.notifications.is_empty());
    let projection = forward.projection.unwrap();
    assert_eq!(projection.session_id.0, "shared-session");
    assert_eq!(projection.last_observed_at_ms, received);
    assert_eq!(projection.revision, 2);
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

#[cfg(unix)]
#[test]
fn ingest_rejects_parent_replacement_and_non_regular_final_targets() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("session.jsonl"), b"").unwrap();
    let descriptor = discover(temp.path()).unwrap().remove(0);

    fs::remove_file(descriptor.path()).unwrap();
    fs::create_dir(descriptor.path()).unwrap();
    assert!(matches!(
        collect_ingest(&descriptor, None, &PriceCatalog::default(), 0, true),
        Err(TranscriptError::NonRegular)
    ));

    fs::remove_dir(descriptor.path()).unwrap();
    fs::remove_dir(&project).unwrap();
    fs::create_dir(outside.path().join("project")).unwrap();
    fs::write(outside.path().join("project/session.jsonl"), b"").unwrap();
    symlink(outside.path().join("project"), &project).unwrap();
    assert!(matches!(
        collect_ingest(&descriptor, None, &PriceCatalog::default(), 0, true),
        Err(TranscriptError::EscapedRoot)
    ));
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
    let task = start_all_history_with_catalog(
        temp.path().to_path_buf(),
        0,
        PriceCatalog::default(),
        move |_| {
            sink_count.fetch_add(1, Ordering::Relaxed);
            Ok(())
        },
        |_| Ok(()),
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
    let task = start_all_history_with_catalog(
        missing,
        0,
        PriceCatalog::default(),
        |_| Ok(()),
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
    let task = start_all_history_with_catalog(
        temp.path().to_path_buf(),
        0,
        PriceCatalog::default(),
        move |batch| {
            if let TranscriptStreamItem::Chunk { events, usage, .. } = batch.item {
                max_sink.fetch_max(events.len() + usage.len(), Ordering::Relaxed);
                chunk_sink.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(())
        },
        |_| Ok(()),
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
    let task = start_all_history_with_catalog(
        temp.path().to_path_buf(),
        0,
        PriceCatalog::default(),
        move |_| {
            if sink_attempts.fetch_add(1, Ordering::Relaxed) == 4 {
                Err("fixture sink failure".into())
            } else {
                Ok(())
            }
        },
        |_| Ok(()),
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

#[tokio::test]
async fn all_history_persists_ten_thousand_present_paths_in_pages_of_at_most_sixty_four() {
    let temp = tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir_all(&project).unwrap();
    for index in 0..10_000 {
        fs::write(project.join(format!("session-{index:05}.jsonl")), b"").unwrap();
    }
    let maximum = Arc::new(AtomicUsize::new(0));
    let total = Arc::new(AtomicUsize::new(0));
    let maximum_sink = Arc::clone(&maximum);
    let total_sink = Arc::clone(&total);
    let task = start_all_history_with_catalog(
        temp.path().to_path_buf(),
        0,
        PriceCatalog::default(),
        |_| Ok(()),
        move |paths| {
            maximum_sink.fetch_max(paths.len(), Ordering::Relaxed);
            total_sink.fetch_add(paths.len(), Ordering::Relaxed);
            Ok(())
        },
        |_| {},
    );
    let summary = task.result.await.unwrap().unwrap();
    assert_eq!(summary.completed, 10_000);
    assert_eq!(total.load(Ordering::Relaxed), 10_000);
    assert_eq!(maximum.load(Ordering::Relaxed), 64);
}

#[tokio::test]
async fn all_history_can_cancel_at_the_first_present_path_page_boundary() {
    let temp = tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir_all(&project).unwrap();
    for index in 0..130 {
        fs::write(project.join(format!("cancel-{index:03}.jsonl")), b"").unwrap();
    }
    let pages = Arc::new(AtomicUsize::new(0));
    let page_sink = Arc::clone(&pages);
    let task = start_all_history_with_catalog(
        temp.path().to_path_buf(),
        0,
        PriceCatalog::default(),
        |_| Ok(()),
        move |paths| {
            assert!(paths.len() <= 64);
            page_sink.fetch_add(1, Ordering::Relaxed);
            Err("index_interrupted".into())
        },
        |_| {},
    );
    assert!(task.result.await.unwrap().is_err());
    assert_eq!(pages.load(Ordering::Relaxed), 1);
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
fn every_streamed_chunk_keeps_the_legacy_sha256_prefix_anchor() {
    let temp = tempdir().unwrap();
    let line =
        b"{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"x\"}]}}\n";
    let mut bytes = Vec::new();
    for _ in 0..700 {
        bytes.extend_from_slice(line);
    }
    let (_, descriptor) = project_file(&temp, &bytes);
    let mut cursors = Vec::new();

    let scan = ingest_streaming(
        &descriptor,
        None,
        &PriceCatalog::default(),
        0,
        true,
        &mut |item| {
            if let TranscriptStreamItem::Chunk { cursor_after, .. } = item {
                cursors.push(cursor_after);
            }
            Ok(())
        },
    )
    .unwrap();

    assert!(cursors.len() >= 3, "fixture must exercise multiple chunks");
    assert!(cursors
        .windows(2)
        .all(|pair| pair[0].byte_offset < pair[1].byte_offset));
    for cursor in cursors.iter().chain(std::iter::once(&scan.cursor)) {
        let prefix = &bytes[..cursor.byte_offset as usize];
        assert_eq!(
            cursor.content_anchor.as_deref(),
            Some(hex::encode(Sha256::digest(prefix)).as_str())
        );
    }
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

#[test]
fn exact_freshness_token_skips_all_content_reads_and_stream_writes() {
    use adapter_claude::transcript::ingest_streaming_with_freshness;

    let temp = tempdir().unwrap();
    let (_path, descriptor) = project_file(
        &temp,
        b"{\"type\":\"assistant\",\"message\":{\"content\":[]}}\n",
    );
    let catalog = PriceCatalog::default();
    let first = collect_ingest(&descriptor, None, &catalog, 1_000, false).unwrap();
    assert!(first.cursor.modified_at_ns.is_some());
    let mut stream_items = 0;
    let second = ingest_streaming_with_freshness(
        &descriptor,
        Some(&first.cursor),
        &catalog,
        1_001,
        false,
        false,
        &mut |_| {
            stream_items += 1;
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(stream_items, 0, "unchanged scans must not begin an ingest");
    assert_eq!(second.bytes_read, 0);
    assert_eq!(second.cursor, first.cursor);
}

#[test]
fn freshness_change_scans_immediately_and_periodic_audit_detects_same_size_rewrite() {
    use adapter_claude::transcript::ingest_streaming_with_freshness;

    let temp = tempdir().unwrap();
    let (path, descriptor) = project_file(
        &temp,
        b"{\"type\":\"assistant\",\"message\":{\"content\":[]}}\n",
    );
    let catalog = PriceCatalog::default();
    let first = collect_ingest(&descriptor, None, &catalog, 1_000, false).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"type\":\"assistant\",\"message\":{\"content\":[]}}\n")
        .unwrap();
    let mut append_items = 0;
    let appended = ingest_streaming_with_freshness(
        &descriptor,
        Some(&first.cursor),
        &catalog,
        1_001,
        false,
        false,
        &mut |_| {
            append_items += 1;
            Ok(())
        },
    )
    .unwrap();
    assert!(append_items > 0);
    assert!(appended.bytes_read > 0);

    let mut rewritten = fs::read(&path).unwrap();
    rewritten[0] = b' ';
    fs::write(&path, rewritten).unwrap();
    let audited = ingest_streaming_with_freshness(
        &descriptor,
        Some(&appended.cursor),
        &catalog,
        1_002,
        false,
        true,
        &mut |_| Ok(()),
    )
    .unwrap();
    assert!(
        audited.reset,
        "a due full audit must detect content changes even when size is unchanged"
    );
}

#[test]
fn repeated_small_appends_read_only_new_bytes_between_periodic_audits() {
    use adapter_claude::transcript::ingest_streaming_with_freshness;

    let temp = tempdir().unwrap();
    let mut aligned_line = vec![b' '; 63];
    aligned_line.push(b'\n');
    let (path, descriptor) = project_file(&temp, &aligned_line);
    let catalog = PriceCatalog::default();
    let mut cursor = collect_ingest(&descriptor, None, &catalog, 1_000, false)
        .unwrap()
        .cursor;
    let mut incremental_bytes = 0_u64;
    for index in 0..40 {
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&aligned_line)
            .unwrap();
        let scan = ingest_streaming_with_freshness(
            &descriptor,
            Some(&cursor),
            &catalog,
            1_001 + index,
            false,
            false,
            &mut |_| Ok(()),
        )
        .unwrap();
        incremental_bytes = incremental_bytes.saturating_add(scan.bytes_read);
        cursor = scan.cursor;
    }
    assert_eq!(
        incremental_bytes,
        (aligned_line.len() * 40) as u64,
        "normal append scans must not reread a previously verified prefix"
    );

    let audited = ingest_streaming_with_freshness(
        &descriptor,
        Some(&cursor),
        &catalog,
        2_000,
        false,
        true,
        &mut |_| Ok(()),
    )
    .unwrap();
    assert_eq!(audited.bytes_read, fs::metadata(path).unwrap().len());
}

#[test]
fn corrupt_or_unknown_hash_checkpoint_falls_back_to_full_prefix_verification() {
    use adapter_claude::transcript::ingest_streaming_with_freshness;

    let temp = tempdir().unwrap();
    let (path, descriptor) = project_file(&temp, b"{}\n");
    let catalog = PriceCatalog::default();
    let mut cursor = collect_ingest(&descriptor, None, &catalog, 1_000, false)
        .unwrap()
        .cursor;
    cursor.hash_checkpoint.as_mut().unwrap()[4] = 255;
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{}\n")
        .unwrap();
    let scan = ingest_streaming_with_freshness(
        &descriptor,
        Some(&cursor),
        &catalog,
        1_001,
        false,
        false,
        &mut |_| Ok(()),
    )
    .unwrap();

    assert!(!scan.reset);
    assert_eq!(
        scan.bytes_read,
        fs::metadata(path).unwrap().len(),
        "an unrecognized checkpoint must never skip file verification"
    );
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
    let path_text = path.to_string_lossy().into_owned();
    let cursor_position = TranscriptCursorPosition {
        file_identity: first.cursor.file_identity.clone(),
        byte_offset: first.cursor.byte_offset as i64,
        file_size: first.cursor.file_size as i64,
        modified_at_ms: first.cursor.modified_at_ms,
        modified_at_ns: first.cursor.modified_at_ns,
        content_anchor: first.cursor.content_anchor.clone(),
        hash_checkpoint: first.cursor.hash_checkpoint.clone(),
    };
    let mut repository = TranscriptIngestRepository::new(pool.clone());
    repository
        .begin(
            path_text.clone(),
            descriptor.session_id().to_owned(),
            false,
            false,
            1,
        )
        .await
        .unwrap();
    repository.commit(&cursor_position, 1).await.unwrap();
    let metadata: Option<Vec<u8>> = sqlx::query("SELECT partial_line FROM transcript_cursors")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
    assert!(metadata
        .as_deref()
        .is_some_and(|value| value.starts_with(b"CMC1")));
    let stored = load_cursor(&pool, &path_text).await.unwrap().unwrap();
    assert_eq!(stored.modified_at_ns, first.cursor.modified_at_ns);
    assert_eq!(stored.hash_checkpoint, first.cursor.hash_checkpoint);
    let cursor = TranscriptCursor {
        file_identity: stored.file_identity,
        byte_offset: stored.byte_offset as u64,
        file_size: stored.file_size as u64,
        modified_at_ms: stored.modified_at_ms,
        modified_at_ns: stored.modified_at_ns,
        content_anchor: stored.content_anchor,
        hash_checkpoint: stored.hash_checkpoint,
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
