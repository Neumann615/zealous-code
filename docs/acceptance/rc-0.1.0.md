# RC 0.1.0 Acceptance Record (spec 120 / spec 150)

## Traceability (all values measured at build time; three-stage model)

- Runtime source commit: 942a390c60694afe80051e1ff5cbaf9ad7564152
- RC record commit: (filled after commit; diff vs runtime source must be
  acceptance/release docs only - spec 150 three-stage model)
- Bundle rotation commit: (created after the RC record commit; NOT part of
  the bundle; see bundle rotation log)
- Rustc release: 1.99.0 (measured via rustc -vV)
- Host triple: x86_64-pc-windows-gnu (measured via rustc -vV)
- Build time: 2026-10-22 01:48:21 UTC
- Target platform: Windows x64
- Distribution: OFFLINE EXTERNAL CANDIDATE, NOT PUBLISHED
- Contents vs previous RC zip: 产品运行时代码无变化(README/spec/
  打包脚本修正不进入二进制);确定性构建下 SHA256 与此前一致
  (46622450…750EDD46),属预期。本次打包首次走失败安全流程
  (staging + 事务式发布)。

## Quality gates (all PASS)

| Gate | Command | Result |
| ---- | ------- | ------ |
| fmt | cargo fmt --check | PASS |
| test | cargo test --all | PASS |
| clippy | cargo clippy --all-targets -- -D warnings | PASS |
| release build | cargo build --release | PASS |
| mock E2E (release binary) | cargo test --release -p zealous-cli --test cli_e2e e2e_real_binary_plan_write_approve_complete | PASS |
| real_model_smoke (release binary) | cargo test --release -p zealous-cli --test cli_e2e real_model_smoke -- --ignored | PASS |
| --version | zealous.exe --version | PASS |
| config check | zealous.exe config check (fake key) | PASS |
| zip extraction verify | extracted zealous.exe --version + config check (fake key) | PASS |
| TUI acceptance | docs/acceptance/tui-alpha.md: 8/8 PASS, scenarios 1/4/7/8 MANUAL | PASS |

## Artifact

- Path: dist/zealous-0.1.0-x86_64-pc-windows-gnu.zip
- Size: 2077328 bytes
- SHA256: 466224505C9D5061ABAAFBF27EBCD7ED90E22E40FF89397643624975750EDD46
- Manifest: zealous.exe (single entry)

## Security boundary

- V1 is NOT an OS sandbox.
- Shell is a T2 approval action.
- File access is limited to the repository (path guard).
- Model endpoint is the only default outbound network access.

## Local immutable backup (dist/, gitignored)

| File | Size | SHA256 |
| ---- | ---- | ------ |
| zealous-0.1.0-x86_64-pc-windows-gnu.zip | 2077328 | 466224505C9D5061ABAAFBF27EBCD7ED90E22E40FF89397643624975750EDD46 |
| zealous-code-main-offline-rc-final.bundle | 312059 | 81877D9DE70417816C75798F045BCAA67974265DD5E6BEFC36E3BFEA428D105E |
| zealous-code-archive-20261015.bundle | 40298742 | AADEBE3095B7F96E3CAA5D9B5D18E3CE924E3687F7C0DE5F7E93C0221A9A827C |

bundle verify:offline-rc-final bundle 记录 complete history,含 main 至
682fa4c(RC record commit);**其后的 bundle rotation commit(本节所在
提交)不进入 bundle**——bundle 无法包含记录自身哈希的提交,此为
spec 150 三段溯源模型的既定限制,非治理缺口。
archive bundle 未改动。备份产物只存放 dist/,不进入 Git;仍不推 remote。

### Bundle rotation log

- 2026-10-22(治理缺口修正阶段末):README brief 修正、spec 150
  三段溯源与失败安全打包修订(942a390)后,以失败安全流程重新打包
  (zip SHA256 不变);RC record commit 682fa4c 后创建最终
  `zealous-code-main-offline-rc-final.bundle`(SHA256 81877D9D…428D105E,
  verify complete history 通过,含 main 至 682fa4c),verify 通过后删除
  旧 `zealous-code-main-offline-rc.bundle`(SHA256
  5B980EEA…34E1147B,含 main 至 1b7a4d9,为新 bundle 历史前缀)。
  archive bundle 未触碰。
- 证据归档最终化:dist/evidence/v0.1.0/ 补齐 dogfooding-0.1.0 /
  dogfooding-140 / spec141 验收快照与 EVIDENCE-MANIFEST.md(含 Dogfooding
  原始 JSON 丢失声明与 docs/acceptance 权威声明);SHA256SUMS.txt 使用
  相对 POSIX 路径,21 项全部核验通过;归档无 .env / 真实密钥。