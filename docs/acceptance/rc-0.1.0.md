# RC 0.1.0 Acceptance Record (spec 120)

## Traceability (all values measured at build time)

- Runtime source commit: 66cad6f63f77adc886ff1b92b23b7709a335f32a
- RC record commit: 3e9426a0663f2246a31762cac011dfb46970a514 (记录首落点;本字段更新为最后一笔修正)
- Rustc release: 1.99.0 (measured via rustc -vV)
- Host triple: x86_64-pc-windows-gnu (measured via rustc -vV)
- Build time: 2026-10-17 10:55:47 UTC
- Target platform: Windows x64

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
- Size: 2070132 bytes
- SHA256: 8D5FE5914E75CBAF4F68F874E481C4043BFB044A72BC544536DABFA676C50052
- Manifest: zealous.exe (single entry)

## Security boundary

- V1 is NOT an OS sandbox.
- Shell is a T2 approval action.
- File access is limited to the repository (path guard).
- Model endpoint is the only default outbound network access.

## Local immutable backup (dist/, gitignored)

| File | Size | SHA256 |
| ---- | ---- | ------ |
| zealous-0.1.0-x86_64-pc-windows-gnu.zip | 2070132 | 8D5FE5914E75CBAF4F68F874E481C4043BFB044A72BC544536DABFA676C50052 |
| zealous-code-main-0.1.0-rc.bundle | 243412 | EAA359DDD28785A6116BF0AC20E8E52C56973D16E89D5E3099C1EA744076B253 |
| zealous-code-archive-20261015.bundle | 40298742 | AADEBE3095B7F96E3CAA5D9B5D18E3CE924E3687F7C0DE5F7E93C0221A9A827C |

bundle verify:两者均记录 complete history(main bundle 含 main 分支全部提交;
archive bundle 含归档分支)。备份产物只存放 dist/,不进入 Git。
