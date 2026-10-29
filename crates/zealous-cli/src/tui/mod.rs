//! `zealous tui` 入口与终端生命周期(spec 90)。
//! 薄客户端:任务事实来自投影/账本,审批与进度经 channel 进入 engine。

pub mod approval;
pub mod progress;
pub mod runner;
pub mod state;
pub mod views;

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;
use zealous_core::ApprovalDecision;
use zealous_engine::task_service::TaskService;

use self::runner::TuiEvent;
use self::state::{Screen, TuiState};

/// 终端守卫:Drop 时恢复 raw mode / alternate screen;
/// panic 路径经 hook 恢复(spec 90 §3)。
struct TerminalGuard;

impl TerminalGuard {
    fn enable() -> io::Result<Self> {
        enable_raw_mode()?;
        crossterm::execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen);
    let _ = io::stdout().flush();
}

pub fn run_command(repo_root: PathBuf) -> i32 {
    let svc = TaskService::new(repo_root);

    // panic hook:先恢复终端再交回默认打印
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));

    let guard = match TerminalGuard::enable() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("终端初始化失败: {e}");
            return 1;
        }
    };

    let abort = Arc::new(AtomicBool::new(false));
    let (event_tx, event_rx) = mpsc::channel();
    let (progress_sink, progress_rx) = progress::channel();
    let progress_sink: Arc<dyn zealous_engine::progress::ProgressSink> = Arc::new(progress_sink);

    let mut state = TuiState::new();
    refresh_task_list(&mut state, &svc);
    // 启动校验(spec 90 收口):Provider 配置错误进错误弹窗,
    // 不阻塞浏览既有任务;运行 R 时会再次解析并失败
    if let Err(e) = zealous_engine::provider::ProviderConfig::resolve() {
        state.set_error(format!(
            "provider 配置无效: {e}(运行任务会失败;请修 .env 或 ~/.zealous/config.json)"
        ));
    }

    let backend = ratatui::backend::CrosstermBackend::new(io::stdout());
    let mut terminal = match Terminal::new(backend) {
        Ok(t) => t,
        Err(e) => {
            drop(guard);
            eprintln!("终端后端初始化失败: {e}");
            return 1;
        }
    };

    // 每次运行(R)持有的决策发送端
    let mut decision_tx: Option<mpsc::Sender<ApprovalDecision>> = None;

    loop {
        // 1) 消费 engine 事件
        while let Ok(ev) = event_rx.try_recv() {
            apply_event(&mut state, &svc, &mut decision_tx, ev);
        }
        // 2) 消费进度通道(runner 的 sink 适配层)
        while let Ok(msg) = progress_rx.try_recv() {
            match msg {
                progress::ProgressMsg::Progress(l) => state.push_progress(l),
                progress::ProgressMsg::Important(l) => state.push_important(&l),
            }
        }

        // 3) 绘制
        let _ = terminal.draw(|f| views::render(f, &mut state));

        // 4) 退出判定:已请求退出且 engine 不再运行
        if state.quit && !state.running {
            break;
        }

        // 5) 输入(100ms 轮询,保证重绘频率)
        if crossterm::event::poll(Duration::from_millis(100)).unwrap_or(false) {
            match crossterm::event::read() {
                Ok(Event::Key(key)) => {
                    handle_key(
                        &mut state,
                        &svc,
                        &mut decision_tx,
                        &event_tx,
                        &progress_sink,
                        &abort,
                        key,
                    );
                }
                Ok(Event::Resize(_, _)) => { /* 下轮重绘 */ }
                _ => {}
            }
        }
    }

    drop(terminal);
    drop(guard);
    0
}

fn list_tasks(svc: &TaskService) -> Vec<zealous_engine::task_service::TaskSummary> {
    svc.list_tasks().map(|(items, _)| items).unwrap_or_default()
}

/// 刷新任务列表;损坏账本计入错误栏(spec 90 场景 8:错误可见且不崩溃)
fn refresh_task_list(state: &mut TuiState, svc: &TaskService) {
    match svc.list_tasks() {
        Ok((items, skipped)) => {
            state.refresh_tasks(items);
            if !skipped.is_empty() {
                let reasons: Vec<String> = skipped
                    .iter()
                    .map(|(id, why)| {
                        let short: String = why.chars().take(120).collect();
                        format!("{}: {}", id, short)
                    })
                    .collect();
                state.set_error(format!(
                    "{} 个损坏账本已跳过(不崩溃) — {}",
                    skipped.len(),
                    reasons.join(" | ")
                ));
            }
        }
        Err(e) => state.set_error(format!("任务列表读取失败: {e}")),
    }
}

