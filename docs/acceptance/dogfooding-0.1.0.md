# RC 0.1.0 Dogfooding 记录(spec 130)

- 执行日期:2026-10-17
- 模型:智谱 glm-5.3-flash(OpenAI-compatible,~/.zealous/config.json)
- Runtime source commit:66cad6f(RC 构建源)
- 临时仓库:C:\dogfood-t1 / t2 / t3(验收后可清理,当前保留备查)

## T1 单文件精确修改

- 任务:config.json 中 port 3000 → 8080,其他内容不变(仅允许 edit_file)。
- 结果:**Completed**;turns=5,tools=3,approvals=1;生成 commit `dcd3601 zealous: edit config.json`。
- 验收:config.json 精确(name/demo、port/8080、debug/false 不变);审批摘要含 exact old/new(3000→8080);回退点存在;seq 25 连续。
- 非预期文件修改:无。敏感信息泄漏:无。

## T2 小型代码修复

- 任务:python test_math.py 失败 → 修复 math_utils.py 的 bug → 重跑测试通过 → 汇报原因。
- 结果:**Completed**;turns=9,tools=7,approvals=5(T1/T2 全部获审批,decisions=5)。
- 流程亮点:首次 edit 因 `__pycache__` 脏工作区被守卫拒绝 → 模型自行 `rmdir /s /q __pycache__` 清理后重新 edit 成功 → 运行测试通过。
- 验收:test_math.py 输出 "all tests passed";`add()` 返回 `a + b`;commit `530b7f6 zealous: edit math_utils.py`;seq 49 连续;汇报含原因(加法写成减法)与修复内容。
- 非预期文件修改:无(仅 math_utils.py + git 产物)。敏感信息泄漏:无。

## T3 中断恢复

- 任务:多步骤(读 README → edit README 加 DONE → git_diff 验证)。
- 中断方式:serve 协议 `interrupt_task`(与 TUI x 键同一 abort 引擎路径;
  TUI 键盘无法脚本化——crossterm 仅控制台输入)。
- 中断时点:第一个 edit_file 审批等待期间。
- 结果:第 1 轮 **Interrupted**,恢复后 **Completed**;turns 合计 8,tools=6,approvals=2;生成 commit `df6858b zealous: edit README.md`。
- 验收:
  - TaskInterrupted 落账 ✓;TaskStatusChanged→Interrupted ✓;
  - dangling approval 收尾为 Reject ✓(spec 30 语义);
  - TaskResumed 落账 ✓;AgentRuns=2(中断+恢复)✓;
  - resume prompt 携带历史投影(brief/plan/工具摘要/上次错误)由
    resume_prompt_carries_projection_context 单测覆盖,真实恢复后
    模型正确续跑(edit + git_diff 验证)✓;
  - 最终 Completed ✓;README.md 精确追加 DONE 行;回退点存在。
- seq 46 连续。非预期文件修改:无。敏感信息泄漏:无。

## 问题分级汇总

- P0:无。
- P1:无。
- P2:2 项(均不阻塞 RC):
  1. TUI 损坏账本错误栏可见性——已在 afb9f02 修复(真实进程取证通过)。
  2. 临时仓库 C:\dogfood-t* 与 C:\tui-s8 保留备查,发布前可清理。

## 结论

3 类任务全部 Completed;所有写入经审批并生成回退 commit;
事件 seq 全部连续;无 P0/P1;RC 0.1.0 实用性超出 smoke,可用于真实小型工程任务。
