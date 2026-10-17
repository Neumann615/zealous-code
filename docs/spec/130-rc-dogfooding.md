# 130 · RC 真实任务 Dogfooding

状态: 已定案 · 2026-10-17
目标: 用真实模型在独立 Git 仓库中完成小型工程任务,验证 RC 超越 smoke 的实用能力。
不做: 桌面壳、Web 壳、远程连接、插件系统、多客户端。

## 任务集(至少 3 类)

### T1 单文件精确修改

- 输入:任务 brief + 目标文件。
- 期望工具:read_file / edit_file。
- 验收:修改精确;审批摘要完整(含 exact old/new);自动生成 rollback
  commit;任务 Completed。

### T2 小型代码修复

- 输入:含已知小 bug 的示例项目。
- 期望流程:定位代码 → 修改 → 运行测试/验证命令 → 汇报原因与修复。
- 验收:测试通过;事件链完整(seq 连续);高风险工具(shell/T1/T2)均获审批。

### T3 中断恢复任务

- 真实模型任务运行中按 x(或等价中断);再 R 恢复。
- 验收:TaskInterrupted 落账;dangling approval 收尾;
  resume prompt 包含历史投影(brief/plan/工具摘要/上次错误);
  最终 Completed 或失败原因可解释。

## 记录模板(docs/acceptance/dogfooding-0.1.0.md)

每任务记录:任务描述、临时仓库路径与清理状态、模型名称、
turns/tool calls 数量、审批次数、结果(Completed/Failed/Interrupted)、
生成 commit、遇到的问题、是否出现非预期文件修改、是否泄漏敏感信息。

## 问题分级

- **P0**:数据丢失;未审批写入;路径越界;密钥泄漏;TUI 崩溃无法恢复终端。
- **P1**:任务状态错误;resume 语义错误;事件 seq 断裂;审批绑定错误;
  模型输出合法但编排失败。
- **P2**:TUI 显示问题;可用性差;非关键错误提示不清楚。

处理规则:P0/P1 先修复并重新跑完整质量门与受影响 dogfooding 场景;
P2 记录后继续,不阻塞 RC 结论(除非影响审批可审查性)。
