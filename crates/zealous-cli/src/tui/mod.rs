//! `zealous tui` 入口与终端生命周期(spec 90)。
//! 薄客户端:任务事实来自投影/账本,审批与进度经 channel 进入 engine。

pub mod approval;
pub mod progress;
pub mod runner;
pub mod state;
pub mod transcript;
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
use zealous_engine::task_service::{CreateOutcome, TaskService};

use self::runner::TuiEvent;
use crate::commands;
use zealous_core::TaskId;
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
    // spec 165:启动即进聊天屏,先恢复最近会话(事实来自账本)
    restore_chat_session(&mut state, &svc);
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
        // 0) spec 173:先同步账本工具事件,保证卡片排在其后到达的流式文本之前
        maybe_sync_chat(&mut state, &svc);
        // 1) 消费 engine 事件
        while let Ok(ev) = event_rx.try_recv() {
            apply_event(&mut state, &svc, &mut decision_tx, ev);
        }
        // 2) 消费进度通道(runner 的 sink 适配层)
        while let Ok(msg) = progress_rx.try_recv() {
            match msg {
                progress::ProgressMsg::Progress(l) => {
                    // 工具进度行意味着账本刚落了工具事件
                    if state.chat_streaming && l.starts_with("工具") {
                        state.chat_sync_pending = true;
                    }
                    state.push_progress(l);
                }
                progress::ProgressMsg::Important(l) => state.push_important(&l),
            }
        }
        // 本轮出现工具信号则立即补一次同步,不等下个轮询周期
        maybe_sync_chat(&mut state, &svc);
        sync_abort_flag(&state, &abort);

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

/// 中断请求(state 标志)传播到 engine 的 abort 信号。
/// 此前只有详情屏的 x 会置位;聊天屏的 Ctrl-C / q 只改 UI 标志,
/// 引擎照常运行,有审批挂起时 TUI 会一直停在"已请求中断,等待引擎落账"。
fn sync_abort_flag(state: &TuiState, abort: &AtomicBool) {
    if state.abort_requested {
        abort.store(true, Ordering::Relaxed);
    }
}

/// spec 173:审批决策(a 批准 / w 本会话始终允许 / d 拒绝)。
/// 弹窗在任意屏都生效;返回 false 表示该键不是决策键。
fn decide_approval(
    state: &mut TuiState,
    decision_tx: &Option<mpsc::Sender<ApprovalDecision>>,
    key: KeyCode,
) -> bool {
    let decision = match key {
        KeyCode::Char('a') | KeyCode::Char('w') => ApprovalDecision::Approve,
        KeyCode::Char('d') => ApprovalDecision::Reject,
        _ => return false,
    };
    if let Some(req) = state.take_pending_approval() {
        if key == KeyCode::Char('w') {
            // spec 167:本会话始终允许该工具
            state.tool_allowlist.insert(req.tool.clone());
            state.push_important(&format!("[{}] 已加入本会话始终允许", req.tool));
        }
        if let Some(tx) = decision_tx {
            let _ = tx.send(decision);
        }
    }
    true
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
        TuiEvent::Approval(req) => {
            // spec 173:审批已落账,卡片应尽快显示"等待审批"
            state.chat_sync_pending = true;
            // spec 167:会话始终允许的工具自动批准(不发弹窗)
            if state.tool_allowlist.contains(req.tool.as_str()) {
                state.push_important(&format!(
                    "[auto] {} 已在本会话始终允许,自动批准",
                    req.tool
                ));
                if let Some(tx) = decision_tx {
                    let _ = tx.send(ApprovalDecision::Approve);
                }
            } else {
                state.approval_arrived(state::ApprovalRequestUi {
                    task_id: req.task_id,
                    call_id: req.call_id,
                    tool: req.tool,
                    summary: req.summary,
                });
            }
        },
        TuiEvent::Finished(status) => {
            state.task_finished(status);
            *decision_tx = None;
            // spec 163:聊天流结束 —— 用账本最终答复替换流式原文并写入对话史
            // spec 173:run 结束前把剩余工具事件收进转录(被中断的工具卡等)
            sync_chat_from_ledger(state, svc);
            state.chat_sync_pending = false;
            if state.chat_streaming {
                state.chat_streaming = false;
                let reply = state.chat_task.as_ref().and_then(|t| svc.events(t).ok()).and_then(
                    |(evs, _)| {
                        evs.iter().rev().find_map(|e| match &e.payload {
                            zealous_core::EventPayload::MessageCompleted { text, .. }
                                if !text.is_empty() =>
                            {
                                Some(text.clone())
                            }
                            _ => None,
                        })
                    },
                );
                state.chat_finish_reply(reply);
            }
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
            state.chat_streaming = false;
            state.chat_sync_pending = false;
            state.set_error(msg);
        }
        TuiEvent::ParallelFinished(summary) => {
            state.running = false;
            *decision_tx = None;
            state.push_important(&format!("parallel: {summary}"));
            refresh_task_list(state, svc);
        }
        TuiEvent::ChatDelta(piece) => {
            state.chat_append_delta(&piece);
        }
        TuiEvent::ReasoningDelta(piece) => {
            state.chat_append_reasoning(&piece);
        }
        TuiEvent::Progress(msg) => state.push_progress(msg),
        TuiEvent::ChatToolCall => state.chat_sync_pending = true,
        TuiEvent::Compacted(result) => {
            state.chat_compacting = false;
            match result {
                // 摘要已落账:立即同步账本,把压缩记录收进转录(与重启恢复同一路径)
                Ok(()) => {
                    state.chat_sync_pending = true;
                    sync_chat_from_ledger(state, svc);
                }
                Err(msg) => {
                    state.chat_entries
                        .push(state::ChatEntry::system(format!("压缩失败: {msg}")));
                    state.chat_scroll = 0;
                }
            }
        }
        TuiEvent::QueueFinished(summary) => {
            state.running = false;
            *decision_tx = None;
            state.push_important(&format!("queue: {summary}"));
            refresh_task_list(state, svc);
        }
    }
}

/// spec 173:流式输出期间每隔多久把账本同步进转录(工具卡片)
const CHAT_SYNC_INTERVAL: Duration = Duration::from_millis(300);
/// spec 173:两次账本同步的最小间隔(信号密集时避免重复读文件)
const CHAT_SYNC_MIN_GAP: Duration = Duration::from_millis(100);

