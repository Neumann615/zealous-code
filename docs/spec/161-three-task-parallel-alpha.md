# spec 161:三任务并行与可恢复集成 Alpha

来源:2026-10-30 阶段规划。spec157-160 已完成双任务并行、三方合并、
冲突工作区与模型辅助解决。本 spec 将 Alpha 并发上限提升到 3,并提供
冲突停止后的剩余任务恢复集成。

## 目标

- 最多同时运行 3 个 isolated task;
- 三个任务基于同一主仓库 base commit;
- 按输入顺序串行集成;
- 第一个 Completed 任务可 fast-forward apply;
- 后续任务使用三方 merge;
- 冲突保留 merge workspace 后停止后续集成;
- 用户解决后可继续集成剩余任务;
- 保持单全局 run lease;
- 不做 4 个以上并发。

## 非目标

不无上限并发 / 不后台 daemon / 不定时调度 / 不自动三方冲突决策 /
不改变 direct task 默认行为。

## 命令

### parallel run

```text
zealous parallel run <a> <b> <c>   --max 3 --auto-apply --integrate merge --on-conflict workspace
```

- Alpha 上限固定为 3;
- `--max` 允许 1..3;
- 默认 max 保持 2(兼容既有行为);
- 显式 IDs 数量不得超过 max;
- 显式 IDs 与 --all 互斥;
- --all 取前 N 个 runnable;
- 全部任务 isolated;
- 启动前全量校验,任一非法则不执行任何任务。

### parallel integrate

```text
zealous parallel integrate <task-id>...   --integrate merge --on-conflict workspace
```

用于冲突停止后继续集成剩余 Completed tasks。不重新运行模型,只执行
apply / merge / merge-workspace,全程持有全局 run lease。

## 执行模型

1. 获取 parallel lease;
2. 全量校验;
3. 读取 base branch / commit;
4. 为全部任务在同一 base 上创建 worktree;
5. 任一 prepare 失败则不启动模型,已创建合法 worktree 保留,lease 释放;
6. 最多 3 个 Orchestrator 并发;
7. 等待全部终态;
8. Failed / Interrupted 不集成,但不阻塞后续 Completed;
9. Completed 按输入顺序 apply / merge;
10. 冲突后停止后续集成,后续 worktree 保留。

## 集成顺序

输入 A/B/C:

- 第一个 Completed 若主 HEAD 仍为共享 base,则 ff apply;
- 后续 Completed 三方 merge;
- Failed / Interrupted 跳过;
- B 冲突时 C 停止,待 B merge-continue 后可
  `parallel integrate C`。

## 冲突策略

沿用:

```text
--on-conflict abort
--on-conflict workspace
```

workspace 模式保留 spec159/160 冲突解决工作区;模型辅助解决仍必须用户
审批,批准后仍不自动 commit。

## 可恢复集成

`parallel integrate` 校验:

- isolated mode;
- Task Completed;
- 未已 applied/merged;
- 主工作区无 UserChange;
- 原 workspace/branch 可验证;
- 全局 lease 可获取。

禁止自动恢复 Interrupted、自动 rerun model、自动 resolve conflict、
自动 commit merge workspace。

## TUI / app-server

TUI Parallel status 显示 base、最多三个 active tasks、integration order、
conflict pending 与 remaining。新增 `Shift+I` 执行恢复集成。

app-server 扩展 run_parallel max=3,新增:

```text
parallel_integrate
get_parallel_integrations
```

通知覆盖 integration started / apply / merge / integrated /
conflict pending / stopped / finished。

## 验收

测试覆盖上限选择、三 worktree 同 base、三 ledger、审批路由、失败跳过、
集成顺序、中间冲突停止、恢复集成、CLI/TUI/app-server。Dogfooding 覆盖
三独立任务、同文件不同区域、中间冲突恢复、一失败两成功。
