# 90 · TUI Alpha

状态: 已定案 · 2026-10-16
目标: 新增 `zealous tui`,把现有 CLI/engine 能力变成可视化任务工作台。
不改变事件账本、审批与执行所有权。
仍不做: 桌面端、app-server、远程控制、插件系统、多任务并发执行。

## 1. 薄客户端边界(硬约束)

- 任务事实只来自 `TaskProjection` / 事件账本;TUI 不维护第二份任务业务状态。
- 审批决策通过 `ApprovalSink`(channel)进入 engine。
- 进度通过 `ProgressSink`(channel)来自编排器事实。
- TUI 不直接读取 `.zealous/tasks/.../events.jsonl` 路径,须经
  `TaskService::events / refresh`。
- ratatui/crossterm 只存在于 zealous-cli,不得进入 core/engine。
- 不引入 async runtime;沿用同步 engine + worker thread 模型。
- `--quiet` 等 CLI 文本输出逻辑与 TUI 无关,TUI 不复用。

## 2. 架构与数据流

```
User keyboard → TUI controller
  → progress/approval channels → Engine worker thread
  ← ProgressSink / ApprovalSink(channel 回传)
TUI state reducer ← 事件/投影刷新(TaskService)
  → ratatui render(TestBackend 可测)
```

模块:`crates/zealous-cli/src/tui/{mod,state,approval,progress,views,runner}.rs`。

## 3. 终端规则

- 启动进入 alternate screen + raw mode;退出、panic、错误路径必须恢复终端。
- `q` / Ctrl-C:先设置 abort,等待 engine 落账结束后退出,不直接丢弃运行线程。
- `x`:请求中断当前任务但不退出 TUI。

## 4. 界面与交互

- 布局:左侧任务列表(id 缩短/status/title),右侧选中任务详情
  (title/brief/status/plan/runs/tool calls/pending approvals/last error/
  最近事件流),底部键位与状态。
- 列表键:↑/↓ 选择,r 刷新,Enter 详情,n 新建任务表单。
- 详情键:R 运行/恢复,x 中断,a 批准当前审批,d 拒绝,e 展开/收起事件流,
  Esc 返回列表,q 中断并退出。
- 表单:title 必填 + brief file 可选(相对仓库根);Tab/Shift+Tab 切换,
  Enter 提交(复用 TaskService,越界/配置错误进错误弹窗),Esc 取消。
- 审批弹窗:call id/tool/风险级/完整摘要(write/edit 展示完整内容或
  exact old/new);a 批准,d 拒绝,x 按中断语义拒绝;弹窗等待决策,不自动消失。
- 错误弹窗不遮蔽关键状态(置底显示,可消除)。

## 5. 测试

- Reducer 单测:选择切换、表单输入、审批弹窗与决策、中断状态、事件流展开收起。
- Channel sink 单测:审批等待决策/abort Reject/断开 fail closed;进度分流。
- 渲染:`ratatui::backend::TestBackend` 断言空列表、多任务选中态、
  详情字段、审批弹窗内容、错误弹窗。
- 集成:mock endpoint 驱动 controller(选择→运行→审批→Completed),
  断言文件修改、seq 连续、最终状态来自投影、审批进 ledger、
  TUI 不复用 CLI 文本输出逻辑。
- 手工验收(Windows Terminal):渲染/brief 创建/真实模型/edit 审批/
  中断恢复/退出后终端无残留。
