# CC Monitor architecture

This document is the product and architecture contract for CC Monitor. The
application is a small, single-user, single-machine Tauri utility. Reliability
comes from committed evidence, deterministic recomputation, and one ordinary
notification outbox—not from distributed coordination.

## Product boundary

CC Monitor observes Claude Code sessions, derives their current state, records
Token usage and known cost, and optionally sends desktop or ntfy
notifications. It provides a macOS menu-bar view and a dashboard.

The supported states are:

- `running`
- `waiting`
- `needs_input`
- `failed`
- ended sessions (`lifecycle = ended`)

All state is derived by the reducer in `monitor-domain`. Adapters provide
evidence; they do not contain competing state machines.

The canonical bundle identifier and application-data namespace are
`com.ccmonitor`. The former `com.lixinyu.ccmonitor` namespace is not imported or
deleted automatically. The legacy `~/.cc-monitor/state.db` is outside
application scope and must never be inspected, migrated, imported, or deleted
by CC Monitor.

## Components

### Hook collector

`cc-monitor-hook` receives one Claude Code Hook invocation, validates bounded
input, appends one sanitized `raw_events` row, and exits successfully.

The Hook:

- never runs migrations;
- never parses transcripts;
- never derives session state or usage;
- never sends notifications;
- never blocks Claude Code on an unavailable or locked database;
- bounds input size, input wait, SQLite wait, retries, and diagnostics;
- records no transcript content, credentials, authorization headers, or other
  secrets.

Only an exact, application-owned Hook installation may write to the application
database. Installation and removal preserve unrelated Claude Code settings and
prune only empty structure created by CC Monitor.

### Transcript adapter and sync

`adapter-claude` discovers main and subagent transcript files, parses them
incrementally, calculates usage using the current price catalog, and emits
bounded chunks. It does not write projections or notifications.

`monitor-storage::TranscriptIngestRepository` is the private atomic publication
mechanism:

1. `begin` discards an abandoned uncommitted attempt for the same path.
2. `chunk` writes bounded event and usage staging rows.
3. `commit` publishes events, usage candidates, and the cursor in one
   transaction.

If parsing or persistence fails before commit, the old committed file result
and cursor remain visible. A full scan collects present paths and then removes
evidence for missing paths. Affected sessions are recomputed by the same
projection pipeline.

Historical/full indexing persists `notifications_allowed = false`. First-time
indexing and historical replay must never create user notifications.

The v13 `transcript_usage_stage` table also retains committed per-source usage
candidates internally. Committed candidates use the private marker
`staged_at_ms = -1`; positive values are uncommitted staging timestamps.
`usage_records` contains one deterministic winner per dedupe key, selected by
latest observation time and then source location. This preserves a losing
candidate so it can become visible if the winning transcript disappears.

### Projection pipeline

`monitor-engine` is independent of Tauri. One bounded tick:

1. selects sessions with unprocessed `raw_events`;
2. loads all committed evidence for one session;
3. calls `monitor-domain::reduce_at`;
4. atomically replaces the current session projection and turn;
5. inserts deduplicated notification intent into `notification_outbox`;
6. marks the evidence processed.

Invalid session evidence is quarantined with a fixed diagnostic code. Storage
failure stops the batch without pretending later sessions succeeded. Restart
simply repeats work from committed evidence.

There is no second projection path, durable replay job, ready queue,
publication fence, active-generation switch, maintenance lease, or multi-phase
job interpreter.

### Notification outbox

The reducer emits notification edges for actionable live transitions. The
projection transaction inserts one row per enabled provider into the v13
`notification_outbox`; its uniqueness constraint makes recomputation
idempotent.

Desktop and ntfy workers:

1. claim one due `pending` or retryable `failed` row as `inflight`;
2. send it through the selected transport;
3. mark it `sent`, or record a fixed error code and bounded retry.

Startup returns interrupted `inflight` rows to retryable failure. A successful
delivery or explicit desktop-notification test marks that provider healthy and
clears its current failure state. Disabling ntfy suppresses unsent ntfy rows.

Notification text and activation metadata contain no transcript content.
Clicking a session notification opens the matching dashboard context; terminal
activation is best effort.

