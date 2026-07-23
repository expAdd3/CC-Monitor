"""Language-neutral characterization contracts for the Rust rewrite.

The small reducer below is deliberately test-only: it validates that committed
fixtures and expected snapshots express the approved specification. Production
Python remains the legacy baseline; future Rust tests should consume the same
fixture files directly.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import sqlite3
import time
import hashlib
from datetime import datetime
from pathlib import Path

import pytest

import cc_pricing
import cc_hook


FIXTURES = Path(__file__).parent / "fixtures"
HOOK_CASES = json.loads(
    (FIXTURES / "hooks" / "transition_cases.json").read_text()
)
TRANSCRIPT_EXPECTED = json.loads(
    (FIXTURES / "expected" / "transcript_contract.json").read_text()
)
NORMALIZED_EVENT_SNAPSHOTS = json.loads(
    (FIXTURES / "hooks" / "normalized_event_snapshots.json").read_text()
)
TRANSITION_STEP_SNAPSHOTS = json.loads(
    (FIXTURES / "hooks" / "transition_step_snapshots.json").read_text()
)
DUPLICATE_JOURNALS = json.loads(
    (FIXTURES / "hooks" / "duplicate_journals.json").read_text()
)
EXPECTED_PROJECTIONS = json.loads(
    (FIXTURES / "hooks" / "expected_projection_snapshots.json").read_text()
)
TERMINAL_GUARD_JOURNALS = json.loads(
    (FIXTURES / "hooks" / "terminal_guard_journals.json").read_text()
)
ORDERING_JOURNAL = json.loads(
    (FIXTURES / "hooks" / "ordering_journal.json").read_text()
)
USAGE_IDENTITY_CASES = json.loads(
    (FIXTURES / "transcripts" / "usage_identity_cases.json").read_text()
)


def _normalize(payload, ingestion_id=None, received_at_ms=None):
    """Create the stable normalized subset Phase 2's adapter must reproduce."""
    event = payload["hook_event_name"]
    detail = ""
    if event == "Notification":
        detail = str(payload.get("notification_type") or "").strip()
    elif event in {"PreToolUse", "PostToolUse"}:
        detail = str(payload.get("tool_name") or "").strip()
    if received_at_ms is None:
        received_at_ms = 1768039200000
    timestamp = payload.get("timestamp")
    try:
        occurred_at_ms = int(
            datetime.fromisoformat(
                timestamp.replace("Z", "+00:00")
            ).timestamp() * 1000
        )
    except (AttributeError, TypeError, ValueError):
        occurred_at_ms = received_at_ms
    if ingestion_id is None:
        ingestion_id = payload.get("_ingestion_id")
    if ingestion_id is None:
        # Test-fixture fallback only: equal fixture invocations get one stable
        # synthetic ingestion identity. Production always supplies UUIDv7.
        digest = hashlib.sha256(
            json.dumps(payload, sort_keys=True).encode()
        ).hexdigest()[:12]
        ingestion_id = f"018f0000-0000-7000-8000-{digest}"
    identity_inputs = ["hook", ingestion_id]
    identity = f"hook|{ingestion_id}"
    normalized_payload = {}
    for field in (
        "notification_type", "tool_name", "cwd", "transcript_path",
        "client_bundle_id",
    ):
        value = payload.get(field)
        if value is not None and value != "":
            normalized_payload[field] = value
    return {
        "id": ingestion_id,
        "agent": "claude",
        "session_id": payload["session_id"],
        "source": "hook",
        "source_event": event,
        "occurred_at_ms": occurred_at_ms,
        # The fixture clock pins receipt to occurrence. Production receipt
        # time comes from the bounded Hook ingestion clock.
        "received_at_ms": received_at_ms,
        "sequence": None,
        "dedupe_key": identity,
        "payload_version": 1,
        "payload": normalized_payload,
        "identity_inputs": identity_inputs,
    }


def _logical_at(event):
    occurred = event["occurred_at_ms"]
    received = event["received_at_ms"]
    if occurred > 0 and abs(occurred - received) <= 86_400_000:
        return occurred
    return received


