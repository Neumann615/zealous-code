# 20 · 事件账本

状态: 已定案 · 2026-10-15

## 原则

1. **事件是唯一事实源**。UI 状态、任务列表、恢复视图全部是 `fold(events)` 的投影,投影不可回写。
2. **只追加,不修改**。账本文件一旦写入,任何代码路径不得改写历史行。
3. **凡对外可见的 Agent 行为必须落账**: 模型输出、工具调用、审批、恢复动作、状态变更。

## 存储

- 每个 Task 一个文件: `<repo>/.zealous/tasks/<task_id>/events.jsonl`,UTF-8,每行一个 JSON 事件。
- `task_id`: `task_<ulid>`,时间有序,无需中心分配器。
- 单写入者: V1 单进程执行,账本写入经**串行队列**(内存 channel → 单个 writer 任务),保证行序即事件序。

## 事件信封

```json
{
  "seq": 42,
  "ts": "2026-10-15T12:00:00.123Z",
  "type": "ToolCallCompleted",
  "task_id": "task_01Jxxx",
  "agent": "worker-1",
  "payload": { }
}
```

- `seq`: task 内单调递增(从 1 开始),写入前分配;读取时校验连续性,跳号视为账本损坏并显式报错(不允许静默吞掉)。
- `agent`: `coordinator` | `worker-N`;用户/CLI 动作记为 `user`。
- `type` 与 `payload` 的具体枚举由 `zealous-core` 中带版本号的结构定义(`"v": 1` 字段);新增事件类型必须新增枚举值而非复用旧类型。

### 事件类型(首批)

`TaskCreated` `TaskStatusChanged` `TaskPlanUpdated` `AgentRunStarted` `AgentRunEnded` `TurnStarted` `TurnCompleted` `MessageCompleted` `ToolCallRequested` `ApprovalRequested` `ApprovalDecided` `ToolCallCompleted` `TaskInterrupted` `TaskResumed` `ErrorOccurred`

## 边界:什么不进账本

- 模型流式 delta(逐 token chunk): 只存在于内存/临时 TUI 缓冲;终稿以 `MessageCompleted`(完整文本)落账。
- `api_key`、`Authorization` 头、endpoint URL 凭据部分。
- 工具结果中的敏感内容按原样落账(本地文件,用户自有数据),但不得做额外采集。

## 崩溃与恢复

- 行级原子性: 每次写入以**单次完整行**追加(整行一次 `write`),进程崩溃最多留下一条残缺尾行。
- 读取器规则: 末行无法完整解析时**丢弃并告警**(标记 `torn_tail=true`),其余行必须全部可解析,否则账本损坏、拒绝恢复。
- 恢复流程: 读全量事件 → 校验 seq 连续 → fold 出 Task/AgentRun/ToolCall 投影 → `TaskResumed` 事件 + 新 `AgentRunStarted` → 继续执行。恢复不修改任何历史事件。
- 幂等: resume 的判定依据是投影状态,不依赖"上次是否正常退出";重复 resume 同一终态 Task 会开启新 Run,天然幂等。
