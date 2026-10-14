# zealous-code

本仓库是从零重建的新产品主线:`main` 分支为 orphan 分支,当前尚无产品代码。
旧工程完整快照保存在归档分支 `archive/pre-agent-assets-20261015`(基于原 master 及其历史),
不再随主线演进。

## agent-assets/ 参考源码区

`agent-assets/` 是本机可更新的只读参考源码区,**不是产品依赖,也不是构建输入**:

- 产品源码禁止直接 import `agent-assets/` 下的任何代码。
- 上游代码只用于范式研究与有意识的局部移植。
- 如需复制上游代码进产品,必须记录来源 commit,并履行对应许可证义务(如保留版权与许可声明)。

| 目录 | upstream 地址 | 用途 |
| ---- | ------------- | ---- |
| `agent-assets/zcode` | https://github.com/zai-org/zcode.git | Agent 运行时与 CLI 范式参考 |
| `agent-assets/codex` | https://github.com/openai/codex.git | Agent CLI / 编码代理范式参考 |

参考仓库使用完整 Git clone(而非 submodule),便于持续跟踪最新上游。
`.gitignore` 已忽略 `/agent-assets/*`,仅其 `README.md` 进入版本管理。

更新方式:

```bash
node scripts/update-agent-assets.mjs
```

脚本行为:缺失则 clone;已存在则要求工作区干净,执行 `git fetch --prune origin`,
并 detach 到 origin 默认分支的最新 commit;最后输出各仓库当前 commit 便于引用。

## 后续原则

- 产品模块、包结构与架构策略在新需求确定后另行建立,不迁移旧仓库的默认架构。
- 上游参考代码只读,不在 `agent-assets/` 内进行开发或提交本地修改。
