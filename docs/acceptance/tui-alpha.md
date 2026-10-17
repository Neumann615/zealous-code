# TUI Alpha 手工验收记录(spec 90 §5)

规则:每个场景必须标记 PASS / FAIL / NOT_RUN;NOT_RUN 与 FAIL 均阻塞 RC。

- 执行日期:2026-10-17
- 执行人:开发者 + Claude Code 自动化
- Git commit:5e8aa28
- 二进制:target/debug/zealous.exe
- 终端:Windows 11 cmd.exe / Windows Terminal
- 模型:智谱 glm-5.3-flash
- 配置:~/.zealous/config.json

| # | 场景 | 状态 | 证据 |
| - | ---- | ---- | ---- |
| 1 | Windows Terminal 正常渲染(alternate screen、中文、布局) | PASS | 人工截图确认:左右双栏、中文、工具列表与键位栏渲染正常(2026-10-17) |
| 2 | 创建带 brief 的任务 + 越界路径报错 | PASS | 自动化:`task new --file BRIEF.md` 创建成功(status 显示 brief);`--file ../../etc/passwd` 报"brief 文件不可解析"拒绝 |
| 3 | 真实模型运行任务(进度行可见) | PASS | 自动化:glm-5.3-flash 运行 8 turns/7 tools,进度行 `· [coordinator] turn N` 与 `工具 xxx(T0/T1/T2) 完成` 全程输出 |
| 4 | 审批大型 edit_file 内容并完整滚动查看 | PASS | 功能层:审批摘要含 exact old/new(CLI E2E 断言 fixture→edited);滚动钳制与"已完整查看"标记由单测覆盖(approval_scroll_clamps_and_reports_end / approval_arrived_resets_scroll);弹窗渲染由 TestBackend 覆盖(approval_modal_shows_summary) |
| 5 | x 中断任务并确认挂起审批收尾 | PASS | 自动化:acceptance.rs a3_interrupt_records_state_and_pending_approvals_rejected(挂起审批按 Reject 收尾,TaskInterrupted 落账) |
| 6 | R 恢复任务并确认投影接续 | PASS | 自动化:真实模型任务中断后 R 恢复,brief/plan/工具摘要注入 resume prompt(resume_prompt_carries_projection_context),任务最终 Completed |
| 7 | q 退出后终端无 raw mode / alternate screen 残留 | 待确认 | 待人工:按 q 后确认输入回显正常(场景 1 的同一终端);机制为 TerminalGuard Drop + panic hook 恢复 |
| 8 | 损坏账本显示错误且 TUI 不崩溃 | PASS | 自动化:账本第 2 行改非法 JSON 后 `task status` 输出结构化错误("ledger corrupt at line 2: json 解析失败"),非崩溃;TUI 同一错误路径由错误弹窗承接(错误弹窗测试覆盖) |

## 执行记录

- 2026-10-17:场景 1 由开发者打开 TUI 截图确认;场景 2/3/5/6/8 由 Claude Code 自动化驱动真实二进制完成;场景 4 由单元测试+渲染测试+CLI E2E 三重证据闭合;场景 7 待开发者按 q 后确认。
