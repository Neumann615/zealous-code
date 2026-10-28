//! 渲染布局(spec 90 §4):纯函数,输入 TuiState,输出 ratatui 绘制;
//! TestBackend 可直接断言缓冲内容。

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::state::{FormField, Screen, TuiState};

fn status_str(s: zealous_core::TaskStatus) -> &'static str {
    match s {
        zealous_core::TaskStatus::Queued => "Queued",
        zealous_core::TaskStatus::Running => "Running",
        zealous_core::TaskStatus::Interrupted => "Interrupted",
        zealous_core::TaskStatus::Completed => "Completed",
        zealous_core::TaskStatus::Failed => "Failed",
        zealous_core::TaskStatus::Cancelled => "Cancelled",
    }
}

fn id_short(id: &str) -> String {
    id.strip_prefix("task_")
        .unwrap_or(id)
        .chars()
        .take(10)
        .collect()
}

fn task_id_short(id: &zealous_core::TaskId) -> String {
    id_short(id.as_str())
}

/// 主渲染:左列表 / 右详情 + 底部帮助;弹窗叠加
pub fn render(f: &mut Frame, state: &mut TuiState) {
    let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(f.area());
    let cols = Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(chunks[0]);
    render_task_list(f, cols[0], state);
    match state.screen {
        Screen::Detail => render_detail_full(f, cols[1], state),
        Screen::Queue | Screen::Parallel | Screen::MergeStatus => {
            let text = match state.screen {
                Screen::Parallel => state
                    .parallel_status_text
                    .clone()
                    .unwrap_or_else(|| "(parallel status unavailable)".into()),
                Screen::MergeStatus => state
                    .merge_status_text
                    .clone()
                    .unwrap_or_else(|| "(merge status unavailable)".into()),
                _ => state
                    .queue_status_text
                    .clone()
                    .unwrap_or_else(|| "(queue status unavailable)".into()),
            };
            let lines: Vec<Line<'static>> =
                text.lines().map(|l| Line::from(l.to_string())).collect();
            f.render_widget(
                Paragraph::new(lines).block(Block::new().borders(Borders::ALL).title("状态")),
                cols[1],
            );
        }
        _ => render_detail_summary(f, cols[1], state),
    }
    render_help(f, chunks[1], state);

    // 弹窗叠加(审批 > 表单 > 错误;错误置底不遮蔽关键状态)
    if state.pending_approval.is_some() {
        render_approval_modal(f, state);
    }
    if state.screen == Screen::Form {
        render_form_modal(f, state);
    }
    // spec 155:确认弹窗(优先级最低,不遮蔽审批)
    if state.confirm.is_some() {
        render_confirm_modal(f, state);
    }
}

fn render_task_list(f: &mut Frame, area: Rect, state: &TuiState) {
    let mut lines = vec![Line::from("Tasks (r 刷新 / n 新建 / Enter 详情)")];
    if state.tasks.is_empty() {
        lines.push(Line::from("(无任务)"));
    }
    for (i, t) in state.tasks.iter().enumerate() {
        let selected = i == state.selected;
        let marker = if selected { "> " } else { "  " };
        let mut line = Line::from(Span::raw(format!(
            "{marker}{} [{:>10}] {}",
            task_id_short(&t.task_id),
            status_str(t.status),
            truncate(&t.title, 24),
        )));
        if selected {
            line.style = Style::default().add_modifier(Modifier::REVERSED);
        }
        lines.push(line);
    }
    f.render_widget(
        Paragraph::new(lines).block(Block::new().borders(Borders::ALL).title("任务")),
        area,
    );
}

