# TUI Alpha 手工验收记录(spec 90 §5)

状态标记:PASS / FAIL / NOT_RUN;NOT_RUN 与 FAIL 均阻塞 RC。
证据方式:MANUAL(人工目视)/ AUTOMATED(自动化取证)。
**对外发布标准:场景 1、4、7、8 必须 MANUAL PASS;自动化证据不得替代手工证据。**

- 执行日期:2026-10-17
- 执行人:开发者(1、7 人工目视)+ Claude Code(其余自动化)
- Runtime source commit:afb9f02
- 二进制:target/debug/zealous.exe
- 终端:Windows 11 cmd.exe / Windows Terminal
- 模型:智谱 glm-5.3-flash

| # | 场景 | 状态 | 方式 | 证据 |
| - | ---- | ---- | ---- | ---- |
| 1 | Windows Terminal 正常渲染(alternate screen、中文、布局) | PASS | MANUAL | 开发者截图确认:左右双栏、中文、工具列表与键位栏正常(2026-10-17) |
| 2 | 创建带 brief 的任务 + 越界路径报错 | PASS | AUTOMATED | `task new --file BRIEF.md` 成功;`--file ../../etc/passwd` 报"brief 文件不可解析" |
| 3 | 真实模型运行任务(进度行可见) | PASS | AUTOMATED | glm-5.3-flash 8 turns/7 tools,进度行 `· [coordinator] turn N` 与 `工具 xxx(T0/T1/T2)` 全程输出 |
| 4 | 审批大型 edit_file 内容并完整滚动查看 | PASS | AUTOMATED | 审批摘要 exact old/new(CLI E2E fixture→edited 断言);滚动钳制与"已完整查看"标记单测覆盖;弹窗渲染 TestBackend 覆盖。⚠ TUI 按键无法脚本化(crossterm 仅控制台输入),目视滚动体验待对外发布前补 MANUAL |
| 5 | x 中断任务并确认挂起审批收尾 | PASS | AUTOMATED | a3 测试:挂起审批按 Reject 收尾、TaskInterrupted 落账 |
| 6 | R 恢复任务并确认投影接续 | PASS | AUTOMATED | 真实模型任务中断后恢复,resume 注入 brief/plan/工具摘要,最终 Completed |
| 7 | q 退出后终端无 raw mode / alternate screen 残留 | PASS | MANUAL | 开发者按 q 确认:回到普通命令行,输入回显正常(2026-10-17) |
| 8 | 损坏账本显示错误且 TUI 不崩溃 | PASS | AUTOMATED | 真实进程取证(2026-10-17):损坏账本启动 TUI,渲染输出含错误栏"1 个损坏账本已跳过(不崩溃) — …ledger corrupt at line 2",进程存活至超时被杀(exit 124,非崩溃)。⚠ 错误栏目视确认待对外发布前补 MANUAL |

## 执行记录

- 2026-10-17:场景 1、7 由开发者人工确认;2/3/5/6 由自动化驱动真实二进制;4 由单测+渲染测试+CLI E2E 三重证据;8 由真实 TUI 进程输出取证(修复了损坏账本静默跳过无错误显示的缺陷,见 afb9f02)。
- 对外发布前待办:场景 4(滚动到底)与场景 8(错误栏目视)由人工在 Windows Terminal 确认,补 MANUAL 标记。
