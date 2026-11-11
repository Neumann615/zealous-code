# 168 · task run 路径迁移原生 tool-calls

状态: Alpha 实施中 · 2026-11-11
来源: spec 164 仅迁移聊天路径;本 spec 将 task run(TUI R/I)、queue、serve
的任务执行统一切换原生 tool-calls,legacy JSON 循环仅保留为
`native_tools=false` 的回退路径(测试与 parallel 暂用)。

## 1. 设计

- `run_task` 入口分流:native_tools=true → `run_task_native`;false → legacy
  (MockProvider 单测与 parallel 不受影响)。
- 原生规划:工具集 = 注册表 + `plan` 工具(参数 plan/workers);
  - 模型调用 plan 工具 → TaskPlanUpdated 落账,进入 Worker 阶段;
  - 调用注册表工具 → Direct Execution(隐式 plan,行为同 spec 110);
  - 纯文本 → 单 Agent 任务(文本即 plan);
- Worker:`run_worker` 增加 native 分支,系统提示改为原生措辞
  (最终报告即纯文本),循环走 `agent_loop_tools`;
- Apply 阶段:复用规划转写,`agent_loop_tools` 以 start_turn 续跑,
  apply 提示改为原生措辞;
- `agent_loop_tools` 增加 ctx/start_turn 参数以支持续跑;
- usage 每轮落账(同 spec 164)。

## 2. 非目标

- parallel 路径迁移(保持 legacy,后续 spec);
- 拒绝理由经 ChatUserTurn 等其它通道。

## 3. 测试策略

- mock endpoint 的 `chat_body` 增加 native 转换:legacy JSON 动作脚本
  在测试进程内转换为原生 tool_calls 响应 —— 现有 E2E 脚本零改动;
- parallel 相关测试走 legacy 透传(chat_body native=false)。
 ## 5. 调试记录(2026-11-11 续)  - cli_e2e BrokenPipe 复现分析:原生 task run 在 mock 下 ~500ms 内完成   (含 edit_file T1 审批,账本 Approve)——盲写 y 的测试在进程退出后写入   即 BrokenPipe;已改为健壮轮询写入(写失败即停),但仍不稳定,   且 T1 在无输入时被批准的路径待查(疑似与 reader 线程/EOF 交互)。 - 下一步:修复 cli_e2e 审批等待逻辑(等"审批请求"标记再写,而非盲写);   排查无输入 Approve 的来源;serve stateful endpoint 原生协议重写。