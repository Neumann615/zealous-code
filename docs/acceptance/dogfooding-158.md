# Dogfooding 158 验收记录(spec 158:并行任务合并 Alpha)

- 执行日期:2026-10-29
- 二进制:`target/debug/zealous.exe`(当前 runtime source 工作区构建)
- 模型:智谱 `glm-5.3-flash`
- 临时仓库:`%TEMP%\zealous-dogfooding-158-*`
- 质量门:fmt / test --all / clippy 全部通过
- 单元 / 集成:workspace merge 3/3、parallel merge 1/1、core projection 1/1、CLI parallel merge E2E 1/1、serve merge_task E2E 1/1 通过

## DM1 独立文件并行合并 —— PASS

- 仓库:`%TEMP%\zealous-dogfooding-158-dm1-8c275368`
- base:`a7eeb3ecabc46f11b47f7dce920667c1f0a50a83`
- 任务:
  - `task_01M61Z9KTM1DR006W4RCJEWA71` 修改 `docs/A.md`
  - `task_01M61Z9KV4DXMMQ0W7EH38ZD9V` 修改 `src/B.rs`
- 命令:`parallel run <A> <B> --auto-apply --integrate merge --yes`
- 结果:
  - 两任务 Completed;
  - A fast-forward 成功至
    `ee0b5fd6ce4f92a30a3263f633fdd256724726fb`;
  - B 无冲突三方合并成功,merge commit:
    `66b0a2d34d1af71b00fe229b678748c87be8d9f5`;
  - B task tip:
    `13d97ce63d0a11231e458cf2a82c91365e12dfb5`;
  - 最终同时包含 `# A merged` 与 `pub const B: u8 = 2;`;
  - B workspace 状态 applied,strategy=`three_way_merge`;
  - 无未审批写入或敏感信息泄漏。

## DM2 同文件不同区域 —— PASS

有效验收仓库:`%TEMP%\zealous-dogfooding-158-dm2c-3f0f4caa`
(包含足够未变上下文行):

- base:`b91d656320edd089ce342d8030ae13e867561f2e`
- 任务:
  - `task_01M61ZDP6RVKFPS17MZV51KDNM`:`top=0 → top=A`
  - `task_01M61ZDP7CZGNDG79J1EDNSMVB`:`bottom=0 → bottom=B`
- 结果:
  - 两任务 Completed;
  - A fast-forward 至
    `4655af7520904e8f54a8870cffac33d36838bbf6`;
  - B 三方合并成功,merge commit:
    `b36632a2ef64cb4533ddb2e9cdd94640d52e2a67`;
  - 最终文件同时包含 `top=A` 与 `bottom=B`;
  - `git status --porcelain` 为空。

附加强验证:DM2 前两轮使用 2 行文件,两个变更行相邻,Git 合并上下文将其视为同一 hunk 并返回冲突。系统按 DM3 语义 fail-closed、abort 后 index 干净,不计产品缺陷。

## DM3 同一行真实冲突 —— PASS

- 仓库:`%TEMP%\zealous-dogfooding-158-dm3-59962822`
- base:`c0f2ea264e1da349cec2e15e7ea95fbc1443b429`
- 任务:
  - `task_01M61ZF1X6RYEBPTARB8BQ5736`:`value=0 → value=A`
  - `task_01M61ZF1Y3P3J9CE94YBY3JRCW`:`value=0 → value=B`
- 结果:
  - 两任务 Completed;
  - A fast-forward 至
    `c50afeb209931bb03d53bb49586aff8bdcab2ed8`;
  - B merge 返回 `merge conflict: shared.md`;
  - 自动 `merge --abort` 后主工作区 status 干净;
  - 主工作区保留 `value=A`,未留下冲突标记;
  - B worktree / branch 保留;
  - B ledger 记录 `workspace_merge` 错误,未落成功 Merged 事件。

## LOCAL TRIAL release 回归 —— PASS

- RC zip SHA256:`1CCAE69DD256E1DF945D748B2EB7CC6982AC94CC8D1649EAFA145E4D1FAC7580`
- 解压二进制:`%TEMP%\zealous-158-localtrial-8f1a8d60\bin\zealous.exe`
- 仓库:`%TEMP%\zealous-158-localtrial-8f1a8d60`
- base:`ef8840d3ac9ca424b6416fdb8dae437b28c5c2df`
- 任务:
  - `task_01M61ZPNT1T2SV7HBBC9P2EXNE` 修改 `docs/A.md`
  - `task_01M61ZPNTGSD5JT80FJJDEK919` 修改 `src/B.rs`
- 结果:
  - 两任务 Completed;
  - 第一个任务 fast-forward;
  - 第二个任务三方合并成功,merge commit:
    `22653a6a3d4ca41435a8fe9b78cbe34e143b60e0`;
  - 主分支同时包含两边修改,工作区干净;
  - 第二个任务 workspace strategy=`three_way_merge`;
  - 无敏感信息泄漏。

## 账本审计

对 DM1 / DM2 / DM3 全部任务账本检查:

- seq 连续;
- 无中间损坏行;
- pending approvals 均为 0;
- 无 `Authorization` / `Bearer` 形态凭据落账;
- 冲突任务保留 `ErrorOccurred(scope=workspace_merge)`;
- 成功合并任务包含 `ExecutionWorkspaceMerged` 且投影为
  `applied_strategy=ThreeWayMerge`。

## 结论

DM1 / DM2 / DM3 全部 PASS。无 P0 / P1。冲突处理保持 fail-closed,
不自动选择侧,不自动 rebase,不自动解决冲突。

## 阶段末本地 bundle(spec 158)

| File | Size | SHA256 |
| ---- | ---- | ------ |
| zealous-code-main-parallelmerge158.bundle | 439251 | D677C72E05E2D12F151607D924DCA3BBC260FA731BE35D0D58C0E2058C24A64E |

- `git bundle verify` 通过,包含 main 至 RC record commit
  `4f0640af51adc77be757b56c0d4de140ff0f72a9`;
- 本提交为 bundle rotation commit,按 spec 150 三段溯源模型不进入 bundle;
- verify 通过后删除旧 `zealous-code-main-parallel157.bundle`
  (`F27F4770439…534906`,为新 bundle 历史前缀);
- archive bundle 保留;OFFLINE RC lock 已刷新;仍不自动推送 remote。
