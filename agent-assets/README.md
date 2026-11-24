# agent-assets 参考源码区

本目录是本机可更新的**只读参考源码区**,不是产品依赖,也不是构建输入。

## 参考仓库

| 目录 | 来源 | 用途 |
| ---- | ---- | ---- |
| `zcode/` | https://github.com/zai-org/zcode.git | Agent 运行时、Host/CLI 协议与进程编排范式参考 |
| `codex/` | https://github.com/openai/codex.git | Agent CLI、工具循环与 TUI 交互范式参考 |
| `dsh-TUI-main/` | https://github.com/ccch1mneyyy/dsh-TUI(MIT) | 源码压缩包快照,**不是 git 仓库**;对话式 TUI 范式参考(事件分型、命令表、工具卡片)。2026-11-25 已按文件哈希与上游 main 比对并同步(v0.20.2);`CLAUDE.md` 因压缩包条目异常无法解出,与功能无关 |

两个目录均为完整 Git clone(非 submodule),由 `scripts/update-agent-assets.mjs` 统一维护:
缺失则 clone;已存在则执行 `git fetch --prune origin`,并 detach 到 origin 默认分支最新 commit。
运行前要求各参考仓库工作区干净,否则拒绝更新。

## 同步与操作安全(2026-11-25 事故后新增)

- 本目录下的参考仓库**不一定自带 `.git`**(例如 `dsh-TUI-main/` 是从源码压缩包解出来的)。
  **禁止对 `agent-assets/` 下的任何路径使用 `git -C`**:子目录不是仓库时,`git -C` 会向外层
  找到 zealous-code 自己的仓库。`git -C agent-assets/<x> reset --hard origin/HEAD` 实际会把
  **zealous-code 主仓库硬重置到 origin/HEAD**(2026-11-25 发生过一次,靠 reflog 恢复,无未提交改动丢失)。
- 同步压缩包型参考仓库:下载到系统临时目录,与本地目录**按文件哈希比对**,确认有差异后再更新;
  全程不经过任何 git 仓库操作。自带 `.git` 的参考仓库(zcode/codex)只用
  `node scripts/update-agent-assets.mjs` 更新。
- `git log` 出现意料之外的提交时,先查 `git reflog` 确认事实,不要在未核实的状态上继续操作。
- 压缩包型参考仓库的同步状态以上表的说明为准;核对方式是「下载到系统临时目录 → 逐文件哈希比对 →
  确认差异后覆盖复制」,不信任任何未经比对的「已是最新」结论。

## 只读规则

- 不在本目录内进行产品开发,不提交任何本地修改;外层仓库忽略 `/agent-assets/*`(仅本 README 进入版本管理)。
- 上游代码只用于范式研究与有意识的局部移植;产品源码禁止直接 import 本目录下的任何代码。
- 如需复制上游代码,必须记录来源 commit 与许可证信息,并履行对应义务(如保留版权与许可声明)。
