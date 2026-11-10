# 165 · 聊天会话持久化(TUI/REPL 可恢复)

状态: 已交付(Alpha) · 2026-11-11
来源: spec 162-163 的聊天会话仅驻内存,退出即丢,与"本地优先、可恢复"
定位矛盾(spec 164 验收后下一步计划,优先级 P1)。

## 1. 设计

- 事实进账本(与 spec 20 哲学一致,不另立会话文件):
  - `ChatSessionStarted`:聊天任务创建后追加,作为会话标记
    (不动 TaskCreated 事件,旧账本零影响);
  - `ChatUserTurn { text }`:每轮用户输入原文落账
    (run_chat_turn 落账;注意保存的是用户原文,不含压缩史注入);
  - 助手答复复用既有 `MessageCompleted`,不重复记。
- 恢复 = 读账本重建:`ChatUserTurn` → 用户条目,
  其后的 `MessageCompleted` → 助手条目;配对写入压缩对话史。
- `TaskService` 新增:
  - `create_chat_task(title, brief)`:建任务 + 追加 ChatSessionStarted;
  - `latest_chat_task()`:按 TaskId 倒序找最近一个含 ChatSessionStarted 的任务;
  - `chat_history(task_id)`:从账本重建 (user, assistant) 对。
- TUI:`c` 进入聊天屏时自动恢复最近会话(转录/史/任务绑定);
  REPL 启动时同样恢复并提示。

## 2. 非目标

- 多会话管理界面(/home 档)、会话重命名/归档;
- 跨仓库会话漫游;图片/文件附件。

## 3. 验收

- 单测:create_chat_task + run_chat_turn(mock)两轮 → latest_chat_task 命中,
  chat_history 重建出 (user, assistant) 对。
- 真实冒烟:REPL 对话 → 退出 → 重启恢复,续问上下文连续。

## 4. 验收记录(2026-11-11)

- workspace 全量测试通过;真实冒烟(glm-5.3-flash):
  会话 A 告知"幸运数字 42"→ 退出 → 重启 REPL 打印
  `[已恢复会话 task_01M7371N9S1YR92KE67BVRAYXB · 1 轮]`,
  续问"幸运数字是多少"模型答出 42,跨进程上下文连续。
- TUI:`c` 进入聊天屏同逻辑恢复转录与压缩史(账本重建)。
- 已知 Alpha 限制:单会话(恢复最近一个);助手中间轮的原始流式内容
  不回放(只回放 MessageCompleted 全文)。