# spec 155:Worktree 隔离 Alpha

来源:2026-10-22 阶段规划。Dogfooding 151-154 的核心残留摩擦是
主工作区 WIP 与任务写入的冲突(A1 场景守卫反复拦截)。本 spec 引入
**opt-in 的任务专属 git worktree 隔离**,为任务队列、多任务并行与
自动合并打基础。

## 核心原则

- Worktree 隔离是 **opt-in**,默认 direct 执行方式完全不变;
- 主仓库事件账本仍是唯一事实源,worktree 不保存任务事件账本;
- 所有 workspace 状态必须来自事件投影,前端不得自行推导;
- **不做自动三方合并**(apply 仅 fast-forward);
- 不做多任务并行(单并发限制保留);
- 不做 OS sandbox。

## CLI 运行模式

- 现有默认:`zealous task run <task-id>`(direct,行为不变);
- 新增:`zealous task run <task-id> --isolate`。

模式规则(执行模式落账,禁止依赖 CLI 本地记忆):

- 首次 `--isolate` 创建任务专属 worktree 并落账;
- 任务进入 isolated mode 后,resume 自动继续使用该 worktree;
- 已有 direct 运行历史的任务禁止切换为 isolated;
- 已有 isolated workspace 的任务禁止退回 direct 模式执行。

## Worktree 布局与命名

- 路径:`<repo-root>/.zealous/worktrees/<task-id>/`;
- 分支:`zealous/<task-id>`;
- 均由 TaskId 派生,模型不可自定义;
- `.zealous/` 保持 ignored;主账本仍在
  `<repo-root>/.zealous/tasks/<task-id>/events.jsonl`。

## Workspace 准备流程(首次 isolated run)

1. `base_branch = git branch --show-current`(空 = detached HEAD,拒绝);
2. `base_commit = git rev-parse HEAD`;
3. `git worktree add -b zealous/<task-id> <path> <base_commit>`;
4. 落账 `ExecutionWorkspacePrepared { kind, branch, path, base_branch,
   base_commit }`。

主工作区存在用户 WIP:**不阻塞** isolated run;WIP 原样保留;
模型 prompt 明确说明当前运行于 isolated worktree(基于 base_commit)、
主工作区未提交改动不可见、不得访问或修改主工作区。

## Resume 流程

已有 ExecutionWorkspacePrepared 时:

1. 校验 worktree 路径存在;2. 校验是 git worktree;3. 校验当前分支为
`zealous/<task-id>`;4. 校验分支历史包含 base_commit;5. 校验 worktree
无用户未提交改动(RuntimeArtifact 不阻塞);6. 继续执行。

特殊恢复:

- worktree 缺失但分支存在:重新 `git worktree add <path> <branch>`;
- worktree 与分支均缺失:**拒绝恢复**,不自动重建(避免丢失历史
  执行结果)。

## 事件与投影扩展

新增事件:`ExecutionWorkspacePrepared` / `ExecutionWorkspaceApplied` /
`ExecutionWorkspaceRemoved`。

TaskProjection 新增:`execution_workspace: Option<ExecutionWorkspaceView>`、
`applied_commit: Option<String>`、`workspace_removed: bool`。
ExecutionWorkspaceView 含 kind / branch / path / base_branch /
base_commit / current_commit / status。

CLI、TUI、app-server 的 workspace 状态必须来自投影。

## 工具执行边界(isolated run)

- ToolExecutor.repo_root 指向任务 worktree;
- 事件账本仍指向主仓库任务 ledger;
- 路径守卫以 worktree 为根;
- write_file / edit_file 在任务分支上自动 commit;
- shell cwd 是 worktree;
- Worker proposal 保存在主仓库任务目录,不进入任务分支;
- 模型不获得 worktree 管理能力(不能 apply / remove / 切分支)。

## 应用回主工作区

新增 `zealous task apply <task-id>`(--yes 支持)。

允许条件(全部满足):isolated mode;Task Completed;存在
ExecutionWorkspacePrepared;无已应用记录;主仓库当前分支 = base_branch;
主仓库 HEAD = base_commit;主工作区无 UserChange;任务分支是
base_commit 的后代。

执行 `git merge --ff-only zealous/<task-id>`,成功落账
`ExecutionWorkspaceApplied { branch, applied_commit }`。

拒绝场景:主分支前进(提示手动 merge/rebase)/主工作区 UserChange /
分支不匹配 / 未 Completed / 分叉 / 重复 apply(返回已应用状态,幂等)。

CLI 交互:展示 base commit、任务分支 tip、将进入主分支的 commits、
变更文件列表,请求确认;`--yes` 跳过。**不执行三方合并。**

## Workspace 查看与清理

- `zealous task workspace <task-id>`:输出 mode / path / branch /
  base_branch / base_commit / current_commit / status / applied_commit;
- `zealous task workspace-remove <task-id> [--force]`:默认仅允许删除
  已 applied 或分支 tip 已包含于 base branch 的 workspace;
  `git worktree remove` + `git branch -d`;落账
  `ExecutionWorkspaceRemoved`;worktree 有用户未提交改动时默认拒绝,
  `--force` 才允许且必须显示风险确认。

## TUI

详情页新增键位:`I` isolated run/resume;`P` apply;`W` 查看 workspace
状态。展示 workspace branch / path / base commit / current commit /
apply 状态。isolated run、apply、workspace remove 均有确认弹窗。
TUI 不直接执行 Git 命令,复用 engine API。

## app-server

- run_task 参数扩展 `isolate: true`;
- 新增命令 `apply_task` / `get_workspace` / `remove_workspace`;
- capability 增加 `isolated_worktree: true`;协议版本保持 1;
- 单并发限制保留。
