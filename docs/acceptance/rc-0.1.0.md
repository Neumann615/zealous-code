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
