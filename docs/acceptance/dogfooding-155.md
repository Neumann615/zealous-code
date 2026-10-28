# Dogfooding 155 验收记录(spec 155:Worktree 隔离 Alpha)

- 执行日期:2026-10-22
- 二进制:target/debug/zealous.exe(b274636 构建;CLI/TUI/serve 全量接入)
- 模型:智谱 glm-5.3-flash
- 临时仓库:%TEMP%\zealous-dogfooding-155\

## 结果:DF1-DF3 全部 PASS,0 P0 / 0 P1

### DF1 主工作区 WIP 保护 —— PASS

- 主工作区 wip.txt 为用户未提交改动;isolated 任务修改 status.txt;
- 任务 Completed(1 次审批,edit_file);worktree 内 status.txt
  WAIT→DONE,**主工作区 status.txt 保持 WAIT、wip.txt 原样**;
- 模型未访问主工作区;apply --yes 被守卫以新中性文案拒绝
  ("工作区存在用户未提交改动…写入已拒绝");
- 附加验证:**误跑 direct 后再 --isolate 被正确拒绝**
  ("任务已有 direct 运行历史,禁止切换为 isolated 模式"),
  重建任务后 isolated 运行正常。

### DF2 正常 apply —— PASS

- 干净主仓库 isolated 运行 → Completed;
- `task apply --yes`:预览展示 base/branch/commits/变更文件
  (M status.txt)→ ff-only 成功;
- 主工作区 status.txt WAIT→DONE,仅获得任务分支的 rollback commit;
- apply 事件落账。

### DF3 主分支前进冲突 —— PASS

- 任务 Completed 后主分支追加 commit("main advanced");
- apply --yes 被拒:"主分支已前进(HEAD 08c167a ≠ base 62425513),
  请手动 merge/rebase";
- 未自动合并;worktree 与任务分支完整保留(status ready,
  branch_tip 不变)。

## 账本与模式规则

- 每任务账本在主仓库 .zealous/tasks,seq 连续;worktree 内无账本;
- direct 历史禁切 isolated(实际误跑验证,拒绝正确);
- isolated 任务 resume 自动继续 worktree(集成测试覆盖);
- 无敏感信息泄漏。

## 质量门与集成

- fmt / test --all / clippy 全绿(每次实现提交后);
- 集成测试 workspace_isolation.rs 2/2
  (CLI 全流 + serve 全流,均含 apply 与 seq/泄漏断言)。

## 阶段末本地 bundle(spec 155)

| File | Size | SHA256 |
| ---- | ---- | ------ |
| zealous-code-main-worktree155.bundle | 381724 | 094D3878BDBB7025EA4F3AFD4E3CC6EB986F03BFA27F144F693392893F11607A |

- verify complete history 通过,含 main 至 66d9e61(RC record commit);
- verify 通过后删除旧 zealous-code-main-spec154.bundle
  (2FE45EA7…23E403C,为新 bundle 历史前缀);
- 本提交为 bundle rotation commit,不进入 bundle(spec 150 既定限制);
- offline RC lock 已刷新至新候选包;archive bundle 保留;仍不推 remote。
## DF1 三任务真实串行队列(spec 156 补验收)—— PASS

- 执行日期:2026-10-29;
- 二进制:OFFLINE RC zip 解压结果 `%TEMP%\zealous-dogfooding-156-df1c-4d46afe7\bin\zealous.exe`(SHA256 与 RC 156 一致:`706C0A4A04C3345AEC063E7E987D34B505A08CC272402CCABE7CBAE23B171E7A`);
- 模型:智谱 `glm-5.3-flash`;
- 临时仓库:`%TEMP%\zealous-dogfooding-156-df1c-4d46afe7`;
- base commit:`57517e70ac59611637d13b15f357dab956b4f557`;
- 命令:`zealous queue run <task-1> <task-2> <task-3> --auto-apply --yes`。

### 任务与结果

| # | Task | 目标 | 状态 | apply 后主 HEAD |
| --- | --- | --- | --- | --- |
| 1 | `task_01M61V572QTENQNS56FFM0AHGJ` | `version.txt`: `0.1.0` → `0.2.0` | Completed | `df15483aa2cc0ad9e5bd918408cc2f08e177d427` |
| 2 | `task_01M61V57347DB1YH2GNK6DMBD0` | 基于 `0.2.0` 更新 `README.md` | Completed | `bebbe3fa2fcc83606275c3d6e85a88ff01b3c3a8` |
| 3 | `task_01M61V573JPRH4X1VJYK8P9WAM` | 新增并验证 `verify.ps1` | Completed | `3017cc64d272902c19a9b3e16808dd338a82795c` |

- 队列输出 `applied 3 / completed 3`,退出码 0;
- 三个任务均自动创建 isolated worktree 并基于上一步 apply 后的主 HEAD;
- 主分支最终包含三个任务 rollback/apply commits,内容分别为 `version=0.2.0`、README `Version 0.2.0`、`verify.ps1`;
- T1/T2 均有审批记录;`verify.ps1` 的 shell 调用被记录为 T2 审批动作;
- 每任务 pending approvals = 0,状态均为 Completed;
- 无路径越界、未审批写入或敏感信息泄漏。

### 附加强验证

- 首次准备仓库时 brief 文件保持 untracked,Task 1 完成后 apply 被 UserChange 守卫拒绝,队列停止且后续任务保持 Queued——fail-closed 正确;
- 该轮只是测试数据准备问题,不计产品缺陷。brief 纳入初始提交后,全新三任务队列完整通过。

## DF2 失败停止(spec 156 队列)—— PASS

- mock ok3_then_500:任务 1 三次请求 OK(Completed+apply);
  任务 2 的请求 4-6 全部 500 → provider_exhausted → **Failed**;
- 队列停止(exit 1);任务 3 保持 Queued;任务 2 worktree 保留;
- 主分支仅包含任务 1 的 rollback commit;
- 已在集成测试 workspace_isolation.rs 完整断言(确定性 mock)。

## DF3 主分支前进冲突 —— PASS

- 集成测试与 engine 单测覆盖:base HEAD 变更后 apply 被拒
  (BaseBranchMoved / DirtyWorktree / NotOnBaseBranch);
- Dogfooding 155 DF3 已验证(同机制)。

## 阶段末本地 bundle(spec 156)

| File | Size | SHA256 |
| ---- | ---- | ------ |
| (待旋转后填入) | | |

- 新 OFFLINE EXTERNAL CANDIDATE 见 RC 记录。
## 阶段末本地 bundle(spec 156)

| File | Size | SHA256 |
| ---- | ---- | ------ |
| zealous-code-main-spec156.bundle | 404338 | 6D31AA149DEEDCE4311866B4DCE8C9A42FD75BA62B87862C1D8108090BFCD623 |

- verify complete history 通过,含 main 至 7505693(RC record commit);
- verify 通过后删除旧 zealous-code-main-worktree155.bundle
  (094D3878…11607A,为新 bundle 历史前缀);
- 本提交为 bundle rotation commit,不进入 bundle(spec 150 既定限制);
- offline RC lock 已刷新;archive bundle 保留;仍不推 remote。