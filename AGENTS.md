# CC-Monitor development rules

These rules apply to the Tauri + Rust rewrite.

- Treat `docs/tauri-rust-refactor-spec.md` as the product and architecture
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
- Every behavior ported from Python needs an automated contract test where
  feasible. macOS tray, Dock, notification-click, and terminal activation
  behavior additionally require manual verification.
