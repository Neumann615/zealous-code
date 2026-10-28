# Dogfooding 157 验收记录(spec 157:并行任务 Alpha)

- 执行日期:2026-10-29
- 二进制:`target/debug/zealous.exe`(当前 runtime source 工作区构建)
- 模型:智谱 `glm-5.3-flash`
- 临时仓库:`%TEMP%\zealous-dogfooding-157-*`
- 质量门:fmt / test --all / clippy 全部通过(见最终实现提交后的复跑记录)
- 单元 / 集成:engine parallel 4/4、CLI parallel E2E 1/1、serve parallel E2E 1/1、TUI parallel 相关单测通过

## PDF1 两个独立真实任务 —— PASS

- 仓库:`%TEMP%\zealous-dogfooding-157-pdf1b-57c4ef89`
- base:`a4446e98fae29845962165e1f08730dcfcadcede`
- 任务:
  - `task_01M61WS4KRTQC9X51X23VGDKTE`:修改 `docs/A.md` → `# A done`
  - `task_01M61WS4MAHQK0YMYVM09WJMBH`:修改 `src/b.ts` → `b = 2`
- 命令:`parallel run <A> <B> --auto-apply --yes`
- 结果:
  - 两个任务均 Completed;
  - 并发审批请求均显示 task id + call id,并按到达顺序处理;
  - A fast-forward 成功,主 HEAD →
    `7b234c93d68832a9db3efe51f55775bebd320eb4`;
  - B apply 因共享 base 已前进被拒绝;
  - B worktree 保留,主工作区 `src/b.ts` 保持原值;
  - 无未审批写入、无敏感信息泄漏。

附加验证:首轮将 console log 写入测试仓库导致 untracked UserChange,
两个 apply 均被守卫拒绝且队列停止。该行为符合 fail-closed,不计产品缺陷;
重新将日志放到仓库外后完整通过。

## PDF2 一失败一成功 —— PASS

- 仓库:`%TEMP%\zealous-dogfooding-157-pdf2-0d5aeb43`
- base:`700a23c12e3063ee36c4f693e71e3786b32f2742`
- 端点:本地 mock/forward 代理;A 命中标题后返回 HTTP 500,
  B 转发到真实智谱 endpoint。真实 API key 不进入命令输出、账本或本记录。
- 任务:
  - `task_01M61WWC3BZEAZA8S1FS14EFQS`:Provider mock 500 → Failed
  - `task_01M61WWC3TKGWB3JH92JE6637J`:真实模型修改 `ok.txt` → Completed
- 结果:
  - A 最终错误为单层
    `provider_exhausted after 4 attempts: http 500:`;
  - B 完成并 fast-forward,主 HEAD →
    `cd9a9b67d8dfcfea4013ff0eee5d37a54d05c94c`;
  - A worktree 保留;
  - `ok.txt` = `real-done`;
  - Failed 任务未被 apply,Completed 任务 apply 成功。

## PDF3 同文件冲突 —— PASS

- 仓库:`%TEMP%\zealous-dogfooding-157-pdf3-4bda2f82`
- base:`184a042266e80b40c37687e8c2e6a252b3091d4c`
- 任务:
  - `task_01M61WXKNN3NZNTQFQSFYSJ1Q9`:`shared.md` `value=0` → `value=A`
  - `task_01M61WXKP45CR6FB6PA9NQ4SFX`:`shared.md` `value=0` → `value=B`
- 结果:
  - 两任务均基于同一 base 且均 Completed;
  - A fast-forward 成功,主 HEAD →
    `6e32295c8109c85006f6f46a34a760977445672d`;
  - B apply 拒绝:主 HEAD 已前进,需手动 merge/rebase;
  - 未执行自动三方合并;
  - B worktree / branch 保留,branch tip =
    `4263bb5a0427b3d6c23d40073e7e6b55c3ba80bb`;
  - 主工作区最终 `value=A`。

## LOCAL TRIAL release 回归 —— PASS

- RC zip SHA256:`4D785E1A8B50D4A229C5C76E22C034EB72C43F7BCCFFF0CD46471AE994B1E5B7`
- 解压二进制:`%TEMP%\zealous-157-localtrial-c32a74b8\bin\zealous.exe`
- 仓库:`%TEMP%\zealous-157-localtrial-c32a74b8`
- base:`8c44714b035f799ee62e1427f2eab868a7950ec7`
- 任务:
  - `task_01M61X8DYWXMZSXYMTD6R78J8D` 修改 `left.md`
  - `task_01M61X8DZANK17974N330YVV27` 修改 `right.md`
- 结果:
  - 两任务均 Completed;
  - 输入顺序第一任务 fast-forward 成功,主 HEAD →
    `e2c392fe33a5306013f31fd1711e2e6ee99de2bd`;
  - 第二任务因共享 base 前进被拒绝,worktree 保留;
  - 审批摘要包含 task id + call id;
  - 无敏感信息泄漏。

## 账本审计

对 PDF1/PDF2/PDF3 全部任务账本检查:

- seq 均连续;
- 无中间损坏行;
- 无 `Authorization` / `Bearer` 形态的凭据落账;
- pending approvals 均为 0;
- Task 状态与 CLI `task status` 一致;
- workspace 状态与保留策略一致。

## 结论

PDF1 / PDF2 / PDF3 全部 PASS。无 P0 / P1。Alpha 的确定性行为是:
第二个并行任务在第一个 apply 后不会自动 rebase / 三方合并,而是拒绝并保留
worktree,交由用户手动处理。
