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
        TuiEvent::Approval(req) => state.approval_arrived(state::ApprovalRequestUi {
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
                state.refresh_tasks(list_tasks(svc));
            }
        }
        TuiEvent::Failure(msg) => {
            state.running = false;
            state.set_error(msg);
        }
    }
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
            // 滚动:审批弹窗优先,其次事件流(spec 90 收口)
            KeyCode::Up => scroll_view(state, 1, false),
            KeyCode::PageUp => scroll_view(state, 10, false),
            KeyCode::Down => scroll_view(state, 1, true),
            KeyCode::PageDown => scroll_view(state, 10, true),
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
        let (approval_sink, request_rx, decision_tx) = approval::channel(abort.clone());
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
        state.refresh_tasks(list_tasks(&svc));
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
