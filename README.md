# zealous-code

本仓库是从零重建的新产品主线:`main` 分支为 orphan 分支。
旧工程完整快照保存在归档分支 `archive/pre-agent-assets-20261015`(基于原 master 及其历史),
不再随主线演进。

## 产品

本地优先的 CLI/TUI 多 Agent 编程助手:任务可拆解、执行可控、风险可审批、过程可恢复。
产品定义、领域模型、事件账本、权限分级与验收场景见 **`docs/spec/`**(spec 先于代码修订)。

Rust workspace:`zealous-core`(领域/事件,纯逻辑)→ `zealous-engine`(编排/工具/权限/Provider)→ `zealous-cli`(二进制 `zealous` + TUI)。
桌面 app 演进路线(Tauri 内嵌 / app-server 协议)见 `docs/spec/50-architecture.md`。

## agent-assets/ 参考源码区

`agent-assets/` 是本机可更新的只读参考源码区,**不是产品依赖,也不是构建输入**:

- 产品源码禁止直接 import `agent-assets/` 下的任何代码。
- 上游代码只用于范式研究与有意识的局部移植。
- 如需复制上游代码进产品,必须记录来源 commit,并履行对应许可证义务(如保留版权与许可声明)。

| 目录 | upstream 地址 | 用途 |
| ---- | ------------- | ---- |
| `agent-assets/zcode` | https://github.com/zai-org/zcode.git | Agent 运行时与 CLI 范式参考 |
| `agent-assets/codex` | https://github.com/openai/codex.git | Agent CLI / 编码代理范式参考 |

参考仓库使用完整 Git clone(而非 submodule),便于持续跟踪最新上游。
`.gitignore` 已忽略 `/agent-assets/*`,仅其 `README.md` 进入版本管理。

更新方式:

```bash
node scripts/update-agent-assets.mjs
```

脚本行为:缺失则 clone;已存在则要求工作区干净,执行 `git fetch --prune origin`,
并 detach 到 origin 默认分支的最新 commit;最后输出各仓库当前 commit 便于引用。

## 开发原则

- 先改 `docs/spec/`,再改代码;spec 与实现不一致视为 bug。
- 上游参考代码只读,不在 `agent-assets/` 内进行开发或提交本地修改。
