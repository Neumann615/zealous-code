# spec 156:串行任务队列 Alpha

来源:2026-10-22 阶段规划。Worktree 隔离(spec 155)解决了单任务
不污染主工作区;本 spec 让多个 isolated task 按顺序串行执行并逐个
fast-forward 回主分支,为并行任务与自动合并打基础。

## 目标

- 多个 Queued task 按指定顺序串行执行;
- 每个任务强制 isolated worktree;
- 当前任务 Completed 后应用回主分支,再准备下一个;
- 全程单并发(全局 run lease);
- 队列状态可观察(queue status);
- 支持中断与恢复语义(不自动续跑)。

## 非目标

多任务并行 / 后台常驻 daemon / 定时任务 / 优先级调度 /
自动三方合并 / direct mode 任务入队。

## CLI

- `zealous queue run <task-id>... [--auto-apply] [--yes] [--no] [--quiet]`
- `zealous queue run --all [...同上]`
- `zealous queue status`

规则:

- 显式 task IDs 与 --all 互斥;显式模式按输入顺序;
- --all 按 TaskId 时间升序取所有"可运行任务";
- 只执行 Queued 任务;已有 direct 运行历史或 isolated workspace 的
  任务不进入队列(避免误接旧状态);
- 队列任务全部强制 isolated;
- 显式模式启动前全量校验:任一任务不存在 / 非 Queued / 有 direct
  历史 / 有 isolated workspace / Provider 配置无效 → 启动失败,
  不执行任何任务;
- --all 无可运行任务 → 输出 `(no runnable queued tasks)`,退出码 0。

## 执行流程(每任务)

1. 校验全局 run lease 可获取(队列整体持有,全程不释放);
2. 打开任务 ledger;3. 校验仍为 Queued;
4. 在当前主仓库 HEAD 上创建 isolated worktree;
5. 启动模型编排;6. 等待审批或终态;
7. Completed → auto-apply 时直接 ff-only apply;默认进入交互确认;
8. apply 成功 → 落账 → 下一个任务基于新主 HEAD 建 worktree;
9. Failed / Interrupted → 队列停止,后续任务保持 Queued,
   当前 worktree 保留;
10. apply 被拒/失败 → 队列停止,后续保持 Queued,当前 worktree 保留。

`--yes/--no` 只影响模型工具审批,不跳过 apply 确认
(`--auto-apply` 除外)。

## 中断语义

Ctrl-C → 当前任务按既有中断语义落账(Interrupted)→ 队列停止 →
后续任务保持 Queued → 当前 worktree 保留。Interrupted 任务不自动
恢复;用户可显式 `task run <id>` 恢复,其余 Queued 任务可重新入队。

## 全局 Run Lease

`<repo-root>/.zealous/run.lock`,所有执行入口共用
(CLI task run / queue run / TUI runner / app-server run_task 与
run_queue):

- OS advisory lock(fs4),非阻塞获取,失败即 already_running;
- 进程退出/崩溃自动释放;lock 文件可残留,以 OS lock 判断持有;
- metadata 写入 PID / task_id / mode / started_at;
- 获取与释放使用 Drop;不轮询等待;
- queue status 可读取 metadata 展示当前运行项。

## queue status

不新增全局事实源、不建 queue ledger。由各任务投影 + run.lock
metadata + .zealous/worktrees/ 状态派生:

- active:task_id / status / mode / workspace branch;
- runnable queue(--all 顺序);
- not runnable:task + 原因(非 Queued / direct history /
  isolated workspace / ledger corrupt / provider invalid);
- 队列运行结束汇总只输出终端,不落全局状态。

## app-server

- `run_queue { task_ids | all, auto_apply }` → 立即 queue_started,
  后台 worker 串行执行;单 session 仅一个 queue worker;
  全局 lease 生效;
- 通知:queue_started / queue_task_started / queue_task_finished /
  queue_apply_finished / queue_finished / queue_error;
- `stop_queue`:中断当前任务并停止队列;
- `get_queue_status`:active / pending / index / total /
  last completed / last failed / apply policy。

## TUI

- 列表页 `A`:确认后运行全部可运行 queued tasks(隔离执行,
  完成后 fast-forward apply);
- 列表页 `Q`:queue status view(当前任务 / pending / completed /
  failed / apply 状态);
- 详情页 I / P 保留;TUI 不直接操作 Git,复用 engine API。

## 事件语义

不新增队列事件类型:单任务事实由各自账本表达;队列顺序是运行时
输入;队列状态从投影与 lease 派生,避免第二份事实源。
