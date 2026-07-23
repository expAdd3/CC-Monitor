# Tauri + Rust Refactor Implementation Plan

This plan implements `docs/tauri-rust-refactor-spec.md`. Each phase is intended
to be executable in a fresh context. Do not begin a phase until its entry
references have been read and the previous phase verification passes.

## Phase 0 — Documentation and baseline discovery

Status: completed for planning; repeat targeted checks when dependency versions
are selected.

### Evidence gathered

- Current Hook/state behavior:
  `cc_hook.py:31-48`, `143-147`, `180-267`.
- Current transcript precedence and heuristics:
  `cc_monitor.py:263-467`.
- Current pricing behavior:
  `cc_pricing.py:118-259`, `272-422`.
- Current notification behavior:
  `cc_monitor.py:489-583`, `cc_notify.py:86-252`.
- Current menu structure:
  `cc_monitor.py:1695-1764`.
- Current Hook installer:
  `install_hooks.py:20-85`.
- Official Tauri and SQLx references are collected in the specification,
  section 13.

### Allowed APIs

- Tauri 2 `tray::TrayIconBuilder` and `menu::{Menu, MenuItem, Submenu}`.
- `WindowEvent::CloseRequested`, `CloseRequestApi::prevent_close`,
  `Window::{show, hide}`, and `AppHandle::exit`.
- macOS-only `AppHandle::set_activation_policy` with
  `ActivationPolicy::{Regular, Accessory}`.
- `app.path().app_data_dir()` and `app.path().app_log_dir()`.
- Tauri Commands for request/response and Events as revision invalidation
  signals.
- `tauri-plugin-notification` for basic notification display and permission.
- `tauri-plugin-autostart` with macOS `LaunchAgent`.
- SQLx `migrate!`, `SqliteConnectOptions`, and `SqlitePoolOptions`.

### Guards

- Do not use Tauri 1 `SystemTray`, `SystemTrayMenu`, or
  `Builder::system_tray`.
- Do not promise notification-click routing through the standard notification
  plugin before the spike proves it.
- Do not hard-code the application data directory.
- Do not hold a synchronous mutex guard across `.await`.
- Do not treat Tauri Events as ordered domain events.

## Phase 1 — Characterization fixtures and contracts

### Implement

1. Add sanitized Hook fixtures for every row of the approved transition table.
2. Add transcript fixtures covering partial lines, malformed records,
   truncation, subagents, duplicate usage, unknown models, and multiple days.
3. Add Python characterization tests that serialize expected normalized
   events, state projections, usage totals, and notification decisions.
4. Record the 45 currently discovered tests as the minimum regression baseline;
   ensure pricing tests are intentionally collected by the selected runner.
5. Add fixture documentation explaining which fields were removed or replaced.

### References

- Copy event classifications from `cc_hook.py:31-48`, `180-208`.
- Copy pricing acceptance behavior from `cc_pricing.py:195-259`, `272-422`.
- Copy transcript inference cases from `cc_monitor.py:263-330`.

### Verify

- Run the full Python test suite.
- Replay every fixture twice and prove identical expected output.
- Verify fixtures contain no real transcript text, paths, usernames, or
  credentials.

### Guards

- Do not infer new desired behavior from accidental Python implementation
  details; use the approved specification when it differs.
- Do not use live user transcripts as committed fixtures.

## Phase 2 — Workspace, database, and domain core

### Implement

1. Scaffold one Cargo workspace with:
   `monitor-domain`, `monitor-storage`, `adapter-claude`,
   `monitor-engine`, `monitor-notify`, `cc-monitor-hook`,
   `cc-monitor-cli`, and `src-tauri`.
2. Scaffold a Vite React + TypeScript UI.
3. Set the approved bundle identifier to `com.lixinyu.ccmonitor` and
   `bundle.macOS.minimumSystemVersion` to `12.0`.
4. Split `docs/schema-v2.sql` into ordered SQLx migrations.
5. Configure SQLite explicitly with create-if-missing, foreign keys, WAL,
   `synchronous=NORMAL`, 5-second busy timeout, and a bounded connection pool.
6. Add `build.rs` migration change tracking before calling
   `tauri_build::build()`.
7. Implement domain IDs, normalized events, state reasons, lifecycle, turn
   state, and the deterministic reducer.
8. Implement repository traits independently of Tauri.

### References

