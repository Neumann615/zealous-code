# RC 0.1.0 Acceptance Record (spec 120)

- Git commit: 587cf4a6674b14ad02e831a8269fc74ca56f64a8
- Build time: 2026-10-17 10:31:01 UTC
- Toolchain: stable-x86_64-pc-windows-gnu (rustc 1.99.0)
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

- Path: dist/zealous-0.1.0-x86_64-pc-windows-msvc.zip
- Size: 2071544 bytes
- SHA256: 069C60FDE13D0D3583057D166CB7F7001D5826E37ADAC03E1BBED925EBBA3BAA
- Manifest: zealous.exe (single entry)

## Security boundary

- V1 is NOT an OS sandbox.
- Shell is a T2 approval action.
- File access is limited to the repository (path guard).
- Model endpoint is the only default outbound network access.
