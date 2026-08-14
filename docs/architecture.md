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

The `transcript_usage_stage` table also retains committed per-source usage
candidates internally. Committed candidates use the private marker
`staged_at_ms = -1`; positive values are uncommitted staging timestamps.
`usage_records` contains one deterministic winner per dedupe key, selected by
latest observation time and then source location. This preserves a losing
candidate so it can become visible if the winning transcript disappears. A
file publication or removal reconciles only the dedupe keys contributed by
that file; unrelated winners are never deleted and rebuilt.

The same publication transaction refreshes materialized per-session,
per-model, and per-local-day usage aggregates. Totals use Rust saturating
integer addition and persist unpriced Token separately from known cost. Since
a saturated total cannot be safely adjusted by subtraction, winner changes
re-stream only their affected session/model/day groups through indexed
`usage_records` lookups; unrelated groups are untouched. Migration 2 performs
a restartable one-time backfill in fixed-size keyset pages before desktop
workers start.

### Projection pipeline

`monitor-engine` is independent of Tauri. One bounded tick:

1. selects sessions with unprocessed `raw_events`;
2. loads all committed evidence for one session;
3. calls `monitor-domain::reduce_at`;
4. atomically replaces the current session projection and turn;
5. inserts deduplicated notification intent into `notification_outbox`;
6. marks only the evidence IDs loaded into that reducer snapshot as processed.

An event committed by the Hook after the snapshot remains pending for the next
tick. All desktop reducer calls and ended-session retention steps share one
in-process serialization boundary; Hook and transcript evidence writers do not
take that lock. Retention also obtains the SQLite writer before selecting its
bounded candidate batch and excludes any session with pending evidence, so a
concurrently resumed session cannot be removed or recreated from a stale
reducer snapshot.

An invalid evidence row is quarantined with a fixed diagnostic code and is
excluded from later reductions, so it cannot poison valid sibling evidence.
Storage failure stops the batch without pretending later sessions succeeded.
Restart simply repeats work from committed evidence.

There is no second projection path, durable replay job, ready queue,
publication fence, active-generation switch, maintenance lease, or multi-phase
job interpreter.

### Notification outbox

The reducer emits notification edges for actionable live transitions. The
projection transaction inserts one row per enabled provider only when the
edge's triggering event was unprocessed in that reducer snapshot and allowed
notifications. Outbox identity is derived from the triggering event, edge
kind, and provider, so historical evidence cannot recreate an already handled
edge merely by shifting its projection revision.

Desktop and ntfy workers:

1. claim one due `pending` or retryable `failed` row as `inflight`;
2. send it through the selected transport;
3. atomically mark it `sent` and the provider healthy, or record a fixed error
   code and bounded retry.

Startup returns interrupted `inflight` rows to retryable failure. A successful
delivery or explicit desktop-notification test marks that provider healthy and
clears its current failure state. Disabling ntfy suppresses unsent ntfy rows.

Notification text and activation metadata contain no transcript content.
Clicking a session notification opens the matching dashboard context; terminal
activation is best effort. Action observation is bounded to one hour so macOS
notification dismissal cannot retain blocking listener work indefinitely.

### Read module

Desktop queries read the materialized tables directly:

- `session_projection` and `turns` for state;
- `usage_session_aggregates`, `usage_model_aggregates`, and
  `usage_daily_aggregates` for bounded daily, per-session, per-model, and
  yearly usage reads;
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

While the dashboard window remains open, it schedules one refresh at the next
local midnight and reschedules after it fires. This advances “today” and the
rolling 30-day and yearly windows even when no monitored event invalidates the
snapshot.

Session details expose per-model input, output, cache-read, and cache-write
Token totals separately, with aggregate Token and known-cost coverage retained
as secondary row context. The session summary receives the wider workspace
column; event history remains a compact chronological companion.

History presents the latest 30 local calendar days as one fixed-width compact
bar chart, filling missing dates with zero rather than omitting them. The chart
uses a linear Token scale, five sampled date ticks, one keyboard focus target,
and one selected-day detail. A non-interactive semantic 30-row table exposes
the same Token and cost-coverage data to assistive technology without adding 30
Tab stops.

The macOS 12 deployment target is retained. CSS that uses newer WebKit features
such as `color-mix()` and `:focus-visible` must keep an earlier declaration or
`:focus` fallback so status levels and keyboard focus remain visible on the
oldest supported system WebView.

Unpriced Token must never be presented as zero-cost Token. Read paths are
bounded for menu responsiveness; aggregate counts remain complete.
The tray shows one disabled `暂无活跃会话` row when its bounded session list is
empty. Otherwise it keeps the complete aggregate status and labels the bounded
list `最近活跃会话` without presenting the rendered row count as a total.
Each session's actionable primary row carries one accessibility label with its
state, short ID, Token, cost coverage, and recent activity time; the visual
metadata row is excluded from the accessibility tree.

### Configuration and lifecycle

