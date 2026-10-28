# spec 158:并行任务合并 Alpha

来源:2026-10-29 阶段规划。spec 157 已允许两个 isolated task 基于同一
base 并行执行,但第一个任务 apply 后,第二个任务会因主 HEAD 前进而无法
fast-forward。本 spec 提供显式、受控的三方合并路径。

## 目标

- 对因共享 base 前进而无法 fast-forward 的 Completed isolated task 执行显式三方合并;
- Git 可自动合并时生成 merge commit;
- 冲突时 fail-closed,自动 `merge --abort`,不留下 index 冲突;
- 保留任务分支与 worktree;
- 为并行任务链式集成提供基础。

## 非目标

不自动解决冲突 / 不自动改写任务分支 / 不做 rebase / 不做 3 个以上并发 /
不绕过主工作区写守卫 / 不做后台合并队列。

## `task merge`

CLI:

- `zealous task merge <task-id> [--yes]`

允许条件:

- Task 是 isolated mode;
- Task status = Completed;
- 存在 `ExecutionWorkspacePrepared`;
- 未 applied;
- 主仓库当前分支等于 `base_branch`;
- 主工作区无 UserChange;
- 任务分支包含原 `base_commit`;
- 主 HEAD 是 `base_commit` 的后代;
- 任务分支与主分支存在可合并变更。

执行:

- `git merge --no-ff zealous/<task-id>`;
- 无冲突 → 记录 merge commit 与事件;
- 冲突 → `git merge --abort`,记录 `workspace_merge` 错误;
- 不留下 index 冲突;
- 不修改任务 worktree;
- 不删除任务分支;
- 不自动选择冲突侧。

若 fast-forward 仍可用,merge 拒绝并提示使用 `task apply`。

## 事件与投影

新增成功事件:

- `ExecutionWorkspaceMerged { branch, base_commit, task_tip, merge_commit, conflicted: false }`

冲突不落成功事件,只落:

- `ErrorOccurred { scope: "workspace_merge", message: "merge conflict: <files>" }`

投影新增:

- `applied_strategy: Option<"fast_forward" | "three_way_merge">`
- `merge_commit: Option<String>`

## Parallel 集成

CLI 新增:

- `--integrate ff-only`(默认,保持 spec157 行为)
- `--integrate merge`

`merge` 策略:

1. 第一个 Completed 任务按输入顺序 fast-forward apply;
2. 后续 Completed 任务尝试 task merge;
3. 无冲突则合并成功;
4. 冲突则停止后续集成并保留 worktree。

## TUI / app-server

TUI 详情页:

- `P`:apply;
- `M`:merge;
- base 前进时提示可用 merge。

app-server 新增 `merge_task`,并推送 parallel merge 通知。所有入口复用 engine API;TUI 不直接执行 Git。

## 测试与 Dogfooding

测试覆盖:

- fast-forward 可用时 merge 拒绝;
- base 前进且无冲突时 merge 成功;
- dirty main / 非 Completed / direct / 分叉拒绝;
- 冲突自动 abort,index 干净,worktree 保留;
- projection / CLI / parallel / TUI / app-server。

真实 Dogfooding:

- DM1:两个独立文件并行后 A apply、B merge;
- DM2:同文件不同区域自动合并;
- DM3:同行真实冲突 fail-closed。
