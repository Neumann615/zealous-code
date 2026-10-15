# 00 · 产品定位

状态: 已定案 · 2026-10-15

## 定位

面向个人开发者的**本地优先 CLI/TUI 多 Agent 编程助手**:用多个受控 Agent 安全地执行、审计和恢复长时间编程任务。

核心价值不是"更会聊天",而是:

1. 任务可拆解(Coordinator → 有界 Worker)
2. 执行可控(风险分级 + 审批)
3. 过程可审计(事件账本为事实源)
4. 会话可恢复(崩溃/中断后从账本重建)

## 形态与接入

- 第一主入口: CLI/TUI(二进制 `zealous`)。
- 模型接入: 首版 OpenAI-compatible endpoint,内部保留 Provider trait 抽象。
- 并发边界: 有界多 Agent(1 Coordinator + ≤3 Worker),不做自由 swarm。
- 目标平台: Windows 优先,macOS/Linux 后续验证。
- 分发名: npm 发布名保留 `@zealous/cli`;Rust crates 为 `zealous-*`。

## 参考,不照搬

参考 `agent-assets/` 中 codex(异步协议、审批与恢复建模)与 zcode(session projection、事件形态)的**设计思路**;禁止 import 其源码。首版不做: 桌面工作台、插件市场、远程控制、云端控制面、OpenAI 账号体系绑定。上游代码若未来被复制,必须记录来源 commit 并履行许可证义务(见 `agent-assets/README.md`)。

## 非目标(V1)

- 团队协作、企业审计、云端管理
- 上传代码/任务/遥测(本地优先,默认零外发;唯一的网络访问是用户配置的模型 endpoint)
- 完整 OS 级 sandbox 宣称(V1 只做策略级边界 + 审计,依赖 Git 作为安全回退)
- 无上限自动多 Agent

## 硬性前提

- 任务必须在 Git 仓库内发起;Git commit/status 是回退安全边界。
- 本地单进程执行(V1 所有 Agent 为同进程内并发任务,不是 OS 进程隔离;OS 进程/worktree 隔离是后续演进)。
