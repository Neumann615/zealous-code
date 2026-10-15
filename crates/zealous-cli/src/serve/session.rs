//! 连接状态与请求路由(spec 100):会话不拥有任务状态,
//! 事实来自 TaskService(投影/账本);engine 是唯一写入者。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;

use zealous_core::{ApprovalDecision, TaskId};
use zealous_engine::orchestrator::Orchestrator;
use zealous_engine::progress::ProgressSink;
use zealous_engine::provider::{OpenAiCompatProvider, ProviderConfig};
use zealous_engine::task_service::TaskService;
use zealous_engine::tools::builtin_registry;

use super::protocol::{ErrorResponse, Notification, ProtocolError, Request, Response};

// 审批通道实现与 TUI 共用(cli 内部共享;engine 不感知通道形态)
use crate::tui::approval;

fn approval_channel(
    abort: Arc<AtomicBool>,
) -> (
    approval::ChannelApprovalSink,
    mpsc::Receiver<approval::ApprovalRequest>,
    mpsc::Sender<ApprovalDecision>,
) {
    approval::channel(abort)
}

fn err_resp(id: u64, code: &str, message: impl Into<String>) -> Response {
    let er: ErrorResponse = ErrorResponse::from_error(id, ProtocolError::new(code, message));
    Response {
        id: er.id,
        ok: false,
        result: None,
        error: Some(er.error),
    }
}

/// 当前运行中的 worker(V1 只允许一个)
pub(crate) struct RunState {
    task_id: TaskId,
    abort: Arc<AtomicBool>,
    /// 当前挂起审批的决策通道(approval_requested 发出后有效)
    decision_tx: mpsc::Sender<ApprovalDecision>,
    join: Option<JoinHandle<()>>,
}

pub struct Session {
    pub svc: TaskService,
    pub repo_root: PathBuf,
    pub initialized: bool,
    /// 订阅:task_id + 已推送的最大 seq
    pub subscription: Option<(TaskId, u64)>,
    pub(crate) run: Option<RunState>,
    /// 通知出口(由 serve/mod.rs 提供写线程)
    pub notif_tx: mpsc::Sender<Notification>,
}

impl Session {
    pub fn new(repo_root: PathBuf, notif_tx: mpsc::Sender<Notification>) -> Self {
        Self {
            svc: TaskService::new(repo_root.clone()),
            repo_root,
            initialized: false,
            subscription: None,
            run: None,
            notif_tx,
        }
    }

    /// 路由一个请求 → 响应。通知(若有)经 notif_tx 发出。
    pub fn handle(&mut self, req: Request) -> Response {
        // initialize 之外的全部命令要求先完成握手;shutdown 例外(总是可退出)
        if !self.initialized && req.cmd != "initialize" && req.cmd != "shutdown" {
            return err_resp(req.id, "not_initialized", "必须先调用 initialize");
        }
        match req.cmd.as_str() {
            "initialize" => self.handle_initialize(&req),
            "list_tasks" => self.handle_list_tasks(&req),
            "get_task" => self.handle_get_task(&req),
            "subscribe_events" => self.handle_subscribe(&req),
            "create_task" => self.handle_create_task(&req),
            "run_task" => self.handle_run_task(&req),
            "interrupt_task" => self.handle_interrupt(&req),
            "decide_approval" => self.handle_decide(&req),
            "shutdown" => {
                self.wait_worker();
                Response::ok(req.id, serde_json::json!({ "bye": true }))
            }
            other => err_resp(req.id, "unknown_command", format!("未知命令: {other}")),
        }
    }

    fn handle_initialize(&mut self, req: &Request) -> Response {
        let version = req.params.get("version").and_then(|v| v.as_u64());
        match version {
            Some(v) if v as u32 == super::protocol::PROTOCOL_VERSION => {
                self.initialized = true;
                Response::ok(
                    req.id,
                    serde_json::json!({
                        "protocol_version": super::protocol::PROTOCOL_VERSION,
                        "capabilities": {
                            "list_tasks": true,
                            "get_task": true,
                            "subscribe_events": true,
                            "create_task": true,
                            "run_task": true,
                            "interrupt_task": true,
                            "decide_approval": true,
                            "max_workers": 3,
                            "concurrent_runs": 1,
                        }
                    }),
                )
            }
            Some(v) => err_resp(
                req.id,
                "version_mismatch",
                format!(
                    "客户端协议版本 {v} 与服务端 {} 不兼容",
                    super::protocol::PROTOCOL_VERSION
                ),
            ),
            None => err_resp(req.id, "missing_field", "initialize.params.version 缺失"),
        }
    }

