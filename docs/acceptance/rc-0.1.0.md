# RC 0.1.0 Acceptance Record (spec 120 / spec 150)

## Traceability (all values measured at build time; three-stage model)

- Runtime source commit: 97e70b7005d70fc64bf79b7e48dab338d1101b05
- RC record commit: (filled after commit; diff vs runtime source must be
  acceptance/release docs only - spec 150 three-stage model)
- Bundle rotation commit: (created after the RC record commit; NOT part of
  the bundle; see bundle rotation log)
- Rustc release: 1.99.0 (measured via rustc -vV)
- Host triple: x86_64-pc-windows-gnu (measured via rustc -vV)
- Build time: 2026-10-22 09:55:18 UTC
- Target platform: Windows x64
- Distribution: LOCAL TRIAL (NOT for external release)

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
- Size: 2896371 bytes
- SHA256: CDA2EF906662C83ED7F7901CA860942A887E777379295B7AB10A567F400A98C1
- Manifest: zealous.exe (single entry)

## Security boundary

- V1 is NOT an OS sandbox.
- Shell is a T2 approval action.
- File access is limited to the repository (path guard).
- Model endpoint is the only default outbound network access.