### Read module

Desktop queries read the v13 materialized tables directly:

- `session_projection` and `turns` for state;
- `usage_records` for daily, per-session, per-model, and yearly usage;
- `notification_outbox` and `notification_provider_health` for notification
  diagnostics;
- `background_task_health` for actionable worker health;
- settings, Hook ownership, and transcript cursor state for configuration and
  indexing diagnostics.

Dashboard and tray use the same presentation semantics:

- the same state labels and short session ID;
- total Token;
- known cost, when any portion can be priced;
- `费用待定` / unpriced Token for the portion without a known price.

Unpriced Token must never be presented as zero-cost Token. Read paths are
bounded for menu responsiveness; aggregate counts remain complete.

### Configuration and lifecycle

Desktop settings are stored as one JSON value in the v13 `settings` table.
Configuration validation occurs before persistence. Updating autostart uses
compensation if database persistence fails. Notification provider policy is
updated in memory only after settings persistence succeeds.

The desktop application is the only database migration owner. Tauri commands
are adapters around the Rust modules and DTOs, not domain logic.

Tauri's single-instance plugin is installed before stateful plugins or setup.
A second launch activates the existing instance. Closing the dashboard keeps
monitoring alive; Quit signals workers and terminates the application.

## Runtime

Startup performs:

1. a read-only schema compatibility preflight;
2. v1–v13 migrations, if required;
3. settings and Hook-health loading;
4. interrupted-notification reconciliation;
5. initial transcript indexing only when no cursor exists;
6. ordinary background workers.

Workers use bounded ticks for:

- Hook event processing;
- incremental transcript scans;
- time-derived transcript reconciliation;
- desktop and ntfy outbox delivery;
- old ended-session cleanup;
- old terminal notification cleanup;
- tray/dashboard invalidation.

Cancellation and worker ownership are in memory. After a crash, committed
evidence, cursors, projections, and outbox rows are sufficient to resume.

## Storage contract

The supported schema version is exactly 13.

- Migrations 1–13 are shipped, ordered, additive history and must never be
  edited after release.
- Migrations 14–42 were withdrawn before release and must not be shipped.
- A database with a successful version greater than 13 is rejected as
  `unsupported_future_schema` during read-only preflight, before a normal WAL
  connection can write.
- Automated tests use temporary databases only.

`docs/schema-v2.sql` is a readable v13 reference. The immutable SQLx migration
files are authoritative.

Some v13 objects such as `daily_usage` and
`transcript_publish_generation` remain in the physical schema for migration
compatibility. Production code does not use them. Removing them would require a
new, explicitly approved migration and has no present product benefit.

SQLite uses WAL, foreign keys, normal synchronous mode, a bounded busy timeout,
and a small connection pool. Secrets accepted for local storage remain limited
to the local SQLite settings value and are never logged.

## Failure and diagnostics contract

Failures expose fixed, user-safe codes. Untrusted filesystem paths, SQLite
messages, HTTP bodies, transcript contents, and credentials must not reach UI
errors or logs.

Diagnostics answer product questions:

- Is the Hook installed and healthy?
- Is indexing active, complete, or failing?
- Are events pending or quarantined?
- Are notifications pending or is a provider failing?
- When did each ordinary background task last succeed or fail?

They do not expose scheduler phases, owner tokens, leases, epochs, or internal
takeover state.

## Verification contract

Every product behavior receives an automated contract test where feasible.
Required gates are:

- migration checksum, fresh-v13, existing-v13, and future-schema rejection;
- Hook bounded-process, sanitization, ownership, install, and uninstall tests;
- transcript streaming, atomic publication, replacement, missing-file,
  dedupe/winner, and historical-notification tests;
- reducer fixture and deterministic ordering tests;
- projection/outbox/retry/provider-health tests;
- dashboard/tray formatting and frontend tests;
- Rust formatting, Clippy with warnings denied, full workspace tests, frontend
  tests/build, and a release application bundle.

macOS menu-bar, Dock behavior, notification authorization/click routing,
terminal activation, and the final signed application still require manual
verification on a real desktop.