fn projection_lines(state: &TuiState, full: bool) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let Some(proj) = &state.detail else {
        lines.push(Line::from("(选择任务查看详情;Enter 打开)"));
        return lines;
    };
    lines.push(Line::from(format!("title:  {}", proj.title)));
    if let Some(b) = &proj.brief {
        let limit = if full { 400 } else { 80 };
        lines.push(Line::from(format!("brief:  {}", truncate(b, limit))));
    }
    lines.push(Line::from(format!("status: {:?}", proj.status)));
    if let Some(plan) = &proj.plan {
        lines.push(Line::from(format!(
            "plan:   {}",
            truncate(plan, if full { 600 } else { 120 })
        )));
    }
    lines.push(Line::from(format!("runs:   {}", proj.runs.len())));
    for c in &proj.tool_calls {
        lines.push(Line::from(format!(
            "  tool {:?} ({:?}) {:?}",
            c.tool, c.risk, c.state
        )));
    }
    for p in &proj.pending_approvals {
        lines.push(Line::from(format!(
            "  ⏳ 待审批 {}: {}",
            p.call_id,
            truncate(&p.summary, 120)
        )));
    }
    if let Some((scope, msg)) = &proj.last_error {
        lines.push(Line::from(format!(
            "last error [{scope}]: {}",
            truncate(msg, 160)
        )));
    }
    // spec 155:workspace 区块(W 切换显示)
    if state.workspace_visible {
        match &proj.execution_workspace {
            Some(ws) => {
                lines.push(Line::from(format!(
                    "workspace: {} ({})",
                    ws.status, ws.kind
                )));
                lines.push(Line::from(format!(
                    "  branch: {}\n  path: {}\n  base: {} @ {}\n  current: {}",
                    ws.branch, ws.path, ws.base_branch, ws.base_commit, ws.current_commit
                )));
                if let Some(applied) = &proj.applied_commit {
                    lines.push(Line::from(format!("  applied: {applied}")));
                }
                if let Some(strategy) = &proj.applied_strategy {
                    let text = match strategy {
                        zealous_core::AppliedStrategy::FastForward => "fast_forward",
                        zealous_core::AppliedStrategy::ThreeWayMerge => "three_way_merge",
                    };
                    lines.push(Line::from(format!("  strategy: {text}")));
                }
                if let Some(commit) = &proj.merge_commit {
                    lines.push(Line::from(format!("  merge commit: {commit}")));
                }
                if let Some(merge_ws) = &proj.merge_workspace {
                    lines.push(Line::from(format!(
                        "  merge ws: {} [{}]",
                        merge_ws.branch, merge_ws.status
                    )));
                }
                for file in &proj.merge_conflicts {
                    lines.push(Line::from(format!("  conflict: {file}")));
                }
                if proj.workspace_removed {
                    lines.push(Line::from("  (workspace 已清理)"));
                }
            }
            None => lines.push(Line::from("workspace: (非 isolated 任务)")),
        }
    }
    if full && state.events_expanded {
        lines.push(Line::from("--- 事件流(↑/↓ 滚动) ---"));
        let all: Vec<String> = state
            .detail_events
            .iter()
            .map(|e| {
                format!(
                    "  {:>3} {}",
                    e.seq,
                    serde_json::to_string(&e.payload).unwrap_or_default()
                )
            })
            .collect();
        let total = all.len();
        if total == 0 {
            lines.push(Line::from("  (无事件)"));
        } else {
            let visible = 8usize;
            let start = state.events_scroll.min(total.saturating_sub(1));
            let end = (start + visible).min(total);
            for l in &all[start..end] {
                lines.push(Line::from(l.clone()));
            }
            lines.push(Line::from(format!(
                "  [事件 {}-{}/{}]",
                start + 1,
                end,
                total
            )));
        }
    }
    lines
}

fn render_detail_summary(f: &mut Frame, area: Rect, state: &mut TuiState) {
    f.render_widget(
        Paragraph::new(projection_lines(state, false))
            .wrap(Wrap { trim: false })
            .block(Block::new().borders(Borders::ALL).title("详情(Enter 打开)")),
        area,
    );
}

fn render_detail_full(f: &mut Frame, area: Rect, state: &mut TuiState) {
    // 事件流总行数供滚动钳制(渲染层测量,UI-only 状态)
    state.set_events_total(state.detail_events.len());
    let mut lines = projection_lines(state, true);
    if state.screen == Screen::Detail {
        lines.push(Line::from(format!(
            "[{}] 事件流(R 运行/恢复 · P apply · m merge · Shift+M 冲突工作区 · Shift+S 状态 · C 继续 · A 中止 · x 中断 · a 批准 · d 拒绝 · e 事件流 · Esc 返回)",
            if state.events_expanded {
                "已展开"
            } else {
                "已收起"
            }
        )));
    }
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(Block::new().borders(Borders::ALL).title("详情")),
        area,
    );
}

