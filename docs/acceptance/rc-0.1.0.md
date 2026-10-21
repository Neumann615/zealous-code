# RC 0.1.0 Acceptance Record (spec 120 / spec 150)

## Traceability (all values measured at build time)

- Runtime source commit: 7a06c219f3647782752e76ebc114618d0e8822fa
- RC record commit: (filled after commit; diff vs runtime source must be this file only)
- Rustc release: 1.99.0 (measured via rustc -vV)
- Host triple: x86_64-pc-windows-gnu (measured via rustc -vV)
- Build time: 2026-10-22 01:14:45 UTC
- Target platform: Windows x64
- Distribution: OFFLINE EXTERNAL CANDIDATE, NOT PUBLISHED
- Contents vs previous RC zip: 运行时代码与 448d0bc 完全一致
  (其后仅 acceptance/README/spec 文档提交);确定性构建下 SHA256 与上一
  LOCAL TRIAL zip 相同(46622450…750EDD46),属预期,非复制产物。

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