Desktop settings are stored as one JSON value in the v1 `settings` table.
User-managed model rates are stored separately in `price_overrides`; they
override the bundled catalog for future transcript scans and full reindexing.
Model IDs are canonicalized by the pricing module before comparison or new
persistence, so each identity has at most one stored row. A stored row with an
incomplete rate set is ignored rather than silently pricing a Token class at
zero.
The product exposes one flat effective catalog: every visible price has the
same edit and delete operations, with no built-in, custom, disabled, or restore
state in the UI. A fresh catalog includes all shipped defaults. Deleting a
shipped default or its override writes an internal tombstone so fallback data
does not resurrect it after restart; deleting a custom-only price removes its
stored row. Saving the same model ID later atomically clears any tombstone and
publishes the entered price. Tombstones never appear in pricing queries. The
Claude pricing adapter owns this identity rule: exact, provider-stripped, and
dated/undated fallback resolution all consult the same catalog state used to
decide whether deletion requires a tombstone. A provider-qualified tombstone
blocks only that qualified identity, while a dated-family tombstone prevents
the deleted version from silently resolving through its undated family;
unrelated model families remain available.
Changing a rate does not silently rewrite historical records: the user can run
the existing full reindex when they want prior usage recalculated.
Configuration validation occurs before persistence. Canonical model IDs are
limited to 256 UTF-8 bytes. Rates accept non-negative decimal USD values with
at most 18 fractional digits and must fit the shared signed 64-bit pico-USD
range after half-up rounding to 12 digits. Tauri returns safe, field-specific
validation codes; the current pricing editor renders save corrections beside
the affected field, while delete failures remain inside their confirmation.
Updating autostart uses
compensation if database persistence fails. Notification provider policy is
updated in memory only after settings persistence succeeds. The remote ntfy
fields have their own form and explicit save action, so Enter in model-pricing
inputs cannot submit ntfy settings. The ntfy enabled state and
autostart both persist immediately. Enabling ntfy validates and commits the
current ntfy fields in the same operation; disabling ntfy and changing
autostart use the last committed settings snapshot so they never implicitly
save unrelated pending ntfy edits. Completion of an in-flight immediate enable
must not clear the dirty state of edits made after that request started.
Settings and pricing mutation commands return the canonical saved value. The
UI publishes that result directly instead of issuing a second read that could
fail after a successful write or overwrite a newer local edit.

The desktop application is the only database migration owner. Tauri commands
are adapters around the Rust modules and DTOs, not domain logic.
The `cc-monitor` library exposes only its application entry point; command
handlers, DTOs, native notification helpers, and desktop state remain
crate-internal. `unreachable_pub` is enabled so this adapter layer does not
accidentally become a second public Rust interface.

The frontend shell owns routing, desktop event listeners, and dashboard
snapshot refresh. Settings, diagnostics, model pricing, and collector status
are feature modules that own their local interaction state behind their React
interfaces. Reusable asynchronous request state is kept in one small frontend
module; it does not duplicate Rust domain state. Frontend contract tests are
grouped at these same feature seams and share one test setup for Tauri adapters.

Tauri's single-instance plugin is installed before stateful plugins or setup.
A second launch activates the existing instance. Closing the dashboard keeps
monitoring alive; Quit signals workers and terminates the application.

## Runtime

Startup performs:

1. a read-only schema compatibility preflight;
2. ordered schema migrations and the restartable v2 usage backfill, if required;
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

Full-history and incremental transcript scans share one in-process mutex. An
incremental tick skips when another scan owns it, so two readers can never
clear or publish staging rows for the same transcript concurrently. Full-scan
results distinguish file failures and quarantined sessions from an interrupted
scan. Quarantined progress is counted by unique session ID across main and
subagent transcript files for the complete run.

Cancellation and worker ownership are in memory. After a crash, committed
evidence, cursors, projections, and outbox rows are sufficient to resume.

## Storage contract

The supported schema version is exactly 2.

- Migration 1 is the complete initial schema and must never be edited after
  release. All future schema changes use new ordered, additive migrations.
- Migration 2 adds materialized usage aggregates and their restartable
  backfill marker.
- The pre-release v1–v14 development chain was squashed before the first
  supported release and is intentionally not an upgrade source.
- A database with a successful version greater than 2 is rejected as
  `unsupported_future_schema` during read-only preflight, before a normal WAL
  connection can write.
- Automated tests use temporary databases only.

`docs/schema-v1.sql` is a readable v1 reference. The immutable SQLx migration
file is authoritative. The pre-release compatibility-only `daily_usage` and
`transcript_publish_generation` tables are not part of the supported schema.

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
takeover state. The schema migration version remains internal during normal
use and is included only in copied diagnostics for support investigations.

## Verification contract

Every product behavior receives an automated contract test where feasible.
Required gates are:

- migration checksum, fresh-v2, existing-v1 upgrade, exact-schema, and
  future-schema rejection;
- Hook bounded-process, sanitization, ownership, install, and uninstall tests;
- transcript streaming, atomic publication, replacement, missing-file,
  dedupe/winner, and historical-notification tests;
- reducer fixture and deterministic ordering tests;
- projection/outbox/retry/provider-health tests;
- dashboard/tray formatting and frontend tests;
- Rust formatting, Clippy with warnings denied, full workspace tests, frontend
  tests/build, and a release application bundle. `mise run verify-release` is
  the single local release gate; `mise run verify` remains the faster source
  gate used during development. The release gate rejects tracked or untracked
  worktree changes and prints the exact source commit before verification and
  packaging; ordinary `mise run build` remains available for local WIP builds.

macOS menu-bar, Dock behavior, notification authorization/click routing,
terminal activation, and the final signed application still require manual
verification on a real desktop.