def _event_order(event):
    priority = {"transcript": 0, "recovery": 1, "hook": 2}
    sequence = event.get("sequence")
    return (
        _logical_at(event),
        priority[event["source"]],
        sequence if sequence is not None else 2**63 - 1,
        event["received_at_ms"],
        event["dedupe_key"],
    )


def _reduce_events(events):
    lifecycle = None
    turn_state = None
    unresolved_question = False
    terminal_notified = False
    notifications = []
    steps = []
    reason = None
    changed_at_ms = None
    last_observed_at_ms = None
    revision = 0

    ordered_events = sorted(
        json.loads(json.dumps(events)), key=_event_order
    )
    last_source = "hook"
    for normalized in ordered_events:
        before_count = len(notifications)
        event = normalized["source_event"]
        payload = normalized["payload"]
        observed = _logical_at(normalized)
        last_source = normalized["source"]
        revision += 1
        last_observed_at_ms = observed
        before_projection = (lifecycle, turn_state, reason)
        if lifecycle == "ended" and event != "SessionStart":
            steps.append({
                "lifecycle": lifecycle,
                "turn_state": turn_state,
                "new_notifications": [],
            })
            continue
        if event == "SessionStart":
            lifecycle, turn_state = "active", "waiting"
            reason = "session_started"
            terminal_notified = False
        elif event == "UserPromptSubmit":
            lifecycle, turn_state = "active", "running"
            reason = "user_prompt_submitted"
            unresolved_question = False
            terminal_notified = False
        elif event == "PreToolUse":
            lifecycle = "active"
            if payload.get("tool_name") == "AskUserQuestion":
                if turn_state != "needs_input":
                    notifications.append("needs_input")
                turn_state = "needs_input"
                reason = "ask_user_question"
                unresolved_question = True
            else:
                turn_state = "running"
                reason = "tool_running"
        elif event == "PostToolUse":
            lifecycle, turn_state = "active", "running"
            reason = "tool_running"
        elif event == "Notification":
            lifecycle = "active"
            kind = payload.get("notification_type") or ""
            if kind == "idle_prompt":
                turn_state = "waiting"
                reason = "idle_prompt"
            elif kind in {
                "auth_success", "elicitation_complete", "elicitation_response"
            }:
                turn_state = "running"
                reason = kind
                unresolved_question = False
            else:
                if turn_state != "needs_input":
                    notifications.append("needs_input")
                turn_state = "needs_input"
                reason = (
                    kind if kind in {"permission_prompt", "elicitation_dialog"}
                    else "unknown_notification"
                )
        elif event == "Stop":
            if unresolved_question:
                turn_state = "needs_input"
            else:
                turn_state = "waiting"
                reason = "turn_stopped"
                if not terminal_notified:
                    notifications.append("done")
                    terminal_notified = True
        elif event == "StopFailure":
            turn_state = "failed"
            reason = "stop_failed"
            if not terminal_notified:
                notifications.append("failed")
                terminal_notified = True
        elif event == "SessionEnd":
            lifecycle = "ended"
            reason = "session_ended"
        if (lifecycle, turn_state, reason) != before_projection:
            changed_at_ms = observed
        steps.append({
            "lifecycle": lifecycle,
            "turn_state": turn_state,
            "new_notifications": notifications[before_count:],
        })

    return {
        "normalized_events": ordered_events,
        "projection": {
            "lifecycle": lifecycle,
            "turn_state": turn_state,
            "reason": reason,
            "source": last_source,
            "confidence": (
                "definitive" if last_source == "hook" else "inferred"
            ),
            "revision": revision,
            "changed_at_ms": changed_at_ms,
            "last_observed_at_ms": last_observed_at_ms,
        },
        "notifications": notifications,
        "steps": steps,
    }


def _reduce(payloads):
    return _reduce_events([_normalize(item) for item in payloads])


def _replay_same_store(payloads):
    """Replay one journal with raw-event and notification-key uniqueness."""
    seen = set()
    accepted = []
    for payload in payloads:
        event = _normalize(payload)
        if event["dedupe_key"] in seen:
            continue
        seen.add(event["dedupe_key"])
        accepted.append(event)
    reduced = _reduce_events(accepted)
    keys = []
    for revision, step in enumerate(reduced["steps"], start=1):
        for kind in step["new_notifications"]:
            keys.append(f"fixture-session:{revision}:{kind}")
    return {
        "revision": len(accepted),
        "lifecycle": reduced["projection"]["lifecycle"],
        "turn_state": reduced["projection"]["turn_state"],
        "notification_keys": keys,
    }


