# 166 · 聊天体验打磨(思考流 / 工具活动 / 多行输入)

状态: 已交付(Alpha) · 2026-11-11
来源: 对照 dsh-TUI 交互范式(spec 164 差距分析 P2 项提级):
`AssistantBlock` 的 reasoning 独立呈现、工具运行卡片、Shift+Enter 多行输入。
本轮全部在 CLI/TUI 表现层,不动 engine 协议。

## 1. 思考流展示(reasoning)

- Provider 的 `ReasoningDelta` 经事件流进入 TUI:
  转录区追加暗色 System 条目实时累积;正文 TextDelta 到达时该条目
  收起为"💭 …(思考已收起)"(对齐 dsh-TUI 思考折叠语义);
- REPL 本版不渲染思考流(事件保留,后续 spec 可加 --show-thinking)。

## 2. 工具活动进聊天转录

- 运行中的工具进度行("工具 x(T0) 完成")在聊天屏以 System 条目呈现,
  形成"工具活动卡"的简化版(dsh-TUI tool.call/result 卡片的 Alpha 替代);
- 非工具类进度行不进转录,仍走详情页进度区。

## 3. 多行输入(TUI)

- Shift+Enter 在输入框插入换行(Enter 发送语义不变);
- 转录渲染按多行展开。

## 4. 验收

- 单测:思考条目生命周期(累积→收起)、TextDelta 在非 Assistant 尾部时
  新建条目、Shift+Enter 换行进输入框。
- 冒烟:TUI 真实模型两轮,思考条目可见并收起,工具活动进转录,
  多行输入正常发送。

## 5. 验收记录(2026-11-11)

- 单测:chat_ux_tests 3 项(思考累积→收起/工具行过滤/多行输入);
  workspace 全量通过(serve isolated 偶发用例单跑复通过)。
- 真实冒烟(glm-5.3-flash,TUI PTY 驱动):进入聊天屏自动恢复上一会话
  (spec 165 链路);工具回合💭思考条目实时累积并在正文到达时收起;
  工具执行后最终答复流式正常;账本 ChatUserTurn/MessageCompleted/
  UsageRecorded 各 3 条,一致。
- 多行输入由单测覆盖(PTY 无法合成 Shift+Enter 控制台事件)。