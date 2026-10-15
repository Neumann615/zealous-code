# 10 · 领域模型

状态: 已定案 · 2026-10-15

## 实体与所有权

| 实体 | 所有者 | 说明 |
| ---- | ------ | ---- |
| `Task` | 事件账本(投影) | 用户提交的一次工程任务;状态只能由事件推导,不存在独立可变状态 |
| `AgentRun` | 事件账本(投影) | 一个 Agent 的**一次连续执行**(进程内);恢复/重试总是开启新 Run,绝不复用已结束的 Run |
| `Turn` | 当前 Run | 模型交互轮次: prompt → 响应(含工具调用) |
| `ToolCall` | 当前 Turn | 一次工具调用: 请求、风险级、审批关联、结果 |
| `ApprovalRequest` / `ApprovalDecision` | 事件账本 | 高风险动作执行前落账本,阻塞等待决策 |
| `TaskEvent` | 账本写入器(唯一) | 统一事件信封,见 `20-event-ledger.md` |

## 状态机

```
Task:      Queued → Running → Completed | Failed | Cancelled
                       ↑  ↓
                    Interrupted(可恢复;resume 开新 AgentRun)

AgentRun:  Running → Completed | Failed | Killed
Turn:      Active → Completed | Failed
ToolCall:  Requested → AwaitingApproval → Approved → Completed
                                      ↘ Rejected(终态,等价失败结果)
```

- `Interrupted` 的唯一入口: 用户中断(SIGINT/`interrupt` 命令)或进程崩溃(重启后由恢复逻辑判定)。
- `Task` 的终态(Completed/Failed/Cancelled)之后仍可被 resume → 回到 Running 并开启新 Run(resume 是一等公民,不是异常路径)。
- Task/AgentRun 状态变更必须由对应事件驱动:`TaskStatusChanged` / `AgentRunEnded`;禁止代码里维护第二份可变状态。

## 拓扑与写入边界

- 每 Task 固定 1 个 Coordinator;Worker 数量在 Task 创建时确定,上限 3,创建后不可增长。
- Coordinator 拆解任务、派发 Worker、串行 apply 结果、验证并汇报。
- **主工作区写入权仅属于 Coordinator,且必须经过 apply 阶段**(见 `30-permissions.md`)。
- Worker 默认只读分析;产出(建议/补丁)落在任务隔离目录 `<repo>/.zealous/tasks/<task_id>/workers/<agent_id>/`。
- V1 Worker 是进程内异步任务(独立 LLM 上下文),不是 OS 进程。

## Provider 配置与失败语义

- 配置项: `base_url`、`api_key`、`model`、超时与重试参数;来源: 环境变量 + 用户配置文件,优先级代码内固定。
- `api_key` 与 `base_url` **禁止**进入事件账本、日志和任何持久化产物。
- 每次模型请求: 超时(默认 120s)+ 指数退避重试(默认 3 次);重试耗尽 → 当前 `Turn.Failed` → `AgentRun.Failed` → `Task.Failed`,失败原因写入事件。
- 配置缺失/非法: Task 创建时即失败(`Failed`,原因 `provider_config_invalid`),不产生任何模型调用。
