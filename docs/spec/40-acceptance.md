# 40 · 验收场景

状态: 已定案 · 2026-10-15
每个场景必须映射到可自动执行的测试(E2E 或集成测试),V1 不接受"手工验证过"作为唯一依据。

| # | 场景 | 判定 |
| - | ---- | ---- |
| A1 | 闭环: 在 Git 仓库提交"理解需求→分析代码→修改文件→运行验证→汇报"任务 | Task 到达 `Completed`;账本含完整事件链(TaskCreated→…→TaskStatusChanged=Completed);主工作区产生预期修改 |
| A2 | 拒绝高危命令: T2 Shell 调用被用户 Reject | 命令未执行(无对应进程副作用);`ToolCall.Rejected` 落账;Task 状态可解释(不静默失败) |
| A3 | 中断恢复: 运行中 SIGINT,再 resume | `TaskInterrupted` 落账;resume 后新 AgentRun,投影与中断前连续;挂起审批按 interrupted 收尾 |
| A4 | 多 Worker 只读分析: Coordinator 管理 ≥2 个 Worker | 各 Worker 均为 T0 工具调用(账本可验);产出串行 apply,apply 各自产生 T1 审批 |
| A5 | 崩溃恢复: kill -9 后重启 | 重启读取账本,seq 连续性校验通过(torn tail 丢弃+告警);任务视图正确重建 |
| A6 | Provider 异常: 配置错误/超时/非 2xx | 分别进入 `provider_config_invalid` / 重试耗尽失败;错误原因可从账本读出;无密钥泄漏 |
| A7 | Windows 可用 | A1–A6 全部在 Windows 通过(路径分隔符、SIGINT 语义差异显式处理) |

## 账本断言公共规则

- 任何场景结束后 `seq` 连续、无跳号、无残缺中间行。
- 投影(api:`TaskProjection::fold(events)`)推导的状态 = CLI 展示状态。
- 密钥/endpoint 凭据不出现在账本文件中(grep 断言)。