def _json_lines(path):
    records = []
    with path.open("rb") as stream:
        for line in stream:
            try:
                records.append(json.loads(line))
            except (json.JSONDecodeError, UnicodeDecodeError):
                continue
    return records


def _last_content_kind(record):
    message = record.get("message") or record
    content = message.get("content") if isinstance(message, dict) else None
    if isinstance(content, list) and content:
        return content[-1].get("type")
    if isinstance(content, str):
        return "text"
    return None


def _assert_usage(actual, expected):
    for field in (
        "input", "output", "cache_write", "cache_read", "total_tokens"
    ):
        assert actual[field] == expected[field]
    assert round(actual["cost_usd"] * 1_000_000_000_000) == (
        expected["cost_pico_usd"]
    )
    assert actual["cost_known"] is expected["cost_known"]


@pytest.mark.parametrize("case", HOOK_CASES, ids=lambda case: case["name"])
def test_hook_transition_contract_is_deterministic(case):
    first = _reduce(case["payloads"])
    second = _reduce(json.loads(json.dumps(case["payloads"])))

    expected = case["expected"]
    assert first["projection"] == EXPECTED_PROJECTIONS[case["name"]]
    assert first["notifications"] == expected["notifications"]
    assert first["steps"] == TRANSITION_STEP_SNAPSHOTS[case["name"]]
    assert json.dumps(first, sort_keys=True) == json.dumps(second, sort_keys=True)


@pytest.mark.parametrize(
    "snapshot", NORMALIZED_EVENT_SNAPSHOTS,
    ids=lambda snapshot: (
        snapshot["payload"]["hook_event_name"]
        + ":" + snapshot["expected"]["identity_inputs"][-1]
    ),
)
def test_normalized_event_matches_explicit_snapshot(snapshot):
    assert _normalize(
        snapshot["payload"],
        snapshot["ingestion_id"],
        snapshot.get("received_at_ms",
                     snapshot["expected"]["received_at_ms"]),
    ) == snapshot["expected"]


def test_serialized_agent_events_alone_replay_state():
    case = next(item for item in HOOK_CASES
                if item["name"] == "stop_after_resolved_intervention")
    serialized = json.loads(json.dumps([
        _normalize(payload) for payload in case["payloads"]
    ]))
    result = _reduce_events(serialized)
    assert result["projection"] == EXPECTED_PROJECTIONS[case["name"]]
    assert result["notifications"] == case["expected"]["notifications"]


def test_total_order_is_insertion_order_independent():
    events = ORDERING_JOURNAL["events"]
    expected = ORDERING_JOURNAL["expected"]
    permutations = [
        events,
        list(reversed(events)),
        [events[index] for index in (2, 5, 0, 4, 1, 3)],
    ]
    for insertion_order in permutations:
        result = _reduce_events(insertion_order)
        assert [event["id"] for event in result["normalized_events"]] == (
            expected["ordered_ids"]
        )
        assert result["projection"] == expected["projection"]
        assert result["notifications"] == expected["notifications"]


@pytest.mark.parametrize(
    "journal", DUPLICATE_JOURNALS, ids=lambda journal: journal["name"]
)
def test_duplicate_journal_replay_is_idempotent_in_one_store(journal):
    first = _replay_same_store(journal["payloads"])
    second = _replay_same_store(journal["payloads"] + journal["payloads"])
    assert first == journal["expected"]
    assert second == journal["expected"]


@pytest.mark.parametrize(
    "journal", TERMINAL_GUARD_JOURNALS, ids=lambda journal: journal["name"]
)
def test_terminal_and_ended_guards(journal):
    result = _reduce(journal["payloads"])
    assert {
        "lifecycle": result["projection"]["lifecycle"],
        "turn_state": result["projection"]["turn_state"],
        "notifications": result["notifications"],
    } == journal["expected"]