fn render_help(f: &mut Frame, area: Rect, state: &TuiState) {
    // 错误不遮蔽关键状态:占用帮助行,红色显示;任意键消除
    let line = if let Some(err) = &state.error {
        Line::from(Span::styled(
            format!("错误: {err}(按任意键消除)"),
            Style::default().red().add_modifier(Modifier::BOLD),
        ))
    } else {
        let status = state
            .last_status
            .map(|s| format!("{s:?}"))
            .unwrap_or_else(|| "-".into());
        let run_state = if state.running { "运行中" } else { "空闲" };
        if state.abort_requested {
            Line::from(format!(
                "[{run_state}] 已请求中断,等待引擎落账… 最终状态: {status}"
            ))
        } else {
            Line::from(format!(
                "[{run_state}] q 退出 · n 新建 · r 刷新 · ↑/↓ 选择 · Enter 详情 · 详情内: R 运行 · I 隔离运行 · P 应用 · W 工作区 · x 中断 · a/d 审批 · e 事件流 · 最终状态: {status}"
            ))
        }
    };
    f.render_widget(Paragraph::new(line), area);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let v = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(area);
    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(v[1])[1]
}

/// spec 155:确认弹窗(I/P/W 触发;Enter 确认 / Esc 取消)
fn render_confirm_modal(f: &mut Frame, state: &TuiState) {
    let Some(kind) = &state.confirm else { return };
    let (title, lines) = match kind {
        super::state::ConfirmKind::IsolatedRun => (
            "Isolated Run".to_string(),
            vec![
                "将在任务专属 worktree 中运行/恢复该任务:".to_string(),
                "(模式落账;主工作区 WIP 不受影响)".to_string(),
            ],
        ),
        super::state::ConfirmKind::Apply => (
            "Apply".to_string(),
            vec![
                "将把任务分支 fast-forward 应用回主工作区。".to_string(),
                "要求:主分支未前进、主工作区干净。".to_string(),
            ],
        ),
        super::state::ConfirmKind::Merge => (
            "Merge".to_string(),
            vec![
                "将对 Completed isolated 任务执行 --no-ff 三方合并。".to_string(),
                "Git 无法自动合并时立即 abort,不保留冲突 index。".to_string(),
            ],
        ),
        super::state::ConfirmKind::MergeWorkspace => (
            "Merge Workspace".to_string(),
            vec![
                "将在独立 worktree 中尝试三方合并。".to_string(),
                "冲突会保留在现场,主工作区保持干净。".to_string(),
            ],
        ),
        super::state::ConfirmKind::MergeContinue => (
            "Merge Continue".to_string(),
            vec![
                "提交你已在 merge workspace 中 stage 的手动解决结果。".to_string(),
                "unresolved files 非空时会拒绝。".to_string(),
            ],
        ),
        super::state::ConfirmKind::MergeAbort => (
            "Merge Abort".to_string(),
            vec![
                "中止并清理当前 merge workspace。".to_string(),
                "存在额外修改时将拒绝,需 CLI --force。".to_string(),
            ],
        ),
        super::state::ConfirmKind::WorkspaceRemove => (
            "Workspace Remove".to_string(),
            vec![
                "将删除任务 worktree 与分支(仅已 applied/已包含时允许)。".to_string(),
                "worktree 中的未提交改动不会被删除(需 CLI --force)。".to_string(),
            ],
        ),
        super::state::ConfirmKind::ParallelRun => {
            let mut lines = vec![
                "Run up to 2 queued tasks in parallel isolated worktrees?".to_string(),
                String::new(),
            ];
            for id in &state.parallel_ids {
                lines.push(format!("  Task: {}", id.as_str()));
            }
            lines.push(String::new());
            lines.push("All tasks share one base commit.".into());
            lines.push("Completed tasks will be fast-forward applied in input order.".into());
            ("Parallel".to_string(), lines)
        }
        super::state::ConfirmKind::QueueRun => {
            let mut lines = vec![
                "Run queued tasks in isolated worktrees?".to_string(),
                String::new(),
            ];
            for id in &state.queue_ids {
                lines.push(format!("  Task: {}", id.as_str()));
            }
            lines.push(String::new());
            lines.push("Completed tasks will be fast-forward applied to main.".into());
            ("Queue".to_string(), lines)
        }
    };
    let area = centered_rect(60, 30, f.area());
    f.render_widget(Clear, area);
    let inner = area.inner(ratatui::layout::Margin::new(1, 1));
    let mut text = vec![Line::from(Span::styled(
        title,
        Style::default().add_modifier(Modifier::BOLD),
    ))];
    text.push(Line::from(""));
    for l in lines {
        text.push(Line::from(l));
    }
    text.push(Line::from(""));
    text.push(Line::from("Enter 确认 · Esc 取消"));
    f.render_widget(Paragraph::new(text), inner);
}

