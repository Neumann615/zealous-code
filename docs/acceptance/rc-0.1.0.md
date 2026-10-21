# RC 0.1.0 Acceptance Record (spec 120 / spec 150)

## Traceability (all values measured at build time)

- Runtime source commit: d7866fc596fe07cf58561c974cf82388d20b88a2
- RC record commit: (filled after commit; diff vs runtime source must be this file only)
- Rustc release: 1.99.0 (measured via rustc -vV)
- Host triple: x86_64-pc-windows-gnu (measured via rustc -vV)
- Build time: 2026-10-22 01:21:13 UTC
- Target platform: Windows x64
- Distribution: OFFLINE EXTERNAL CANDIDATE, NOT PUBLISHED
- Contents vs previous RC zip: 运行时代码与 448d0bc 完全一致
  (其后仅文档提交);确定性构建下 SHA256 与此前各次打包一致
  (46622450…750EDD46),属预期。

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
| zealous-code-main-offline-rc.bundle | 306572 | 5B980EEA8B5E6BC4E03CAE416AA2977CAFAC88CBF691B585350A97AE34E1147B |
| zealous-code-archive-20261015.bundle | 40298742 | AADEBE3095B7F96E3CAA5D9B5D18E3CE924E3687F7C0DE5F7E93C0221A9A827C |

bundle verify:offline-rc bundle 记录 complete history,含 main 至
1b7a4d9(tui 证据路径更新提交;其后仅本哈希轮换记录提交);archive bundle 未改动。
备份产物只存放 dist/,不进入 Git;仍不推 remote。

### Bundle rotation log

- 2026-10-22(spec 150 离线发布治理阶段末):RC zip 以默认模式重建
  (OFFLINE EXTERNAL CANDIDATE,见 Artifact 节);main bundle 轮换至
  zealous-code-main-offline-rc.bundle(SHA256 5B980EEA…34E1147B,
  verify complete history 通过,含 main 至 1b7a4d9)。archive bundle
  未触碰。旧 zealous-code-main-spec141.bundle(SHA256 03C79BA8…34998,
  含 main 至 54573ad)已删除:为新 bundle 历史前缀,冗余已消除。
- 2026-10-22 事故记录:首次轮换后 dist/ 因 git clean -fX pathspec
  失误被整目录误删。按确定性构建逐一重建:archive bundle 字节级一致
  (SHA256 与历史记录一致);zip 重新执行全部 12 gate 后 SHA256 与
  历史记录一致;本 bundle 为重建后的新轮换(此前 A0AB0BB1…AEF50717
  的 bundle 随 dist 丢失,未分发过,已由本 bundle 取代)。
  Dogfooding 原始结果文件(dogfood140-results 等)无法确定性重建,
  已丢失;权威结论以 docs/acceptance/ 验收记录为准。TUI 截图证据用
  scripts/tui-console-drive.py 夹具重新生成后归档。
- 2026-10-22(最终轮换):阶段收尾提交(截图工具修复 1e24b5f、tui
  证据路径更新 1b7a4d9)后,bundle 重建为含 main 至 1b7a4d9 的最终版
  (SHA256 5B980EEA…34E1147B),取代此前仅含 ca56fba 的中间版
  (665722D7…3099C288,未分发)。本提交(哈希轮换记录)为 bundle 之后的
  最后提交,与前阶段 d580d5d/54573ad 模式一致。证据归档最终化:
  dist/evidence/v0.1.0/ 共 17 文件(tui-evidence 9、tui-drive 3、
  rc-record 2、SHA256SUMS 清单、关键哈希已核)。