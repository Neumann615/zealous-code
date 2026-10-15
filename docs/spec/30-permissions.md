# 30 · 权限与风险分级

状态: 已定案 · 2026-10-15

## 分级表

| 级别 | 动作 | 策略 |
| ---- | ---- | ---- |
| T0 自动 | 读文件、目录列举、内容搜索、`git status/log/diff`(只读) | 直接执行,落账 |
| T1 审批 | 主工作区文件写入/删除、apply 补丁 | `ApprovalRequest` → 用户决策后执行 |
| T2 审批 | Shell 执行、网络访问(模型 endpoint 除外)、依赖安装、跨出仓库根的路径访问 | 同上,且 payload 必须完整展示将执行的命令/URL/路径 |

- 分级是代码内固定策略表(V1 不暴露配置面);任何新工具接入必须先归级。
- 归级原则就高不就低: 无法明确判定为 T0 的动作一律按 T1+ 处理。
- Worker 没有写入权:Worker 可发起的只有 T0 工具;Worker 产出补丁写入任务隔离目录属于框架行为,不经过工具层。

## 审批语义

- 阻塞式: `ApprovalRequested` 落账后,对应 `ToolCall` 停在 `AwaitingApproval`,任务事件循环继续(其他 Agent 不被阻塞),仅该调用挂起。
- 决策枚举: `Approve` / `Reject`,每次请求逐一决策(V1 不做"始终允许"记忆,避免静默扩权)。
- `Reject` → `ToolCall.Rejected`,以失败结果回给模型(含"用户拒绝"语义),模型可调整方案;任务不因此失败。
- 用户中断(SIGINT)时所有 `AwaitingApproval` 的调用以 `Rejected(reason=interrupted)` 收尾,Task → `Interrupted`。
- CLI 进程之外无审批通道: 没有 attaching 的 TUI 时,审批挂起即任务挂起(事件中可见),下次 attach/resume 后继续处理。

## 边界与回退

- V1 不宣称 OS sandbox: 高危动作的防线是 **分级 + 审批 + 事前完整展示 + 账本审计**,外加 Git 提交作为回退点(apply 阶段前若工作区不干净,apply 工具必须提示用户先 commit/stash)。
- 路径越界判定在工具层统一实现(仓库根解析后前缀校验,symlink 解析后的真实路径同样校验),不依赖各工具自觉。