    fn handle_list_tasks(&mut self, req: &Request) -> Response {
        match self.svc.list_tasks() {
            Ok((items, skipped)) => Response::ok(
                req.id,
                serde_json::json!({
                    "tasks": items,
                    "skipped": skipped
                        .iter()
                        .map(|(id, why)| serde_json::json!({
                            "task_id": id,
                            "reason": why,
                        }))
                        .collect::<Vec<_>>(),
                }),
            ),
            Err(e) => err_resp(req.id, "internal", e.to_string()),
        }
    }

    /// 解析 params.task_id → 校验过的 TaskId
    fn parse_task_id(&self, req: &Request) -> Result<TaskId, Response> {
        let raw = req
            .params
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| err_resp(req.id, "missing_field", "params.task_id 缺失或非字符串"))?;
        TaskId::parse(raw)
            .map_err(|e| err_resp(req.id, "invalid_params", format!("非法 task_id: {e}")))
    }

    fn handle_get_task(&mut self, req: &Request) -> Response {
        let task_id = match self.parse_task_id(req) {
            Ok(id) => id,
            Err(resp) => return resp,
        };
        match self.svc.refresh(&task_id) {
            Ok(proj) => Response::ok(
                req.id,
                serde_json::to_value(&proj).unwrap_or_else(|_| serde_json::json!({})),
            ),
            Err(e) => err_resp(req.id, "task_not_found", e.to_string()),
        }
    }

    fn handle_subscribe(&mut self, req: &Request) -> Response {
        let task_id = match self.parse_task_id(req) {
            Ok(id) => id,
            Err(resp) => return resp,
        };
        let after_seq = req
            .params
            .get("after_seq")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        // 重放:读取账本中 seq > after_seq 的事件,逐条推送;
        // 损坏账本返回结构化错误,不产生部分重放
        let (events, report) = match self.svc.events(&task_id) {
            Ok(x) => x,
            Err(e) => return err_resp(req.id, "internal", e.to_string()),
        };
        if report.torn_tail {
            let _ = self.notif_tx.send(Notification::error_occurred(
                "账本存在残缺尾行,已忽略(上次进程中断残留)",
            ));
        }
        for e in events.iter().filter(|e| e.seq > after_seq) {
            let _ = self
                .notif_tx
                .send(Notification::task_updated(task_id.as_str(), e));
        }
        self.subscription = Some((task_id.clone(), after_seq));
        Response::ok(
            req.id,
            serde_json::json!({ "task_id": task_id.as_str(), "replayed": events.len() }),
        )
    }

    fn handle_create_task(&mut self, req: &Request) -> Response {
        let title = match req
            .params
            .get("title")
            .and_then(|v| v.as_str())
            .map(String::from)
        {
            Some(t) => t,
            None => return err_resp(req.id, "missing_field", "params.title 缺失"),
        };
        let brief = req
            .params
            .get("brief")
            .and_then(|v| v.as_str())
            .map(String::from);
        match self.svc.create_task_with_brief_text(&title, brief) {
            Ok(zealous_engine::task_service::CreateOutcome::Ready { task_id, .. }) => Response::ok(
                req.id,
                serde_json::json!({ "task_id": task_id.as_str(), "created": true }),
            ),
            Ok(zealous_engine::task_service::CreateOutcome::FailedAtCreation {
                task_id,
                reason,
            }) => Response::ok(
                req.id,
                serde_json::json!({
                    "task_id": task_id.as_str(),
                    "created": true,
                    "failed_at_creation": true,
                    "reason": reason,
                }),
            ),
            Err(e) => err_resp(req.id, "internal", e.to_string()),
        }
    }

    fn handle_run_task(&mut self, req: &Request) -> Response {
        if self.run.is_some() {
            return err_resp(req.id, "already_running", "已有运行中的任务(V1 单任务)");
        }
        let task_id = match self.parse_task_id(req) {
            Ok(id) => id,
            Err(resp) => return resp,
        };
        let config = match ProviderConfig::resolve() {
            Ok(c) => c,
            Err(e) => return err_resp(req.id, "internal", e.to_string()),
        };
        let abort = Arc::new(AtomicBool::new(false));
        let (approval_sink, request_rx, decision_tx) = approval_channel(abort.clone());
        let progress_sink: Arc<dyn ProgressSink> = {
            let tx = self.notif_tx.clone();
            Arc::new(NotifProgressSink { tx })
        };

        // 审批请求 → 通知(转发线程)
        {
            let tx = self.notif_tx.clone();
            std::thread::spawn(move || {
                for r in request_rx {
                    if tx
                        .send(Notification::approval_requested(&r.call_id, &r.summary))
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }

        let title = self
            .svc
            .refresh(&task_id)
            .map(|p| p.title)
            .unwrap_or_default();
        let _ = &title;
        let executor = Arc::new(zealous_engine::executor::ToolExecutor::new(
            self.repo_root.clone(),
            builtin_registry(),
            Arc::new(approval_sink),
        ));
        let model = config.model.clone();
        let orchestrator = Orchestrator {
            provider: Arc::new(OpenAiCompatProvider::new(config)),
            executor,
            abort: abort.clone(),
            max_turns: 24,
            model,
            progress: Some(progress_sink),
        };
        let svc = self.svc.clone();
        let notif_tx = self.notif_tx.clone();
        let run_task_id = task_id.clone();
        let join: JoinHandle<()> = std::thread::spawn(move || {
            let (ledger, proj, _) = match svc.open_task(&run_task_id) {
                Ok(x) => x,
                Err(e) => {
                    let _ =
                        notif_tx.send(Notification::error_occurred(&format!("打开任务失败: {e}")));
                    return;
                }
            };
            let ledger = Arc::new(ledger);
            let status = match orchestrator.run_task(&ledger, &proj.title) {
                Ok(s) => s,
                Err(e) => {
                    let _ =
                        notif_tx.send(Notification::error_occurred(&format!("编排失败: {e:?}")));
                    return;
                }
            };
            let _ = notif_tx.send(Notification::run_finished(
                run_task_id.as_str(),
                &format!("{status:?}"),
            ));
        });
        self.run = Some(RunState {
            task_id,
            abort,
            decision_tx,
            join: Some(join),
        });
        Response::ok(req.id, serde_json::json!({ "started": true }))
    }

    fn handle_interrupt(&mut self, req: &Request) -> Response {
        let task_id = match self.parse_task_id(req) {
            Ok(id) => id,
            Err(resp) => return resp,
        };
        match &self.run {
            Some(run) if run.task_id == task_id => {
                run.abort.store(true, Ordering::Relaxed);
                Response::ok(req.id, serde_json::json!({ "interrupting": true }))
            }
            _ => err_resp(req.id, "invalid_params", "该任务当前未在运行"),
        }
    }

    fn handle_decide(&mut self, req: &Request) -> Response {
        let decision_str = req
            .params
            .get("decision")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let decision = match decision_str {
            "approve" => ApprovalDecision::Approve,
            "reject" => ApprovalDecision::Reject,
            other => {
                return err_resp(req.id, "invalid_params", format!("未知 decision: {other}"));
            }
        };
        match &self.run {
            Some(run) => {
                // 未知 call id 校验:请求可携带 call_id,与运行中任务不强关联,
                // V1 只存在当前 pending 一个;无 pending 即错误
                let _ = req.params.get("call_id");
                let sent = run.decision_tx.send(decision).is_ok();
                if sent {
                    Response::ok(req.id, serde_json::json!({ "decided": decision_str }))
                } else {
                    err_resp(req.id, "no_pending_approval", "决策通道已关闭(无待决审批)")
                }
            }
            None => err_resp(
                req.id,
                "no_pending_approval",
                "当前没有运行中的任务或待决审批",
            ),
        }
    }

    pub fn worker_finished(&mut self) {
        if let Some(mut run) = self.run.take() {
            if let Some(j) = run.join.take() {
                let _ = j.join();
            }
        }
    }

    fn wait_worker(&mut self) {
        self.worker_finished();
    }
}

/// 进度 → 通知桥
struct NotifProgressSink {
    tx: mpsc::Sender<Notification>,
}

impl ProgressSink for NotifProgressSink {
    fn progress(&self, line: &str) {
        let _ = self.tx.send(Notification::progress("progress", line));
    }
    fn important(&self, line: &str) {
        let _ = self.tx.send(Notification::progress("important", line));
    }
}
