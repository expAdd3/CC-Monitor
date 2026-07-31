# Local acceptance guide

This guide covers the local-only Tauri + Rust acceptance build. The generated
application is ad-hoc signed so macOS can keep one stable local application
identity, but it is not Developer ID signed or notarized. Automatic updates and
release publishing remain outside the local-tool scope.

## Build and install

Install Rust, Bun, and the Tauri system prerequisites, then run:

```sh
bun install
bun run verify
bun run bundle:mac
```

The command builds the release Hook sidecar first, then invokes the installed
`cargo tauri build --bundles app` command and includes the sidecar in the
generated application bundle. It does not resolve a transient CLI package at
build time; the repository's mise tool definition pins the accepted CLI
version. The price catalog is compiled into the Claude adapter and the
application icon is configured through `tauri.conf.json`.
`bun run verify` is the canonical automated gate. `bun run bundle:mac` builds
the application and Hook, verifies that both the bundle and code-signing
identifier are `com.ccmonitor`, and performs a strict signature check.

Copy `target/release/bundle/macos/CC Monitor.app` to `/Applications`, then open
it locally. Because the build is only ad-hoc signed, it is for local acceptance
and is not a distributable Gatekeeper release.

## Native macOS acceptance matrix

Automated tests cannot establish AppKit and system-permission behavior. Check
each item from the bundled `/Applications/CC Monitor.app`:

- First launch shows the menu-bar item and keeps the Dock icon hidden until the
  dashboard opens.
- Closing the dashboard keeps monitoring active; **退出 CC Monitor** stops it.
- When notifications have no decision for `com.ccmonitor`, testing a desktop
  notification requests permission. A successful test clears an earlier
  provider-failure warning.
- Denied notification permission gives an actionable message and **打开通知设置**
  opens the correct system pane.
- Live **需要介入**, completion, and failure transitions notify promptly;
  historical reindexing does not notify.
- Clicking a notification or one session menu row opens the matching session;
  terminal activation is best effort and never opens an unrelated application.
- Menu rows remain readable in light/dark appearance and with long Chinese and
  Latin project names.
- Keyboard navigation and VoiceOver announce each session as one actionable
  item containing its state, short ID, Token usage, and cost coverage.
- Dashboard navigation focus remains visible, and History exposes each day's
  Token and cost coverage without adding 365 Tab stops.

## Install or repair the Claude Code Hook

Open **Settings**, then select **安装 Hook** (or **修复 Hook** when repair is
required). Restart active Claude Code
sessions after changing Hook configuration. Repair is idempotent and preserves
unrelated hooks.

## Reindex transcripts

Open **Diagnostics** and select **重新索引会话记录**. Progress and the final
result appear on that page. Historical replay never creates user
notifications.

## Copy diagnostics and inspect logs

Select **复制诊断** on the Diagnostics page. The report excludes
Transcript contents, Hook paths, notification error text, authorization
headers, and ntfy credentials.

The application writes fixed event codes to the platform application log
directory. `cc-monitor.log` rolls to `cc-monitor.log.1` at 512 KiB; the log
does not contain Transcript text or credentials.

## Reset settings

Settings can be returned to defaults from the Settings page by disabling ntfy
and autostart and clearing non-secret ntfy fields before saving. Do not delete
the legacy `~/.cc-monitor/state.db`; it is outside application ownership.

## Uninstall

Use **Settings → 卸载 Hook** to remove only the Hook installation owned by this
application. Then quit CC Monitor and remove the app from `/Applications`.
Application data may be removed manually only after local acceptance confirms
it is no longer needed. Never remove or migrate the legacy
`~/.cc-monitor/state.db`.

Developer ID signing, notarization, an updater, and CI release publishing need
an explicit distribution decision and Apple/repository credentials. They are
not hidden acceptance requirements for this local utility.