def test_approved_stop_failure_divergence_from_legacy_is_explicit():
    conn = sqlite3.connect(":memory:")
    try:
        cc_hook.ensure_schema(conn)
        base = {
            "session_id": "fixture-legacy-session",
            "cwd": "/fixture/project",
            "transcript_path": "/fixture/session.jsonl",
        }
        cc_hook.upsert(conn, {**base, "hook_event_name": "UserPromptSubmit"})
        cc_hook.upsert(conn, {**base, "hook_event_name": "StopFailure"})
        legacy = conn.execute(
            "SELECT status, notify_kind FROM sessions WHERE session_id=?",
            ("fixture-legacy-session",),
        ).fetchone()
    finally:
        conn.close()
    assert legacy == ("WAITING", "DONE")
    approved = next(case for case in HOOK_CASES
                    if case["name"] == "stop_failure")["expected"]
    assert (approved["turn_state"], approved["notifications"]) == (
        "failed", ["failed"]
    )


def test_transcript_usage_contract_replays_identically():
    transcript = FIXTURES / "transcripts" / "usage-session.jsonl"
    cc_pricing._SUMMARY_CACHE.clear()
    first = cc_pricing.summarize_transcript(str(transcript))
    cc_pricing._SUMMARY_CACHE.clear()
    second = cc_pricing.summarize_transcript(str(transcript))

    assert first == second
    _assert_usage(first, TRANSCRIPT_EXPECTED["usage"])


def test_transcript_daily_usage_contract_covers_multiple_days():
    transcript = FIXTURES / "transcripts" / "usage-session.jsonl"
    actual = cc_pricing.summarize_transcript_by_day(str(transcript))
    expected = TRANSCRIPT_EXPECTED["daily_usage"]

    first_day = cc_pricing._day_from_timestamp("2026-01-10T00:30:01Z")
    second_day = cc_pricing._day_from_timestamp("2026-01-12T12:01:00Z")
    expected_by_day = {
        first_day: expected["2026-01-10"],
        second_day: expected["2026-01-12"],
    }
    assert set(actual) == set(expected_by_day)
    for day, day_expected in expected_by_day.items():
        _assert_usage(actual[day], day_expected)


@pytest.mark.parametrize("timezone,expected_days", [
    ("UTC", ("2026-01-10", "2026-01-12")),
    ("Asia/Shanghai", ("2026-01-10", "2026-01-12")),
    ("America/Los_Angeles", ("2026-01-09", "2026-01-12")),
])
def test_daily_usage_uses_current_system_timezone(timezone, expected_days):
    if not hasattr(time, "tzset"):
        pytest.skip("timezone switching requires time.tzset")
    original = os.environ.get("TZ")
    try:
        os.environ["TZ"] = timezone
        time.tzset()
        # Reindex semantics: rebuild derived local-day data under the current
        # timezone rather than reuse a prior timezone's legacy Python cache.
        cc_pricing._SUMMARY_CACHE.clear()
        transcript = FIXTURES / "transcripts" / "usage-session.jsonl"
        actual = cc_pricing.summarize_transcript_by_day(str(transcript))
        assert set(actual) == set(expected_days)
        _assert_usage(
            actual[expected_days[0]],
            TRANSCRIPT_EXPECTED["daily_usage"]["2026-01-10"],
        )
        _assert_usage(
            actual[expected_days[1]],
            TRANSCRIPT_EXPECTED["daily_usage"]["2026-01-12"],
        )
    finally:
        if original is None:
            os.environ.pop("TZ", None)
        else:
            os.environ["TZ"] = original
        time.tzset()


def test_partial_and_malformed_lines_leave_complete_records_usable():
    path = FIXTURES / "transcripts" / "partial-line.jsonl"
    records = _json_lines(path)

    assert len(records) == TRANSCRIPT_EXPECTED["partial_line"]["parsed_records"]
    assert _last_content_kind(records[-1]) == (
        TRANSCRIPT_EXPECTED["partial_line"]["last_complete_kind"]
    )


