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
   分发状态;RC 记录提交与 runtime source commit 的差异仅限文档。

## `-LocalTrial` 模式

保留为本地试用包通道:8/8 PASS 仍必需,但不要求场景 1/4/7/8 为 MANUAL,
RC 记录标记 `LOCAL TRIAL (NOT for external release)`,不得对外分发。

## 本地不可变备份

最终 RC 记录提交后执行:

- `git bundle create dist/zealous-code-main-offline-rc.bundle main`;
- `git bundle verify` 通过后删除旧 `zealous-code-main-spec141.bundle`;
- `zealous-code-archive-20261015.bundle` 保留;
- bundle SHA256 记入 RC 记录。仍不推 remote。

## 证据归档

`dist/evidence/v0.1.0/` 归档 TUI 截图、Dogfooding 结果、mock 驱动脚本、
spec 141 结果、RC 记录副本:

- 归档前计算 SHA256,归档后确认关键文件存在;
- 不删除开发者尚未确认的截图;
- 严禁把 `.env` 或任何真实 API key 放入证据归档。

## 产物验证清单

- zip 仅含 `zealous.exe`;
- 解压后 `--version` 正确、`config check` 不泄漏密钥;
- SHA256 写入 RC 记录;
- RC 记录中的 runtime commit 与实际构建源一致;
- bundle verify 通过;
- 包内无 `.env`、`.zealous/`、Git 元数据、`agent-assets/` 或任务数据。
