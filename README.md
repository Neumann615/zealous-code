# zealous-code

本仓库是从零重建的新产品主线:`main` 分支为 orphan 分支。
旧工程完整快照保存在归档分支 `archive/pre-agent-assets-20261015`(基于原 master 及其历史),
不再随主线演进。

## 产品

本地优先的 CLI/TUI 多 Agent 编程助手:任务可拆解、执行可控、风险可审批、过程可恢复。
产品定义、领域模型、事件账本、权限分级与验收场景见 **`docs/spec/`**(spec 先于代码修订)。

Rust workspace:`zealous-core`(领域/事件,纯逻辑)→ `zealous-engine`(编排/工具/权限/Provider)→ `zealous-cli`(二进制 `zealous` + TUI)。
桌面 app 演进路线(Tauri 内嵌 / app-server 协议)见 `docs/spec/50-architecture.md`。

## Requirements

- Windows 10/11 x64(发布产物为 `x86_64-pc-windows-gnu` 构建)。
- Git(zealous 的任务必须在 Git 仓库内执行)。
- 一个 OpenAI-compatible chat completions endpoint(如 `ZEALOUS_BASE_URL` /
  `ZEALOUS_API_KEY` / `ZEALOUS_MODEL` 所指向的服务)。

## Quick Start

1. 配置真实模型(三选一,优先级从高到低):
   1. 环境变量 `ZEALOUS_BASE_URL` / `ZEALOUS_API_KEY` / `ZEALOUS_MODEL`;
   2. 当前仓库根 `.env`(同上三个键;`.gitignore` 已忽略该文件);
   3. 全局 `~/.zealous/config.json`(跨仓库使用推荐)。
2. 验证配置与 Git 仓库状态:

   ```bash
   zealous config check
   ```

3. 创建任务(brief 输入方式按入口而定:CLI 为 `--file <仓库内相对路径>`;
   TUI 为新建表单的 brief file 字段;app-server 为 `create_task` 请求的
   `brief` 文本字段。CLI 当前不支持 stdin brief):

   ```bash
   zealous task new "修复登录空指针" --file BRIEF.md
   ```

4. 运行任务(默认经 stdin 交互审批;脚本化可用 `--yes` / `--no`;
   `--quiet` 只输出审批请求、错误与最终结果):

   ```bash
   zealous task run <task-id>
   ```

5. 或进入可视化工作台:

   ```bash
   zealous tui
   ```

## Security Model

工具按风险分级,高风险动作必须经人工审批:

| 级别 | 工具 | 策略 |
| ---- | ---- | ---- |
| T0 | `read_file` / `list_dir` / `search_text` / `git_status` / `git_log` / `git_diff` | 自动执行(只读) |
| T1 | `write_file` / `edit_file` | 写入前人工审批 |
| T2 | `shell` | 高危,人工审批 |

- 主工作区写入前会检查用户未提交改动;真实用户改动存在时拒绝写入,
  保护你的 WIP。
- 每次写入后自动生成 rollback commit(仅包含目标文件,不 stage 你的
  无关改动与运行副产物)。
- 路径访问严格限制在当前 Git 仓库内,含 symlink 与路径穿越防护。
- Worker Agent 只读;只有 Coordinator 可以 apply,且同样必须审批。
- **zealous V1 不是 OS sandbox**:`shell` 是 T2 审批动作,审批即授信。
- 模型 endpoint 是唯一默认外发网络访问;API key 不进入事件账本,
  不出现在 stdout / stderr。

## Data Location

- 任务数据、事件账本(append-only JSONL)与状态全部存放在当前仓库的
  `.zealous/` 目录下。
- 全局配置位于 `~/.zealous/config.json`。
- 任务数据归用户本地所有;zealous 不自动上传任何任务内容。

## Resume & Audit

- `zealous task status <task-id>`:查看任务投影状态。
- `zealous task events <task-id>`:查看完整事件账本(seq 连续、可审计)。
- Ctrl-C 或崩溃后任务规范化为 Interrupted,挂起审批自动收尾;
  再次 `zealous task run <task-id>` 或 TUI `R` 即可恢复。
- 恢复会开启新的 AgentRun,并向模型注入原任务、brief、已有 plan、
  工具调用摘要与上次错误——这是续跑,不会重做已完成的部分。

## Known Limitations

- V1 单并发任务(app-server 亦为单客户端、单并发 run)。
- 不是 OS sandbox(见 Security Model)。
- Windows 优先;macOS / Linux 未完整验证。
- 真实模型 smoke 等发布验证会消耗模型额度。

## Release Verification

发布产物与验收记录:

- 产物:`dist/zealous-0.1.0-<host-triple>.zip`,清单仅含 `zealous.exe`。
- 校验:

  ```powershell
  Get-FileHash dist\zealous-0.1.0-x86_64-pc-windows-gnu.zip -Algorithm SHA256
  ```

- 哈希以 RC 记录为准:`docs/acceptance/rc-0.1.0.md`
  (含 runtime source commit、质量门结果、zip/bundle SHA256 与分发状态)。
- 当前分发状态见 RC 记录 `Distribution` 字段;未标记为已发布的产物
  不得对外分发。

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

## TUI 快速开始(zealous tui)

在任意 Git 仓库根目录运行 `zealous tui` 进入可视化任务工作台。

| 键位(列表) | 作用 | | 键位(详情) | 作用 |
| ------------ | ---- | - | ------------ | ---- |
| `↑/↓` | 选择任务 | | `R` | 运行/恢复 |
| `r` | 刷新列表 | | `x` | 中断任务 |
| `Enter` | 打开详情 | | `a` / `d` | 批准/拒绝当前审批 |
| `n` | 新建任务表单 | | `e` | 展开/收起事件流 |
| `q` | 退出 | | `Esc` | 返回列表 |

新建表单:title 必填;brief file 可选(相对当前 Git 仓库根)。
`--quiet` 等 CLI 文本输出参数与 TUI 无关;审批、进度、最终状态均来自
engine 编排事实,TUI 不维护第二份任务状态。

真实模型配置(三选一,优先级从高到低):
1. 环境变量 `ZEALOUS_BASE_URL` / `ZEALOUS_API_KEY` / `ZEALOUS_MODEL`;
2. 当前目录 `.env`(同上三个键,.gitignore 已忽略);
3. 全局 `~/.zealous/config.json`(跨仓库使用推荐)。

配置错误不会阻塞浏览既有任务,错误会显示在底部,按任意键消除。
