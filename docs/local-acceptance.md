# Local acceptance guide

This guide covers the local-only Tauri + Rust acceptance build. The generated
application is ad-hoc signed so macOS can keep one stable local application
identity, but it is not Developer ID signed or notarized. Automatic updates and
release publishing remain outside the local-tool scope.

## Build and install

Install Rust, Bun, and the Tauri system prerequisites, then run:

```sh
mise run bootstrap
mise run verify-release
```

`verify-release` accepts only a clean Git worktree and prints the exact commit
used for the build. Use `mise run build` instead when validating uncommitted
local changes.

The release gate builds the release Hook sidecar first, then invokes the installed
`cargo tauri build --bundles app` command and includes the sidecar in the
generated application bundle. It does not resolve a transient CLI package at
build time; the repository's mise tool definition pins the accepted CLI
version. The price catalog is compiled into the Claude adapter and the
application icon is configured through `tauri.conf.json`.
`mise run verify` is the source gate. `mise run build` builds the application
and Hook, verifies that both the bundle and code-signing identifier are
`com.ccmonitor`, and performs a strict signature check. The release gate above
runs both in order.
The local bundle targets the build machine's architecture and verifies that
the application and Hook sidecar contain the same architecture. It is not a
universal2 distribution build.

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
- Denied notification permission gives an actionable message and **打开 macOS 通知设置**
  opens the correct system pane.
- Live **需要介入**, completion, and failure transitions notify promptly;
  historical reindexing does not notify.
- Clicking a notification or one session menu row opens the matching session;
  terminal activation is best effort and never opens an unrelated application.
- Menu rows remain readable in light/dark appearance and with long Chinese and
  Latin project names.
- Keyboard navigation and VoiceOver announce each session as one actionable
  item containing its state, short ID, Token usage, cost coverage, and recent
  activity time; the visual metadata row is not announced separately.
- Dashboard navigation focus remains visible, and History exposes each day's
  Token and cost coverage without adding 365 Tab stops.

## UI and accessibility acceptance matrix

Run this matrix against the bundled application with realistic data. Automated
DOM and CSS contracts protect the structure, but they do not replace checking
the actual macOS rendering, system appearance, zoom, and VoiceOver output.

| View or mode | What to verify |
| --- | --- |
| 1440 × 900 | Dashboard, History, Settings, Diagnostics, and a session detail use the available width without oversized dead space; section actions align consistently and no content overlaps. |
| 1121 × 800 | The regular content layout remains stable immediately above the single compact breakpoint; the left sidebar does not move. |
| 1120 × 800 | Session and diagnostics content switches to the compact one-column layout; section actions wrap in reading order and tables scroll inside their own containers. |
| 820 × 1180 (minimum window width) | The same left sidebar remains in place instead of changing into a top navigation. Forms, charts, action rows, and destructive controls remain reachable without page-level horizontal scrolling. |
| 200% zoom | Repeat the 1440 × 900 flow. The effective narrow layout must keep navigation, model pricing, remote-notification actions, confirmations, and diagnostic maintenance controls reachable and unobscured. |
| Light and dark appearance | Text, interactive-control focus rings, status colors, table borders, disabled controls, and heatmap levels remain distinguishable in both system appearances. Programmatically focused page titles do not show a decorative frame. |
| Oldest supported macOS | On macOS 12, heatmap levels remain distinct and keyboard focus remains visible through the CSS fallbacks even when newer WebKit color and focus selectors are unavailable. |
| Keyboard only | Tab first reveals **跳到主要内容**; activating it moves to the main-content anchor. All navigation, toggles, tests, saves, table-row actions, confirmations, retries, and disclosure controls have a visible focus indicator and a predictable order. A route change moves focus to its visible H1 once; background data refreshes do not steal focus. |
| VoiceOver | The named application sidebar, main navigation, and main-content landmarks are discoverable. Every route—including Settings and session loading/error states—has one H1. Model-usage and model-pricing tables announce their captions, column headers, and model row headers. The annual heatmap is announced as one concise yearly summary rather than 365 individual cells. |
| Extreme content | Use a long project name, session ID, event name, model ID, translated error, and large Token/cost values. Text wraps or truncates intentionally, rows remain associated with their actions, and no value overlays a neighboring section. |
| Pending and destructive states | While install/repair, save/test, pricing mutations, re-scan, copy, and cleanup are pending, repeated activation is blocked and status feedback is announced. Stop/delete/uninstall/cleanup confirmations name the actual consequence and keep cancel available. |
| Model pricing CRUD | A fresh profile lists every shipped price without source or status badges. Every row offers **编辑** and **删除**. Delete a shipped default, an edited shipped default, a provider-qualified override, one dated model used by undated fallback, and a newly added price; all deleted identities remain unavailable after restart while unrelated families remain priced. Re-add each deleted model ID and confirm it returns with the entered values. Verify an over-256-byte canonical model ID and an out-of-range rate show correction beside the affected editor field; a failed delete stays in its confirmation. Pressing Enter in a pricing input must not save remote-notification settings. |
| Collector no-nag states | Verify deferred onboarding and deliberate uninstall do not recreate an install callout on Dashboard or Diagnostics. Those pages remain status-only; Settings is the sole install/repair/uninstall surface. Repair-required state still provides one quiet link to Settings. |

## Install or repair the Claude Code Hook

Open **Settings**, then select **安装事件采集器** (or **立即修复** when repair is
required). Restart active Claude Code
sessions after changing Hook configuration. Repair is idempotent and preserves
unrelated hooks.

## Reindex transcripts

Open **Diagnostics** and select **重新扫描历史记录**. Progress and the final
result appear on that page. Historical replay never creates user
notifications.

## Copy diagnostics and inspect logs

Select **复制诊断信息** on the Diagnostics page. The report excludes
Transcript contents, Hook paths, notification error text, authorization
headers, and ntfy credentials. Confirm the event collection card does not show
the internal database schema migration version, while the copied report still
contains its `migration_version` field.

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
