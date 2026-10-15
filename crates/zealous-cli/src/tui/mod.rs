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
    state.refresh_tasks(list_tasks(&svc));

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
            match ev {
                TuiEvent::Approval(req) => state.approval_arrived(state::ApprovalRequestUi {
                    call_id: req.call_id,
                    summary: req.summary,
                }),
                TuiEvent::Finished(status) => {
                    state.task_finished(status);
                    decision_tx = None;
                    // 运行结束即刷新投影与列表
                    if let Some(t) = state.selected_task() {
                        let id = t.task_id.clone();
                        if let Ok(p) = svc.refresh(&id) {
                            if let Ok((evs, _)) = svc.events(&id) {
                                state.set_detail(p, evs);
                            }
                        }
                        state.refresh_tasks(list_tasks(&svc));
                    }
                }
                TuiEvent::Failure(msg) => {
                    state.running = false;
                    state.set_error(msg);
                }
            }
        }
        // 2) 消费进度通道(runner 的 sink 适配层)
        while let Ok(msg) = progress_rx.try_recv() {
            match msg {
                progress::ProgressMsg::Progress(l) => state.push_progress(l),
                progress::ProgressMsg::Important(l) => state.push_important(&l),
            }
        }

        // 3) 绘制
        let _ = terminal.draw(|f| views::render(f, &state));

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
                        state.refresh_tasks(list_tasks(svc));
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
        Screen::Detail => match key.code {
            KeyCode::Char('q') => {
                state.request_quit();
            }
            KeyCode::Esc => state.screen = Screen::List,
            KeyCode::Char('e') => state.toggle_events(),
            KeyCode::Char('x') => {
                state.request_abort();
                abort.store(true, Ordering::Relaxed);
            }
            KeyCode::Char('R') => {
                if !state.running {
                    let id = state.selected_task().map(|t| t.task_id.clone());
                    if let Some(id) = id {
                        state.running = true;
                        state.abort_requested = false;
                        abort.store(false, Ordering::Relaxed);
                        let (approval_sink, request_rx, decision_tx_ch) =
                            approval::channel(abort.clone());
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
