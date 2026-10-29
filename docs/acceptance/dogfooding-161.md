# Dogfooding 161 验收记录(spec 161:三任务并行与可恢复集成 Alpha)

- 执行日期:2026-10-30
- 二进制:`target/debug/zealous.exe`
- 模型:智谱 `glm-5.3-flash`;DT4 的失败任务经本地 mock/forward 代理注入 HTTP 500
- 临时仓库:`%TEMP%\zealous-dogfooding-161-*`
- 质量门:fmt / test --all / clippy 全部通过
- 单元 / 集成:engine parallel 8/8、三任务顺序集成 1/1、中间冲突恢复 1/1、
  CLI 三任务冲突后 integrate E2E 1/1 通过

## DT1 三独立真实任务 —— PASS

- 仓库:`%TEMP%\zealous-dogfooding-161-dt1-9d77c4da`
- base:`53dd68ac443eb882e69275489d314f933c8279c3`
- 任务:
  - `task_01M649HYKYY8T6FHA95BY5PRDA` 修改 `docs/A.md`
  - `task_01M649HYMKAR3HPE5Y5TPWQNEQ` 修改 `src/B.rs`
  - `task_01M649HYN5SRAYQ997V1XN4GDP` 修改 `tests/C.txt`
- 命令:
  `parallel run <A> <B> <C> --max 3 --auto-apply --integrate merge --on-conflict workspace --yes`
- 结果:
  - 三任务同时 Running,均 Completed;
  - A fast-forward 至
    `51a7b9996c286b417307dc34c70b55a3d4d929f0`;
  - B 三方 merge 至
    `dbdcfb12ec3615014ad05ea558328b14781e4822`;
  - C 三方 merge 至
    `3fdf58cfe12d1470f4feffe3616a9eb8300b9ee6`;
  - 主分支包含三组变更,工作区干净;
  - merge history 两个 merge commit,顺序清晰。

## DT2 三任务同文件不同区域 —— PASS

- 仓库:`%TEMP%\zealous-dogfooding-161-dt2-32cf8b3f`
- base:`d2beb5a39039838e34ac588ff53b1d1cccd3ddba`
- 三任务分别修改:
  - `top=0 → top=A`
  - `middle=0 → middle=B`
  - `bottom=0 → bottom=C`
- 结果:
  - A apply;
  - B 三方 merge;
  - C 三方 merge;
  - 最终文件同时包含 `top=A`、`middle=B`、`bottom=C`;
  - 主工作区干净;
  - 两个 merge commit 保留。

## DT3 中间任务冲突并恢复集成 —— PASS

有效仓库:`%TEMP%\zealous-dogfooding-161-dt3b-abb88b39`
(首轮 DT3 实际为 C 冲突,行为正确;为精确覆盖“中间 B 冲突”重跑此仓库):

- base:`fe29f366ba916d937e5584b380267474fe371666`
- 任务:
  - A:`value=0 → value=A`
  - B:`value=0 → value=B`
  - C:`value=0 → value=C`
- 运行结果:
  - A Completed 并 fast-forward 至
    `c155438c90dd3b1a05b85758cba4471bebd55418`;
  - B merge conflict 保留 workspace;
  - C 停止并保留 task worktree;
  - 主工作区 porcelain 为空。
- B 恢复:
  - spec160 resolver 对 scalar 冲突返回 manual;
  - 用户写入 `value=B-USER-RESOLVED` 并 stage;
  - merge-continue 成功,merge commit:
    `05b9a325ebbd628305a10b8bb5902bfd68a3911c`。
- C 恢复:
  - `parallel integrate C --integrate merge --on-conflict workspace`;
  - C 与 B 的用户解决结果冲突,workspace 保留;
  - 用户写入 `value=FINAL-RESOLVED` 并 stage;
  - merge-continue 成功,最终 merge commit:
    `d3e737f2316dfdc343bb3b6202ab8e23149e3d82`。
- 最终:
  - 主工作区干净;
  - conflict.md 为 `value=FINAL-RESOLVED`;
  - 两个 merge commit 保留;
  - 原 task worktrees 保留。

## DT4 一失败两成功 —— PASS

- 仓库:`%TEMP%\zealous-dogfooding-161-dt4-525c25c2`
- base:`c2effd5d80fff7d907752e49e9ef5f799403d95f`
- 任务:
  - A:`dt4 provider fail`,本地代理注入 HTTP 500;
  - B:修改 `src/B.rs`;
  - C:修改 `tests/C.txt`。
- 结果:
  - A Failed,错误为单层
    `provider_exhausted after 4 attempts: http 500:`;
  - B Completed 并 fast-forward 至
    `c53f06dcb9ea0112e60da34a1d0ebe930f81147b`;
  - C Completed 并三方 merge 至
    `8fc6787e024b55f6e91b50e86c68d56673989af8`;
  - A worktree 保留;
  - B/C 集成不受 A 失败阻塞;
  - 主工作区干净。

## LOCAL TRIAL release 回归 —— PASS

- RC zip SHA256:`52120E388B4161D5AF7E181AC472A9A2D6C24250C1E4ED870B4C953542EC5027`
- 解压二进制:`%TEMP%\zealous-161-localtrial-e6a60d3d\bin\zealous.exe`
- 仓库:`%TEMP%\zealous-161-localtrial-e6a60d3d`
- base:`d5cffc5f46f5c2c02ab88f3d7904b775d4182b04`
- 三任务分别修改 `docs/A.md`、`src/B.rs`、`tests/C.txt`
- 结果:
  - 三任务 Completed;
  - A fast-forward 至
    `1b33de644f79a5c112b2f676214f389b9ca43357`;
  - B merge 至
    `345968fa2cc1c1aeeec847f1e58737e32bfd7e06`;
  - C merge 至
    `7bfe7b9b982c68d86748fd0e331f998ed005b92f`;
  - 主工作区干净,三组变更全部保留。

## 账本审计

对 DT1 / DT2 / DT3(含首轮与有效重跑)/ DT4 全部任务账本检查:

- seq 连续;
- 无中间损坏行;
- pending approvals 为 0(终态任务);
- 无 `Authorization` / `Bearer` 形态凭据落账;
- Failed / Completed / workspace 状态一致;
- 三任务 ledger 相互独立。

## 结论

DT1 / DT2 / DT3 / DT4 全部 PASS。无 P0 / P1。
Failed task 不阻塞 Completed tasks 的顺序集成;
冲突后后续任务停止并保留,可通过 `parallel integrate` 恢复。
