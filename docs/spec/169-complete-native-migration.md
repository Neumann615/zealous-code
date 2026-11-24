# 169 · 原生迁移收尾(parallel / queue / serve)

状态: 已交付 · 2026-11-25(此前记录有误,见 §3)
来源: spec 168 遗留 —— parallel / queue / serve 路径仍走 legacy。

## 1. 内容

- parallel.rs / queue.rs / serve/runner.rs:编排器 native_tools=true + 注册表工具;
  run_task 分流自动走 run_task_native(plan 工具规划 / 直接执行 / 单 Agent)。
- 至此所有运行入口(chat、task run、TUI、queue、parallel、serve)均为原生 tool-calls;
  legacy JSON 动作循环仅作为 native_tools=false 的回退路径保留。

## 2. 非目标

- legacy agent_loop 代码退役。

## 3. 状态记录(更正)

- 2026-11-12 的记录称入口翻转已回退、main 保持 legacy —— 与代码不符。
  入口翻转从未真正回退,但对应的测试基建没有迁移完,导致 main 上有 16 个测试失败
  (zealous-engine 10、workspace_isolation 5、serve_negative 1)。
  当时多次报告"全绿"是错的:没有带 --no-fail-fast,cargo 在第一个失败套件就停了,
  后面的套件根本没跑。
- 2026-11-25 修复,根因四条:
  1. 级联假失败:parallel 测试的 provider_env() 用 ENV_LOCK.lock().unwrap(),
     锁被毒化后整模块连锁 panic;改为与其它调用点一致的 unwrap_or_else(into_inner)。
  2. 真失败:parallel/queue 的 MockProvider 仍返回 legacy 动作文本,工具从未执行。
     新增 MockProvider::scripted_actions + action_text_to_response,把 `{"action":...}`
     脚本逐条映射为原生 tool_calls 响应;测试脚本保持可读写法。
  3. 集成测试的 stateful mock 端点用旧标记 "tool `x` result" 判断工具已执行,
     原生路径回填的是 "ok=<bool>",永远匹配不上。workspace_isolation 4 处补上标记,
     serve_negative 的 mock 补上动作转换。
  4. neg_already_running 的断言曾被改松并丢失 already_seen 赋值,必然 90s 超时;
     已恢复严格断言(第二轮 run_task 必须返回 already_running)。
- 附带修复:流式降级嗅探只在流式模式生效,非流式返回的是结构化 tool_calls,不再重复请求。

## 4. 验收

- `cargo test --workspace --no-fail-fast`:220 passed, 0 failed, 1 ignored
  (cli_e2e::real_model_smoke 消耗真实额度,须显式执行)。
