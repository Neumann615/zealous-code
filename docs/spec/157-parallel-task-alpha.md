# spec 157:并行任务 Alpha

来源:2026-10-29 阶段规划。spec 155 已解决单任务不污染主工作区,spec 156 已解决串行队列;本 spec 在保持事实源与安全边界不变的前提下,允许最多两个 isolated task 同时执行,并为后续冲突处理积累基础。

## 目标

- 最多同时运行 2 个 isolated task;
- 全局仅持有一个 parallel batch run lease;
- 所有并行任务基于同一个主仓库 base commit;
- 全部任务到达终态后,按输入顺序尝试 fast-forward apply;
- 并发审批请求携带 task id 并精确路由到对应 task + call;
- 支持中断、失败与 apply 冲突的可观察语义。

## 非目标

3 个以上并发 / 后台 daemon / 定时调度 / direct mode 并行 / 自动三方合并 / 自动 rebase / 改变既有 task run 与 queue run 默认行为。

## CLI

- `zealous parallel run <task-id>... [--auto-apply] [--yes] [--no] [--quiet]`
- `zealous parallel run --all [--max 2] [--auto-apply] [...]`
- `zealous parallel status`

规则:

- `--max` 默认 2,Alpha 上限 2,超过直接拒绝;
- 显式 IDs 与 `--all` 互斥;
- 显式模式任一任务非法则启动失败,不执行任何任务;
- `--all` 按 TaskId 时间升序取前 N 个可运行任务;
- 只接受 Queued、无任何 AgentRun、无 ExecutionWorkspacePrepared 的任务;
- `--yes/--no` 只影响模型工具审批,不跳过 apply 确认;
- apply 顺序固定为输入顺序。

## Run lease

复用全局 `.zealous/run.lock`:

- parallel batch 全程仅获取一个 lease;
- metadata `mode=parallel`,`task_id=parallel`;
- metadata 需记录全部参与 task ids 与 active task ids;
- task run / queue run / TUI runner / serve run_task / run_queue / run_parallel 全部共用该 lease;
- 获取失败返回 `already_running`;
- 不引入 per-task lease。

## 执行模型

启动前:

1. 获取 parallel lease;
2. 全量校验 runnable;
3. 记录主仓库 base_branch / base_commit;
4. 为每个任务在同一 base_commit 上创建 isolated worktree;
5. 任一 prepare 失败则不启动模型执行,并保留已创建的合法 worktree。

运行中:

- 最多两个 Orchestrator 线程同时执行;
- 每个任务独立 ledger、worktree、approval routing 与 progress;
- shell cwd 是该任务自己的 worktree;
- Worker proposal 仍在主仓库任务目录;
- 任一任务失败或中断不立即终止另一任务,允许其到达终态。

终态后:

1. 等待全部任务终态;
2. 汇总 Completed / Failed / Interrupted;
3. 仅按输入顺序对 Completed 任务执行 apply;
4. Failed / Interrupted 不 apply,worktree 保留;
5. apply 被拒绝或失败时停止后续 apply,已 apply 状态保留。

## Apply 语义

并行任务共享同一 base_commit,因此 apply 严格串行:

1. 第一个任务:主 HEAD 仍等于 base 时 fast-forward;
2. 第二个任务:主 HEAD 已前进,base 不再等于当前 HEAD,直接拒绝并提示手动 merge/rebase。

不做自动 rebase 或三方合并。这是 Alpha 的显式边界。

## 中断语义

- Ctrl-C / stop_parallel 对所有运行中任务设置 abort;
- 各任务按既有语义落账为 Interrupted;
- 不自动 apply;
- worktree 保留,lease 释放;
- 不自动恢复。

## 审批

并发请求按到达顺序逐个展示,展示必须包含 task_id、call_id、tool、risk 与 full summary。决策必须绑定 task_id + call_id;错误绑定、重复决策、EOF 均拒绝,不得影响其他任务。TUI 弹窗与 app-server notification 同样携带 task_id。

## app-server

新增 `run_parallel`、`stop_parallel`、`get_parallel_status`。

`run_parallel` 立即返回 `parallel_started`,后台 worker 执行;单 session 仅一个 parallel worker;全局 lease 仍生效。通知包括 `parallel_started`、`parallel_task_started`、`parallel_task_finished`、`parallel_apply_started`、`parallel_apply_finished`、`parallel_finished`、`parallel_error`。

`get_parallel_status` 返回 active / pending / completed / failed / interrupted、apply policy、base commit 与 apply index。shutdown 等待 parallel worker 结束。

## TUI

列表页 `Shift+A` 并行运行可运行 queued tasks,`Shift+Q` 打开 parallel status view。Parallel view 显示 base commit、active tasks、各任务 workspace branch、pending apply order 与 apply policy。审批弹窗显示 task id。TUI 不直接执行 Git,复用 engine API。

## 事件语义

不新增并行事件类型。单任务事实仍在各自 ledger;parallel 顺序与运行时状态由 run lease metadata + 任务投影派生,不创建第二份事实源。
