# 80 · 实用编码 Alpha

状态: 已定案 · 2026-10-16
目标: 从"能完成 smoke 任务"推进到"能处理真实小型代码任务"。
仍不做: TUI、桌面端、app-server、插件系统、OS sandbox。

## 1. 任务 Brief(结构化任务输入)

- CLI:`zealous task new <title> --file <relative-task-file>`。
- brief 文件必须位于当前 Git 仓库内(越界拒绝),内容整体作为任务简报。
- 新增事件 `TaskBriefAttached { brief }`;投影保存 brief。
- 运行与 resume 的规划 prompt 同时使用 title + brief;resume 摘要也含 brief。
- `task status` 显示 brief 摘要;`task events` 可审计完整 brief。

## 2. Git 只读工具(补齐 spec 30 承诺)

| 工具 | 参数 | 行为 |
| ---- | ---- | ---- |
| `git_log` | `{"limit":20, "path":"optional/path"}` | 默认 20 条,上限 100;可限定路径 |
| `git_diff` | `{"path":"optional/path","staged":false}` | 默认工作区 diff;输出上限 64 KiB,超限显式标记截断 |

两者均为 T0:只读、无网络、无配置写入。

## 3. 编辑工具 edit_file(T1)

```json
{"action":"tool","tool":"edit_file","args":{
  "path":"relative/path",
  "old_text":"exact existing text",
  "new_text":"replacement text",
  "replace_all":false
}}
```

- `old_text` 非空且必须与文件内容精确匹配;默认必须唯一匹配,
  `replace_all=true` 才允许多处替换;零匹配报错。
- `old_text == new_text` 拒绝;`new_text` 可为空串(删除片段)。
- 复用 apply 事务(spec 70 §4):原子替换、成功自动 commit
  (message 为 `zealous: edit <relative-path>`)、失败恢复文件与 index。
- 策略:已有文件的修改优先 edit_file;write_file 用于新建或整体重写。

## 4. 审批展示可审查性

- `write_file` 审批摘要:目标路径、新建或覆盖、完整新内容。
- `edit_file` 审批摘要:目标路径、exact old text、exact new text、匹配次数策略。
- 禁止用 `args_bytes` 之类的模糊量词代替可审查内容。

## 5. CLI 进度反馈

- `zealous task run <id> [--yes] [--no] [--quiet]`。
- 默认输出轻量进度:Agent/turn 开始、工具名与风险级、工具完成或拒绝、
  Worker 结束、最终状态。
- `--quiet` 只保留审批请求、错误与最终结果。
- 进度只来自编排器事实;Engine 通过注入的 progress sink 输出,
  engine 内不直接打印业务日志;CLI 不维护第二份任务状态。

## 验收

- 既有全部测试继续通过;新增:brief 越界拒绝与投影/prompt 断言、
  git_log/git_diff 参数与截断、edit_file 全部分支(唯一/多处/零匹配/
  空 old/相同 old+new/越界/自动 commit/失败回滚)、审批摘要内容、
  进度输出与 --quiet。
- 真实模型验收:修正后的 real_model_smoke 显式执行通过(不建 Worker、
  内容精确);后续真实小任务可依赖 brief + edit_file 完成。
