# 110 · Alpha 稳定性收口

状态: 已定案 · 2026-10-17
目标: 修复真实模型暴露的稳定性问题,使 real_model_smoke 恢复通过。
不做: 桌面壳、Web 壳、远程连接、插件系统、多客户端。

## 1. 规划阶段兼容真实模型(Direct Execution Mode)

真实模型暴露:规划轮返回合法 tool 动作(如先 read_file)曾导致任务失败
("planning turn returned tool action")。该输出合理,是编排器不兼容,
修订 spec 70 规划语义:

- 首选行为仍是返回 plan。
- 规划轮返回**合法 tool 动作** → 进入 Direct Execution Mode:
  1. 生成隐式 plan:`Direct execution: <task title>`;
  2. 落账 TaskPlanUpdated;
  3. 同一 Coordinator Run、同一 turn 内执行该工具调用
     (TurnStarted → ToolCallRequested → ToolCallCompleted → TurnCompleted);
  4. 工具结果作为下一轮 prompt;
  5. 不创建 Worker;
  6. turn 编号连续,apply 阶段接续。
- Direct/apply 阶段模型再返回 plan → 视为非法动作进修复轮
  (ErrorOccurred(scope=model_action)),超限 Run Failed。
- 首轮 message 行为维持现状(作为方案落地,不创建 Worker)。

## 2. subscribe_events 持续订阅(spec 100 修订)

- 先重放 seq > after_seq 的账本事件(replayed 返回实际重放条数);
- 重放完成保存 cursor;服务运行期间周期轮询账本,
  新事件按 seq 顺序推送 task_updated;
- after_seq 大于当前最新 seq → invalid_params;
- 中间损坏行:error_occurred 通知 + 清除订阅,不得部分重放;
- torn tail:仅提示已丢弃,不视为致命错误;
- V1 单订阅,新订阅覆盖旧订阅。

## 3. 审批决策绑定(spec 100 修订)

decide_approval 参数:`{task_id, call_id, decision}`。

- task_id 必须匹配当前运行任务;call_id 必须匹配当前 pending approval;
- 未知/重复/过期 call_id → no_pending_approval;
- 决策发送后清除 pending 绑定;
- approval_requested 通知必须携带 task_id、call_id、tool、risk、完整 summary;
- 客户端不得仅凭 decision 字符串影响下一个未知审批。

## 4. serve 生命周期清理

- 不用字符串 contains 判断 shutdown;
- Session::handle 返回 `Handled(Response)` / `ShutdownRequested(Response)`;
- shutdown:等待 worker join → 输出响应 → 干净退出;
- stdin EOF:等待当前 worker 完成 → 冲刷剩余通知;
- broken pipe:不输出敏感信息,非零码退出;
- 单客户端限制保留。

## 5. TUI 手工验收

docs/acceptance/tui-alpha.md 的 8 个场景必须全部有真实结果
(通过/失败/未执行),失败项附复现步骤;全部通过前 TUI Alpha 不算完成。

## 测试

- Direct Execution Mode 完整链路(mock):无 plan 首轮 → 事件顺序/
  隐式 plan/无 Worker/文件修改/seq 连续。
- 执行阶段 plan 进修复轮。
- serve:订阅重放/实时推送/cursor/损坏账本/torn tail;
  审批绑定正确/错误 task/错误 call/重复/无 pending/通道关闭。
- real_model_smoke 显式执行通过(真实模型回归)。
