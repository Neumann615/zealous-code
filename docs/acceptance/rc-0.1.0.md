# RC 0.1.0 验收记录(spec 120)

本文件在完整 RC 流程通过后由开发者填写;全部字段必填。
TUI 手工验收(docs/acceptance/tui-alpha.md)8 项全部通过前,本文件不生效。

## 基本信息

- Git commit: (待填)
- 构建时间: (待填)
- 工具链: (待填:rustc version + host triple)
- 目标平台: Windows x64

## 质量门

| 门 | 命令 | 结果 |
| -- | ---- | ---- |
| fmt | cargo fmt --check | (待填) |
| test | cargo test --all | (待填) |
| clippy | cargo clippy --all-targets -- -D warnings | (待填) |
| real_model_smoke | cargo test --test cli_e2e real_model_smoke -- --ignored | (待填) |
| release build | cargo build --release | (待填) |
| --version | zealous.exe --version | (待填) |
| config check | zealous.exe config check(假密钥) | (待填) |

## 产物

- 路径: dist/zealous-0.1.0-x86_64-pc-windows-msvc.zip
- 大小: (待填 bytes)
- SHA256: (待填)
- 清单: zealous.exe(仅此一项)

## 安全边界声明

- V1 不是 OS sandbox。
- Shell 是 T2 审批动作。
- 文件访问限制在仓库内(路径守卫)。
- 模型 endpoint 是唯一默认外发网络访问。

## TUI 手工验收

结果: 见 docs/acceptance/tui-alpha.md(须全部"通过")