fn render_approval_modal(f: &mut Frame, state: &mut TuiState) {
    let Some(req) = state.pending_approval.clone() else {
        return;
    };
    let area = centered_rect(70, 50, f.area());
    f.render_widget(Clear, area);
    // 内容区:标题 1 行 + 摘要滚动窗口 + 位置指示 1 行 + 键位 1 行
    let inner = area.inner(ratatui::layout::Margin::new(1, 1));
    let visible = inner.height.saturating_sub(3) as usize;
    const CONTENT_WIDTH: usize = 62;
    let summary_lines = wrap_lines(
        &format!(
            "task: {}\ncall: {}\n{}",
            req.task_id, req.call_id, req.summary
        ),
        CONTENT_WIDTH,
    );
    let total = summary_lines.len();
    state.set_approval_total(total);
    let scroll = state.approval_scroll.min(total.saturating_sub(1));
    let start = scroll.min(total);
    let end = (scroll + visible).min(total);
    let window: Vec<Line<'static>> = summary_lines[start..end]
        .iter()
        .map(|l| Line::from(l.clone()))
        .collect();

    let at_end = state.approval_at_end(visible);
    let pos = if at_end {
        Line::from(Span::styled(
            format!("滚动 {}/{}(已完整查看)", end, total),
            Style::default().green(),
        ))
    } else {
        Line::from(format!(
            "滚动 {}/{} 行 —— 审批前请滚动到末尾确认全部内容(↑/↓ 或 PgUp/PgDn)",
            end, total
        ))
    };
    let mut text = vec![
        Line::from(format!("审批请求 {}(下方为将执行的完整内容)", req.call_id)),
        Line::from(""),
    ];
    text.extend(window);
    text.push(pos);
    text.push(Line::from("a 批准 · d 拒绝 · x 中断任务"));

    f.render_widget(
        Paragraph::new(text).block(Block::new().borders(Borders::ALL).title("审批").bold()),
        area,
    );
}

fn render_form_modal(f: &mut Frame, state: &TuiState) {
    let area = centered_rect(70, 30, f.area());
    f.render_widget(Clear, area);
    let field = |active: bool, label: &str, value: &str| -> Line<'static> {
        let (l, r) = if active { ("> ", " <") } else { ("  ", "  ") };
        let mut line = Line::from(Span::raw(format!("{l}{label}: {value}{r}")));
        if active {
            line.style = Style::default().add_modifier(Modifier::REVERSED);
        }
        line
    };
    let text = vec![
        field(
            state.form.field == FormField::Title,
            "title",
            &state.form.title,
        ),
        field(
            state.form.field == FormField::Brief,
            "brief file",
            &state.form.brief,
        ),
        Line::from("Tab 切换 · Enter 提交 · Esc 取消"),
    ];
    f.render_widget(
        Paragraph::new(text).block(Block::new().borders(Borders::ALL).title("新建任务")),
        area,
    );
}

