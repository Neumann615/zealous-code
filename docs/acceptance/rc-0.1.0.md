# RC 0.1.0 Acceptance Record (spec 120)

## Traceability (all values measured at build time)

- Runtime source commit: 448d0bc39844cf9d7986c2158044984fe09d56af
- RC record commit: (filled after commit; diff vs runtime source must be this file only)
- Contents vs previous RC zip: spec 141 工具副产物治理(be2f206)+ serve
  already_running 修复(a57dc71);二进制按 host triple 命名规则打包
- Rustc release: 1.99.0 (measured via rustc -vV)
- Host triple: x86_64-pc-windows-gnu (measured via rustc -vV)
- Build time: 2026-10-20 02:58:01 UTC
- Target platform: Windows x64
- Distribution: LOCAL TRIAL (NOT for external release)

## Quality gates (all PASS)

| Gate | Command | Result |
| ---- | ------- | ------ |
| fmt | cargo fmt --check | PASS |
| test | cargo test --all | PASS |
| clippy | cargo clippy --all-targets -- -D warnings | PASS |
| real_model_smoke | cargo test --test cli_e2e real_model_smoke -- --ignored | PASS |
| release build | cargo build --release | PASS |
| --version | zealous.exe --version | PASS |
| config check | zealous.exe config check (fake key) | PASS |

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
| zealous-code-main-extended-dogfooding.bundle | 274594 | 9B783C984BE958FB543A66E58609DE07BE192DDC4AEEF2B1FA8BD7193572E41D |
| zealous-code-archive-20261015.bundle | 40298742 | AADEBE3095B7F96E3CAA5D9B5D18E3CE924E3687F7C0DE5F7E93C0221A9A827C |

bundle verify:均记录 complete history(extended-dogfooding bundle 含 main
全部提交至扩展 dogfooding 阶段末;archive bundle 含归档分支,未改动)。
备份产物只存放 dist/,不进入 Git。

### Bundle rotation log

- 2026-10-20(扩展 dogfooding 阶段末):新增
  `zealous-code-main-extended-dogfooding.bundle`(SHA256
  9B783C984BE958FB543A66E58609DE07BE192DDC4AEEF2B1FA8BD7193572E41D,
  verify complete history 通过)后,删除过期
  `zealous-code-main-0.1.0-rc.bundle`(旧 SHA256
  EAA359DDD28785A6116BF0AC20E8E52C56973D16E89D5E3099C1EA744076B253,
  含 main 至 RC 0.1.0;其内容为新 bundle 的历史前缀,冗余已消除)。
  archive bundle 未触碰。RC zip 不重建、不外发(分布状态见 tui-alpha:
  场景 4/8 补 MANUAL 前为 LOCAL TRIAL)。
- 2026-10-20(spec 141 阶段末):RC zip 按 `-LocalTrial` 重建(含 spec 141
  与 a57dc71,SHA256 见 Artifact 节);main bundle 轮换至
  `zealous-code-main-spec141.bundle`,verify 通过后删除
  `zealous-code-main-extended-dogfooding.bundle`(旧 SHA256
  9B783C984BE958FB543A66E58609DE07BE192DDC4AEEF2B1FA8BD7193572E41D,
  为新 bundle 的历史前缀)。archive bundle 未触碰。
  分布状态:**LOCAL TRIAL (NOT for external release)**,场景 4/8 补
  MANUAL 前不外发。
