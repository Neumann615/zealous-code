# spec 159:冲突解决工作区 Alpha

来源:2026-10-29 阶段规划。spec 158 已支持显式三方合并,但真实冲突只能
在主工作区尝试后 abort。本 spec 将冲突现场移入独立 merge workspace,
让用户手动查看、解决并 stage,再受控集成回主分支。

## 目标

- 冲突不在主工作区解决;
- 冲突现场保留在独立 merge workspace;
- 用户可查看 conflict 文件;
- 用户手动解决并 stage 后可继续合并;
- 主分支只接收已解决并校验通过的 merge commit;
- 不自动选择 ours/theirs;
- 不自动改写任务分支。

## 非目标

不自动解决冲突 / 不让模型自动修改冲突文件 / 不做 rebase /
不做 3 个以上并发任务 / 不改变 spec158 默认 fail-closed 行为。

## 布局

- Path:`<repo-root>/.zealous/merges/<task-id>/`
- Branch:`zealous-merge/<task-id>`

规则:

- merge workspace 基于当前主分支 HEAD 创建;
- merge 在该 workspace 中执行;
- 主工作区始终保持干净;
- 原 isolated task worktree 不修改;
- 原任务分支 `zealous/<task-id>` 不改写;
- 冲突保留在 merge workspace 中。

## CLI

### `zealous task merge-workspace <task-id>`

1. 校验 isolated Completed task 与分支关系;
2. 创建 `zealous-merge/<task-id>` worktree;
3. 在 merge workspace 执行 `git merge --no-ff zealous/<task-id>`;
4. 无冲突 → 完成 merge commit → 主分支 ff-only 到 merge commit →
   落账集成事件 → 删除临时 workspace;
5. 冲突 → 不 abort → 保留 conflict 状态 → 输出文件列表与后续命令。

### `zealous task merge-status <task-id>`

输出 status / workspace / merge branch / main head / task branch /
task tip / conflicted files。

### `zealous task merge-continue <task-id> [--yes]`

条件:

- workspace 存在且处于 conflicted merge;
- `git ls-files -u` 为空(用户已解决并 stage);
- 主仓库仍在原 main_head;
- 主工作区无 UserChange。

执行 `git commit --no-edit`,随后主仓库
`git merge --ff-only zealous-merge/<task-id>`。成功后落账
`ExecutionWorkspaceMerged`,清理 merge workspace,保留原 task worktree。

### `zealous task merge-abort <task-id> [--force]`

执行 `git merge --abort`,删除 merge workspace 与 merge 分支。
若 workspace 有用户额外未 staged / untracked 修改,默认拒绝;`--force`
才允许清理。

## 事件与投影

新增:

- `MergeWorkspacePrepared { branch, path, main_head, task_branch, task_tip }`
- `MergeConflictPending { files }`
- `MergeWorkspaceResolved { merge_commit }`
- `MergeWorkspaceAborted`

成功集成主分支继续复用 `ExecutionWorkspaceMerged`。

投影新增:

- `merge_workspace: Option<MergeWorkspaceView>`
- `merge_conflicts: Vec<String>`

视图字段:

- branch / path / main_head / task_branch / task_tip / status /
  merge_commit

状态:

- `conflicted`
- `resolved`
- `integrated`
- `aborted`

状态只来自事件投影;CLI/TUI/app-server 不自行推导第二份业务状态。

## Parallel 集成

新增:

```text
--on-conflict abort      # 默认,spec158 行为
--on-conflict workspace  # 冲突时保留 merge workspace
```

workspace 模式下,第一个任务 fast-forward;后续任务无冲突则自动 merge;
冲突则主工作区保持干净,创建 merge workspace,保留现场并停止后续集成。
不调用模型解决冲突。

## TUI / app-server

TUI 新增:

- `Shift+M`:创建 merge workspace;
- `Shift+S`:查看 merge status;
- `C`:merge continue;
- `A`:merge abort。

app-server 新增:

- `start_merge_workspace`
- `get_merge_status`
- `continue_merge_workspace`
- `abort_merge_workspace`

通知携带 task id / merge branch / main head / task tip / files;
unresolved files 非空时返回结构化错误。

## 验收

单元 / 集成覆盖启动、状态、continue、abort、parallel workspace、
TUI 与 app-server。真实 Dogfooding 覆盖:

- DM1:独立文件自动合并;
- DM2:同文件不同区域自动合并;
- DM3:同区域冲突 → 手动解决 → continue;
- DM4:冲突 → abort。
