# Dogfooding 159 验收记录(spec 159:冲突解决工作区 Alpha)

- 执行日期:2026-10-29
- 二进制:`target/debug/zealous.exe`(当前 runtime source 工作区构建)
- 模型:智谱 `glm-5.3-flash`
- 临时仓库:`%TEMP%\zealous-dogfooding-159-*`
- 质量门:fmt / test --all / clippy 全部通过
- 单元 / 集成:merge workspace 5/5、parallel workspace conflict 1/1、
  projection lifecycle 1/1、CLI conflict-continue E2E 1/1、
  serve conflict-continue E2E 1/1 通过

## DM1 独立文件自动合并 —— PASS

- 仓库:`%TEMP%\zealous-dogfooding-159-dmw1-2b605806`
- base:`46f6a0277726c8284aeb6e6ab968dd7ac2edcfdc`
- 任务:
  - `task_01M622F93TWQJ4CSFXMFNEZX20` 修改 `docs/A.md`
  - `task_01M622F94A318WA8NDPSW8FFWW` 修改 `src/B.rs`
- 流程:
  1. 两个任务并行 Completed;
  2. 第一个任务 fast-forward;
  3. 第二个任务执行 `task merge-workspace`;
  4. Git 无冲突,自动完成三方合并并集成。
- 结果:
  - merge commit:`f54dd402653b6d49cdc1f7d7ae1e55c28f168df2`;
  - task tip:`162cb5da2b5aabad73478b314351f0c015cf91a5`;
  - 主分支同时包含 `# A workspace` 与 `pub const B: u8 = 2;`;
  - merge workspace 已清理;
  - 原 task worktree 保留;
  - workspace strategy=`three_way_merge`,merge workspace status=`integrated`;
  - 主工作区干净。

## DM2 同文件不同区域 —— PASS

- 仓库:`%TEMP%\zealous-dogfooding-159-dmw2-e901aa16`
- base:`19dd2cec9135c104f6d0076a8305abd2a3ab2be6`
- 任务:
  - `task_01M622G9Y77CPPVTK2FCMHAPXF`:`top=0 → top=A`
  - `task_01M622G9YWCQCGTKTRWVWVWW57`:`bottom=0 → bottom=B`
- 结果:
  - 第一个任务 fast-forward;
  - 第二个任务 `merge-workspace` 自动合并;
  - merge commit:`809aa773fc8a2686be9d1b0b546ed908c7d4a239`;
  - 最终内容包含 `top=A`、三行未变上下文、`bottom=B`;
  - merge workspace 清理;
  - 原 task worktree 保留;
  - 主工作区干净。

## DM3 同区域冲突 → 手动解决 → continue —— PASS

- 仓库:`%TEMP%\zealous-dogfooding-159-dmw3-2959989c`
- base:`3c0359e20feb3a4fc38dabc25335e12cba5f8f03`
- 任务:
  - `task_01M622H605H4K9FA43XX5486B3`:`value=0 → value=A`
  - `task_01M622H60TV3SGVYM57VYGT4Y3`:`value=0 → value=B`
- 流程:
  1. 第一个任务 fast-forward 到
     `eb6f6fee7c87478d152ebc8c21d531a611ac6cb5`;
  2. 第二个任务 `merge-workspace` 进入 conflicted;
  3. conflict 文件:`shared.md`;
  4. `merge-status` 显示 unresolved `shared.md`;
  5. 主工作区 porcelain 为空;
  6. 用户在 merge workspace 写入 `value=USER-RESOLVED` 并 `git add`;
  7. `merge-continue --yes` 成功。
- 结果:
  - merge commit:`acfb6af08d09db4675c198ed995b8ef66760faf7`;
  - 主分支内容为 `value=USER-RESOLVED`;
  - merge workspace 清理;
  - 原 task worktree 保留;
  - strategy=`three_way_merge`,merge workspace status=`integrated`;
  - 主工作区干净。

## DM4 冲突中止 —— PASS

- 仓库:`%TEMP%\zealous-dogfooding-159-dmw4-f703a61b`
- base:`aded28054c11d9040d396de451c62b84d0edc27c`
- 任务:
  - `task_01M622J9B7A3A294J22X534JFE`:`value=0 → value=A`
  - `task_01M622J9BQCARZXN21YP24Z2SS`:`value=0 → value=B`
- 流程:
  1. 第一个任务 fast-forward 到
     `6defabf9063f27ad6e693f85a450173d399446c0`;
  2. 第二个任务 `merge-workspace` 进入 conflicted;
  3. 执行 `task merge-abort`。
- 结果:
  - 主 HEAD 不变;
  - 主工作区干净且内容保持第一个任务的 `value=A`;
  - merge workspace 清理;
  - merge workspace status=`aborted`;
  - 原 task worktree 保留;
  - 任务分支 tip `9012db509fda162afffb7a3fa471d21688e3370a` 保留。

## 账本审计

对 DM1-DM4 全部任务账本检查:

- seq 连续;
- 无中间损坏行;
- pending approvals 均为 0;
- 无 `Authorization` / `Bearer` 形态凭据落账;
- Prepared / ConflictPending / Resolved / Aborted / Merged 事件与最终状态一致。

## 结论

DM1 / DM2 / DM3 / DM4 全部 PASS。无 P0 / P1。
Zealous 不自动选择 ours/theirs,不自动修改冲突文件,不改写任务分支;
所有冲突解决均由用户在独立 merge workspace 手动完成并 stage。
