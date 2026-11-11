# 169 · 原生迁移收尾(parallel / serve)

状态: 设计定案 · 实现回退待单独一轮 · 2026-11-12
来源: spec 168 遗留 —— parallel 路径与 serve runner 仍走 legacy。

## 1. 内容

- parallel.rs:编排器 native_tools=true + 注册表工具;
  run_task 分流自动走 run_task_native(plan 工具规划/直接执行/单 Agent)。
- serve/runner.rs:同上切换;run_task 分流生效。
- 测试:parallel_stage_endpoint 路由增加原生 tool_result 标记
  (ok=True → done);serve_integration 的顺序脚本改写为原生响应体,
  chat_body 增加透传。workspace_isolation 的 serve 脚本同步迁移。

## 2. 非目标

- legacy agent_loop 代码退役(保留为 native_tools=false 回退);
- steer 插话、压缩、权限三态 TUI 拒绝理由输入。
 ## 3. 状态记录(2026-11-12)  - 尝试翻转 parallel 与 serve runner 后,serve_negative/serve_integration/   workspace_isolation 的多个测试失败:这些测试的 mock 端点/脚本仍是   legacy 协议(stateful 内容路由 + 顺序脚本),需整体原生协议化,   工作量超出单轮。已回退入口翻转,main/wip 均保持 legacy 路径。 - 已完成并保留:chat_body 透传改造、native_tool_response 助手、   parallel_stage 路由原生标记(部分)等测试基建。 - 后续:专门一轮完成 mock 基建原生协议化 + 入口翻转 + 全量测试。