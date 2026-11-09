# 164 · 原生 tool-calls 协议与用量记账 Alpha

状态: 已交付(Alpha) · 2026-11-10
来源: 对照 dsh-TUI 的差距分析(P0 项)。端点探测证实 glm-5.3-flash 完整支持
OpenAI 工具调用(含 delta 分片)与 `stream_options.include_usage`;
流式 delta 分 `content` / `reasoning_content` / `tool_calls` 三型,
与 dsh-TUI 的 text/reasoning/tool-args 分型词汇同构。

## 1. 目标

- 聊天路径(REPL + TUI chat)切换原生 tool-calls:模型正文即最终答复,
  流式输出变成干净的打字机效果,消除"流式输出是 JSON"的违和;
  解析修复轮次随之消失。
- 用量记账:每轮 usage(prompt/completion/total)落账并汇总展示,
  成本从黑盒变为可见。

## 2. Provider 层

- `ChatRole` 增加 `Tool`;`ChatMessage` 扩展:`tool_calls`(assistant 消息)、
  `tool_call_id`(tool 消息);既有 user()/assistant() 构造器不变。
- `ToolDef { name, description, parameters }`;`ChatRequest.tools`。
- `ChatResponse` 增加 `tool_calls: Vec<ToolCallInfo>` 与 `usage: Option<Usage>`。
- 流式:`on_delta` 升级为分型回调
  `on_event(ProviderStreamEvent)`:`TextDelta / ReasoningDelta /
  ToolCallDelta{index,id,name,args_delta}`;末块 usage 并入返回值。
- tool_calls delta 按官方语义按 index 分桶累积(id/name/arguments 分片拼接)。
- MockProvider: 支持带 tool_calls 的脚本项(供新循环单测)。

## 3. Orchestrator

- 新字段 `native_tools: bool`(默认 false;task run/serve/queue/parallel 保持
  legacy,后续 spec 迁移)与 `tools: Vec<ToolDef>`。
- 工具循环(native):`resp.tool_calls` 非空 → 逐个经既有 ApprovalSink 审批、
  ToolExecutor 执行、ToolCallRequested/Completed 落账 → 结果以 tool 消息回填;
  无 tool_calls 且 content 非空 → 即最终答复(MessageCompleted,正文 delta
  即打字机)。轮次上限与 abort 语义不变。
- Coordinator 首轮规划:`plan` 成为原生工具(参数 plan/workers);
  Worker 无需 message 工具(最终答复即纯文本)。
- 降级(调整):端点探测证实 glm-5.3-flash 非流式返回结构化 tool_calls,
  但流式工具回合会退化为伪标签/文本形式(`<loong…>` 或 `name(args)`)。
  处置:流式响应嗅探到工具意图文本(伪标签或以声明工具名开头)时,
  该轮改走非流式重试;legacy JSON 循环整体保留不动。
- Trait `Tool` 增加 `parameters_schema()`(正式 JSON Schema;11 个工具各自实现),
  Orchestrator 由注册表生成 ToolDef。

## 4. 用量记账

- `EventPayload::UsageRecorded { run_id, turn, prompt_tokens,
  completion_tokens, total_tokens }`:每轮模型调用后落账
  (对齐 dsh-TUI "usage 与转录分离"原则:记 token 不产生转录行)。
- run 结束 emit important 汇总行(REPL 直显,TUI 进 important 区);
  reasoning/cached tokens 本版不入账。

## 5. 非目标

- reasoning_content 流式展示(事件已分型,渲染留后续);
- task run/serve/queue/parallel 的 tools 迁移;
- steer/followup 投递、压缩、权限三态。

## 6. 验收

- 单测:tool_calls delta 分桶累积;usage 解析(流式末块与非流式);
  native 循环(工具调用→结果回填→最终答复);Rejected 降级;
  UsageRecorded 落账。
- 真实模型冒烟:聊天两轮,打字机正文 + usage 汇总可见;账本含
  UsageRecorded 且 seq 连续。

## 7. 验收记录(2026-11-10)

- 单测:SSE 分型解析(文本/思考/工具分片按 index 聚合/末块 usage)3 项;
  native 循环(工具调用→结果回填→最终答复,UsageRecorded 每轮落账)1 项;
  workspace 全量通过。
- 真实模型冒烟(glm-5.3-flash):第一轮模型发起 list_dir,嗅探降级后工具真实
  执行(返回目录清单),最终答复准确;第二轮纯文本流式打字机正常;
  每轮 `[usage] prompt/completion/total` 可见,账本 UsageRecorded 落档。
- 端点怪癖记录:glm-5.3-flash 流式 tool_calls 模板不稳定(伪标签与
  `name(args)` 文本两种退化形式),嗅探降级为当前最稳路径;
  tool 执行中偶发一次 ToolNotFound 后模型自行重试成功(模板噪声,不阻断)。