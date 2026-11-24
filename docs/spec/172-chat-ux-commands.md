# 172 · 聊天体验增强(工具结果进转录 + / 命令系统)

状态: 部分交付 · 2026-11-25(§1 未实现,见 §4)
来源: 用户反馈 —— 聊天屏转录区缺少工具执行结果,且缺少 / 命令系统。

## 1. 工具结果进转录 —— 未实现

> 2026-11-25 核对:本节没有任何实现。当前只有进度行 `⚙ 工具 x 完成`
> (state.chat_tool_line),工具的输出摘要不进转录。这是用户反馈
> "模型调了工具却看不到结果"的直接原因,列为下一步第一项。

- ToolCallCompleted 事件到达时追加 System 条目:
  ⚙ 工具名 ✓ / ✗ + 结果摘要(前 200 字符)
- 运行结束后这些条目保留在转录中(不像 chat_tool_line 只显示进度);
- 这解决了"模型调了工具但用户看不到结果"的问题。

## 2. / 命令系统

| 命令 | 功能 |
|------|------|
| /help | 显示可用命令 |
| /new | 新会话 |
| /exit | 退出 |
| /task | 当前任务 id |
| /status | 任务状态 |
| /usage | token 用量 |
| /clear | 清屏(转录) |

## 3. REPL 同步

REPL 已有 /new /task /status /exit;新增 /help /usage /clear。

## 4. 验收记录(2026-11-25)

实际交付(已在真实 TUI 里逐项驱动验证):

- 聊天屏 `/` 命令:`/help` `/new` `/task` `/status` `/usage` `/clear` `/exit`;
  未知命令(如 `/context`)返回 `未知命令: /context(输入 /help 查看可用命令)`,
  不再被静默吞掉,也不会发给模型。
- 启动即进聊天屏,并从账本恢复最近会话(mod.restore_chat_session;
  此前 2a1804f 的提交说明声称已做,实际插错了文件,未生效)。
- 聊天屏底栏改为聊天屏专属提示(之前显示的是任务列表的按键说明)。
- REPL:`--show-thinking` 与 `/help` `/usage` `/clear` `/new` `/task` `/status` `/exit`。

未实现:

- §1 工具结果进转录。
- `/context` `/resume` `/compact` `/model` 等 Claude Code / Codex 常用命令。

测试:3 个列表渲染测试因默认屏改为 Chat 而失败,已让它们显式切到 List 屏;
新增 default_screen_is_chat 锁定默认屏契约。