def test_truncation_fixture_recovers_from_replacement(tmp_path):
    live = tmp_path / "live.jsonl"
    before = FIXTURES / "transcripts" / "truncation-before.jsonl"
    after = FIXTURES / "transcripts" / "truncation-after.jsonl"
    shutil.copyfile(before, live)

    old_size = live.stat().st_size
    old_records = _json_lines(live)
    shutil.copyfile(after, live)
    new_size = live.stat().st_size
    new_records = _json_lines(live)

    assert new_size < old_size
    assert _last_content_kind(old_records[-1]) == (
        TRANSCRIPT_EXPECTED["truncation"]["before_last_kind"]
    )
    assert _last_content_kind(new_records[-1]) == (
        TRANSCRIPT_EXPECTED["truncation"]["after_last_kind"]
    )


@pytest.mark.parametrize(
    "case",
    json.loads(
        (FIXTURES / "transcripts" / "status_cases.json").read_text()
    ),
    ids=lambda case: case["name"],
)
def test_transcript_status_inference_contract(case):
    kind = _last_content_kind(case["object"])
    role = case["object"].get("role") or case["object"].get("type")
    age = case["age_seconds"]

    if role == "assistant" and kind in {"tool_use", "thinking"}:
        actual = "running"
    elif kind == "tool_result":
        actual = "running"
    elif role == "assistant" and kind in {"text", None}:
        actual = "waiting" if age > 30 else "running"
    else:
        actual = "running"

    assert actual == case["expected_state"]


def test_fixtures_have_only_synthetic_paths_and_no_credentials():
    forbidden = (
        re.compile(r"(?i)\b(password|secret|credential|authorization|bearer|basic)\b"),
        re.compile(r"(?i)\btoken\s*="),
        re.compile(r"(?i)https?://"),
        re.compile(r"(?i)\b[\w.+-]+@[\w.-]+\.[a-z]{2,}\b"),
        re.compile(r"\b(?:\d{1,3}\.){3}\d{1,3}\b"),
        re.compile(r"(?:/Users/|/home/|~/)"),
    )
    for path in sorted(FIXTURES.rglob("*")):
        if not path.is_file() or path.suffix not in {".json", ".jsonl"}:
            continue
        text = path.read_text(errors="replace")
        assert not any(pattern.search(text) for pattern in forbidden), path
        username = os.environ.get("USER")
        if username:
            assert not re.search(
                rf"(?i)(?:/|\\\\){re.escape(username)}(?:/|\\\\)", text
            ), path


def test_same_ms_distinct_ingestions_and_exact_retry_identity():
    journal = next(item for item in TERMINAL_GUARD_JOURNALS
                   if item["name"] == "same_ms_distinct_ingestions_are_distinct")
    first, second = journal["payloads"]
    first_event = _normalize(first)
    second_event = _normalize(second)
    assert first_event["occurred_at_ms"] == second_event["occurred_at_ms"]
    assert first_event["dedupe_key"] != second_event["dedupe_key"]
    assert _normalize(first) == first_event
    replay = _replay_same_store([first, first, second])
    assert replay["revision"] == 2


def _dedupe_usage(records):
    chosen = {}
    for record in records:
        prefix = (record["agent_kind"], record["session_id"])
        message_id = record.get("message_id") or ""
        request_id = record.get("request_id") or ""
        if message_id and request_id:
            key = (*prefix, "message_request", message_id, request_id)
        elif message_id:
            key = (*prefix, "message", message_id)
        elif request_id:
            key = (*prefix, "request", request_id)
        else:
            key = (*prefix, "anonymous_latest")
        previous = chosen.get(key)
        rank = (record["observed_at_ms"], record["source_location"])
        if previous is None or rank > (
            previous["observed_at_ms"], previous["source_location"]
        ):
            chosen[key] = record
    return [chosen[key] for key in sorted(chosen)]


@pytest.mark.parametrize(
    "case", USAGE_IDENTITY_CASES, ids=lambda case: case["name"]
)
def test_usage_identity_contract(case):
    assert all(record["source_location"] for record in case["records"])
    first = _dedupe_usage(case["records"])
    second = _dedupe_usage(list(reversed(case["records"])))
    assert [record["source_location"] for record in first] == (
        case["expected_locations"]
    )
    assert first == second
