# agent-assets 参考源码区

本目录是本机可更新的**只读参考源码区**,不是产品依赖,也不是构建输入。

## 参考仓库

| 目录 | 来源 | 用途 |
| ---- | ---- | ---- |
| `zcode/` | https://github.com/zai-org/zcode.git | Agent 运行时、Host/CLI 协议与进程编排范式参考 |
| `codex/` | https://github.com/openai/codex.git | Agent CLI、工具循环与 TUI 交互范式参考 |

两个目录均为完整 Git clone(非 submodule),由 `scripts/update-agent-assets.mjs` 统一维护:
缺失则 clone;已存在则执行 `git fetch --prune origin`,并 detach 到 origin 默认分支最新 commit。
运行前要求各参考仓库工作区干净,否则拒绝更新。

## 只读规则

- 不在本目录内进行产品开发,不提交任何本地修改;外层仓库忽略 `/agent-assets/*`(仅本 README 进入版本管理)。
- 上游代码只用于范式研究与有意识的局部移植;产品源码禁止直接 import 本目录下的任何代码。
- 如需复制上游代码,必须记录来源 commit 与许可证信息,并履行对应义务(如保留版权与许可声明)。
