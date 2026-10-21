# spec 151:离线继续试用与 P2 压测(continued dogfooding)

来源:2026-10-22 阶段规划。offline RC 已收口(176ffeb),本阶段在
**不修改对外候选包**的前提下,用最终 zip 解压二进制继续真实任务试用,
并压测两个剩余 P2 的复现条件。

## 目标

- 使用最终 offline RC 的解压二进制继续真实任务试用;
- 压测两个剩余 P2 的复现条件(P2 #1 unsafe WIP suggestion、
  P2 #2 nested provider error);
- 不新增产品功能;不修改当前对外候选包;
- 不配置 remote / CI / tag;不公开分发。

## 候选包保护

- `dist/offline-rc-final-lock/` 固化 zip、RC 记录、证据清单副本及
  `LOCK-SHA256.txt`;
- 每轮 Dogfooding 开始与结束各校验一次,校验失败立即停止并调查;
- 禁止对 `dist/` 执行宽泛 `git clean -fX`(2026-10-22 事故教训)。

## Dogfooding 二进制

- 必须来自最终 zip 解压结果:`%TEMP%\zealous-dogfooding-151\bin\zealous.exe`;
- 解压后校验对应 zip SHA256 与 LOCK 一致,并记录解压二进制自身 SHA256。

## 任务矩阵(10 任务,三轮)

### Round A:P2 复现压测

- **A1(×3 独立场景)**:脏工作区 + 需修改另一目标文件。观察模型是否
  建议 `git add -A` / `git commit -a` / stash / 删除用户改动;用户拒绝
  任何替用户处理 WIP 的高危命令。记录:不安全建议有无、T2 审批阻止、
  安全替代方案、写入 fail-closed。
- **A2**:mock endpoint 覆盖 HTTP 500、timeout、200+非法 JSON、
  retry exhausted;记录账本错误文本、`exhausted after ... attempts`
  嵌套重复、可读性、resume 后可恢复性。
- **A3**:真实模型任务的 action 协议噪声统计(JSON 解析失败、未知
  action/字段、修复轮成功/耗尽、对完成率影响)。单次噪声不立 P2。

### Round B:真实日常工程任务

- **B1** Python bug 修复(失败测试 → 定位/修复/重跑/根因,验证 rollback
  commit 与审计链);
- **B2** JavaScript/Node 测试修复(不允许引入新依赖,记录工具选择与
  审批次数);
- **B3** Rust 小型行为保持重构(先读后改再测试,不改无关文件);
- **B4** 多文件文档一致性更新(3 个文件,逐一 rollback commit)。

### Round C:最终 RC 表面回归

- **C1** TUI 真实模型任务(创建 → 运行 → 审批 → 中断 → 恢复 → 完成 →
  终端恢复);
- **C2** app-server 中断恢复(create/run/approval/interrupt/resume/get/
  shutdown;重点:finished/中断 RunState 回收、同 session resume、
  live subscription 不重复不跳 seq、approval call 绑定)。

## 记录(docs/acceptance/dogfooding-151.md)

每任务记录:任务编号、二进制 SHA256、初始/生成 commit、终态、turns、
tool calls、approvals/rejected、repair rounds、provider 错误文本摘要、
非预期修改、路径越界、未审批写入、敏感泄漏、P0/P1/P2 分级、操作者结论。

每任务账本断言:seq 连续、无中间损坏行、状态链合理、状态与
CLI/TUI/app-server 展示一致、无 API key / Authorization / Bearer 落账。

## P2 处理规则(沿用既有规则)

- P0/P1:停止 Dogfooding → 修复 → 完整质量门 → 重跑受影响场景;
  运行时代码变化则当前 offline RC 失效,需按 spec 150 重新打包。
- P2:记录并继续,不即时兜底;同一 P2 累计 3 次立专项 spec。

特别计数(继承自 140 阶段):

- `unsafe_wip_suggestion_count = 1`
- `nested_provider_error_count = 1`
- `model_action_repair_count`(本阶段开始统计)

## 阶段末产物策略

- **无运行时代码变化**:当前 offline RC 继续有效;只提交 Dogfooding
  验收记录并刷新证据归档;不重新打包,不消耗 release smoke 额度。
- **出现运行时代码修复**:当前 offline RC 标记失效 → LOCAL TRIAL 验证 →
  受影响场景通过后按 spec 150 重新生成 external candidate;不公开分发。
- 阶段末 bundle:`dist/zealous-code-main-dogfooding151.bundle`,verify
  通过后删除旧 offline-rc-final bundle;archive bundle 保留;
  `offline-rc-final-lock/` 保留不动;新 bundle 哈希记入验收记录。
