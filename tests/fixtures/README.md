# Rust contract fixtures

These synthetic fixtures are consumed by the Rust adapter, reducer, engine,
and storage tests. Tests may copy them to temporary directories but must not
rewrite the committed files.

## Sanitization

- Identifiers use `fixture-*`.
- Paths use `/fixture/...`; no username or home directory is present.
- Text, timestamps, token values, and tool data were created for this project.
- No real transcripts, prompts, endpoints, credentials, authorization headers,
  or terminal metadata are included.

## Contents

- `hooks/transition_cases.json` covers the Hook state-transition table.
- `hooks/transition_step_snapshots.json` records per-step projections.
- `hooks/normalized_event_snapshots.json` records normalized Hook events.
- `hooks/ordering_journal.json` covers logical-time validation, source
  priority, tie-breaking, and deterministic replay.
- `hooks/terminal_guard_journals.json` covers ended-session reactivation and
  one terminal notification per turn.
- `hooks/duplicate_journals.json` and
  `hooks/expected_projection_snapshots.json` cover idempotency.
- `transcripts/usage-session.jsonl` covers duplicate usage, malformed records,
  known and unknown models, and multiple local-calendar days.
- `transcripts/usage-session/subagents/agent-fixture.jsonl` covers subagent
  usage.
- `transcripts/truncation-before.jsonl` and `truncation-after.jsonl` cover
  cursor reset and deterministic replacement.
- `transcripts/usage_identity_cases.json` covers stable-ID, message-only,
  request-only, and anonymous-latest usage identity.
- `ntfy_topics.json` is the language-neutral topic grammar contract consumed
  independently by the Rust provider, React settings tests, and Bash deploy
  contract tests.

Run the complete Rust, React, and deploy contract suites with:

```sh
cargo test --workspace --all-features --all-targets
bun run test
bun run test:deploy
```

The deploy suite consumes the same `ntfy_topics.json` fixture and also checks
that a leading-hyphen topic is passed to the ntfy CLI after an explicit `--`
option terminator.
