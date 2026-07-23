# Phase 1 characterization fixtures

These fixtures are the language-neutral compatibility contract for the Rust
rewrite. Tests may copy them to temporary directories, but must not rewrite
the committed files.

## Sanitization

All identifiers, paths, timestamps, prompts, tool inputs, and message text were
created for this repository. They were not copied from a user transcript.

- Session and request identifiers use `fixture-*`.
- Paths use `/fixture/...`; no username or home directory is present.
- Text content is short synthetic prose with no source code or prompt history.
- No hostnames, notification endpoints, credentials, authorization headers, or
  terminal metadata are included.
- Token values are deliberately small and artificial.

## Contents

- `hooks/transition_cases.json` covers every Hook row in the approved state
  transition table. Cases contain synthetic input journals and final decisions;
  normalized events and per-step projections live in the explicit snapshot
  files below.
- `transcripts/usage-session.jsonl` covers duplicate usage, malformed records,
  known and unknown models, and multiple local-calendar days.
- `transcripts/usage-session/subagents/agent-fixture.jsonl` covers included
  subagent usage and sidechain acceptance.
- `transcripts/partial-line.jsonl` ends with an intentionally incomplete JSON
  line. The complete record before it remains usable.
- `transcripts/truncation-before.jsonl` and `truncation-after.jsonl` are
  recovery inputs for the future cursor implementation. Phase 1 only verifies
  their before/after parser expectations; cursor persistence is implemented
  and tested in Phase 4.
- `transcripts/status_cases.json` describes transcript status inference at
  controlled file ages.
- `expected/transcript_contract.json` stores normalized usage totals, daily
  totals, and parser expectations.
- `hooks/normalized_event_snapshots.json` stores explicit normalized event
  snapshots. Each isolated snapshot supplies a stable synthetic UUIDv7-style
  ingestion ID. The missing-timestamp case proves `occurred_at_ms` falls back
  to the fixture receipt clock. Production generates one ingestion UUID per
  Hook invocation and reuses it only for retries of that invocation.
- `hooks/ordering_journal.json` freezes logical-time validation, cross-source
  priority, tie-breaking, and shuffled-insertion replay.
- `hooks/terminal_guard_journals.json` covers ended-session reactivation and
  one terminal notification per turn.
- `transcripts/usage_identity_cases.json` freezes session-scoped stable-ID,
  message-only, request-only, and anonymous-latest deduplication, including
  replacement. Every record has a non-empty stable `source_location`.
- Initial-index notification suppression is contract metadata in Phase 1. It
  becomes executable engine/Outbox coverage in Phases 4 and 5.

The JSONL fixture containing a partial final line intentionally has no valid
JSON at EOF. Do not “fix” it with a formatter.

## Running the baseline

Use pytest as the canonical Python runner:

```sh
python3 -m pytest -q
```

`python3 -m unittest discover` still runs the 45 legacy `unittest.TestCase`
tests, but it cannot discover the pytest-style pricing suite. The pytest
command collects both styles and is therefore the intentional full regression
baseline.
