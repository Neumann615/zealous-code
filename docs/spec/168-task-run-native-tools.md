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
 ## 5. 调试记录(2026-11-11 续)  - cli_e2e BrokenPipe 复现分析:原生 task run 在 mock 下 ~500ms 内完成   (含 edit_file T1 审批,账本 Approve)——盲写 y 的测试在进程退出后写入   即 BrokenPipe;已改为健壮轮询写入(写失败即停),但仍不稳定,   且 T1 在无输入时被批准的路径待查(疑似与 reader 线程/EOF 交互)。 - 下一步:修复 cli_e2e 审批等待逻辑(等"审批请求"标记再写,而非盲写);   排查无输入 Approve 的来源;serve stateful endpoint 原生协议重写。 ## 6. 调试记录二(标记驱动改造后)  - e2e 改为标记驱动(等"审批请求"再写 y)后:进程在 `provider: ...` 行后   **立即以 exit 0 退出**,无"任务进入 Running"/turn 输出 ——   run_task 顶部分流(if self.native_tools → run_task_native)存在且   cmd_run 已 native_tools=true,但 native 分支未执行;   run_chat_turn(chat)同一分流工作正常(此前冒烟已验证)。 - 待查:cmd_run 的 Orchestrator 构造点是否被后续编辑回退   (main.rs 718 行 native_tools: true 已确认)与 run_task 实际调用关系;   怀疑构造点注入到错误字面量(main.rs 有多个 Orchestrator 字面量)。
## 7. 验收记录(2026-11-12)

- workspace 全量测试通过(37 cli bin / 131 engine / 其余套件全绿)。
- cli_e2e 两个用例在 native 路径下通过:
  e2e_real_binary(plan → read_file → git_diff → edit_file[T1 审批] → message,
  README 断言 "fixture edited" 通过)与 e2e_spec154(plan → read_file 分片 →
  search_text 正则 → git_show → edit_file → message,账本断言全部通过)。
- 脚本迁移:测试 mock 脚本已改写为原生 tool_calls 响应体
  (native_tool_response),chat_body 对含 choices 的响应体直接透传。
- 根因记录:cargo test 环境 stdin 为空 → StdinApproval reader 立即 EOF →
  审批按 fail-closed 自动拒绝(spec 30 语义正确)——交互式 e2e 必须用
  --yes 或 TTY 驱动;"无输入 Approve"之谜同源。
- parallel 路径保持 legacy(后续 spec 迁移)。