/// 滚动路由:审批弹窗优先,其次已展开的事件流
fn scroll_view(state: &mut TuiState, page: usize, down: bool) {
    if state.pending_approval.is_some() {
        if down {
            state.approval_scroll_down(page);
        } else {
            state.approval_scroll_up(page);
        }
    } else if state.events_expanded {
        if down {
            state.events_scroll_down(page);
        } else {
            state.events_scroll_up(page);
        }
    }
}

/// engine 事件 → TUI 状态的应用逻辑(spec 90:控制器与渲染分离,
/// 集成测试直接驱动本函数,与终端循环共享同一实现)
pub(crate) fn apply_event(
    state: &mut TuiState,
    svc: &TaskService,
    decision_tx: &mut Option<mpsc::Sender<ApprovalDecision>>,
    ev: TuiEvent,
) {
    match ev {
        TuiEvent::ApprovalChannel(tx) => *decision_tx = Some(tx),
        TuiEvent::Approval(req) => state.approval_arrived(state::ApprovalRequestUi {
            task_id: req.task_id,
            call_id: req.call_id,
            summary: req.summary,
        }),
        TuiEvent::Finished(status) => {
            state.task_finished(status);
            *decision_tx = None;
            // 运行结束即刷新投影与列表(事实只来自 TaskService)
            let id = state.selected_task().map(|t| t.task_id.clone());
            if let Some(id) = id {
                if let Ok(p) = svc.refresh(&id) {
                    if let Ok((evs, _)) = svc.events(&id) {
                        state.set_detail(p, evs);
                    }
                }
                refresh_task_list(state, svc);
            }
        }
        TuiEvent::Failure(msg) => {
            state.running = false;
            state.set_error(msg);
        }
        TuiEvent::ParallelFinished(summary) => {
            state.running = false;
            *decision_tx = None;
            state.push_important(&format!("parallel: {summary}"));
            refresh_task_list(state, svc);
        }
        TuiEvent::Progress(msg) => {
            state.push_progress(msg);
        }
        TuiEvent::QueueFinished(summary) => {
            state.running = false;
            *decision_tx = None;
            state.push_important(&format!("queue: {summary}"));
            refresh_task_list(state, svc);
        }
    }
}

fn format_resolution(proposal: &zealous_engine::merge_resolution::Proposal) -> String {
    use zealous_engine::merge_resolution::ProposalFile;
    let mut text = format!(
        "resolution: {}\nsummary: {}\nproposed: {}\nmanual: {}\n\n",
        proposal.resolution_id,
        proposal.summary,
        proposal.resolved_files().join(", "),
        proposal.manual_files().join(", ")
    );
    for file in &proposal.files {
        match file {
            ProposalFile::Resolved {
                path,
                content,
                rationale,
            } => {
                text.push_str(&format!(
                    "FILE {path}\nrationale: {rationale}\nproposed:\n{content}\n\n"
                ));
            }
            ProposalFile::Manual { path, reason } => {
                text.push_str(&format!("FILE {path}\nmode: manual\nreason: {reason}\n\n",));
            }
        }
    }
    text
}

