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

人工目检闭环(spec 120 / spec 150 对外 RC 门槛):

- 人工目检执行日期:2026-10-22
- 执行人:开发者(目检截图,确认与验收描述一致)
- 目检时 Git commit:69e36eb
- 证据二进制:target/debug/zealous.exe(截图 2026-10-20 由
  scripts/tui-console-drive.py 控制台级驱动真实 TUI 生成)
- 终端:Windows 11 Windows Terminal(真实进程,窗口 zealous-drive-capture)

| # | 场景 | 状态 | 方式 | 证据 |
| - | ---- | ---- | ---- | ---- |
| 1 | Windows Terminal 正常渲染(alternate screen、中文、布局) | PASS | MANUAL | 开发者截图确认:左右双栏、中文、工具列表与键位栏正常(2026-10-17) |
| 2 | 创建带 brief 的任务 + 越界路径报错 | PASS | AUTOMATED | `task new --file BRIEF.md` 成功;`--file ../../etc/passwd` 报"brief 文件不可解析" |
| 3 | 真实模型运行任务(进度行可见) | PASS | AUTOMATED | glm-5.3-flash 8 turns/7 tools,进度行 `· [coordinator] turn N` 与 `工具 xxx(T0/T1/T2)` 全程输出 |
| 4 | 审批大型 edit_file 内容并完整滚动查看 | PASS | MANUAL | 2026-10-22 开发者人工目检确认(dist/evidence/v0.1.0/tui-evidence/s4-modal-end.png;原件因 2026-10-22 dist 事故丢失,同日以同一控制台级夹具 + mock 模型重新生成并归档):审批弹窗完整展示 call_id 与 exact old/new(66 行);滚动到底出现绿色"滚动 66/66(已完整查看)";a 批准后 Completed、rollback commit 生成;TUI 不崩溃。自动化证据保留:2026-10-20 控制台级按键注入(WriteConsoleInput)+ 真实 Windows Terminal 截图(滚动 10/66、40/66 无"已完整查看",66/66 出现标记);CLI E2E 断言、滚动钳制单测、TestBackend 渲染 |
| 5 | x 中断任务并确认挂起审批收尾 | PASS | AUTOMATED | a3 测试:挂起审批按 Reject 收尾、TaskInterrupted 落账 |
| 6 | R 恢复任务并确认投影接续 | PASS | AUTOMATED | 真实模型任务中断后恢复,resume 注入 brief/plan/工具摘要,最终 Completed |
| 7 | q 退出后终端无 raw mode / alternate screen 残留 | PASS | MANUAL | 开发者按 q 确认:回到普通命令行,输入回显正常(2026-10-17) |
| 8 | 损坏账本显示错误且 TUI 不崩溃 | PASS | MANUAL | 2026-10-22 开发者人工目检确认(dist/evidence/v0.1.0/tui-evidence/s8-errorbar.png;原件因 2026-10-22 dist 事故丢失,同日以同一控制台级夹具重新生成并归档):红色错误栏"2 个损坏账本已跳过(不崩溃)— …ledger corrupt at line 2: key must be a string…";任务列表显示"(无任务)",不展示损坏任务虚假状态;错误可消除(s8-error-dismissed.png),q 消除后再 q 退出终端正常恢复、TUI 不崩溃。自动化证据保留:2026-10-20 控制台级取证 + 真实进程截图;2026-10-17 afb9f02 修复取证 |

## 执行记录

- 2026-10-22(dist 事故恢复):原始截图随 dist/ 误删。以同一夹具
  (scripts/tui-console-drive.py + mock 模型)重新生成场景 4/8 全套截图:
  场景 4 审批滚动到底出现"已完整查看"后 a 批准 → Completed、exit 0;
  场景 8 错误栏 ledger corrupt、损坏任务不显示、q 消除后 exit 0。
  人工目检结论基于原件作出,维持有效;重生成件作为补充证据归档至
  dist/evidence/v0.1.0/tui-evidence/。修复截图工具后台窗口抓取缺陷(1e24b5f)。
- 2026-10-17:场景 1、7 由开发者人工确认;2/3/5/6 由自动化驱动真实二进制;4 由单测+渲染测试+CLI E2E 三重证据;8 由真实 TUI 进程输出取证(修复了损坏账本静默跳过无错误显示的缺陷,见 afb9f02)。
- 2026-10-20:场景 4、8 由 `scripts/tui-console-drive.py` 控制台级按键注入(AttachConsole + WriteConsoleInput)驱动真实 TUI,并捕获真实 Windows Terminal 截图至 `dist/tui-evidence/`。修复了验收夹具缺陷:`task new` 单事件账本导致损坏注入被静默跳过(现 1 行账本追加完整非法行并自检)。行为发现:错误弹窗为"任意键消除"优先级最高,场景 8 退出需 q(消除)→ q(退出),属设计行为。
- 2026-10-22:开发者人工目检 s4/s8 截图,确认与验收描述一致,场景 4、8 升级 MANUAL PASS。
  对外 RC 的 TUI 门槛(1、4、7、8 全 MANUAL)已满足,进入 spec 150
  离线发布治理默认打包。
