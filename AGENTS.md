# CC-Monitor development rules

These rules apply to the Tauri + Rust application.

- Treat `docs/architecture.md` as the product and architecture
  contract. Update that document when an implementation decision changes.
- The application must never delete or migrate the legacy
  `~/.cc-monitor/state.db`. Legacy cleanup is explicitly outside application
  scope.
- The Claude Code hook is a short-lived event collector. It must not send
  notifications, parse transcripts, calculate usage, or run migrations.
- The hook must always exit successfully and must never block Claude Code.
  Bound all input, database waits, retries, and diagnostic output.
- Only the desktop application may run database migrations. Use additive,
  ordered SQLx migrations; never edit a migration after it has shipped.
- Keep the Rust core independent of Tauri. Tauri commands and events are UI
  adapters, not the domain model.
- Domain state is derived by the reducer in one place. Hook and transcript
  adapters must not implement competing session state machines.
- First-time transcript indexing and historical replay must never create user
  notifications.
- Do not log transcript contents, ntfy credentials, authorization headers, or
  other secrets. The accepted plaintext credential storage is local SQLite
  only.
- Every product behavior needs an automated contract test where feasible.
  macOS tray, Dock, notification-click, and terminal activation behavior
  additionally require manual verification.
- Use repository-defined `mise` tasks for development, testing, linting,
  formatting, verification, and builds whenever a matching task exists. In
  particular, prefer `mise run test`, `mise run lint`, `mise run format`,
  `mise run verify`, `mise run build`, and `mise run clean` over their
  underlying Cargo or Bun commands so the repository's pinned tool versions
  are used.
- Direct Cargo or Bun commands are allowed only for narrowly targeted
  diagnostics or individual tests that do not have a matching `mise` task.
