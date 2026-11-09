# 163 · TUI 聊天屏(对话式工作台)

状态: 已交付(Alpha) · 2026-11-10
来源: spec 162 交付终端 REPL 后,dogfooding 反馈希望可视化界面同样具备
"对话即操作"形态。本 spec 在既有 ratatui 工作台内新增聊天屏,
分层范式参考 dsh-TUI(agent-assets/dsh-TUI-main):事件驱动投影,
live 流与历史共用同一事实源(事件账本)。

## 1. 范围

- `Screen::Chat`: 全屏聊天视图。列表页 `c` 进入,`Esc` 返回。
- 底部输入框: 字符输入/Backspace,Enter 发送,运行中禁发(输入保留)。
- 会话连续性: 同 spec 162 §5 —— 首条消息创建任务,后续消息在
  `chat_task` 上 resume;客户端压缩对话史随 prompt 注入。
- 流式: `TuiEvent::ChatDelta` 增量追加到当前助手条目(通知通道,不落账本);
  run 结束后用账本末条 `MessageCompleted` 全文替换流式原文并写入对话史。
- 审批: 复用全局审批弹窗与 a/d 键;`↑/↓/PgUp/PgDn` 滚动转录区。
- 事实源不变: 转录区是 UI 派生数据,任务状态/结果仍以投影与账本为准。

## 2. 非目标

- 不做 Markdown 渲染、diff 分栏、鼠标支持(记入后续 spec 候选)。
- 不做多会话并存;chat session 状态仅驻内存,退出 TUI 即失
  (任务与账本持久,重新进入可通过 task 详情 R 恢复)。
- 不改 queue/parallel/merge 等既有屏。

## 3. 交互

| 键 | 行为 |
| --- | --- |
| `c`(列表页) | 进入聊天屏 |
| 字符/Backspace | 编辑输入框 |
| `Enter` | 发送;`chat_task` 不存在则以消息为 brief 建任务(direct 模式) |
| `Esc` | 返回列表(会话保留) |
| `↑/↓` `PgUp/PgDn` | 转录区滚动(自底部偏移) |
| `a/d`(审批弹窗) | 批准/拒绝(全局语义不变) |
| 运行中 `Enter` | 忽略,输入保留 |

## 4. 验收

- 单测: 对话史 prompt 构造(含截断/空史退化)、delta 追加、
  finish 后原文替换与史写入。
- 构建: workspace 全量测试通过。
- 人工冒烟: 真实模型两轮对话,流式可见,结束后转录呈现最终答复。

## 5. 验收记录(2026-11-10)

- 单测: chat_state_tests 4 项通过;cli 33 项、workspace 全量通过(serve isolated
  用例单跑复通过,失败为既有偶发超时,与本变更无关)。
- 真实模型冒烟(glm-5.3-flash,人工 PTY 驱动): 列表 `c` 进入聊天屏;
  首条消息建任务 task_01M70PR1V1(标题取消息前 48 字符),流式动作 JSON 实时
  可见,完成后转录替换为最终答复"2+3=5。";第二条消息同任务 resume,
  模型基于对话史回答"5+10=15。";账本 runs=2、MessageCompleted=2、seq 连续;
  `q` 正常退出。
- 已知 Alpha 限制: 动作 JSON 原样流式(美化依赖后续 tool-calls 协议);
  会话状态仅驻内存;无 Markdown 渲染与鼠标支持。