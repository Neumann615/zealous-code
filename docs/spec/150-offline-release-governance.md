# spec 150:离线发布治理(v0.1.0 对外候选)

来源:2026-10-22 阶段规划。TUI 场景 4/8 人工目检闭环后进入发布治理;
远端策略为"暂不远端",因此本阶段只做离线发布治理。

## 目标

产出可对外分发的 v0.1.0 候选包:

- `dist/zealous-0.1.0-<host-triple>.zip`(仅含 `zealous.exe`);
- 分发状态标记:`OFFLINE EXTERNAL CANDIDATE, NOT PUBLISHED`。

所有验证必须在本地以真实命令完成;所有结论以验收记录、命令输出、
产物 SHA256 与 `git bundle verify` 结果为准,**不以提交信息为准**。

## 明确不做

- remote 推送;
- tag;
- 云端 CI;
- 公开上传 / 公开分发;
- 安装器;
- 自动更新。

## 溯源模型(三段,spec 150 修订版)

发布溯源固定为三段提交,避免"bundle 无法包含记录自身哈希的提交"
被误读为治理不一致:

1. **Runtime source commit**
   产品运行时代码,以及影响证据生成的脚本(如
   `scripts/package-release.ps1`、`scripts/tui-console-drive.py`)、
   对外文档(README)所在提交。zip 由该提交构建。
2. **RC record commit**
   相对 runtime source commit,只允许修改 RC 记录、验收记录与纯文档
   (`docs/` 下)。由打包脚本自动生成 RC 记录后提交。
3. **Bundle rotation commit**
   bundle 创建并 verify 通过后,最后提交 bundle 哈希与事故/轮换记录。
   **该提交不进入 bundle**(bundle 无法包含记录自身哈希的提交),
   必须在 RC 记录中明确说明这一限制。

## 发布门槛(默认打包模式全部硬性)

1. TUI 验收 8/8 PASS,且场景 1、4、7、8 证据方式为 MANUAL
   (场景 4/8 为 AUTOMATED 时默认打包必须失败;任一场景非 PASS 必须失败)。
2. 基础质量门:`cargo fmt --check`、`cargo test --all`、
   `cargo clippy --all-targets -- -D warnings`。
3. release build 通过。
4. release-mode mock E2E:`cargo test --release -p zealous-cli --test cli_e2e
   e2e_real_binary_plan_write_approve_complete`(验证 mock 闭环跑在实际
   发布形态二进制上)。
5. release-mode 真实模型 smoke:`cargo test --release -p zealous-cli
   --test cli_e2e real_model_smoke -- --ignored --nocapture`。
   说明:会消耗模型额度;每次打包只执行一次;验证对象是实际发布二进制。
6. zip 解压验证:解压到临时目录后
   - `zealous.exe --version` 输出精确为 `zealous 0.1.0`;
   - 假密钥环境变量下 `config check` 退出码 0、输出含 `[ok]`、
     不含假密钥;
   - 验证后删除临时目录。
7. RC 记录完整:zip SHA256、bundle SHA256、verify 结果、runtime commit、
   分发状态。

## 打包失败安全(硬性)

- 打包脚本**不得在任何 gate 通过之前删除正式候选包**;
- 新 zip 一律先写入 `dist/.rc-staging/`,全部 gate(含清单、解压验证)
  通过后才替换正式 zip;
- 替换前备份旧 zip 与旧 RC 记录;替换或 RC 记录写入失败时:
  恢复旧 zip、恢复旧 RC 记录、清理 staging、退出码 1;
- Gate 1 只允许清理 `.rc-staging` 残留。

## `-LocalTrial` 模式

保留为本地试用包通道:8/8 PASS 仍必需,但不要求场景 1/4/7/8 为 MANUAL,
RC 记录标记 `LOCAL TRIAL (NOT for external release)`,不得对外分发。

## 本地不可变备份

最终 RC record commit 后执行:

- `git bundle create dist/zealous-code-main-offline-rc-final.bundle main`;
- `git bundle verify` 通过后再提交 bundle rotation 记录(见溯源模型第 3 段);
- rotation 记录提交后删除旧 `zealous-code-main-offline-rc.bundle`;
- `zealous-code-archive-20261015.bundle` 保留;
- bundle SHA256 记入 RC 记录。仍不推 remote。

## 证据归档

`dist/evidence/v0.1.0/` 归档:

- TUI 截图与驱动脚本;
- Dogfooding 验收文档快照(`dogfooding-0.1.0.md`、`dogfooding-140.md`);
- spec 141 验收文档快照(`spec141-temp-artifact-hygiene.md`);
- 最终 RC 记录与 TUI 验收记录快照;
- `EVIDENCE-MANIFEST.md`:逐文件说明证据作用;明确 Dogfooding 原始
  JSON 结果因 dist 事故丢失不可确定性重建;声明 `docs/acceptance/`
  为权威验收记录;说明重生成 TUI 证据与原始证据的关系。

要求:

- `SHA256SUMS.txt` 使用相对 `dist/evidence/v0.1.0/` 的 POSIX 路径,
  不含 Windows 盘符;
- 归档后确认关键文件存在;
- 严禁把 `.env` 或任何真实 API key 放入证据归档。

## 产物验证清单

- zip 仅含 `zealous.exe`;
- 解压后 `--version` 正确、`config check` 不泄漏密钥;
- SHA256 写入 RC 记录;
- RC 记录中的 runtime commit 与实际构建源一致;
- 三段溯源清晰(runtime source / RC record / bundle rotation);
- bundle verify 通过;
- 包内无 `.env`、`.zealous/`、Git 元数据、`agent-assets/` 或任务数据。