/// spec 173:按需把账本里的事件同步进转录。
/// 触发条件:有工具信号待处理,或流式输出中到达节流间隔。
fn maybe_sync_chat(state: &mut TuiState, svc: &TaskService) {
    if state.chat_task.is_none() {
        return;
    }
    let since = state.chat_last_sync.map(|t| t.elapsed());
    if since.is_some_and(|d| d < CHAT_SYNC_MIN_GAP) {
        return;
    }
    let due = state.chat_sync_pending
        || (state.chat_streaming && since.map_or(true, |d| d >= CHAT_SYNC_INTERVAL));
    if due {
        sync_chat_from_ledger(state, svc);
    }
}

/// spec 173:读会话账本并把游标之后的事件归并进转录。
/// 流式输出中信号可能先于账本落盘,此时保留 pending 等下次重试;
/// 空闲时没有新事件就清掉 pending,避免空转。
fn sync_chat_from_ledger(state: &mut TuiState, svc: &TaskService) {
    let Some(tid) = state.chat_task.clone() else {
        return;
    };
    let Ok((evs, _)) = svc.events(&tid) else {
        return;
    };
    let applied = transcript::replay(state, &evs, false);
    if applied || !state.chat_streaming {
        state.chat_sync_pending = false;
    }
    state.chat_last_sync = Some(std::time::Instant::now());
}

/// spec 173:/clear 只清转录显示;游标对齐到账本末尾,之后的新事件仍生成卡片
fn clear_chat_view(state: &mut TuiState, svc: &TaskService) {
    let cursor = state
        .chat_task
        .as_ref()
        .and_then(|t| svc.events(t).ok())
        .map_or(0, |(evs, _)| evs.len());
    state.chat_clear_transcript();
    state.chat_event_cursor = cursor;
}

/// spec 165/173:把会话任务的转录、工具卡片、对话史与摘要装入状态。
/// 启动恢复与 /resume 共用;失败时不改变现有状态。
fn load_chat_session(state: &mut TuiState, svc: &TaskService, tid: &TaskId) -> Result<(), String> {
    let (evs, _) = svc.events(tid).map_err(|e| format!("读取账本失败: {e}"))?;
    let hist = svc.chat_history(tid).map_err(|e| format!("读取对话史失败: {e}"))?;
    let summary = svc.chat_summary(tid).map_err(|e| format!("读取摘要失败: {e}"))?;
    // 先读完再改状态:读取失败时原会话显示保持不变
    state.chat_reset();
    state.chat_task = Some(tid.clone());
    transcript::replay(state, &evs, true);
    // 上次进程退出时没来得及落中断事件:仍显示运行/待审批的卡片已不可能有结果
    transcript::finalize_unfinished(state, "未完成(会话已中断)");
    state.chat_history = hist;
    state.chat_summary = summary.filter(|x| !x.is_empty());
    Ok(())
}

/// spec 165/173:恢复最近一次聊天会话。已绑定会话或没有可恢复的转录时不动。
fn restore_chat_session(state: &mut TuiState, svc: &TaskService) {
    if state.chat_task.is_some() {
        return;
    }
    let Ok(Some(tid)) = svc.latest_chat_task() else {
        return;
    };
    if load_chat_session(state, svc, &tid).is_err() {
        return;
    }
    // 只有会话标记、没有任何对话:不绑定,保持新会话
    if state.chat_entries.is_empty() && state.chat_history.is_empty() && state.chat_summary.is_none() {
        state.chat_reset();
    }
}

/// spec 173:/model 显示用的当前模型(会话覆盖优先,其次 provider 配置)
fn current_model_name(state: &TuiState) -> Result<String, String> {
    if let Some(m) = &state.chat_model_override {
        return Ok(m.clone());
    }
    zealous_engine::provider::ProviderConfig::resolve()
        .map(|c| c.model)
        .map_err(|e| e.to_string())
}

/// spec 173:/compact —— 后台线程调用模型压缩对话史并落账,结果经 TuiEvent::Compacted 回传。
/// 摘要本身不在这里进转录:落账后由账本同步(transcript::replay)回放出压缩记录,
/// 与重启恢复走同一条路径。
fn compact_chat(state: &mut TuiState, svc: &TaskService, event_tx: &mpsc::Sender<TuiEvent>) {
    use zealous_engine::provider::OpenAiCompatProvider;
    let say = |state: &mut TuiState, text: String| {
        state.chat_entries.push(state::ChatEntry::system(text));
        state.chat_scroll = 0;
    };
    if state.running || state.chat_streaming {
        say(state, "运行中不能压缩,请等本轮结束".into());
        return;
    }
    if state.chat_compacting {
        return;
    }
    let Some(tid) = state.chat_task.clone() else {
        say(state, "(无会话任务)".into());
        return;
    };
    if state.chat_history.is_empty() {
        say(state, "没有新的对话可压缩".into());
        return;
    }
    let history = state.chat_history.clone();
    let prior = state.chat_summary.clone();
    let model_override = state.chat_model_override.clone();
    let svc = TaskService::new(svc.repo_root().to_path_buf());
    let tx = event_tx.clone();
    state.chat_compacting = true;
    std::thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let mut config = zealous_engine::provider::ProviderConfig::resolve()
                .map_err(|e| format!("provider 配置无效: {e}"))?;
            if let Some(m) = model_override {
                config.model = m;
            }
            let provider = OpenAiCompatProvider::new(config.clone());
            let turns = history.len() as u64;
            let summary = zealous_engine::task_service::summarize_history(
                &provider,
                &config.model,
                &history,
                prior.as_deref(),
            )
            .map_err(|e| format!("模型调用失败: {e}"))?;
            svc.append_chat_compacted(&tid, &summary, turns)
                .map_err(|e| format!("摘要落账失败: {e}"))?;
            Ok(())
        })();
        let _ = tx.send(TuiEvent::Compacted(result));
    });
}

