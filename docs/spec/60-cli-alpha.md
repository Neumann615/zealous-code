# 60 · CLI Alpha 加固

状态: 已定案 · 2026-10-15
目标: 让真实 OpenAI-compatible endpoint 能通过 CLI 稳定跑完一次任务。
仍不做: TUI、桌面端、app-server、插件系统、OS sandbox。

## 1. TaskId 校验(路径穿越防线)

- 合法 TaskId = `task_` + 26 位 Crockford Base32 ULID。构造函数不再暴露原始字符串字段。
- 解析 API:`TaskId::parse(&str) -> Result<TaskId, IdError>`;CLI/API 的任何 id 输入必须先解析,非法输入在触达文件系统前被拒绝。
- `Ledger::open` / `TaskService::open_task` 等接受 id 的入口同样只接受解析后的值。

## 2. 异常 Running 的规范化(崩溃恢复语义)

新进程打开投影为 `Running` 的任务 = 上次进程异常终止。resume 前先规范化:

```
open ledger
  → projection.status == Running
      append TaskInterrupted(reason=process_restart)
      (fold 语义自动把 dangling pending approval 收为 Rejected——与 spec 30 一致,
       Rejected 是 ToolCall 终态,不再追加 ToolCallCompleted;
       "失败结果回给模型"由投影状态承载,新 Run 的上下文可见)
      append TaskStatusChanged Running→Interrupted
  → 正常 resume 流程:TaskResumed + 新 AgentRunStarted → Running
```

注:spec 30 规定 Rejected 终态;本节"追加失败工具结果"指模型在新 Run 中从投影看到失败,而非额外事件。

## 3. Ctrl-C 语义

- CLI 安装 ctrlc handler:仅设置 abort 标志(AtomicBool),handler 内不做 IO。
- 审批等待 = 后台 stdin reader 线程 + channel + 100ms 轮询;轮询同时检查 abort。
- abort 后:当前审批立即 Reject → 循环在轮次边界停止 → Task 落账 Interrupted。
- 再次 `task run` 开启新 AgentRun(既有 resume 语义)。

## 4. 脏 Git 工作区保护

- `write_file`(主工作区写入)执行前运行 `git status --porcelain`;输出非空 → 拒绝写入,提示用户先 commit/stash。
- 测试夹具必须带初始 commit。

## 5. 工具 manifest(模型契约)

模型提示必须包含稳定的工具 manifest:工具名、风险级、参数 JSON 形态、返回语义,以及 action 格式与示例。manifest 由工具注册表生成,保证与实现一致;测试断言覆盖全部内置工具。

## 6. 真实端到端验证

CLI 集成测试:本地 mock `/chat/completions` server(脚本化 plan→read→write→message)→ 以子进程运行真实 `zealous` 二进制 → stdin 自动批准 → 断言:文件修改存在、任务 Completed、events.jsonl seq 连续且含 plan/tool/approval/complete 事件、API key 与 Authorization 不落账。真实用户 endpoint 的手工 smoke 在自动化之后执行,且任何输出不得包含密钥。

## 验收

- 既有 A1–A6 全部继续通过。
- 新增 CLI 端到端测试通过。
- 异常重启后 `task status` / `task run` 语义可解释(Running → Interrupted → resume)。
- Ctrl-C → Interrupted;脏工作区拒绝写入;非法 task ID 无法构造路径穿越。
