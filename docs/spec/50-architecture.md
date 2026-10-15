# 50 · 架构与演进

状态: 已定案 · 2026-10-15

## Crate 结构与依赖方向(单向,禁止反向/循环)

```
zealous-cli   →  zealous-engine  →  zealous-core
(二进制+TUI)     (编排/工具/权限/     (领域模型/事件定义/
                  Provider 实现)      账本投影,纯逻辑无 IO*)
```

- `zealous-core`: 实体、状态机、事件信封与投影(`fold`)。不触碰文件系统/网络,可全量单测。
- `zealous-engine`: 账本写入器、Agent 编排(Coordinator/Worker)、工具注册表与风险分级、审批流、OpenAI-compatible Provider(`Provider` trait 隔离,便于测试替身)。
- `zealous-cli`: clap 参数解析、TUI(ratatui)、`app-server` 模式入口。只做展示与交互,不含业务规则。

## 内核复用路径(桌面 app 演进)

内核 = `zealous-core` + `zealous-engine`。UI 永远是薄客户端,两条既有验证路径:

1. **Tauri**: 内核以 library 形式直接编入桌面 app 进程。
2. **app-server 模式**: `zealous serve` 通过 stdio 暴露 JSON 协议(事件订阅 + 命令),Electron/Web 壳以子进程方式连接——与 Codex app-server、旧 zcode Host/协议同构。

为保住这条演进线,现在就定的规则:

- 协议载荷 = 事件账本的事件类型 + 少量命令枚举,不出现"TUI 专用"概念;
- TUI 状态只允许来自投影,禁止 TUI 内私设业务状态;
- CLI 与未来桌面共享同一套投影 API。

## 目录

```
Cargo.toml            # workspace
crates/zealous-core
crates/zealous-engine
crates/zealous-cli
docs/spec/            # 本目录,先于代码修订
```

## CLI 命令面(V1,已实现)

均在 Git 仓库根目录执行;Provider 配置来源优先级:进程 env(`ZEALOUS_BASE_URL/API_KEY/MODEL`)> 当前目录 `.env`(gitignore 忽略)> `~/.zealous/config.json`。

| 命令 | 行为 |
| ---- | ---- |
| `zealous task new <title>` | 创建任务并校验 Provider 配置;配置非法 → 任务落账为 Failed(可恢复) |
| `zealous task list` | 列出任务与损坏账本(不阻塞) |
| `zealous task status <id>` | 投影状态:runs/pending approvals/last error |
| `zealous task run <id> [--yes/--no]` | 运行或恢复;T1/T2 审批经 stdin 交互(--yes/--no 脚本化) |
| `zealous task events <id>` | 输出事件账本(seq/ts/agent/payload) |

退出码:0 = Completed;1 = Failed/Interrupted/配置错误;2+ = clap 用法错误。
TUI(ratatui)与 `app-server` 模式是 cli crate 的下一形态,复用同一投影 API。

## 验证命令(建立后以实际为准)

`cargo check` / `cargo test` / `cargo fmt --check` / `cargo clippy -- -D warnings`

## 后续演进顺序(不在 V1)

OS 进程隔离与 Git worktree 并行执行 → 自动合并 → macOS/Linux 验证 → 桌面壳。