#[allow(clippy::too_many_arguments)]
fn handle_key(
    state: &mut TuiState,
    svc: &TaskService,
    decision_tx: &mut Option<mpsc::Sender<ApprovalDecision>>,
    event_tx: &mpsc::Sender<TuiEvent>,
    progress_sink: &Arc<dyn zealous_engine::progress::ProgressSink>,
    abort: &Arc<AtomicBool>,
    key: KeyEvent,
) {
    // 错误弹窗:任意键消除,且优先级最高
    if state.error.is_some() {
        state.dismiss_error();
        return;
    }
    // Ctrl-C:中断并退出
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        state.request_quit();
        return;
    }
    // spec 155:确认弹窗优先于普通键位(Enter 确认 / Esc 取消)
    if let Some(kind) = state.take_confirm() {
        match key.code {
            KeyCode::Esc => state.confirm = None,
            KeyCode::Enter => match kind {
                state::ConfirmKind::IsolatedRun => {
                    if !state.running {
                        if let Some(task_id) = state.selected_task().map(|t| t.task_id.clone()) {
                            state.running = true;
                            state.abort_requested = false;
                            abort.store(false, Ordering::Relaxed);
                            let (approval_sink, request_rx, decision_tx_ch) =
                                approval::channel(task_id.as_str().to_string(), abort.clone());
                            let fwd_tx = event_tx.clone();
                            std::thread::spawn(move || {
                                for req in request_rx {
                                    if fwd_tx.send(TuiEvent::Approval(req)).is_err() {
                                        break;
                                    }
                                }
                            });
                            *decision_tx = Some(decision_tx_ch);
                            runner::spawn_run(
                                TaskService::new(svc.repo_root().to_path_buf()),
                                task_id,
                                event_tx.clone(),
                                Arc::new(approval_sink),
                                progress_sink.clone(),
                                abort.clone(),
                                true,
                            );
                        }
                    }
                }
                state::ConfirmKind::MergeWorkspace => {
                    if let Some(task_id) = state.selected_task().map(|t| t.task_id.clone()) {
                        match svc.open_task(&task_id) {
                            Ok((ledger, _proj, _)) => {
                                match zealous_engine::merge_workspace::start(
                                    svc.repo_root(),
                                    &task_id,
                                    &ledger,
                                ) {
                                    Ok(zealous_engine::merge_workspace::StartOutcome::Integrated {
                                        branch,
                                        merge_commit,
                                        task_tip,
                                        ..
                                    }) => state.push_important(&format!(
                                        "merged workspace: {branch} @ {merge_commit} (tip {task_tip})"
                                    )),
                                    Ok(zealous_engine::merge_workspace::StartOutcome::Conflicted {
                                        path,
                                        files,
                                        ..
                                    }) => {
                                        state.push_important(&format!(
                                            "merge conflicts retained: {} ({})",
                                            path.display(),
                                            files.join(", ")
                                        ));
                                    }
                                    Err(e) => state.push_important(&format!(
                                        "merge workspace 失败: {e}"
                                    )),
                                }
                            }
                            Err(e) => state.push_important(&format!("打开任务失败: {e}")),
                        }
                    }
                }
                state::ConfirmKind::MergeResolutionApprove => {
                    let proposal = state.merge_resolution_proposal.clone();
                    if let (Some(task_id), Some(proposal)) =
                        (state.selected_task().map(|t| t.task_id.clone()), proposal)
                    {
                        match svc.open_task(&task_id) {
                            Ok((ledger, proj, _)) => {
                                let Some(merge_ws) = proj.merge_workspace.clone() else {
                                    state.set_error("merge workspace 不存在".into());
                                    return;
                                };
                                match zealous_engine::merge_resolution::apply_proposal(
                                    std::path::Path::new(&merge_ws.path),
                                    &ledger,
                                    &proposal,
                                ) {
                                    Ok((staged, manual)) => state.push_important(&format!(
                                        "resolution staged: {}; manual: {}",
                                        staged.join(", "),
                                        manual.join(", ")
                                    )),
                                    Err(e) => state.set_error(e.to_string()),
                                }
                            }
                            Err(e) => state.set_error(e.to_string()),
                        }
                    }
                }
                state::ConfirmKind::MergeResolutionReject => {
                    let proposal = state.merge_resolution_proposal.clone();
                    if let (Some(task_id), Some(proposal)) =
                        (state.selected_task().map(|t| t.task_id.clone()), proposal)
                    {
                        match svc.open_task(&task_id) {
                            Ok((ledger, _, _)) => {
                                match zealous_engine::merge_resolution::reject(
                                    &ledger,
                                    &proposal.resolution_id,
                                    Some("TUI user rejected".into()),
                                ) {
                                    Ok(()) => {
                                        state.merge_resolution_proposal = None;
                                        state.push_important("resolution rejected");
                                    }
                                    Err(e) => state.set_error(e.to_string()),
                                }
                            }
                            Err(e) => state.set_error(e.to_string()),
                        }
                    }
                }
                state::ConfirmKind::MergeContinue => {
                    if let Some(task_id) = state.selected_task().map(|t| t.task_id.clone()) {
                        match svc.open_task(&task_id) {
                            Ok((ledger, _proj, _)) => {
                                match zealous_engine::merge_workspace::continue_merge(
                                    svc.repo_root(),
                                    &ledger,
                                ) {
                                    Ok(commit) => {
                                        state.push_important(&format!("merge continued: {commit}"))
                                    }
                                    Err(e) => {
                                        state.push_important(&format!("merge continue 失败: {e}"))
                                    }
                                }
                            }
                            Err(e) => state.push_important(&format!("打开任务失败: {e}")),
                        }
                    }
                }
                state::ConfirmKind::MergeAbort => {
                    if let Some(task_id) = state.selected_task().map(|t| t.task_id.clone()) {
                        match svc.open_task(&task_id) {
                            Ok((ledger, _proj, _)) => {
                                match zealous_engine::merge_workspace::abort(
                                    svc.repo_root(),
                                    &ledger,
                                    false,
                                ) {
                                    Ok(()) => state.push_important("merge workspace aborted"),
                                    Err(e) => {
                                        state.push_important(&format!("merge abort 失败: {e}"))
                                    }
                                }
                            }
                            Err(e) => state.push_important(&format!("打开任务失败: {e}")),
                        }
                    }
                }
                state::ConfirmKind::Merge => {
                    if let Some(task_id) = state.selected_task().map(|t| t.task_id.clone()) {
                        match svc.open_task(&task_id) {
                            Ok((ledger, _proj, _)) => {
                                match zealous_engine::workspace::merge(svc.repo_root(), &ledger) {
                                    Ok(out) => state.push_important(&format!(
                                        "merged: {} @ {} (task tip {})",
                                        out.branch, out.merge_commit, out.task_tip
                                    )),
                                    Err(e) => state.push_important(&format!("merge 失败: {e}")),
                                }
                            }
                            Err(e) => state.push_important(&format!("打开任务失败: {e}")),
                        }
                    }
                }
                state::ConfirmKind::Apply => {
                    if let Some(task_id) = state.selected_task().map(|t| t.task_id.clone()) {
                        match svc.open_task(&task_id) {
                            Ok((ledger, _proj, _)) => {
                                match zealous_engine::workspace::apply(svc.repo_root(), &ledger) {
                                    Ok(out) => state.push_important(&format!(
                                        "applied: {} @ {}{}",
                                        out.branch,
                                        out.applied_commit,
                                        if out.already { " (already)" } else { "" }
                                    )),
                                    Err(e) => state.push_important(&format!("apply 失败: {e}")),
                                }
                            }
                            Err(e) => state.push_important(&format!("打开任务失败: {e}")),
                        }
                    }
                }
                state::ConfirmKind::ParallelRun => {
                    if !state.running {
                        let ids = state.parallel_ids.clone();
                        if !ids.is_empty() {
                            state.running = true;
                            state.abort_requested = false;
                            abort.store(false, Ordering::Relaxed);
                            runner::spawn_parallel(
                                TaskService::new(svc.repo_root().to_path_buf()),
                                ids,
                                event_tx.clone(),
                                progress_sink.clone(),
                                abort.clone(),
                            );
                        }
                    }
                }
                state::ConfirmKind::ParallelIntegrate => {
                    if !state.running {
                        let ids = state.parallel_ids.clone();
                        if !ids.is_empty() {
                            state.running = true;
                            runner::spawn_parallel_integrate(
                                TaskService::new(svc.repo_root().to_path_buf()),
                                ids,
                                event_tx.clone(),
                                progress_sink.clone(),
                            );
                        }
                    }
                }
                state::ConfirmKind::QueueRun => {
                    if !state.running {
                        let ids = state.queue_ids.clone();
                        if !ids.is_empty() {
                            state.running = true;
                            runner::spawn_queue(
                                TaskService::new(svc.repo_root().to_path_buf()),
                                ids,
                                event_tx.clone(),
                                progress_sink.clone(),
                                abort.clone(),
                            );
                        }
                    }
                }
                state::ConfirmKind::WorkspaceRemove => {
                    if let Some(task_id) = state.selected_task().map(|t| t.task_id.clone()) {
                        match svc.open_task(&task_id) {
                            Ok((ledger, _proj, _)) => {
                                match zealous_engine::workspace::remove(
                                    svc.repo_root(),
                                    &ledger,
                                    false,
                                ) {
                                    Ok(info) => state.push_important(&format!(
                                        "workspace removed: {}",
                                        info.branch
                                    )),
                                    Err(e) => {
                                        state.push_important(&format!("workspace remove 失败: {e}"))
                                    }
                                }
                            }
                            Err(e) => state.push_important(&format!("打开任务失败: {e}")),
                        }
                    }
                }
            },
            _ => {}
        }
        return;
    }
    if key.kind != crossterm::event::KeyEventKind::Press {
        return;
    }

    match state.screen {
        Screen::Form => match key.code {
            KeyCode::Esc => state.form_close(),
            KeyCode::Tab => state.form_toggle_field(),
            KeyCode::Backspace => state.form_backspace(),
            KeyCode::Enter => {
                if let Err(e) = state.form_validate() {
                    state.set_error(e);
                    return;
                }
                let brief_path = if state.form.brief.trim().is_empty() {
                    None
                } else {
                    Some(PathBuf::from(state.form.brief.trim()))
                };
                match svc.create_task_with_brief(state.form.title.trim(), brief_path.as_deref()) {
                    Ok(_) => {
                        refresh_task_list(state, svc);
                        state.form_close();
                    }
                    Err(e) => state.set_error(e.to_string()),
                }
            }
            KeyCode::Char(c) => state.form_char(c),
            _ => {}
        },
        Screen::List => match key.code {
            KeyCode::Char('q') => state.request_quit(),
            KeyCode::Down | KeyCode::Char('j') => state.select_next(),
            KeyCode::Up | KeyCode::Char('k') => state.select_prev(),
            KeyCode::Char('r') => state.refresh_tasks(list_tasks(svc)),
            KeyCode::Char('n') => state.open_form(),
            KeyCode::Char('A') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                // spec 157/161:确认后并行运行最多三个 runnable queued tasks
                match zealous_engine::parallel::select_runnable(
                    svc,
                    zealous_engine::parallel::MAX_PARALLEL_TASKS,
                ) {
                    Ok((runnable, _)) => {
                        if runnable.is_empty() {
                            state.set_error("(no runnable queued tasks)".into());
                        } else {
                            state.parallel_ids = runnable;
                            state.open_confirm(state::ConfirmKind::ParallelRun);
                        }
                    }
                    Err(e) => state.set_error(e.clone()),
                }
            }
            KeyCode::Char('I') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                let mut integrable = Vec::new();
                for summary in list_tasks(svc) {
                    if integrable.len() == zealous_engine::parallel::MAX_PARALLEL_TASKS {
                        break;
                    }
                    if summary.status != zealous_core::TaskStatus::Completed {
                        continue;
                    }
                    if let Ok((_, proj, _)) = svc.open_task(&summary.task_id) {
                        let eligible = proj.execution_workspace.is_some()
                            && proj.applied_commit.is_none()
                            && !proj.workspace_removed
                            && proj
                                .merge_workspace
                                .as_ref()
                                .map(|w| w.status != "conflicted" && w.status != "resolved")
                                .unwrap_or(true)
                            && proj
                                .execution_workspace
                                .as_ref()
                                .map(|w| std::path::Path::new(&w.path).exists())
                                .unwrap_or(false);
                        if eligible {
                            integrable.push(summary.task_id);
                        }
                    }
                }
                if integrable.is_empty() {
                    state.set_error("(no integrable completed tasks)".into());
                } else {
                    state.parallel_ids = integrable;
                    state.open_confirm(state::ConfirmKind::ParallelIntegrate);
                }
            }
            KeyCode::Char('a') => {
                // spec 156:确认后串行运行全部可运行 queued 任务(auto apply)
                match zealous_engine::queue::select_runnable(svc) {
                    Ok((runnable, _)) => {
                        if runnable.is_empty() {
                            state.set_error("(no runnable queued tasks)".into());
                        } else {
                            state.queue_ids = runnable;
                            state.open_confirm(state::ConfirmKind::QueueRun);
                        }
                    }
                    Err(e) => state.set_error(e.clone()),
                }
            }
            KeyCode::Char('Q') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                match zealous_engine::parallel::select_runnable(
                    svc,
                    zealous_engine::parallel::MAX_PARALLEL_TASKS,
                ) {
                    Ok((runnable, not_runnable)) => {
                        let mut text = String::new();
                        if zealous_engine::lease::is_held(svc.repo_root()) {
                            if let Some(info) = zealous_engine::lease::peek_info(svc.repo_root()) {
                                text.push_str(&format!(
                                    "active: pid {} task {} mode {}\n",
                                    info.pid, info.task_id, info.mode
                                ));
                                if info.mode == "parallel" {
                                    text.push_str(&format!(
                                        "tasks: {}\n",
                                        info.task_ids.join(", ")
                                    ));
                                }
                            }
                        } else {
                            text.push_str("active: (none)\n");
                        }
                        text.push_str(&format!("runnable parallel ({}):\n", runnable.len()));
                        for id in &runnable {
                            text.push_str(&format!("  {}\n", id.as_str()));
                        }
                        if !not_runnable.is_empty() {
                            text.push_str("not runnable:\n");
                            for (id, reason) in &not_runnable {
                                text.push_str(&format!("  {}  {reason}\n", id.as_str()));
                            }
                        }
                        state.parallel_status_text = Some(text);
                        state.screen = Screen::Parallel;
                    }
                    Err(e) => state.set_error(e.clone()),
                }
            }
            KeyCode::Char('Q') => match zealous_engine::queue::select_runnable(svc) {
                Ok((runnable, not_runnable)) => {
                    let mut text = String::new();
                    if zealous_engine::lease::is_held(svc.repo_root()) {
                        if let Some(info) = zealous_engine::lease::peek_info(svc.repo_root()) {
                            text.push_str(&format!(
                                "active: pid {} task {} mode {}\n",
                                info.pid, info.task_id, info.mode
                            ));
                        }
                    } else {
                        text.push_str("active: (none)\n");
                    }
                    text.push_str(&format!("runnable ({}):\n", runnable.len()));
                    for id in &runnable {
                        text.push_str(&format!("  {}\n", id.as_str()));
                    }
                    if !not_runnable.is_empty() {
                        text.push_str("not runnable:\n");
                        for (id, reason) in &not_runnable {
                            text.push_str(&format!("  {}  {reason}\n", id.as_str()));
                        }
                    }
                    state.queue_status_text = Some(text);
                    state.screen = Screen::Queue;
                }
                Err(e) => state.set_error(e.clone()),
            },
            KeyCode::Enter => {
                let id = state.selected_task().map(|t| t.task_id.clone());
                if let Some(id) = id {
                    if let Ok(proj) = svc.refresh(&id) {
                        let (evs, _) = svc
                            .events(&id)
                            .unwrap_or_else(|_| (Vec::new(), Default::default()));
                        state.set_detail(proj, evs);
                        state.screen = Screen::Detail;
                    }
                }
            }
            _ => {}
        },
        Screen::MergeStatus => match key.code {
            KeyCode::Esc => state.screen = Screen::List,
            KeyCode::Char('q') => state.request_quit(),
            KeyCode::Char('R') => {
                if let Some(task_id) = state.selected_task().map(|t| t.task_id.clone()) {
                    match svc.open_task(&task_id) {
                        Ok((ledger, proj, _)) => {
                            let Some(merge_ws) = proj.merge_workspace.clone() else {
                                state.set_error("merge workspace 不存在".into());
                                return;
                            };
                            let config = zealous_engine::provider::ProviderConfig::resolve();
                            match config {
                                Ok(config) => {
                                    let provider =
                                        zealous_engine::provider::OpenAiCompatProvider::new(
                                            config.clone(),
                                        );
                                    match zealous_engine::merge_resolution::propose(
                                        std::path::Path::new(&merge_ws.path),
                                        &ledger,
                                        &provider,
                                        &config.model,
                                    ) {
                                        Ok(proposal) => {
                                            state.merge_resolution_proposal =
                                                Some(proposal.clone());
                                            state.merge_status_text =
                                                Some(format_resolution(&proposal));
                                        }
                                        Err(e) => state.set_error(e.to_string()),
                                    }
                                }
                                Err(e) => state.set_error(e.to_string()),
                            }
                        }
                        Err(e) => state.set_error(e.to_string()),
                    }
                }
            }
            KeyCode::Enter => {
                if let Some(proposal) = &state.merge_resolution_proposal {
                    state.merge_status_text = Some(format_resolution(proposal));
                }
            }
            KeyCode::Char('A') => {
                if state.merge_resolution_proposal.is_some() {
                    state.open_confirm(state::ConfirmKind::MergeResolutionApprove);
                }
            }
            KeyCode::Char('D') => {
                if state.merge_resolution_proposal.is_some() {
                    state.open_confirm(state::ConfirmKind::MergeResolutionReject);
                }
            }
            KeyCode::Char('S') => {
                if let Some(task_id) = state.selected_task().map(|t| t.task_id.clone()) {
                    if let Ok((ledger, _, _)) = svc.open_task(&task_id) {
                        if let Ok(status) =
                            zealous_engine::merge_workspace::status(svc.repo_root(), &ledger)
                        {
                            state.merge_status_text = Some(format!(
                                "status: {}\nunresolved: {}\n",
                                status.status,
                                status.unresolved_files.join(", ")
                            ));
                        }
                    }
                }
            }
            _ => {}
        },
        Screen::Queue | Screen::Parallel => match key.code {
            KeyCode::Char('q') | KeyCode::Esc => state.screen = Screen::List,
            _ => {}
        },
        Screen::Detail => match key.code {
            KeyCode::Char('q') => {
                state.request_quit();
            }
            KeyCode::Esc => state.screen = Screen::List,
            KeyCode::Char('e') => state.toggle_events(),
            // 滚动:审批弹窗优先,其次事件流(spec 90 收口)
            KeyCode::Up => scroll_view(state, 1, false),
            KeyCode::PageUp => scroll_view(state, 10, false),
            KeyCode::Down => scroll_view(state, 1, true),
            KeyCode::PageDown => scroll_view(state, 10, true),
            KeyCode::Char('x') => {
                state.request_abort();
                abort.store(true, Ordering::Relaxed);
            }
            KeyCode::Char('I') => {
                if !state.running {
                    state.open_confirm(state::ConfirmKind::IsolatedRun);
                }
            }
            KeyCode::Char('P') => {
                state.open_confirm(state::ConfirmKind::Apply);
            }
            KeyCode::Char('m') => {
                state.open_confirm(state::ConfirmKind::Merge);
            }
            KeyCode::Char('M') => {
                state.open_confirm(state::ConfirmKind::MergeWorkspace);
            }
            KeyCode::Char('S') => {
                if let Some(task_id) = state.selected_task().map(|t| t.task_id.clone()) {
                    match svc.open_task(&task_id) {
                        Ok((ledger, _proj, _)) => {
                            match zealous_engine::merge_workspace::status(svc.repo_root(), &ledger)
                            {
                                Ok(status) => {
                                    let mut text = format!(
                                        "status: {}\nworkspace: {}\nmerge branch: {}\nmain head: {}\ntask tip: {}\n",
                                        status.status,
                                        status.path,
                                        status.branch,
                                        status.main_head,
                                        status.task_tip
                                    );
                                    text.push_str("unresolved files:\n");
                                    if status.unresolved_files.is_empty() {
                                        text.push_str("  (none)\n");
                                    }
                                    for file in status.unresolved_files {
                                        text.push_str(&format!("  {file}\n"));
                                    }
                                    state.merge_status_text = Some(text);
                                    state.screen = Screen::MergeStatus;
                                }
                                Err(e) => state.set_error(e.to_string()),
                            }
                        }
                        Err(e) => state.set_error(e.to_string()),
                    }
                }
            }
            KeyCode::Char('C') => {
                state.open_confirm(state::ConfirmKind::MergeContinue);
            }
            KeyCode::Char('A') => {
                state.open_confirm(state::ConfirmKind::MergeAbort);
            }
            KeyCode::Char('D') if state.workspace_visible => {
                state.open_confirm(state::ConfirmKind::WorkspaceRemove);
            }
            KeyCode::Char('W') => {
                state.toggle_workspace();
            }
            KeyCode::Char('R') => {
                if !state.running {
                    let id = state.selected_task().map(|t| t.task_id.clone());
                    if let Some(id) = id {
                        state.running = true;
                        state.abort_requested = false;
                        abort.store(false, Ordering::Relaxed);
                        let (approval_sink, request_rx, decision_tx_ch) =
                            approval::channel(id.as_str().to_string(), abort.clone());
                        // 审批请求转发进统一事件流
                        let fwd_tx = event_tx.clone();
                        std::thread::spawn(move || {
                            for req in request_rx {
                                if fwd_tx.send(TuiEvent::Approval(req)).is_err() {
                                    break;
                                }
                            }
                        });
                        *decision_tx = Some(decision_tx_ch);
                        runner::spawn_run(
                            TaskService::new(svc.repo_root().to_path_buf()),
                            id,
                            event_tx.clone(),
                            Arc::new(approval_sink),
                            progress_sink.clone(),
                            abort.clone(),
                            false,
                        );
                    }
                }
            }
            KeyCode::Char('a') | KeyCode::Char('d') if state.pending_approval.is_some() => {
                if let Some(req) = state.take_pending_approval() {
                    let decision = if key.code == KeyCode::Char('a') {
                        ApprovalDecision::Approve
                    } else {
                        ApprovalDecision::Reject
                    };
                    let _ = req;
                    if let Some(tx) = decision_tx {
                        let _ = tx.send(decision);
                    }
                }
            }
            _ => {}
        },
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::path::Path;
    use std::sync::atomic::AtomicBool;
    use zealous_core::TaskStatus;

    /// 本地 mock /chat/completions:按请求顺序返回脚本(spec 60 §6 同款)
    fn mock_endpoint(script: Vec<String>) -> String {
        let counter = std::sync::atomic::AtomicU32::new(0);
        let script = std::sync::Mutex::new(script);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || loop {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let mut raw: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                let n = match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                raw.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&raw);
                if let Some(h) = text.find("\r\n\r\n") {
                    let len = text
                        .split("\r\n")
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if raw.len() >= h + 4 + len {
                        break;
                    }
                }
            }
            let idx = counter.fetch_add(1, Ordering::Relaxed) as usize;
            let content = {
                let script = script.lock().unwrap();
                if idx < script.len() {
                    script[idx].clone()
                } else {
                    "guard".into()
                }
            };
            let body = serde_json::json!({
                "choices": [{ "message": { "content": content } }]
            })
            .to_string();
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .as_bytes(),
            );
            let _ = stream.shutdown(std::net::Shutdown::Write);
        });
        format!("http://{addr}/v1")
    }

    fn git(repo: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {:?}", out.stderr);
    }

    #[test]
    fn tui_controller_drives_mock_run_to_completed() {
        // ---- 夹具:真实 git 仓库 + 初始 commit + README ----
        let repo = std::env::temp_dir().join(format!(
            "zealous-tui-e2e-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        );

        // ---- Provider 配置指向 mock endpoint ----
        let endpoint = mock_endpoint(vec![
            r#"{"action":"plan","plan":"read then edit","workers":[]}"#.into(),
            r#"{"action":"tool","tool":"read_file","args":{"path":"README.md"}}"#.into(),
            r#"{"action":"tool","tool":"edit_file","args":{"path":"README.md","old_text":"fixture","new_text":"fixture by tui"}}"#.into(),
            r#"{"action":"message","text":"tui run done"}"#.into(),
        ]);
        std::env::set_var("ZEALOUS_BASE_URL", &endpoint);
        std::env::set_var("ZEALOUS_API_KEY", "tui-secret-key");
        std::env::set_var("ZEALOUS_MODEL", "mock-model");

        let svc = TaskService::new(&repo);
        let task_id = match svc.create_task("tui: read then edit README").unwrap() {
            zealous_engine::task_service::CreateOutcome::Ready { task_id, .. } => task_id,
            other => panic!("expected Ready: {other:?}"),
        };

        // ---- TUI 控制器接线(与 run_command 循环同构,无终端)----
        let abort = Arc::new(AtomicBool::new(false));
        let (event_tx, event_rx) = mpsc::channel();
        let (approval_sink, request_rx, decision_tx) =
            approval::channel("tui-it".into(), abort.clone());
        let (progress_sink, progress_rx) = progress::channel();
        // 审批请求转发(同 run_command 内的转发线程)
        {
            let tx = event_tx.clone();
            std::thread::spawn(move || {
                for req in request_rx {
                    if tx.send(TuiEvent::Approval(req)).is_err() {
                        break;
                    }
                }
            });
        }

        let mut state = TuiState::new();
        refresh_task_list(&mut state, &svc);
        state.select_next();
        let selected_id = state.selected_task().unwrap().task_id.clone();
        assert_eq!(selected_id, task_id);

        // ---- 模拟 R:启动运行 ----
        state.running = true;
        runner::spawn_run(
            TaskService::new(&repo),
            task_id.clone(),
            event_tx.clone(),
            Arc::new(approval_sink),
            Arc::new(progress_sink),
            abort.clone(),
            false,
        );

        // ---- 消费事件循环(同 apply_event),审批到达时模拟按键 a ----
        let mut decision_tx = Some(decision_tx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            while let Ok(ev) = event_rx.try_recv() {
                apply_event(&mut state, &svc, &mut decision_tx, ev);
            }
            while let Ok(msg) = progress_rx.try_recv() {
                match msg {
                    progress::ProgressMsg::Progress(l) => state.push_progress(l),
                    progress::ProgressMsg::Important(l) => state.push_important(&l),
                }
            }
            if state.pending_approval.is_some() {
                if let Some(_req) = state.take_pending_approval() {
                    if let Some(tx) = &decision_tx {
                        let _ = tx.send(zealous_core::ApprovalDecision::Approve);
                    }
                }
            }
            if !state.running {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("TUI run 30s 未完成");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            state.last_status,
            Some(TaskStatus::Completed),
            "最终状态: {:?}",
            state.error
        );

        // ---- 断言:文件修改 / 审批进账本 / seq 连续 / 投影 Completed ----
        assert_eq!(
            std::fs::read_to_string(repo.join("README.md")).unwrap(),
            "fixture by tui\n"
        );
        let (events, report) = svc.events(&task_id).unwrap();
        assert!(!report.torn_tail);
        for (i, e) in events.iter().enumerate() {
            assert_eq!(e.seq, (i + 1) as u64, "seq 跳号");
        }
        assert!(
            events.iter().any(|e| matches!(
                e.payload,
                zealous_core::EventPayload::ApprovalDecided {
                    decision: zealous_core::ApprovalDecision::Approve,
                    ..
                }
            )),
            "审批决策应进入账本"
        );
        // 最终状态来自投影(而非 TUI 私有状态)
        let proj = svc.refresh(&task_id).unwrap();
        assert_eq!(proj.status, TaskStatus::Completed);
        assert_eq!(state.detail.as_ref().unwrap().status, TaskStatus::Completed);
        assert!(state
            .detail
            .as_ref()
            .unwrap()
            .tool_calls
            .iter()
            .any(|c| c.tool == "edit_file"));

        // 密钥不落账
        let text = std::fs::read_to_string(zealous_engine::ledger::events_path(
            &zealous_engine::ledger::task_dir(&repo, &task_id),
        ))
        .unwrap();
        assert!(!text.contains("tui-secret-key"));
        std::env::remove_var("ZEALOUS_BASE_URL");
        std::env::remove_var("ZEALOUS_API_KEY");
        std::env::remove_var("ZEALOUS_MODEL");
        let _ = std::fs::remove_dir_all(repo);
    }
}
