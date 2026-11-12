# 170 · 对话转录增强(工具结果 / 思考流 / REPL --show-thinking)

状态: Alpha 实施中 · 2026-11-12
来源: 对照 dsh-TUI 的 reasoning 块与工具卡片,补齐对话转录的表达力。

## 1. 工具结果进转录

- TUI chat:ToolCallCompleted 事件到达时追加 System 条目:
  `⚙ <工具名> ✓` 或 `⚙ <工具名> ✗ <原因>`;
  与既有 chat_tool_line 合并(去重:同一 turn 的同一工具只追加一次)。

## 2. 思考流展示

- TUI:💭 条目已在 spec 166 实现(累积+收起),本轮验证 + 样式微调;
- REPL:新增 `--show-thinking` flag,开启后 ReasoningDelta 以灰色缩进
  打印;默认不显示(避免干扰)。

## 3. 验收

- TUI:真实模型冒烟,💭 可见;工具执行后 ⚙ 条目出现。
- REPL:--show-thinking 开启时可见思考流;默认关闭时不可见。
