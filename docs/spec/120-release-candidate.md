# 120 · 本地发布候选(RC)

状态: 已定案 · 2026-10-17
目标: 产出可分发的本地 RC 包(Windows x64),并固化打包前的强制校验。
仍不做: remote 推送、tag、CI、公开分发、安装器。

## 1. 版本与平台

- 版本:workspace `0.1.0`(不递增)。
- 平台:Windows x64;**工具链 triple 以 `rustc -vV` 实测 host 为准**,
  zip 命名动态生成 `dist/zealous-<version>-<host>.zip`,禁止硬编码
  MSVC 或 GNU(命名误导分发对象即为溯源缺陷)。
- 构建命令:`cargo build --release`。
- 产物:`target/release/zealous.exe`。

## 2. 打包前强制校验(任一失败即中止)

按顺序执行,全部通过才允许打包:

1. `git status --porcelain` 为空(工作区干净);
2. `cargo fmt --check` / `cargo test --all` / `cargo clippy --all-targets -- -D warnings` 通过;
3. release 二进制 `--version` 输出正确(动态实测,不得硬编码);
4. `config check` 全 ok 且不回显密钥;
5. mock 模型 E2E(cli_e2e)通过;
6. real_model_smoke 显式执行通过(消耗额度,发布候选前一次)。

## 3. 包内容

`dist/zealous-0.1.0-<host-triple>.zip`(host 由 `rustc -vV` 动态解析),
**仅含**:

- `zealous.exe`

明确排除:`.env`、API key、`.zealous/`、Git 元数据、`agent-assets`、
任务数据、任何测试产物。

打包脚本:`scripts/package-release.ps1`;每步校验失败即中止并保留现场。

## 3.5 RC 溯源(记录必须为动态实测值)

RC 记录(`docs/acceptance/rc-<version>.md`)必须记录:

- **runtime source commit**:构建二进制时的 git HEAD(打包脚本运行时读取);
- **RC record commit**:承载记录文件的提交(记录提交后由工作流核对);
- **实际 rustc version 与 host triple**(`rustc -vV` 实测,禁止硬编码);
- zip SHA256 与清单。

约束:RC 记录提交后,`git diff <runtime source commit>..HEAD --name-only`
的差异**只允许**是 RC 记录本身;若运行时代码变化,必须重新打包。
RC 记录中的工具链字段若与实测不符,即为溯源缺陷,不得分发。

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

打包前 `docs/acceptance/tui-alpha.md` 的 8 个场景必须全部 PASS;
NOT_RUN 与 FAIL 均阻塞打包(打包脚本 gate 3 解析记录,不通过即拒绝)。
夹具脚本: `scripts/tui-acceptance-fixture.ps1`。

Gate 3 必须同时解析**状态**列与**方式**列:

- **对外 RC(默认)**:场景 1、4、7、8 必须为 `PASS | MANUAL`,
  场景 2、3、5、6 可为 AUTOMATED;任一不满足即拒绝打包
  (自动化证据不得替代手工证据)。
- **本地试用 RC(`-LocalTrial` 开关)**:8/8 PASS 即可,不要求 MANUAL;
  RC 记录必须标注 `Distribution: LOCAL TRIAL (NOT for external release)`,
  控制台输出同样显著告警。