/// 按显示宽度把文本折行(CJK 每字符占一格),返回总行数;
/// 弹窗/事件流的"总行数"由本函数与渲染使用同一实现,保证一致
fn wrap_lines(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.split('\n') {
        if para.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut cur = String::new();
        for c in para.chars() {
            cur.push(c);
            if cur.chars().count() >= width {
                out.push(std::mem::take(&mut cur));
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

// ---- 渲染测试支撑与快照式断言 ----

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    pub(crate) fn render_to_string(state: &mut TuiState) -> String {
        render_rows(state).join("")
    }

    /// 按 buffer 行切块并去除空格(CJK 每字符占一格,join 后字间出现空格)
    fn render_rows(state: &mut TuiState) -> Vec<String> {
        const WIDTH: u16 = 100;
        let backend = TestBackend::new(WIDTH, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, state)).unwrap();
        let buf = terminal.backend().buffer();
        let text: String = buf.content.iter().map(|c| c.symbol()).collect();
        text.chars()
            .collect::<Vec<_>>()
            .chunks(WIDTH as usize)
            .map(|row| row.iter().collect::<String>().replace(' ', ""))
            .collect()
    }

    fn summary(
        _id: &str,
        status: zealous_core::TaskStatus,
        title: &str,
    ) -> zealous_engine::task_service::TaskSummary {
        zealous_engine::task_service::TaskSummary {
            task_id: zealous_core::TaskId::generate(),
            status,
            title: title.into(),
        }
    }

    #[test]
    fn empty_task_list_renders_placeholder() {
        let mut s = TuiState::new();
        s.refresh_tasks(vec![]);
        let text = render_to_string(&mut s);
        assert!(text.contains("(无任务)"), "{text}");
    }

    #[test]
    fn task_list_renders_entries_and_selection() {
        let mut s = TuiState::new();
        s.refresh_tasks(vec![
            summary("task_aaa", zealous_core::TaskStatus::Queued, "first"),
            summary("task_bbb", zealous_core::TaskStatus::Running, "second"),
        ]);
        let text = render_to_string(&mut s);
        assert!(text.contains("first"));
        assert!(text.contains("second"));
        assert!(text.contains("Running"));
        // 选中态:所在行以 > 开头(去空格后)
        let row = render_rows(&mut s)
            .into_iter()
            .find(|r| r.contains("first"))
            .expect("first row");
        assert!(
            row.trim_matches('│').starts_with('>'),
            "选中行应有标记: {row}"
        );
    }

    #[test]
    fn detail_renders_projection_fields_and_error_bottom() {
        use zealous_core::TaskId;
        let mut s = TuiState::new();
        s.refresh_tasks(vec![summary(
            "task_x",
            zealous_core::TaskStatus::Failed,
            "broken task",
        )]);
        s.selected = 0;
        let proj = zealous_core::TaskProjection {
            task_id: TaskId::generate(),
            title: "broken task".into(),
            repo_root: "C:/r".into(),
            status: zealous_core::TaskStatus::Failed,
            plan: Some("step1".into()),
            brief: Some("brief text".into()),
            runs: Vec::new(),
            tool_calls: vec![zealous_core::projection::ToolCallView {
                call_id: "call_1".into(),
                run_id: "run_1".into(),
                tool: "shell".into(),
                args_json: "{}".into(),
                risk: zealous_core::RiskTier::T2,
                state: zealous_core::projection::ToolCallState::Rejected,
            }],
            pending_approvals: Vec::new(),
            last_error: Some(("orchestrator".into(), "验证失败".into())),
            execution_workspace: None,
            applied_commit: None,
            applied_strategy: None,
            merge_commit: None,
            merge_workspace: None,
            merge_conflicts: Vec::new(),
            workspace_removed: false,
            interrupted_count: 1,
            last_event_seq: 9,
        };
        s.set_detail(proj, vec![]);
        s.screen = Screen::Detail;
        let text = render_to_string(&mut s);
        // 去空格比较(CJK 渲染占格)
        let strip = |t: &str| t.replace(' ', "");
        for needle in ["brokentask", "brieftext", "step1", "shell", "验证失败"] {
            assert!(text.contains(&strip(needle)), "详情缺少 {needle}: {text}");
        }
    }

    #[test]
    fn approval_modal_shows_summary() {
        let mut s = TuiState::new();
        s.approval_arrived(super::super::state::ApprovalRequestUi {
            task_id: "task_9".into(),
            call_id: "call_9".into(),
            summary: "write_file 新建文件: out.txt\n--- 完整新内容 ---\nbody".into(),
        });
        // 去空格比较(CJK 渲染占格)
        let text = render_to_string(&mut s).replace(' ', "");
        assert!(text.contains("task_9"), "{text}");
        assert!(text.contains("call_9"), "{text}");
        assert!(text.contains("out.txt"), "{text}");
        assert!(text.contains("body"), "{text}");
        assert!(text.contains("a批准"), "{text}");
    }

    #[test]
    fn error_does_not_hide_key_state() {
        let mut s = TuiState::new();
        s.refresh_tasks(vec![summary(
            "task_aaa",
            zealous_core::TaskStatus::Completed,
            "visible",
        )]);
        s.set_error("配置无效".into());
        let text = render_to_string(&mut s);
        assert!(text.contains("配置无效"), "错误应显示: {text}");
        assert!(text.contains("visible"), "错误不应遮蔽任务列表: {text}");
    }
}
