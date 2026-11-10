# 167 · 权限三态(allow-once / allow-always / reject+理由)

状态: 已交付(Alpha) · 2026-11-11
来源: 对照 dsh-TUI `PermissionDecision` 词汇(allow-once / allow-always /
reject{message})的 P1 差距项。

## 1. 设计

- allow-always 为会话层状态(sink 内 allowlist),批准时账本仍记 Approve
  (语义诚实;"始终"是 UI 会话意图,不入账本);命中 allowlist 的后续调用
  自动批准并发 [auto] 提示,不再弹审批。
- 覆盖三处 sink:StdinApproval(task run)、ChatApproval(REPL)、
  TUI 弹窗(w 键;TUI allowlist 存于 TuiState,跨 run 存活)。
- reject 理由:`ApprovalSink` 新增默认方法 `reject_reason()`;
  拒绝时提示输入理由(可空),executor 将理由拼入工具失败结果文本回传模型
  ("rejected by user: <理由>"),模型可据此调整方案。

## 2. 非目标

- 持久化 allowlist(重启即失效);并行路径 ParallelApprovalSink;
- TUI 拒绝理由输入(弹窗内文本框,留后续)。

## 3. 验收

- 单测:allowlist 命中自动批准(无需输入);拒绝理由落 last_reason 并
  经 reject_reason() 回读。
- 人工:REPL/TUI 审批提示展示三选项;TUI w 键后同工具自动批准。

## 4. 验收记录(2026-11-11)

- 单测:allowlisted_tool_auto_approves_without_prompt、
  stdin_line_parses_to_decision(含拒绝理由回读)通过;workspace 全量通过。
- 真实冒烟:会话恢复链路正常;GLM 该轮未发起工具调用直接作答,
  管道脚本中的 'a' 落为普通消息 —— 交互式审批与脚本输入的固有序列问题,
  allow-always 逻辑以单测覆盖;交互终端下 y/a/n 提示与 w 键人工验证可用。