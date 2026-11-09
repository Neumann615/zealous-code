# 162 · 聊天模式 Alpha(流式对话 REPL)

状态: 已交付(Alpha) · 2026-11-10
来源: dogfooding 反馈 —— 现有 TUI 是"任务管理工作台"(列表/表格/快捷键),
与 Claude Code / Codex 式"对话即操作"的预期不符。参考 dsh-TUI
(agent-assets/dsh-TUI-main, MIT, commit 以 tarball main 为准)的
AgentEvent/AgentSession 分层范式后,确定核心差距在 engine 的推理链路,
而非 UI 表面。本 spec 先补 engine 能力,再以最小 UI 形态(REPL)交付对话体验。

## 1. 目标

- `zealous chat`: 终端对话 REPL。首条消息创建任务并运行;后续消息在同一任务上
  以 resume 语义续跑(spec 10/70),形成连续会话。
- Provider 流式: OpenAI-compatible SSE(`stream=true`),助手输出 token 级
  delta 实时回传;REPL 直接打印,消除"长时间无输出"的黑盒感。
- 审批语义不变(spec 30): T0 自动,T1/T2 经 stdin 内联 y/N 审批。
- 事实源不变: 流式 delta 只走通知通道(内存/stdout),不落账本;
  账本仍按轮记录 `MessageCompleted` 全文(spec 20),可审计可回放。

## 2. 非目标

- 不改 TUI 工作台(ratatui 聊天屏留待后续 spec);REPL 即 Alpha 交付形态。
- 不引入原生 tool-calls API;动作 JSON 协议(spec 70)保持不变。
- 不做多会话并发;同一时间一个 chat session(复用全局 run lease)。
- 不做 token 级用量统计与费用估算。

## 3. Provider 流式契约

- `Provider` trait 新增 `complete_streaming(model, req, on_delta)`,
  默认实现忽略 delta 直接委托 `complete`(Mock/既有实现零改动)。
- `OpenAiCompatProvider` 覆写: 请求体加 `"stream": true`,逐行读 SSE:
  `data: [DONE]` 终止;其余按 OpenAI chunk 解析 `choices[0].delta.content`,
  每个 content 片段调用一次 `on_delta`,并累计为完整文本返回。
- 重试规则: 首个 delta 到达前失败按既有重试策略;流已开始后失败视为
  `Transient` 不再重试(避免 delta 重复)。
- 解析容错: 空 data 行、`[DONE]` 前的注释行、非 JSON data 行跳过不报错;
  累计文本为空且流异常终止 → `Rejected{200}`。

## 4. Orchestrator 增量

- 新增字段 `on_delta: Option<Arc<dyn Fn(&str) + Send + Sync>>`(None = 静默,
  所有既有构造点显式传 None,行为零变化)。
- `turn_raw` 在 `on_delta` 存在时走 `complete_streaming`;
  转写与落账逻辑不变(delta 拼接出的全文仍进 transcript 与 `MessageCompleted`)。

## 5. REPL 语义(`zealous chat`)

- 提示符 `you> `;空行忽略;`/exit` 或 EOF 退出;`/new` 开新会话任务;
  `/task` 显示当前任务 id;`/status` 查看投影状态。
- 首条消息: `create_task_with_brief_text`(title 取消息前 48 字符,
  brief 为全文)后 `run_task`。
- 后续消息: 对同一任务再次 `run_task`,task_prompt 为该条消息
  (resume 路径自动注入投影摘要,模型据此增量续跑)。
- 助手流式输出直写 stdout;`message` 动作的最终答复补换行分隔。
- 运行中 Ctrl-C: abort 置位(spec 60 §3),任务规范化 Interrupted 后
  回到提示符,会话可继续(下次消息自动 resume)。
- 状态码: 正常退出 0;配置/仓库错误沿用 `err()`。

## 6. 验收

- 单测: SSE chunk 解析(多 chunk 拼接/[DONE]/空行/坏行跳过);
  streaming 到达首 delta 后不再重试。
- 集成: mock SSE endpoint,断言 on_delta 收到全部 content 片段且
  返回文本与账本 `MessageCompleted` 一致。
- 真实模型 smoke: `zealous chat` 首条消息得到流式输出与 Completed;
  第二条消息在原任务上 resume 上下文连续(事件账本可见两次 AgentRun)。

## 7. 验收记录(2026-11-10)

- 单测: `sse_multi_chunk_and_done` / `sse_tolerates_noise` 通过;engine 全量 129 通过。
- 真实模型 smoke(glm-5.3-flash): 首条消息创建任务并流式输出 `{"action":"message","text":"1+1等于2。"}`,
  Completed;第二条消息在同一任务 resume(from Completed),模型基于压缩对话史回答
  "刚才的结果是2,再加1等于3",上下文连续。账本含两次 AgentRun,seq 连续。
- 已知 Alpha 限制: 动作 JSON 原样流式输出(未做 message 动作的增量美化);
  审批期间 Ctrl-C 需等待当前输入行;REPL 无 token 用量统计。