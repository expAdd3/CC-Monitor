<div align="center">

<img src="assets/app_icon_color.svg" width="144" alt="CC Monitor">

# CC Monitor

macOS 菜单栏中的 Claude Code 会话状态、通知与 Token 用量监控工具。

**Tauri 2 · Rust · React · SQLite**

</div>

## 功能

- 菜单栏显示最近 24 小时的会话状态、会话编号与 Token 用量
- Dashboard 提供会话详情、历史趋势、设置和诊断
- Claude Code Hook 采集确定性事件，Transcript 提供状态和用量补充
- macOS 桌面通知与可选 ntfy 远程通知
- 点击通知或会话菜单可返回对应会话
- 异步历史索引、进度反馈及安全的数据维护

## 架构

```mermaid
flowchart LR
    Claude["Claude Code"] --> Hook["Rust Hook"]
    Claude --> Transcript["Transcript JSONL"]
    Hook --> Events[("SQLite events")]
    Transcript --> Adapter["Transcript adapter"]
    Adapter --> Events
    Events --> Engine["Reducer + Engine"]
    Engine --> Outbox[("Notification outbox")]
    Engine --> Tauri["Tauri desktop adapter"]
    Outbox --> Desktop["macOS notification"]
    Outbox --> Ntfy["ntfy"]
    Tauri --> React["React Dashboard"]
```

当前架构约束与产品行为见
[`docs/architecture.md`](docs/architecture.md)。

## 开发环境

- macOS
- Rust 1.88 或更高版本
- Bun
- Tauri CLI 2

使用 `mise` 时：

```sh
mise install
```

安装前端依赖：

```sh
bun install
```

## 开发与构建

启动开发版本：

```sh
mise run dev
```

运行前端测试：

```sh
bun run test
```

运行完整自动验证：

```sh
bun run verify
```

运行 Rust 工作区测试：

```sh
cargo test --workspace
```

检查 Rust 格式和 lint：

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

构建 macOS 应用：

```sh
bun run bundle:mac
```

该命令使用本机已安装且受 Rust 工具链管理的 `cargo tauri`，不会在构建时
临时下载或解析另一个 CLI 版本；仓库的 mise 工具定义固定为已验收的
Tauri CLI 2.11.4。

本地产物使用 ad-hoc 签名并验证 `com.ccmonitor` 标识，适合本机验收；
它不等同于 Developer ID 签名和 Apple 公证的分发版本。原生通知、状态栏、
键盘和 VoiceOver 验收矩阵见
[`docs/local-acceptance.md`](docs/local-acceptance.md)。

产物位于：

```text
target/release/bundle/macos/CC Monitor.app
```

## 数据与隐私

- 应用数据库位于 macOS Application Support 目录。
- Transcript 正文不会写入应用数据库或日志。
- ntfy 凭据仅保存在本地 SQLite，不进入日志或诊断报告。
- ntfy 公网及非本机地址必须使用 HTTPS。HTTP 只允许无凭据的
  `localhost`、`127.0.0.1` 或 `::1` 本机测试服务。
- 首次索引和历史重放不会创建用户通知。
- 应用不会删除或迁移旧版 `~/.cc-monitor/state.db`。

## 测试夹具

`tests/fixtures/` 是 Rust 适配器和 reducer 使用的语言无关契约数据，
其中只包含为本项目创建的合成内容。
