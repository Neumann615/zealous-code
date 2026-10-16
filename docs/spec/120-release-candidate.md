# 120 · 本地发布候选(RC)

状态: 已定案 · 2026-10-17
目标: 产出可分发的本地 RC 包(Windows x64),并固化打包前的强制校验。
仍不做: remote 推送、tag、CI、公开分发、安装器。

## 1. 版本与平台

- 版本:workspace `0.1.0`(不递增)。
- 平台:Windows x64(MSVC/GNU 均可,以实际构建工具链为准)。
- 构建命令:`cargo build --release`。
- 产物:`target/release/zealous.exe`。

## 2. 打包前强制校验(任一失败即中止)

按顺序执行,全部通过才允许打包:

1. `git status --porcelain` 为空(工作区干净);
2. `cargo fmt --check` / `cargo test --all` / `cargo clippy --all-targets -- -D warnings` 通过;
3. release 二进制 `--version` 输出正确;
4. `config check` 全 ok 且不回显密钥;
5. mock 模型 E2E(cli_e2e)通过;
6. real_model_smoke 显式执行通过(消耗额度,发布候选前一次)。

## 3. 包内容

`dist/zealous-0.1.0-x86_64-pc-windows-msvc.zip`,**仅含**:

- `zealous.exe`

明确排除:`.env`、API key、`.zealous/`、Git 元数据、`agent-assets`、
任务数据、任何测试产物。

打包脚本:`scripts/package-release.ps1`;每步校验失败即中止并保留现场。

## 4. 发布前安全检查

- 用假密钥跑真实二进制 E2E,断言 stdout/stderr/事件账本/包内容四处
  均不泄漏;
- 检查 zip 文件清单仅含预期产物;
- real_model_smoke 保持默认 ignored,显式执行才消耗额度。

## 5. 已声明安全边界(V1,随包说明)

- 不是 OS sandbox;
- Shell 为 T2 审批动作;
- 文件访问限制在仓库内(路径守卫);
- 模型 endpoint 是唯一默认外发网络访问。

## 6. TUI 手工验收前置

打包前 `docs/acceptance/tui-alpha.md` 的 8 个场景必须全部"通过";
任一失败先修复;未执行视为未通过。夹具脚本:
`scripts/tui-acceptance-fixture.ps1`。
