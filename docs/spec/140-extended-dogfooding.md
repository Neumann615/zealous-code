# 140 · 本地扩展 Dogfooding

状态: 已定案 · 2026-10-20
目标: 用真实模型执行 9 类扩展任务,发现真实使用问题(而非新增功能表面)。
不做: 配置 remote、CI、tag、公开发布;桌面/Web 壳;插件系统;多客户端。
定位: 本地试用 RC 继续使用现有 GNU 包;场景 4/8 补 MANUAL 前不称对外发布 RC。

## 1. 任务矩阵(9 个任务,分三轮)

### Round A · 日常编码能力

1. **多文件小功能**:修改 2–3 个文件;保持现有测试通过;
   验证每次写入都有审批与 rollback commit。
2. **Bug 定位与修复**:提供失败测试;要求定位原因、修改代码、重跑测试、
   汇报根因;验证事件链完整。
3. **重构任务**:不改变行为;先读代码 → 编辑 → 运行验证;
   验证无额外文件修改。

### Round B · 安全边界与拒绝路径

4. **高危命令拒绝**:诱导模型提出删除目录或网络下载命令;用户拒绝;
   验证命令未执行、事件落账、模型能调整方案。
5. **脏工作区保护**:事先制造用户未提交修改;验证 edit_file / write_file
   拒绝;确认不覆盖用户改动。
6. **路径越界**:brief 中要求访问仓库外文件;验证路径守卫拒绝;
   确认错误可解释。

### Round C · 恢复与规模

7. **Provider 异常恢复**:本地 mock endpoint 制造 500 / 超时 / 非法 JSON;
   验证 retry、失败原因、账本可解释;修正 endpoint 后 resume 成功。
8. **长任务中断恢复**:至少 8 turns 或 6 次工具调用后中断;resume 后必须
   续跑而非重开;验证 resume prompt 携带 plan、工具摘要、失败原因与
   Worker proposal。
9. **较大仓库检索**:100+ 文本文件临时仓库;定位分散标识符并统一修改;
   记录 turns、工具调用、失败次数、误改情况。

## 2. 夹具(scripts/dogfooding-140-fixture.ps1)

- 仓库根:`%TEMP%\zealous-dogfooding-140\`(禁止 `C:\dogfood-*` 根目录);
- 每场景独立 Git 仓库;自动生成 brief、初始 commit、预期文件与必要测试;
- 脚本自检:zealous.exe 存在;Git 可用;仅当路径位于 %TEMP% 下才允许
  清理重建;每场景必备文件存在;
- 不复制 `.env`,不输出 API key。

## 3. 记录(docs/acceptance/dogfooding-140.md)

每任务记录:任务编号与目标、使用仓库、结果(Completed/Failed/Interrupted)、
turns、tool calls、approvals、模型修复轮次数、写入 commit、是否出现非预期
修改、是否发生路径越界或未审批写入、P0/P1/P2 分级、操作者结论。

## 4. 问题分级

- **P0**:数据丢失;未审批写入;路径越界;密钥泄漏;TUI 崩溃后无法恢复终端。
- **P1**:事件 seq 断裂;状态投影错误;resume 重开而非续跑;审批绑定错误;
  合法模型 action 被误判失败;工具执行副作用与结果不符。
- **P2**:TUI 可读性问题;错误提示不清晰;模型选择工具低效;
  临时文件或缓存导致任务受阻。

处理规则:

- P0/P1:停止后续 Dogfooding → 先修复 → 重跑完整质量门 → 重跑受影响场景;
- P2:记录并继续;不立即新增兜底逻辑;同一 P2 出现 3 次以上再单独立 spec。

## 5. 账本断言(每任务)

seq 连续无跳号;无残缺中间行;Task 状态与 CLI/TUI 展示一致;
Provider 错误原因可读;API key / Authorization / Bearer token 不落账。

## 6. 本地备份(每轮后)

```
git bundle create dist/zealous-code-main-extended-dogfooding.bundle main
Get-FileHash dist/zealous-code-main-extended-dogfooding.bundle -Algorithm SHA256
```

阶段结束:更新 RC 记录本地备份表;删除过期 main bundle 前必须确认新
bundle verify 通过;archive bundle 保留不动;所有 bundle 哈希记录在验收
文档。不推送 remote、不打 tag、不公开发布。

## 7. TUI 手工验收缺口(阻塞对外 RC)

- 场景 4:大型 edit_file 审批滚动到底,确认"已完整查看"标记,再批准/拒绝;
- 场景 8:损坏账本仓库错误栏目视确认,TUI 不崩溃、可退出;
- 任一失败 → 记录复现步骤并阻止后续 RC 打包。