- Copy the connection-option pattern from
  [SQLx SqliteConnectOptions](https://docs.rs/sqlx/latest/sqlx/sqlite/struct.SqliteConnectOptions.html).
- Copy embedded migration use from
  [SQLx migrate](https://docs.rs/sqlx/latest/sqlx/macro.migrate.html).
- Use `docs/schema-v2.sql` as the normative logical schema.

### Verify

- Migration test creates a fresh temporary database.
- A second migration run is a no-op.
- Foreign keys, WAL, synchronous mode, and busy timeout are asserted.
- Reducer table tests cover every specification row, duplicates, and shuffled
  event order.
- Core crates compile and test without Tauri.

### Guards

- Do not run migrations from the Hook.
- Do not scatter runtime `CREATE TABLE` or `ALTER TABLE` statements.
- Do not modify a migration after it is committed as released.
- Do not import Tauri in domain, storage, adapter, engine, or notify crates.

## Phase 3 — Claude Hook and installation

### Implement

1. Implement bounded stdin ingestion and Claude payload normalization.
2. Generate stable event IDs/deduplication keys from normalized fields.
3. Insert only into `raw_events`.
4. Apply bounded SQLite timeout/retry and unconditional successful process exit.
5. Implement Hook staging in Tauri's resolved application data directory and
   pass the resolved absolute database path to the installed Hook command.
6. Back up and atomically edit `~/.claude/settings.json`.
7. Install the current Claude event set and AskUserQuestion matcher.
8. Add an installation UUID to the command and removal ownership check.
9. Remove only recognized legacy CC-Monitor commands; preserve unrelated hooks.

The existing Python `uninstall.py` remains a legacy-only tool during
coexistence. New Tauri code must not call it and must never delete or migrate
the legacy database. Phase 8 removes or excludes the legacy script from the new
product and distribution.

### References

- Copy the event list/matcher intent from `install_hooks.py:26-36`.
- Copy the unconditional-exit safety behavior from `cc_hook.py:244-267`.
- Follow the ownership rules in specification section 10.

### Verify

- Invalid, empty, oversized, and valid payload integration tests all exit 0.
- Locked/missing/corrupt database tests exit within the allowed bound.
- Repeated payloads create one raw event.
- Installer is idempotent and preserves unrelated settings.
- Malformed settings fail safely without overwriting the file.
- Uninstaller removes only the matching installation.

### Guards

- No networking, notification, transcript parsing, pricing, state reduction, or
  migration in the Hook.
- No fuzzy deletion based only on a filename substring.
- No deletion of `~/.cc-monitor`.

## Phase 4 — Transcript and pricing adapters

### Implement

1. Discover Claude project transcripts and retain the `--` internal-directory
   exclusion.
2. Implement durable cursor ingestion with partial-line buffering and
   truncation/replacement detection.
3. Port usage extraction, request/message deduplication, subagent inclusion,
   local-day grouping, and per-model summaries.
   Dedupe is session-scoped and follows the stable-ID/message-only/request-only/
   anonymous rules in specification section 7, including
   cross-main/subagent collapse, and persisted stable source locations.
4. Port model normalization and bundled pricing; add SQLite UI overrides.
5. Implement asynchronous all-history first indexing and progress reporting.
6. Build active projections only for the approved 24-hour visibility window.
7. Guarantee initial indexing cannot enqueue notifications.

### References

- Copy malformed-line and truncation handling intent from
  `cc_monitor.py:263-295`.
- Copy status heuristics from `cc_monitor.py:298-330`.
- Copy pricing/dedup rules from `cc_pricing.py:118-259`, `272-422`.

### Verify

- Rust outputs match Phase 1 fixtures.
- Resume from cursor processes only appended complete lines.
- Truncation and replacement recover without double counting.
- Reindex produces the same usage aggregates.
- Unknown prices keep tokens and mark cost unknown.
- UI/event loop remains responsive during full indexing.

### Guards

- Do not store transcript bodies.
- Do not perform synchronous whole-history scanning on the UI thread.
- Do not let transcript inference overwrite recent definitive Hook state.
- Do not create notifications from historical replay.

## Phase 5 — Engine and notification delivery

### Implement

1. Consume `raw_events` transactionally and update projections/turns.
2. Create provider-specific Outbox rows in the same transaction as a
   notification-worthy transition.
3. Implement desktop and ntfy dispatchers with bounded retry.
4. Implement restart suppression for historical Done/Failed and one-time
   recovery notification for unresolved NeedsInput.
5. Implement 30-day retention for raw events and completed notification rows.
6. Port ntfy validation, JSON API, Basic auth, timeout, readable errors, and
   failure recovery diagnostics.

### References

- Use the policy in specification section 6.
- Copy ntfy request/error behavior from `cc_notify.py:86-98`, `172-252`.
- Compare deliberately changed Outbox semantics with
  `cc_monitor.py:520-583`.

### Verify

- Crash between projection and delivery cannot lose the Outbox row.
- Reprocessing the same event cannot duplicate a notification.
- Partial provider failure does not mark the other provider unsent.
- Startup fixtures prove old Done is suppressed and unresolved NeedsInput is
  sent once.
- Credentials and headers are absent from logs and errors.

### Guards

- Do not use a single `notify_pending` boolean as a queue.
- Do not dispatch a notification before committing its Outbox row.
- Do not send ntfy from the Hook.

## Phase 6 — Tauri tray, Dashboard, and settings

### Implement

1. Build the native tray with current counts, today totals, 7/30-day trends,
   session submenus, Dashboard, Settings, and Quit.
2. Implement Commands for snapshots and mutations.
3. Emit revision-only invalidation events; React refetches snapshots.
4. Build React routes/components for Dashboard, session detail, history,
   settings, Hook health, index progress, and diagnostics.
5. Intercept Dashboard close and hide it without stopping the engine.
6. Switch macOS activation policy between Accessory and Regular.
7. Add autostart settings using the official plugin.
8. Mask ntfy credentials in UI while storing them in SQLite.

### References

- Copy the Tauri 2 tray pattern from
  [System Tray](https://v2.tauri.app/learn/system-tray/).
- Copy close interception from `WindowEvent::CloseRequested` and
  `CloseRequestApi::prevent_close`.
- Copy Commands/Events/State patterns from the official references in the
  specification.
- Use `cc_monitor.py:1695-1764` as the current menu information contract.

### Verify

- React unit tests cover view states and listener cleanup.
- Command tests cover validation and serialized errors.
- Tray counts and Dashboard snapshots share one engine revision.
- Closing the window leaves tray and notifications working.
- Quit terminates the engine and creates no realtime notifications afterward.
- Manual test Dock, `⌘Tab`, Spaces, light/dark mode, and multiple sessions.

### Guards

- Do not use Tauri 1 tray APIs.
- Do not put domain decisions in React or command handlers.
- Do not send full session histories through invalidation events.
- Do not implement close with `Window::destroy()`.

## Phase 7 — macOS notification click and terminal activation spike

### Implement

1. Prove whether the selected official notification plugin version supports a
   macOS click callback with a session identifier.
2. If it does not, implement a narrow macOS UserNotifications adapter.
3. Map captured Bundle IDs to Terminal, iTerm, Warp, and VS Code activation.
4. On activation, open the matching Dashboard session detail.
5. Fall back to opening Dashboard when terminal activation is unavailable.

### References

- Basic display:
  [Tauri notification plugin](https://v2.tauri.app/plugin/notification/).
- Existing Bundle ID capture: `cc_hook.py:58-85`.
- Existing terminal-notifier activation: `cc_monitor.py:489-517`.

### Verify

- Real macOS notification display and permission flow.
- Click behavior for each available terminal application.
- Missing/uninstalled terminal falls back safely.
- No promise or test requires exact tab/pane navigation.

### Guards

- Do not assume `.show()` provides click routing.
- Do not add an undocumented plugin API.
- Keep macOS-specific code behind a platform adapter.

## Phase 8 — Packaging and final verification

### Implement

1. Add local development packaging and resource inclusion for Hook, icons, and
   price catalog.
2. Add rolling sanitized logs and “Copy diagnostics”.
3. Document local install, Hook repair, reindex, settings reset, and uninstall.
4. Defer updater, signing, notarization, and GitHub Release automation until
   local acceptance is complete.

### Verify

- Run Rust formatting, linting, unit, integration, and migration tests.
- Run React type-check, lint, unit tests, and production build.
- Run the retained Python baseline tests until the Python implementation is
  removed.
- Search for Tauri 1 tray APIs, scattered DDL, secret logging, legacy database
  deletion, and Hook networking.
- Test a packaged app, not only `tauri dev`.
- Complete the manual macOS checklist from specification sections 6 and 8.
- Confirm the application never touches `~/.cc-monitor/state.db`.

### Guards

- Do not claim macOS 12 support until it is tested or CI-covered.
- Do not ship a browser-downloaded GitHub Release as production without signing
  and notarization.
- Do not enable the updater until signing keys and recovery ownership are
  established.

## Deferred work

Only after the first macOS/Claude release is stable:

1. evaluate Windows, then Linux;
2. extract a public Agent Adapter contract from at least two real adapters;
3. design remote identity, authentication, authorization, offline sync, and
   protocol versioning before choosing WebSocket, MQTT, or gRPC.