/// spec 172/173:执行一条聊天命令。查询类命令直接读账本;
/// 有副作用的只有 /new /clear /exit /model /resume /compact。
fn run_chat_command(
    state: &mut TuiState,
    svc: &TaskService,
    event_tx: &mpsc::Sender<TuiEvent>,
    cmd: commands::Command,
    arg: String,
) {
    use commands::Command;
    let say = |state: &mut TuiState, text: String| {
        state.chat_entries.push(state::ChatEntry::system(text));
        state.chat_scroll = 0;
    };
    let session_events = |state: &TuiState, svc: &TaskService| -> Option<Vec<zealous_core::TaskEvent>> {
        state
            .chat_task
            .as_ref()
            .and_then(|t| svc.events(t).ok())
            .map(|(evs, _)| evs)
    };
    match cmd {
        Command::Help => say(state, commands::help_text()),
        Command::New => {
            state.chat_reset();
            say(state, "会话已重置".into());
        }
        Command::Task => say(
            state,
            state
                .chat_task
                .as_ref()
                .map(|t| format!("当前任务: {}", t.as_str()))
                .unwrap_or_else(|| "(无会话任务)".into()),
        ),
        Command::Status => {
            let text = match state.chat_task.as_ref().map(|t| svc.open_task(t)) {
                Some(Ok((_, proj, _))) => format!("状态: {:?}", proj.status),
                Some(Err(e)) => format!("error: {e}"),
                None => "(无会话任务)".to_string(),
            };
            say(state, text);
        }
        Command::Usage => say(
            state,
            session_events(state, svc)
                .map(|evs| commands::usage_text(&evs))
                .unwrap_or_else(|| "(无会话任务)".into()),
        ),
        Command::Context => say(
            state,
            session_events(state, svc)
                .map(|evs| commands::context_text(&evs, state.chat_history.len()))
                .unwrap_or_else(|| "(无会话任务)".into()),
        ),
        Command::Model => {
            let name = arg.trim();
            if name.is_empty() {
                let text = match (&state.chat_model_override, current_model_name(state)) {
                    (Some(m), _) => format!("当前模型: {m}(本会话 /model 覆盖)"),
                    (None, Ok(m)) => format!("当前模型: {m}(来自 provider 配置)"),
                    (None, Err(e)) => format!("当前模型无法确定: {e}"),
                };
                say(state, text);
            } else {
                state.chat_model_override = Some(name.to_string());
                say(
                    state,
                    format!("后续运行使用模型: {name}(仅本会话生效,不改配置文件)"),
                );
            }
        }
        Command::Clear => {
            // 只清转录显示;会话任务、对话史、摘要与账本都保留
            clear_chat_view(state, svc);
            say(state, "转录区已清空(会话与账本保留)".into());
        }
        Command::Exit => state.screen = Screen::List,
        Command::Resume => {
            if state.running || state.chat_streaming {
                say(state, "运行中不能切换会话,请等本轮结束".into());
                return;
            }
            let sessions = match svc.chat_sessions() {
                Ok(list) => list,
                Err(e) => {
                    say(state, format!("读取会话列表失败: {e}"));
                    return;
                }
            };
            let pick = arg.trim();
            if pick.is_empty() {
                say(state, commands::resume_text(&sessions, state.chat_task.as_ref()));
                return;
            }
            match commands::pick_session(&sessions, pick) {
                Err(text) => say(state, text),
                Ok(target) => {
                    let (tid, turns) = (target.task_id.clone(), target.turns);
                    let text = match load_chat_session(state, svc, &tid) {
                        Ok(()) => format!(
                            "已切换到会话 {}({turns} 轮)",
                            commands::short_task_id(tid.as_str())
                        ),
                        Err(e) => format!("切换会话失败: {e}"),
                    };
                    say(state, text);
                }
            }
        }
        Command::Compact => compact_chat(state, svc, event_tx),
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
                                true,
                                None,
                                None,
                                None,
                                None,
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

    // spec 173:审批弹窗在任意屏都可滚动与决策。此前 a/d/w 只在详情屏处理,
    // 聊天屏(默认屏)里审批是死路:按键被直接打进输入框。
    if state.pending_approval.is_some()
        && key.modifiers.intersection(KeyModifiers::CONTROL | KeyModifiers::ALT).is_empty()
    {
        match key.code {
            KeyCode::Up => state.approval_scroll_up(1),
            KeyCode::Down => state.approval_scroll_down(1),
            KeyCode::PageUp => state.approval_scroll_up(10),
            KeyCode::PageDown => state.approval_scroll_down(10),
            KeyCode::Char('x') => {
                state.request_abort();
                abort.store(true, Ordering::Relaxed);
            }
            // 离开当前屏不吞掉:弹窗保留,到别的屏仍可决策
            KeyCode::Esc | KeyCode::Char('q') => {}
            key => {
                if !decide_approval(state, decision_tx, key) {
                    return;
                }
            }
        }
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
            // spec 163:进入聊天屏
            KeyCode::Char('c') => {
                // spec 165:进入聊天屏时恢复最近会话(事实来自账本)
                restore_chat_session(state, svc);
                state.screen = Screen::Chat;
            }
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
            Screen::Chat => match key.code {
                KeyCode::Esc => state.screen = Screen::List,
                // spec 173:/ 补全菜单打开时 ↑/↓ 在候选间移动,否则滚动转录
                KeyCode::Up => {
                    if !state.chat_menu_move(false) {
                        state.chat_scroll_up(1);
                    }
                }
                KeyCode::Down => {
                    if !state.chat_menu_move(true) {
                        state.chat_scroll_down(1);
                    }
                }
                // spec 173:Tab 补全为高亮的候选(带参数的命令补出尾随空格)
                KeyCode::Tab | KeyCode::Char('\t') => {
                    if let Some(c) = state
                        .chat_menu_choice()
                        .and_then(|i| state.chat_completions().get(i).cloned())
                    {
                        state.chat_apply_completion(&c);
                    }
                }
                KeyCode::PageUp => state.chat_scroll_up(10),
                KeyCode::PageDown => state.chat_scroll_down(10),
                KeyCode::Backspace => state.chat_input_backspace(),
                // spec 166:Shift+Enter 多行输入
                KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    state.chat_input_char('\n');
                }
                KeyCode::Enter => {
                    // 运行中/流式中禁发;空输入忽略(spec 163 §3)
                    if state.running || state.chat_streaming {
                        return;
                    }
                    let msg = state.chat_input.trim().to_string();
                    if msg.is_empty() {
                        return;
                    }
                    // spec 172/173:/ 命令拦截(不发给模型);命令表与解析在 commands.rs
                    if let Some(parsed) = commands::parse(&msg) {
                        state.chat_input.clear();
                        match parsed {
                            Err(text) => {
                                state.chat_entries.push(state::ChatEntry::system(text));
                            }
                            Ok((cmd, arg)) => run_chat_command(state, svc, event_tx, cmd, arg),
                        }
                        return;
                    }
                    state.chat_input.clear();
                    // 会话任务:首条消息以消息全文为 brief 建任务(direct 模式)
                    let tid = match state.chat_task.clone() {
                        Some(t) => t,
                        None => {
                            let title: String = msg.chars().take(48).collect();
                            match svc.create_chat_task(&title, Some(msg.clone())) {
                                Ok(CreateOutcome::Ready { task_id, .. }) => {
                                    state.chat_task = Some(task_id.clone());
                                    refresh_task_list(state, svc);
                                    task_id
                                }
                                Ok(CreateOutcome::FailedAtCreation { task_id, reason }) => {
                                    state.chat_task = Some(task_id.clone());
                                    state
                                        .set_error(format!(
                                            "任务创建即失败: {reason}(账本已留档,可 resume)"
                                        ));
                                    task_id
                                }
                                Err(e) => {
                                    state.set_error(format!("创建任务失败: {e}"));
                                    return;
                                }
                            }
                        }
                    };
                    state.chat_begin_turn(&msg);
                    state.running = true;
                    state.chat_streaming = true;
                    state.abort_requested = false;
                    abort.store(false, Ordering::Relaxed);
                    let (approval_sink, request_rx, decision_tx_ch) =
                        approval::channel(tid.as_str().to_string(), abort.clone());
                    let fwd_tx = event_tx.clone();
                    std::thread::spawn(move || {
                        for req in request_rx {
                            if fwd_tx.send(TuiEvent::Approval(req)).is_err() {
                                break;
                            }
                        }
                    });
                    *decision_tx = Some(decision_tx_ch);
                    let prompt = state.chat_history_prompt(&msg);
                    // spec 163:流式 delta 经事件流回传转录区(通知通道)
                    let delta_tx = event_tx.clone();
                    let on_delta: Arc<dyn Fn(zealous_engine::provider::ProviderStreamEvent) + Send + Sync> =
                        Arc::new(move |ev| {
                            let _ = match ev {
                                zealous_engine::provider::ProviderStreamEvent::TextDelta(piece) => {
                                    let _ = delta_tx.send(TuiEvent::ChatDelta(piece));
                                }
                                zealous_engine::provider::ProviderStreamEvent::ReasoningDelta(piece) => {
                                    let _ = delta_tx.send(TuiEvent::ReasoningDelta(piece));
                                }
                                // spec 173:工具参数开始流式返回 → 本轮正文已结束,提示同步账本
                                zealous_engine::provider::ProviderStreamEvent::ToolCallDelta { .. } => {
                                    let _ = delta_tx.send(TuiEvent::ChatToolCall);
                                }
                            };
                        });
                    runner::spawn_run(
                        TaskService::new(svc.repo_root().to_path_buf()),
                        tid,
                        event_tx.clone(),
                        Arc::new(approval_sink),
                        progress_sink.clone(),
                        abort.clone(),
                        false,
                        true,
                        state.chat_model_override.clone(),
                        Some(prompt),
                        Some(msg.clone()),
                        Some(on_delta),
                    );
                }
                // Ctrl+N:重置会话(/new 语义;任务与账本不动)
                KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    state.chat_reset();
                }
                // spec 173:Ctrl+O 展开/收起工具卡片的完整输出
                KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    state.chat_expand_tools = !state.chat_expand_tools;
                }
                KeyCode::Char(c) => state.chat_input_char(c),
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
                            false,
                            None,
                            None,
                            None,
                            None,
                        );
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

    /// mock 端点经进程级环境变量注入:用到的测试串行执行
    static MOCK_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 本地 mock /chat/completions:按请求顺序返回脚本(spec 60 §6 同款)
    fn mock_endpoint(script: Vec<String>) -> String {
        mock_endpoint_msgs(
            script
                .into_iter()
                .map(|c| serde_json::json!({ "content": c }))
                .collect(),
        )
    }

    /// 同上,但脚本项是完整的 assistant message(可带原生 tool_calls)
    /// 同上,但脚本项是完整的 assistant message(可带原生 tool_calls)
    fn mock_endpoint_msgs(script: Vec<serde_json::Value>) -> String {
        mock_endpoint_msgs_capture(script).0
    }

    /// 同 mock_endpoint_msgs,并返回收到的请求体(断言 /model 覆盖是否真的生效)
    fn mock_endpoint_msgs_capture(
        script: Vec<serde_json::Value>,
    ) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        let counter = std::sync::atomic::AtomicU32::new(0);
        let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
        let bodies_sink = bodies.clone();
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
            let full = String::from_utf8_lossy(&raw).to_string();
            bodies_sink
                .lock()
                .unwrap()
                .push(full.split("\r\n\r\n").nth(1).unwrap_or("").to_string());
            let idx = counter.fetch_add(1, Ordering::Relaxed) as usize;
            let content = {
                let script = script.lock().unwrap();
                if idx < script.len() {
                    script[idx].clone()
                } else {
                    serde_json::json!("guard")
                }
            };
            // 每次应答带一份固定用量(prompt 11 / completion 7),/usage /context 才有账可查
            let body = serde_json::json!({
                "choices": [{ "message": content }],
                "usage": { "prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18 }
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
        (format!("http://{addr}/v1"), bodies)
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
        let _env = MOCK_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

        // ---- 模拟 R:启动运行(spec 173:绑定会话任务,工具事件同步进转录)----
        state.chat_task = Some(task_id.clone());
        state.running = true;
        runner::spawn_run(
            TaskService::new(&repo),
            task_id.clone(),
            event_tx.clone(),
            Arc::new(approval_sink),
            Arc::new(progress_sink),
            abort.clone(),
            false,
            false,
            None,
            None,
            None,
            None,
        );

        // ---- 消费事件循环(同 apply_event),审批到达时模拟按键 a ----
        let mut decision_tx = Some(decision_tx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            // 模拟工具信号:每个周期都要求同步账本(真实循环由 ToolCallDelta/进度触发)
            state.chat_sync_pending = true;
            maybe_sync_chat(&mut state, &svc);
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

        // spec 173:工具调用以卡片进转录(账本 → 卡片),按调用顺序;无"⚙ 工具"进度行
        let cards: Vec<_> = state
            .chat_entries
            .iter()
            .filter_map(|e| e.tool.as_ref())
            .collect();
        assert_eq!(cards.len(), 2, "工具卡片数: {cards:?}");
        assert_eq!((cards[0].tool.as_str(), cards[0].state), ("read_file", state::ChatToolState::Ok));
        assert_eq!(cards[0].args_summary, "README.md");
        assert_eq!((cards[1].tool.as_str(), cards[1].state), ("edit_file", state::ChatToolState::Ok));
        assert!(cards[0].output.contains("fixture"), "{}", cards[0].output);
        assert!(state.chat_entries.iter().all(|e| !e.text.starts_with("⚙ 工具")));
        // 重启恢复:新状态从同一账本回放出同样的卡片
        let mut fresh = TuiState::new();
        fresh.chat_task = Some(task_id.clone());
        let (ledger_events, _) = svc.events(&task_id).unwrap();
        assert!(transcript::replay(&mut fresh, &ledger_events, false));
        let restored: Vec<_> = fresh.chat_entries.iter().filter_map(|e| e.tool.as_ref()).collect();
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[1].state, state::ChatToolState::Ok);

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

    fn init_repo(tag: &str) -> std::path::PathBuf {
        let repo = std::env::temp_dir().join(format!(
            "zealous-tui-{tag}-{}-{}",
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
            &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"],
        );
        repo
    }

    /// spec 173:重启后恢复会话,工具卡片从账本回放(与运行中同步共用 transcript::replay)
    #[test]
    fn restore_chat_session_replays_tool_cards_after_restart() {
        let _env = MOCK_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let repo = init_repo("restart");
        // Provider 指向本地 mock:与真实 TUI 聊天路径一致(原生 tool-calls)
        let endpoint = mock_endpoint_msgs(vec![
            serde_json::json!({
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_readme",
                    "type": "function",
                    "function": {
                        "name": "read_file",
                        "arguments": "{\"path\":\"README.md\"}"
                    }
                }]
            }),
            serde_json::json!({ "role": "assistant", "content": "README 里是 fixture" }),
        ]);
        std::env::set_var("ZEALOUS_BASE_URL", &endpoint);
        std::env::set_var("ZEALOUS_API_KEY", "tui-secret-key");
        std::env::set_var("ZEALOUS_MODEL", "mock-model");

        let svc = TaskService::new(&repo);
        let tid = match svc.create_chat_task("tui: restart", Some("读 README".into())).unwrap() {
            CreateOutcome::Ready { task_id, .. } => task_id,
            other => panic!("expected Ready: {other:?}"),
        };

        // 第一次运行(无 TUI 状态):账本由引擎真实写入
        let abort = Arc::new(AtomicBool::new(false));
        let (event_tx, event_rx) = mpsc::channel();
        let (approval_sink, _request_rx, _decision_tx) =
            approval::channel(tid.as_str().to_string(), abort.clone());
        let (progress_sink, _progress_rx) = progress::channel();
        runner::spawn_run(
            TaskService::new(&repo),
            tid.clone(),
            event_tx.clone(),
            Arc::new(approval_sink),
            Arc::new(progress_sink),
            abort.clone(),
            false,
            true,
            None,
            Some("读 README".into()),
            Some("读 README".into()),
            None,
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let final_status = loop {
            if let Ok(ev) = event_rx.try_recv() {
                if let TuiEvent::Finished(status) = ev {
                    break status;
                }
                if let TuiEvent::Failure(msg) = ev {
                    panic!("运行失败: {msg}");
                }
            }
            assert!(std::time::Instant::now() < deadline, "30s 未完成");
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(final_status, TaskStatus::Completed);
        std::env::remove_var("ZEALOUS_BASE_URL");
        std::env::remove_var("ZEALOUS_API_KEY");
        std::env::remove_var("ZEALOUS_MODEL");

        // "重启":全新 TUI 状态,只凭账本恢复
        let mut state = TuiState::new();
        restore_chat_session(&mut state, &svc);
        assert_eq!(state.chat_task.as_ref(), Some(&tid));
        let roles: Vec<_> = state.chat_entries.iter().map(|e| e.role).collect();
        assert_eq!(
            roles,
            [state::ChatRole::User, state::ChatRole::Tool, state::ChatRole::Assistant]
        );
        assert_eq!(state.chat_entries[0].text, "读 README");
        let card = state.chat_entries[1].tool.as_ref().unwrap();
        assert_eq!(card.tool, "read_file");
        assert_eq!(card.args_summary, "README.md");
        assert_eq!(card.state, state::ChatToolState::Ok);
        assert_eq!(card.output, "fixture\n");
        assert_eq!(state.chat_entries[2].text, "README 里是 fixture");
        assert_eq!(
            state.chat_history,
            vec![("读 README".to_string(), "README 里是 fixture".to_string())]
        );
        // 已绑定会话时不覆盖
        let mut bound = TuiState::new();
        bound.chat_task = Some(tid.clone());
        restore_chat_session(&mut bound, &svc);
        assert!(bound.chat_entries.is_empty());
        let _ = std::fs::remove_dir_all(repo);
    }

    /// spec 173:审批弹窗在聊天屏(默认屏)也必须能决策,且按键不进输入框
    #[test]
    fn approval_keys_decide_on_chat_screen() {
        let repo = std::env::temp_dir().join(format!("zealous-tui-keys-{}", std::process::id()));
        std::fs::create_dir_all(&repo).unwrap();
        let svc = TaskService::new(&repo);
        let (sink, _progress_rx) = progress::channel();
        let sink: Arc<dyn zealous_engine::progress::ProgressSink> = Arc::new(sink);
        let key = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let req = || state::ApprovalRequestUi {
            task_id: "t".into(),
            call_id: "c1".into(),
            tool: "shell".into(),
            summary: "echo hi".into(),
        };

        // a:批准(有审批挂起 = 引擎运行中)
        let mut state = TuiState::new();
        assert_eq!(state.screen, Screen::Chat);
        state.running = true;
        state.approval_arrived(req());
        let (tx, rx) = mpsc::channel();
        let mut decision_tx = Some(tx);
        let (event_tx, _event_rx) = mpsc::channel();
        let abort = Arc::new(AtomicBool::new(false));
        handle_key(&mut state, &svc, &mut decision_tx, &event_tx, &sink, &abort, key('a'));
        assert_eq!(rx.try_recv().unwrap(), ApprovalDecision::Approve);
        assert!(state.pending_approval.is_none());
        assert!(state.chat_input.is_empty(), "审批键不应打进输入框");
        assert_eq!(state.screen, Screen::Chat);
        assert!(state.tool_allowlist.is_empty());

        // w:批准 + 本会话始终允许
        state.approval_arrived(req());
        handle_key(&mut state, &svc, &mut decision_tx, &event_tx, &sink, &abort, key('w'));
        assert_eq!(rx.try_recv().unwrap(), ApprovalDecision::Approve);
        assert!(state.tool_allowlist.contains("shell"));

        // d:拒绝
        state.approval_arrived(req());
        handle_key(&mut state, &svc, &mut decision_tx, &event_tx, &sink, &abort, key('d'));
        assert_eq!(rx.try_recv().unwrap(), ApprovalDecision::Reject);
        assert!(state.pending_approval.is_none());

        // x:中断任务(abort 信号置位)
        state.approval_arrived(req());
        handle_key(&mut state, &svc, &mut decision_tx, &event_tx, &sink, &abort, key('x'));
        assert!(abort.load(Ordering::Relaxed));
        assert!(state.abort_requested);

        // 无审批时 a 仍是普通输入字符
        let mut state = TuiState::new();
        let abort2 = Arc::new(AtomicBool::new(false));
        handle_key(&mut state, &svc, &mut decision_tx, &event_tx, &sink, &abort2, key('a'));
        assert_eq!(state.chat_input, "a");
        let _ = std::fs::remove_dir_all(repo);
    }

    /// spec 173:Ctrl-C / q 的中断请求必须传到 engine 的 abort 信号
    #[test]
    fn quit_and_abort_requests_reach_engine_flag() {
        let repo = std::env::temp_dir().join(format!("zealous-tui-abort-{}", std::process::id()));
        std::fs::create_dir_all(&repo).unwrap();
        let svc = TaskService::new(&repo);
        let (sink, _progress_rx) = progress::channel();
        let sink: Arc<dyn zealous_engine::progress::ProgressSink> = Arc::new(sink);
        let mut state = TuiState::new();
        state.running = true;
        let (tx, _rx) = mpsc::channel();
        let mut decision_tx = Some(tx);
        let (event_tx, _event_rx) = mpsc::channel();
        let abort = Arc::new(AtomicBool::new(false));
        handle_key(
            &mut state,
            &svc,
            &mut decision_tx,
            &event_tx,
            &sink,
            &abort,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert!(state.quit && state.abort_requested);
        assert!(!abort.load(Ordering::Relaxed), "尚未经主循环传播");
        sync_abort_flag(&state, &abort);
        assert!(abort.load(Ordering::Relaxed));
        // 未请求中断时不改动信号
        let idle = TuiState::new();
        let fresh = Arc::new(AtomicBool::new(false));
        sync_abort_flag(&idle, &fresh);
        assert!(!fresh.load(Ordering::Relaxed));
        let _ = std::fs::remove_dir_all(repo);
    }

    /// 原生 tool-calls 脚本:读 README → 最终答复;第三条给 /compact 的摘要调用备用
    fn chat_then_summary_script(final_text: &str, summary: Option<&str>) -> Vec<serde_json::Value> {
        let mut script = vec![
            serde_json::json!({
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_readme",
                    "type": "function",
                    "function": { "name": "read_file", "arguments": "{\"path\":\"README.md\"}" }
                }]
            }),
            serde_json::json!({ "role": "assistant", "content": final_text }),
        ];
        if let Some(sm) = summary {
            script.push(serde_json::json!({ "role": "assistant", "content": sm }));
        }
        script
    }

    /// 真实跑一轮聊天(原生 tool-calls,与 TUI 发送路径同构),等到 Finished。
    /// `endpoint` 是本轮专用的 mock(model 配置经环境变量注入)
    fn run_mock_chat_turn(repo: &Path, tid: &zealous_core::TaskId, user: &str, endpoint: &str) {
        std::env::set_var("ZEALOUS_BASE_URL", endpoint);
        let abort = Arc::new(AtomicBool::new(false));
        let (event_tx, event_rx) = mpsc::channel();
        let (approval_sink, _request_rx, _decision_tx) =
            approval::channel(tid.as_str().to_string(), abort.clone());
        let (progress_sink, _progress_rx) = progress::channel();
        runner::spawn_run(
            TaskService::new(repo),
            tid.clone(),
            event_tx.clone(),
            Arc::new(approval_sink),
            Arc::new(progress_sink),
            abort.clone(),
            false,
            true,
            None,
            Some(user.into()),
            Some(user.into()),
            None,
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if let Ok(ev) = event_rx.try_recv() {
                match ev {
                    TuiEvent::Finished(status) => {
                        if status != TaskStatus::Completed {
                            let (evs, _) = TaskService::new(repo).events(tid).unwrap();
                            let tail: Vec<String> = evs
                                .iter()
                                .rev()
                                .take(8)
                                .rev()
                                .map(|e| format!("{} {:?}", e.seq, e.payload))
                                .collect();
                            panic!("运行状态 {status:?};账本尾部:\n{}", tail.join("\n"));
                        }
                        break;
                    }
                    TuiEvent::Failure(msg) => panic!("运行失败: {msg}"),
                    _ => {}
                }
            }
            assert!(std::time::Instant::now() < deadline, "30s 未完成");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn chat_task(svc: &TaskService, title: &str) -> zealous_core::TaskId {
        match svc.create_chat_task(title, Some("brief".into())).unwrap() {
            CreateOutcome::Ready { task_id, .. } => task_id,
            other => panic!("expected Ready: {other:?}"),
        }
    }

    /// TUI 控制器接线(无终端):用于逐键驱动 / 命令
    struct Harness {
        svc: TaskService,
        decision_tx: Option<mpsc::Sender<ApprovalDecision>>,
        event_tx: mpsc::Sender<TuiEvent>,
        event_rx: mpsc::Receiver<TuiEvent>,
        sink: Arc<dyn zealous_engine::progress::ProgressSink>,
        abort: Arc<AtomicBool>,
    }

    impl Harness {
        fn new(repo: &Path) -> Self {
            let svc = TaskService::new(repo);
            let (event_tx, event_rx) = mpsc::channel();
            let (sink, _progress_rx) = progress::channel();
            Self {
                svc,
                decision_tx: None,
                event_tx,
                event_rx,
                sink: Arc::new(sink),
                abort: Arc::new(AtomicBool::new(false)),
            }
        }

        fn press(&mut self, state: &mut TuiState, key: KeyEvent) {
            let Harness { svc, decision_tx, event_tx, sink, abort, .. } = self;
            handle_key(state, svc, decision_tx, event_tx, sink, abort, key);
        }

        /// 逐字符输入一行再回车(与真实按键路径一致)
        fn type_line(&mut self, state: &mut TuiState, line: &str) {
            for c in line.chars() {
                self.press(state, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            }
            self.press(state, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        }

        /// 把后台事件应用到状态(与 run_command 主循环同构)
        fn drain(&mut self, state: &mut TuiState) {
            loop {
                let mut got = false;
                while let Ok(ev) = self.event_rx.try_recv() {
                    let Harness { svc, decision_tx, .. } = self;
                    apply_event(state, svc, decision_tx, ev);
                    got = true;
                }
                state.chat_sync_pending = true;
                maybe_sync_chat(state, &self.svc);
                if !got {
                    break;
                }
            }
        }

        /// 最后一条系统提示(命令的文本产出)
        fn last_system(state: &TuiState) -> String {
            state
                .chat_entries
                .iter()
                .rev()
                .find(|e| e.role == state::ChatRole::System)
                .map(|e| e.text.clone())
                .expect("应有系统提示")
        }
    }

    /// spec 172/173:聊天屏 / 命令走共享命令表;查询类命令读账本,
    /// /model /resume /exit /Tab 补全改变实际状态;未知命令不发给模型。
    #[test]
    fn slash_commands_in_chat_screen() {
        let _env = MOCK_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let repo = init_repo("commands");
        // 建任务要校验 provider 配置:先给占位地址,每轮运行前再指向各自的 mock
        std::env::set_var("ZEALOUS_BASE_URL", "http://127.0.0.1:9/v1");
        std::env::set_var("ZEALOUS_API_KEY", "tui-secret-key");
        std::env::set_var("ZEALOUS_MODEL", "mock-model");
        let mut h = Harness::new(&repo);
        let first = chat_task(&h.svc, "first");
        run_mock_chat_turn(
            &repo,
            &first,
            "读 README",
            &mock_endpoint_msgs(chat_then_summary_script("读完了", None)),
        );
        // 第二个会话最后活动,排在 /resume 列表第 1 位
        let second = chat_task(&h.svc, "second");
        run_mock_chat_turn(
            &repo,
            &second,
            "看看第二个会话",
            &mock_endpoint_msgs(chat_then_summary_script("第二个会话读完了", None)),
        );
        let (endpoint, request_bodies) =
            mock_endpoint_msgs_capture(chat_then_summary_script("答复", None));
        std::env::set_var("ZEALOUS_BASE_URL", &endpoint);

        let mut state = TuiState::new();
        load_chat_session(&mut state, &h.svc, &first).unwrap();
        assert_eq!(state.chat_history.len(), 1);
        assert!(state.chat_tool_index.values().count() == 1, "工具卡片随会话恢复");

        // /help:列出全部命令
        h.type_line(&mut state, "/help");
        let help = Harness::last_system(&state);
        for name in ["/help", "/new", "/task", "/status", "/context", "/usage", "/model", "/resume", "/compact", "/clear", "/exit"] {
            assert!(help.contains(name), "/help 缺少 {name}: {help}");
        }
        // /context /usage:数据来自账本
        h.type_line(&mut state, "/context");
        let ctx = Harness::last_system(&state);
        // 一轮 = 工具调用 + 最终答复 = 2 次模型调用,每次 prompt 11 / completion 7
        assert!(
            ctx.contains("上下文: 1 轮 · 对话史 1 条 · 累计 36 tokens(prompt 22 / completion 14)"),
            "{ctx}"
        );
        assert!(ctx.contains("最近一次 prompt: 11 tokens · 模型调用 2 次"), "{ctx}");
        h.type_line(&mut state, "/usage");
        assert_eq!(
            Harness::last_system(&state),
            "用量: prompt=22 completion=14 total=36 tokens"
        );
        // /task /status
        h.type_line(&mut state, "/task");
        assert!(Harness::last_system(&state).contains(first.as_str()));
        h.type_line(&mut state, "/status");
        assert!(Harness::last_system(&state).contains("Completed"), "{}", Harness::last_system(&state));

        // Tab 补全:输入 /com → /compact;/mo → "/model "(带尾随空格)
        for c in "/com".chars() {
            h.press(&mut state, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(state.chat_completions(), vec!["/compact"]);
        h.press(&mut state, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(state.chat_input, "/compact");
        state.chat_input.clear();
        for c in "/mo".chars() {
            h.press(&mut state, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        h.press(&mut state, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(state.chat_input, "/model ");
        // 补全后直接接参数:/model foo
        for c in "foo".chars() {
            h.press(&mut state, KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert!(state.chat_completions().is_empty(), "输入参数后不再弹菜单");
        h.press(&mut state, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(state.chat_model_override.as_deref(), Some("foo"));
        assert!(Harness::last_system(&state).contains("后续运行使用模型: foo"));
        // /model(无参数)显示当前模型
        h.type_line(&mut state, "/model");
        assert!(Harness::last_system(&state).contains("当前模型: foo"), "{}", Harness::last_system(&state));
        // 覆盖真的进入模型请求:发一条消息,抓到的请求体里 model 是 foo
        h.type_line(&mut state, "再读一次");
        assert!(state.running && state.chat_streaming, "消息应触发运行");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while state.running {
            h.drain(&mut state);
            assert!(std::time::Instant::now() < deadline, "运行 30s 未完成");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let bodies = request_bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1, "这一轮只调用一次模型: {bodies:?}");
        let sent: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
        assert_eq!(sent["model"], "foo", "请求应使用 /model 设置的模型: {}", bodies[0]);
        // 流式请求体带工具声明(回归:此前流式漏发 tools)
        assert!(sent["tools"].is_array() && !sent["tools"].as_array().unwrap().is_empty());
        // prompt 注入了对话史
        let messages = sent["messages"].as_array().unwrap();
        assert!(
            messages.iter().any(|m| m["content"]
                .as_str()
                .is_some_and(|c| c.contains("[历史对话(压缩参考)]") && c.contains("再读一次"))),
            "{messages:?}"
        );
        drop(bodies);
        state.chat_model_override = None;
        h.type_line(&mut state, "/model");
        assert!(Harness::last_system(&state).contains("mock-model"));

        // 未知命令 / 多余参数:提示且不发给模型
        h.type_line(&mut state, "/nope");
        assert!(Harness::last_system(&state).starts_with("未知命令: /nope"));
        h.type_line(&mut state, "/compact later");
        assert!(Harness::last_system(&state).contains("不接受参数"));
        assert!(!state.running, "命令不应触发运行");

        // /resume:列出会话(最近活动在前),按序号切换并恢复转录
        // (上面在第一个会话里又发了一轮,它仍是最近活动,排第 1)
        h.type_line(&mut state, "/resume");
        let list = Harness::last_system(&state);
        assert!(
            list.contains(&format!(
                "1. 读 README · 2 轮 · {} · 当前",
                commands::short_task_id(first.as_str())
            )),
            "{list}"
        );
        assert!(list.contains("2. 看看第二个会话 · 1 轮"), "{list}");
        h.type_line(&mut state, "/resume 99");
        assert!(Harness::last_system(&state).contains("没有第 99 个会话"));
        assert_eq!(state.chat_task.as_ref(), Some(&first), "失败的切换不改当前会话");
        h.type_line(&mut state, "/resume 2");
        assert_eq!(state.chat_task.as_ref(), Some(&second));
        assert_eq!(
            Harness::last_system(&state),
            format!("已切换到会话 {}(1 轮)", commands::short_task_id(second.as_str()))
        );
        // 转录来自第二个会话的账本
        assert!(state.chat_entries.iter().any(|e| e.role == state::ChatRole::User && e.text == "看看第二个会话"));
        assert!(state.chat_tool_index.len() == 1, "切换后工具卡片索引只含新会话的");
        // 按任务 id 前缀切回第一个会话
        let prefix = &first.as_str()[..18];
        h.type_line(&mut state, &format!("/resume {prefix}"));
        assert_eq!(state.chat_task.as_ref(), Some(&first));

        // /exit:回任务列表
        h.type_line(&mut state, "/exit");
        assert_eq!(state.screen, Screen::List);

        std::env::remove_var("ZEALOUS_BASE_URL");
        std::env::remove_var("ZEALOUS_API_KEY");
        std::env::remove_var("ZEALOUS_MODEL");
        let _ = std::fs::remove_dir_all(repo);
    }

    /// spec 173:/compact 调模型生成摘要 → 落账 → 转录回放 → 注入后续 prompt;重启后仍在
    #[test]
    fn compact_summarizes_persists_and_replays() {
        let _env = MOCK_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let repo = init_repo("compact");
        let endpoint = mock_endpoint_msgs(chat_then_summary_script(
            "读完了",
            Some("摘要: 用户让我读 README,已读完。"),
        ));
        std::env::set_var("ZEALOUS_BASE_URL", &endpoint);
        std::env::set_var("ZEALOUS_API_KEY", "tui-secret-key");
        std::env::set_var("ZEALOUS_MODEL", "mock-model");
        std::env::set_var("ZEALOUS_API_KEY", "tui-secret-key");
        std::env::set_var("ZEALOUS_MODEL", "mock-model");
        let mut h = Harness::new(&repo);
        let tid = chat_task(&h.svc, "compact-me");
        run_mock_chat_turn(&repo, &tid, "读 README", &endpoint);

        let mut state = TuiState::new();
        load_chat_session(&mut state, &h.svc, &tid).unwrap();
        assert_eq!(state.chat_history.len(), 1);
        assert_eq!(state.chat_summary, None);

        // 没有会话时 /compact 给出提示;有会话且有对话时才调用模型
        h.type_line(&mut state, "/compact");
        assert!(state.chat_compacting, "压缩在后台线程进行");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while state.chat_compacting {
            h.drain(&mut state);
            assert!(std::time::Instant::now() < deadline, "压缩 30s 未完成");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // 摘要落账
        let summary = h.svc.chat_summary(&tid).unwrap();
        assert_eq!(summary.as_deref(), Some("摘要: 用户让我读 README,已读完。"));
        assert!(h.svc.chat_history(&tid).unwrap().is_empty(), "摘要取代此前的对话史");
        // 账本事件可投影(压缩不改变任务状态)
        assert_eq!(h.svc.refresh(&tid).unwrap().status, TaskStatus::Completed);
        // TUI 状态:对话史清空、摘要到位、转录里有一条压缩记录
        assert!(state.chat_history.is_empty());
        assert_eq!(state.chat_summary, summary);
        assert!(Harness::last_system(&state).starts_with("✂ 已压缩 1 轮对话为摘要:"), "{}", Harness::last_system(&state));
        // 后续 prompt 注入摘要
        let prompt = state.chat_history_prompt("继续");
        assert!(prompt.contains("用户消息: 继续") && prompt.contains("摘要: 用户让我读 README"), "{prompt}");
        assert!(!prompt.contains("[历史对话(压缩参考)]"), "没有新的原文对话时不带历史块: {prompt}");
        // 再次压缩:没有新对话
        h.type_line(&mut state, "/compact");
        assert!(Harness::last_system(&state).contains("没有新的对话可压缩"));

        // 重启恢复:摘要与压缩记录都从账本回放
        let mut fresh = TuiState::new();
        restore_chat_session(&mut fresh, &h.svc);
        assert_eq!(fresh.chat_task.as_ref(), Some(&tid));
        assert_eq!(fresh.chat_summary, summary);
        assert!(fresh.chat_history.is_empty());
        assert!(fresh.chat_entries.iter().any(|e| e.text.starts_with("✂ 已压缩 1 轮对话为摘要:")));

        std::env::remove_var("ZEALOUS_BASE_URL");
        std::env::remove_var("ZEALOUS_API_KEY");
        std::env::remove_var("ZEALOUS_MODEL");
        let _ = std::fs::remove_dir_all(repo);
    }

}
