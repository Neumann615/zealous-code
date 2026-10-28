//! 连接状态与请求路由(spec 100 + 110 §2/§3):会话不拥有任务状态,
//! 事实来自 TaskService(投影/账本);engine 是唯一写入者。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use zealous_core::{ApprovalDecision, TaskId, TaskStatus};
use zealous_engine::approval::ApprovalSink;
use zealous_engine::parallel as parallel_mod;
use zealous_engine::provider::{OpenAiCompatProvider, ProviderConfig};
use zealous_engine::queue as queue_mod;
use zealous_engine::task_service::TaskService;

use super::protocol::{ErrorResponse, Notification, ProtocolError, Request, Response};
use super::runner;

fn err_resp(id: u64, code: &str, message: impl Into<String>) -> Response {
    let er: ErrorResponse = ErrorResponse::from_error(id, ProtocolError::new(code, message));
    Response {
        id: er.id,
        ok: false,
        result: None,
        error: Some(er.error),
    }
}

/// 请求处理结果(spec 100 生命周期):shutdown 携带最终响应,
/// 调用方写完响应、join worker 后退出。
pub enum Handled {
    Response(Response),
    Shutdown(Response),
    /// 请求行本身无法解析(id 未知):错误响应直接落 stdout
    ParseError(Response),
}

/// 事件订阅(spec 110 §2):cursor 之后的账本事件持续推送
pub struct Subscription {
    pub task_id: TaskId,
    pub cursor: u64,
    /// torn tail 提示只发一次
    torn_warned: bool,
}

/// 当前运行中的 worker(V1 只允许一个)
pub struct RunState {
    pub task_id: TaskId,
    handles: runner::RunHandles,
}

/// spec 156:队列运行时共享状态(线程 ↔ 会话)
pub struct QueueShared {
    pub abort: Arc<AtomicBool>,
    pub task_ids: Vec<TaskId>,
    pub index: AtomicUsize,
    pub auto_apply: bool,
    pub completed: Mutex<Vec<String>>,
    pub last_failed: Mutex<Option<String>>,
    pub finished: AtomicBool,
}

/// spec 157:parallel batch 状态(线程 ↔ 会话)
pub struct ParallelShared {
    pub abort: Arc<AtomicBool>,
    pub task_ids: Vec<TaskId>,
    pub active: Mutex<Vec<String>>,
    pub auto_apply: bool,
    pub base_commit: String,
    pub completed: Mutex<Vec<String>>,
    pub failed: Mutex<Vec<String>>,
    pub interrupted: Mutex<Vec<String>>,
    pub applied: Mutex<Vec<String>>,
    pub merged: Mutex<Vec<String>>,
    pub apply_index: AtomicUsize,
    pub finished: AtomicBool,
    pub approvals: Arc<ParallelApprovals>,
}

/// parallel 审批绑定:task_id + call_id → decision sender
pub struct ParallelApprovals {
    pub pending: Mutex<HashMap<String, mpsc::Sender<ApprovalDecision>>>,
}

impl ParallelApprovals {
    fn insert(&self, key: String, tx: mpsc::Sender<ApprovalDecision>) {
        self.pending
            .lock()
            .expect("parallel approvals poisoned")
            .insert(key, tx);
    }

    fn remove(&self, key: &str) {
        self.pending
            .lock()
            .expect("parallel approvals poisoned")
            .remove(key);
    }

    fn decide(&self, key: &str, decision: ApprovalDecision) -> bool {
        let mut pending = self.pending.lock().expect("parallel approvals poisoned");
        match pending.remove(key) {
            Some(tx) => tx.send(decision).is_ok(),
            None => false,
        }
    }
}

struct ParallelApprovalSink {
    shared: Arc<ParallelShared>,
    notif: mpsc::Sender<Notification>,
}

impl parallel_mod::ParallelApprovalSink for ParallelApprovalSink {
    fn decide(
        &self,
        task_id: &TaskId,
        info: &zealous_engine::approval::ApprovalRequestInfo,
    ) -> ApprovalDecision {
        let key = format!("{}:{}", task_id.as_str(), info.call_id);
        let (tx, rx) = mpsc::channel();
        self.shared.approvals.insert(key.clone(), tx);
        let sent = self.notif.send(Notification::approval_requested(
            task_id.as_str(),
            &info.call_id,
            &info.tool,
            &format!("{:?}", info.risk),
            &info.summary,
        ));
        if sent.is_err() {
            self.shared.approvals.remove(&key);
            return ApprovalDecision::Reject;
        }
        loop {
            if self.shared.abort.load(Ordering::Relaxed) {
                self.shared.approvals.remove(&key);
                return ApprovalDecision::Reject;
            }
            match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(decision) => {
                    self.shared.approvals.remove(&key);
                    return decision;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.shared.approvals.remove(&key);
                    return ApprovalDecision::Reject;
                }
            }
        }
    }
}

pub struct Session {
    pub svc: TaskService,
    pub repo_root: PathBuf,
    pub initialized: bool,
    pub subscription: Option<Subscription>,
    pub run: Option<RunState>,
    pub queue: Option<Arc<QueueShared>>,
    pub queue_join: Option<std::thread::JoinHandle<()>>,
    pub parallel: Option<Arc<ParallelShared>>,
    pub parallel_join: Option<std::thread::JoinHandle<()>>,
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
            queue: None,
            queue_join: None,
            parallel: None,
            parallel_join: None,
            notif_tx,
        }
    }

    /// 路由一个请求。返回 Handled(正常响应)或 Shutdown(带最终响应,
    /// 调用方写完响应、join worker 后退出)。
    pub fn handle(&mut self, req: Request) -> Handled {
        if !self.initialized && req.cmd != "initialize" && req.cmd != "shutdown" {
            return Handled::Response(err_resp(req.id, "not_initialized", "必须先调用 initialize"));
        }
        match req.cmd.as_str() {
            "initialize" => Handled::Response(self.handle_initialize(&req)),
            "list_tasks" => Handled::Response(self.handle_list_tasks(&req)),
            "get_task" => Handled::Response(self.handle_get_task(&req)),
            "subscribe_events" => Handled::Response(self.handle_subscribe(&req)),
            "create_task" => Handled::Response(self.handle_create_task(&req)),
            "run_task" => Handled::Response(self.handle_run_task(&req)),
            "interrupt_task" => Handled::Response(self.handle_interrupt(&req)),
            "decide_approval" => Handled::Response(self.handle_decide(&req)),
            "run_queue" => Handled::Response(self.handle_run_queue(&req)),
            "stop_queue" => Handled::Response(self.handle_stop_queue(&req)),
            "get_queue_status" => Handled::Response(self.handle_get_queue_status(&req)),
            "run_parallel" => Handled::Response(self.handle_run_parallel(&req)),
            "stop_parallel" => Handled::Response(self.handle_stop_parallel(&req)),
            "get_parallel_status" => Handled::Response(self.handle_get_parallel_status(&req)),
            "apply_task" => Handled::Response(self.handle_apply_task(&req)),
            "merge_task" => Handled::Response(self.handle_merge_task(&req)),
            "get_workspace" => Handled::Response(self.handle_get_workspace(&req)),
            "remove_workspace" => Handled::Response(self.handle_remove_workspace(&req)),
            "shutdown" => {
                self.wait_worker();
                self.wait_queue();
                self.wait_parallel();
                Handled::Shutdown(Response::ok(req.id, serde_json::json!({ "bye": true })))
            }
            other => Handled::Response(err_resp(
                req.id,
                "unknown_command",
                format!("未知命令: {other}"),
            )),
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
                            "isolated_worktree": true,
                            "parallel_tasks": parallel_mod::MAX_PARALLEL_TASKS,
                            "merge_task": true,
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

    /// subscribe(spec 110 §2):after_seq 超最新 → invalid_params;
    /// 中间损坏 → error_occurred + 清除订阅 + 不部分重放;
    /// torn tail 仅提示,重放可读部分
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

        let (events, report) = match self.svc.events(&task_id) {
            Ok(x) => x,
            Err(e) => {
                // 中间损坏:error_occurred + 清除订阅,不部分重放
                let _ = self.notif_tx.send(Notification::error_occurred(&format!(
                    "订阅失败(账本损坏): {e}"
                )));
                self.subscription = None;
                return err_resp(req.id, "internal", e.to_string());
            }
        };
        let latest = events.last().map(|e| e.seq).unwrap_or(0);
        if after_seq > latest {
            return err_resp(
                req.id,
                "invalid_params",
                format!("after_seq {after_seq} 超过最新 seq {latest}"),
            );
        }
        if report.torn_tail {
            let _ = self.notif_tx.send(Notification::error_occurred(
                "账本尾行残缺已丢弃(上次进程中断残留)",
            ));
        }
        let mut replayed = 0usize;
        for e in events.iter().filter(|e| e.seq > after_seq) {
            let _ = self
                .notif_tx
                .send(Notification::task_updated(task_id.as_str(), e));
            replayed += 1;
        }
        let cursor = events.last().map(|e| e.seq).unwrap_or(after_seq);
        self.subscription = Some(Subscription {
            task_id: task_id.clone(),
            cursor,
            torn_warned: report.torn_tail,
        });
        Response::ok(
            req.id,
            serde_json::json!({ "task_id": task_id.as_str(), "replayed": replayed }),
        )
    }

    /// 周期调用(spec 110 §2):推送 cursor 之后的新账本事件;
    /// 中间损坏 → error_occurred + 清除订阅;新 torn tail → 提示一次
    pub fn poll_subscription(&mut self) {
        let Some(sub) = &self.subscription else {
            return;
        };
        let task_id = sub.task_id.clone();
        let mut cursor = sub.cursor;
        let mut torn_warned = sub.torn_warned;
        let (events, report) = match self.svc.events(&task_id) {
            Ok(x) => x,
            // 中间损坏:error_occurred + 清除订阅
            Err(e) => {
                let _ = self.notif_tx.send(Notification::error_occurred(&format!(
                    "订阅失败(账本损坏): {e}"
                )));
                self.subscription = None;
                return;
            }
        };
        if report.torn_tail && !torn_warned {
            let _ = self.notif_tx.send(Notification::error_occurred(
                "账本尾行残缺已丢弃(上次进程中断残留)",
            ));
            torn_warned = true;
        }
        for e in events.iter() {
            if e.seq <= cursor {
                continue;
            }
            let _ = self
                .notif_tx
                .send(Notification::task_updated(task_id.as_str(), e));
            cursor = e.seq;
        }
        if let Some(sub) = &mut self.subscription {
            sub.cursor = cursor;
            sub.torn_warned = torn_warned;
        }
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
        // 先回收已结束的 worker:结束后同任务可续跑(V1 限制的是"同时一个")
        let _ = self.worker_finished();
        if self.run.is_some() {
            return err_resp(req.id, "already_running", "已有运行中的任务(V1 单任务)");
        }
        let task_id = match self.parse_task_id(req) {
            Ok(id) => id,
            Err(resp) => return resp,
        };
        if ProviderConfig::resolve().is_err() {
            return err_resp(
                req.id,
                "internal",
                "provider 配置无效(修 .env 或 ~/.zealous/config.json)",
            );
        }
        let isolate = req
            .params
            .get("isolate")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // spec 156:全局 run lease(单并发防线)
        let mode = if isolate { "isolated" } else { "direct" };
        let run_lease =
            match zealous_engine::lease::try_acquire(&self.repo_root, task_id.as_str(), mode) {
                Ok(l) => l,
                Err(e) => return err_resp(req.id, "already_running", e.to_string()),
            };
        let handles = runner::spawn_worker(
            self.svc.clone(),
            self.repo_root.clone(),
            task_id.clone(),
            self.notif_tx.clone(),
            Arc::new(AtomicBool::new(false)),
            isolate,
            run_lease,
        );
        self.run = Some(RunState { task_id, handles });
        Response::ok(req.id, serde_json::json!({ "started": true }))
    }

    fn handle_interrupt(&mut self, req: &Request) -> Response {
        let task_id = match self.parse_task_id(req) {
            Ok(id) => id,
            Err(resp) => return resp,
        };
        match &self.run {
            Some(run) if run.task_id == task_id => {
                run.handles.interrupt();
                Response::ok(req.id, serde_json::json!({ "interrupting": true }))
            }
            _ => err_resp(req.id, "invalid_params", "该任务当前未在运行"),
        }
    }

    /// decide_approval(spec 110 §3):task 与 call_id 双重绑定校验,
    /// 决策后清除 pending;任何不匹配一律 no_pending_approval(fail closed)
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

        let param_task = req
            .params
            .get("task_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let param_call = req
            .params
            .get("call_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if self.run.is_none() {
            if let Some(parallel) = &self.parallel {
                let key = format!("{param_task}:{param_call}");
                if !param_task.is_empty()
                    && !param_call.is_empty()
                    && parallel.approvals.decide(&key, decision)
                {
                    return Response::ok(req.id, serde_json::json!({ "decided": decision_str }));
                }
                return err_resp(
                    req.id,
                    "no_pending_approval",
                    "task/call 不匹配当前 parallel 待决审批",
                );
            }
            return err_resp(req.id, "no_pending_approval", "当前没有运行中的任务");
        }
        // task 绑定:必须匹配当前运行任务
        let Some(run) = &self.run else {
            return err_resp(req.id, "no_pending_approval", "当前没有运行中的任务");
        };
        if param_task != run.task_id.as_str() {
            return err_resp(
                req.id,
                "no_pending_approval",
                format!("task_id 不匹配当前运行任务: {param_task}"),
            );
        }

        // call_id 绑定:必须与当前 pending 一致(未知/重复/过期均拒绝)
        let current_call = run.handles.pending.lock().expect("pending slot").clone();
        match current_call {
            Some(pending_call) if !param_call.is_empty() && pending_call == param_call => {}
            _ => {
                return err_resp(
                    req.id,
                    "no_pending_approval",
                    format!("call_id 不匹配当前待决审批: {param_call}"),
                );
            }
        }

        // 决策发送后清除 pending 绑定(重复决策一律拒绝)
        let sent = run.handles.decision_tx.send(decision).is_ok();
        if let Some(pending) = run.handles.pending.lock().expect("pending slot").as_mut() {
            pending.clear();
        }
        if sent {
            Response::ok(req.id, serde_json::json!({ "decided": decision_str }))
        } else {
            err_resp(req.id, "no_pending_approval", "决策通道已关闭(无待决审批)")
        }
    }

    /// worker 是否已结束;结束后回收 join。
    /// 主循环每轮调用:RunState 不回收会让二次 run_task 撞 already_running(P1)。
    pub fn worker_finished(&mut self) -> bool {
        let Some(run) = &self.run else { return true };
        let finished = run.handles.done.load(Ordering::SeqCst)
            || run
                .handles
                .join
                .as_ref()
                .map(|j| j.is_finished())
                .unwrap_or(true);
        if finished {
            if let Some(mut run) = self.run.take() {
                if let Some(j) = run.handles.join.take() {
                    let _ = j.join();
                }
            }
        }
        finished
    }

    fn wait_worker(&mut self) {
        if let Some(mut run) = self.run.take() {
            if let Some(j) = run.handles.join.take() {
                let _ = j.join();
            }
        }
    }

    /// spec 156:等待 queue worker 结束(shutdown)
    fn wait_queue(&mut self) {
        if let Some(join) = self.queue_join.take() {
            let _ = join.join();
        }
    }

    /// 回收已结束的 queue 运行时
    fn reap_queue(&mut self) {
        if let Some(q) = &self.queue {
            if q.finished.load(Ordering::SeqCst) {
                self.wait_queue();
                self.queue = None;
            }
        }
    }

    /// run_queue(spec 156):后台串行执行;单 session 一个 queue worker
    fn handle_run_queue(&mut self, req: &Request) -> Response {
        self.reap_queue();
        if let Some(q) = &self.queue {
            if !q.finished.load(Ordering::SeqCst) {
                return err_resp(req.id, "already_running", "已有队列在运行");
            }
        }
        let all = req
            .params
            .get("all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let auto_apply = req
            .params
            .get("auto_apply")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut task_ids: Vec<TaskId> = Vec::new();
        if all {
            match queue_mod::select_runnable(&self.svc) {
                Ok((runnable, _)) => task_ids = runnable,
                Err(e) => return err_resp(req.id, "internal", e),
            }
            if task_ids.is_empty() {
                return err_resp(req.id, "no_runnable_tasks", "(no runnable queued tasks)");
            }
        } else if let Some(arr) = req.params.get("task_ids").and_then(|v| v.as_array()) {
            for v in arr {
                match v.as_str().map(TaskId::parse) {
                    Some(Ok(id)) => task_ids.push(id),
                    _ => {
                        return err_resp(req.id, "invalid_params", "task_ids 含非法任务 ID");
                    }
                }
            }
        } else {
            return err_resp(req.id, "invalid_params", "task_ids 或 all 必填");
        }
        if !all {
            if let Err((i, id, reason)) = queue_mod::validate_explicit(&self.svc, &task_ids) {
                return err_resp(
                    req.id,
                    "queue_start_failed",
                    format!("第 {} 个任务 {id}: {reason}", i + 1),
                );
            }
        }
        if ProviderConfig::resolve().is_err() {
            return err_resp(req.id, "internal", "provider 配置无效");
        }
        // 全局 run lease(队列全程持有,线程结束 Drop 释放)
        let lease = match zealous_engine::lease::try_acquire(&self.repo_root, "queue", "queue") {
            Ok(l) => l,
            Err(e) => return err_resp(req.id, "already_running", e.to_string()),
        };
        let total = task_ids.len();
        let shared = Arc::new(QueueShared {
            abort: Arc::new(AtomicBool::new(false)),
            task_ids: task_ids.clone(),
            index: AtomicUsize::new(0),
            auto_apply,
            completed: Mutex::new(Vec::new()),
            last_failed: Mutex::new(None),
            finished: AtomicBool::new(false),
        });
        let notif = self.notif_tx.clone();
        let svc = self.svc.clone();
        let repo = self.repo_root.clone();
        let shared_t = shared.clone();
        let join = std::thread::spawn(move || {
            let _lease = lease;
            let config = match ProviderConfig::resolve() {
                Ok(c) => c,
                Err(e) => {
                    shared_t.finished.store(true, Ordering::SeqCst);
                    let _ = notif.send(Notification::error_occurred(&format!(
                        "provider 配置无效: {e}"
                    )));
                    return;
                }
            };
            let model = config.model.clone();
            let provider: Arc<dyn zealous_engine::provider::Provider> =
                Arc::new(OpenAiCompatProvider::new(config));
            let (approval_sink, request_rx, _decision) =
                crate::tui::approval::channel("queue".into(), Arc::new(AtomicBool::new(false)));
            // 队列内无交互审批对象:请求一律转 error 通知(V1 队列建议 auto_apply/yes 语义由模型侧脚本控制;
            // 真实交互审批场景请使用 CLI queue run)
            {
                let notif2 = notif.clone();
                std::thread::spawn(move || {
                    for r in request_rx {
                        let _ = notif2.send(Notification::error_occurred(&format!(
                            "队列内收到审批请求(无交互对象,已拒绝): {} {}",
                            r.tool, r.summary
                        )));
                    }
                });
            }
            let progress: Option<zealous_engine::progress::SharedSink> = None;
            let mut hooks = queue_mod::QueueHooks {
                on_task_started: Box::new(|tid, i, total| {
                    shared_t.index.store(i - 1, Ordering::SeqCst);
                    let _ = notif.send(Notification::progress(
                        "queue_task_started",
                        &format!("{} ({i}/{total})", tid.as_str()),
                    ));
                }),
                on_task_finished: Box::new(|tid, status| {
                    if status != TaskStatus::Completed {
                        *shared_t.last_failed.lock().unwrap() =
                            Some(format!("{:?}: {}", status, tid.as_str()));
                    }
                    let _ = notif.send(Notification::progress(
                        "queue_task_finished",
                        &format!("{} {:?}", tid.as_str(), status),
                    ));
                }),
                on_apply_finished: Box::new(|tid, ok, detail| {
                    if ok {
                        shared_t.completed.lock().unwrap().push(tid.as_str().into());
                    }
                    let _ = notif.send(Notification::progress(
                        "queue_apply_finished",
                        &format!("{} {} {}", tid.as_str(), ok, detail),
                    ));
                }),
                confirm_apply: Box::new(|_| auto_apply),
            };
            let summary = queue_mod::run_serial(
                &repo,
                &svc,
                provider,
                model.clone(),
                24,
                &task_ids,
                auto_apply,
                Arc::new(approval_sink) as Arc<dyn ApprovalSink>,
                progress,
                shared_t.abort.clone(),
                &_lease,
                &mut hooks,
            );
            shared_t.finished.store(true, Ordering::SeqCst);
            let _ = notif.send(Notification::progress(
                "queue_finished",
                &format!(
                    "completed={} applied={} stopped={}",
                    summary.completed.len(),
                    summary.applied.len(),
                    summary.stopped_reason
                ),
            ));
        });
        self.queue = Some(shared);
        self.queue_join = Some(join);
        Response::ok(
            req.id,
            serde_json::json!({ "queue_started": true, "total": total }),
        )
    }

    fn handle_stop_queue(&mut self, req: &Request) -> Response {
        match &self.queue {
            Some(q) if !q.finished.load(Ordering::SeqCst) => {
                q.abort.store(true, Ordering::SeqCst);
                Response::ok(req.id, serde_json::json!({ "stopping": true }))
            }
            _ => err_resp(req.id, "invalid_params", "当前没有运行中的队列"),
        }
    }

    fn handle_get_queue_status(&mut self, req: &Request) -> Response {
        match &self.queue {
            Some(q) => {
                let index = q.index.load(Ordering::SeqCst);
                let pending: Vec<String> = q
                    .task_ids
                    .iter()
                    .skip(index + 1)
                    .map(|t| t.as_str().into())
                    .collect();
                Response::ok(
                    req.id,
                    serde_json::json!({
                        "active": q.task_ids.get(index).map(|t| t.as_str()),
                        "index": index,
                        "total": q.task_ids.len(),
                        "pending": pending,
                        "completed": q.completed.lock().unwrap().clone(),
                        "last_failed": q.last_failed.lock().unwrap().clone(),
                        "apply_policy": if q.auto_apply { "auto" } else { "confirm" },
                        "finished": q.finished.load(Ordering::SeqCst),
                    }),
                )
            }
            None => Response::ok(
                req.id,
                serde_json::json!({ "active": null, "total": 0, "finished": true }),
            ),
        }
    }

    /// spec 157:等待 parallel worker 结束(shutdown)
    fn wait_parallel(&mut self) {
        if let Some(join) = self.parallel_join.take() {
            let _ = join.join();
        }
    }

    fn reap_parallel(&mut self) {
        if let Some(p) = &self.parallel {
            if p.finished.load(Ordering::SeqCst) {
                self.wait_parallel();
                self.parallel = None;
            }
        }
    }

    fn handle_run_parallel(&mut self, req: &Request) -> Response {
        self.reap_parallel();
        if let Some(p) = &self.parallel {
            if !p.finished.load(Ordering::SeqCst) {
                return err_resp(req.id, "already_running", "已有 parallel batch 在运行");
            }
        }
        let all = req
            .params
            .get("all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let max = req
            .params
            .get("max")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(parallel_mod::MAX_PARALLEL_TASKS);
        if let Err(e) = parallel_mod::validate_limit(max) {
            return err_resp(req.id, "invalid_params", e);
        }
        let auto_apply = req
            .params
            .get("auto_apply")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let integrate = req
            .params
            .get("integrate")
            .and_then(|v| v.as_str())
            .unwrap_or("ff-only");
        let integration = match parallel_mod::IntegrationStrategy::parse(integrate) {
            Ok(strategy) => strategy,
            Err(e) => return err_resp(req.id, "invalid_params", e),
        };
        let mut task_ids: Vec<TaskId> = Vec::new();
        if all {
            match parallel_mod::select_runnable(&self.svc, max) {
                Ok((runnable, _)) => task_ids = runnable,
                Err(e) => return err_resp(req.id, "internal", e),
            }
            if task_ids.is_empty() {
                return err_resp(req.id, "no_runnable_tasks", "(no runnable queued tasks)");
            }
        } else if let Some(arr) = req.params.get("task_ids").and_then(|v| v.as_array()) {
            if arr.len() > parallel_mod::MAX_PARALLEL_TASKS {
                return err_resp(
                    req.id,
                    "invalid_params",
                    format!(
                        "Alpha supports at most {} tasks",
                        parallel_mod::MAX_PARALLEL_TASKS
                    ),
                );
            }
            for v in arr {
                match v.as_str().map(TaskId::parse) {
                    Some(Ok(id)) => task_ids.push(id),
                    _ => return err_resp(req.id, "invalid_params", "task_ids 含非法任务 ID"),
                }
            }
        } else {
            return err_resp(req.id, "invalid_params", "task_ids 或 all 必填");
        }
        if !all {
            if let Err((i, id, reason)) = parallel_mod::validate_explicit(&self.svc, &task_ids) {
                return err_resp(
                    req.id,
                    "parallel_start_failed",
                    format!("第 {} 个任务 {id}: {reason}", i + 1),
                );
            }
        }
        if ProviderConfig::resolve().is_err() {
            return err_resp(req.id, "internal", "provider 配置无效");
        }
        let base_commit = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&self.repo_root)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        let lease =
            match zealous_engine::lease::try_acquire(&self.repo_root, "parallel", "parallel") {
                Ok(l) => l,
                Err(e) => return err_resp(req.id, "already_running", e.to_string()),
            };
        let total = task_ids.len();
        let shared = Arc::new(ParallelShared {
            abort: Arc::new(AtomicBool::new(false)),
            task_ids: task_ids.clone(),
            active: Mutex::new(task_ids.iter().map(|id| id.as_str().to_string()).collect()),
            auto_apply,
            base_commit: base_commit.clone(),
            completed: Mutex::new(Vec::new()),
            failed: Mutex::new(Vec::new()),
            interrupted: Mutex::new(Vec::new()),
            applied: Mutex::new(Vec::new()),
            merged: Mutex::new(Vec::new()),
            apply_index: AtomicUsize::new(0),
            finished: AtomicBool::new(false),
            approvals: Arc::new(ParallelApprovals {
                pending: Mutex::new(HashMap::new()),
            }),
        });
        let notif = self.notif_tx.clone();
        let svc = self.svc.clone();
        let repo = self.repo_root.clone();
        let shared_t = shared.clone();
        let join = std::thread::spawn(move || {
            let _lease = lease;
            let config = match ProviderConfig::resolve() {
                Ok(c) => c,
                Err(e) => {
                    shared_t.finished.store(true, Ordering::SeqCst);
                    let _ = notif.send(Notification::error_occurred(&format!(
                        "provider 配置无效: {e}"
                    )));
                    return;
                }
            };
            let model = config.model.clone();
            let provider: Arc<dyn zealous_engine::provider::Provider> =
                Arc::new(OpenAiCompatProvider::new(config));
            let providers = task_ids
                .iter()
                .map(|_| provider.clone())
                .collect::<Vec<_>>();
            let approval_sink = Arc::new(ParallelApprovalSink {
                shared: shared_t.clone(),
                notif: notif.clone(),
            });
            let mut hooks = parallel_mod::ParallelHooks {
                on_task_started: Box::new(|tid, i, total| {
                    let _ = notif.send(Notification::progress(
                        "parallel_task_started",
                        &format!("{} ({i}/{total})", tid.as_str()),
                    ));
                }),
                on_task_finished: Box::new(|tid, status| {
                    let label = tid.as_str().to_string();
                    match status {
                        TaskStatus::Completed => {
                            shared_t.completed.lock().unwrap().push(label.clone())
                        }
                        TaskStatus::Interrupted => {
                            shared_t.interrupted.lock().unwrap().push(label.clone())
                        }
                        _ => shared_t.failed.lock().unwrap().push(label.clone()),
                    }
                    let _ = notif.send(Notification::progress(
                        "parallel_task_finished",
                        &format!("{} {:?}", label, status),
                    ));
                }),
                on_apply_started: Box::new(|tid| {
                    shared_t.apply_index.fetch_add(1, Ordering::SeqCst);
                    let _ = notif.send(Notification::progress(
                        "parallel_apply_started",
                        tid.as_str(),
                    ));
                }),
                on_apply_finished: Box::new(|tid, ok, detail| {
                    if ok {
                        shared_t
                            .applied
                            .lock()
                            .unwrap()
                            .push(tid.as_str().to_string());
                    }
                    let _ = notif.send(Notification::progress(
                        "parallel_apply_finished",
                        &format!("{} {} {}", tid.as_str(), ok, detail),
                    ));
                }),
                on_merge_started: Box::new(|tid| {
                    shared_t.apply_index.fetch_add(1, Ordering::SeqCst);
                    let _ = notif.send(Notification::progress(
                        "parallel_merge_started",
                        tid.as_str(),
                    ));
                }),
                on_merge_finished: Box::new(|tid, ok, detail| {
                    if ok {
                        shared_t
                            .applied
                            .lock()
                            .unwrap()
                            .push(tid.as_str().to_string());
                        shared_t
                            .merged
                            .lock()
                            .unwrap()
                            .push(tid.as_str().to_string());
                    }
                    let _ = notif.send(Notification::progress(
                        "parallel_merge_finished",
                        &format!("{} {} {}", tid.as_str(), ok, detail),
                    ));
                }),
                confirm_apply: Box::new(|_| auto_apply),
            };
            let summary = parallel_mod::run_parallel(
                &repo,
                &svc,
                providers,
                model,
                24,
                &task_ids,
                auto_apply,
                integration,
                approval_sink,
                None,
                shared_t.abort.clone(),
                &_lease,
                &mut hooks,
            );
            *shared_t.active.lock().unwrap() = Vec::new();
            shared_t.finished.store(true, Ordering::SeqCst);
            let _ = notif.send(Notification::progress(
                "parallel_finished",
                &format!(
                    "completed={} failed={} interrupted={} integrated={} merged={} base={} stopped={}",
                    summary.completed.len(),
                    summary.failed.len(),
                    summary.interrupted.len(),
                    summary.applied.len(),
                    summary.merged.len(),
                    summary.base_commit,
                    summary.stopped_reason
                ),
            ));
        });
        self.parallel = Some(shared);
        self.parallel_join = Some(join);
        Response::ok(
            req.id,
            serde_json::json!({ "parallel_started": true, "total": total, "base_commit": base_commit, "integrate": integrate }),
        )
    }

    fn handle_stop_parallel(&mut self, req: &Request) -> Response {
        match &self.parallel {
            Some(p) if !p.finished.load(Ordering::SeqCst) => {
                p.abort.store(true, Ordering::SeqCst);
                Response::ok(req.id, serde_json::json!({ "stopping": true }))
            }
            _ => err_resp(req.id, "invalid_params", "当前没有运行中的 parallel batch"),
        }
    }

    fn handle_get_parallel_status(&self, req: &Request) -> Response {
        match &self.parallel {
            Some(p) => Response::ok(
                req.id,
                serde_json::json!({
                    "active": *p.active.lock().unwrap(),
                    "pending": [],
                    "completed": p.completed.lock().unwrap().clone(),
                    "failed": p.failed.lock().unwrap().clone(),
                    "interrupted": p.interrupted.lock().unwrap().clone(),
                    "applied": p.applied.lock().unwrap().clone(),
                    "merged": p.merged.lock().unwrap().clone(),
                    "apply_index": p.apply_index.load(Ordering::SeqCst),
                    "total": p.task_ids.len(),
                    "apply_policy": if p.auto_apply { "auto" } else { "confirm" },
                    "base_commit": p.base_commit,
                    "finished": p.finished.load(Ordering::SeqCst),
                }),
            ),
            None => Response::ok(
                req.id,
                serde_json::json!({ "active": [], "total": 0, "finished": true }),
            ),
        }
    }

    fn parse_task(&self, req: &Request) -> Result<TaskId, Response> {
        self.parse_task_id(req)
    }

    /// apply_task(spec 155):fast-forward 应用;拒绝场景原样透传错误码
    fn handle_apply_task(&mut self, req: &Request) -> Response {
        let task_id = match self.parse_task(req) {
            Ok(id) => id,
            Err(resp) => return resp,
        };
        match self.svc.open_task(&task_id) {
            Ok((ledger, _proj, _)) => {
                match zealous_engine::workspace::apply(&self.repo_root, &ledger) {
                    Ok(out) => Response::ok(
                        req.id,
                        serde_json::json!({
                            "branch": out.branch,
                            "applied_commit": out.applied_commit,
                            "already": out.already,
                        }),
                    ),
                    Err(e) => err_resp(req.id, "workspace", e.to_string()),
                }
            }
            Err(e) => err_resp(req.id, "internal", e.to_string()),
        }
    }

    /// get_workspace(spec 155):workspace 状态必须来自事件投影
    /// merge_task(spec 158):显式三方合并 Completed isolated task
    fn handle_merge_task(&self, req: &Request) -> Response {
        let task_id = match self.parse_task_id(req) {
            Ok(id) => id,
            Err(resp) => return resp,
        };
        let (ledger, _projection, _) = match self.svc.open_task(&task_id) {
            Ok(value) => value,
            Err(e) => return err_resp(req.id, "task_not_found", e.to_string()),
        };
        let _ = self
            .notif_tx
            .send(Notification::progress("merge_started", task_id.as_str()));
        match zealous_engine::workspace::merge(&self.repo_root, &ledger) {
            Ok(outcome) => {
                let _ = self.notif_tx.send(Notification::progress(
                    "merge_finished",
                    &format!(
                        "{} {} {}",
                        task_id.as_str(),
                        outcome.task_tip,
                        outcome.merge_commit
                    ),
                ));
                Response::ok(
                    req.id,
                    serde_json::json!({
                        "merged": true,
                        "branch": outcome.branch,
                        "base_commit": outcome.base_commit,
                        "task_tip": outcome.task_tip,
                        "merge_commit": outcome.merge_commit
                    }),
                )
            }
            Err(e) => {
                let _ = self
                    .notif_tx
                    .send(Notification::error_occurred(&e.to_string()));
                err_resp(req.id, "merge_failed", e.to_string())
            }
        }
    }

    fn handle_get_workspace(&mut self, req: &Request) -> Response {
        let task_id = match self.parse_task(req) {
            Ok(id) => id,
            Err(resp) => return resp,
        };
        match self.svc.open_task(&task_id) {
            Ok((_ledger, proj, _)) => match &proj.execution_workspace {
                Some(ws) => Response::ok(
                    req.id,
                    serde_json::json!({
                        "mode": "isolated",
                        "kind": ws.kind,
                        "path": ws.path,
                        "branch": ws.branch,
                        "base_branch": ws.base_branch,
                        "base_commit": ws.base_commit,
                        "current_commit": ws.current_commit,
                        "status": ws.status,
                        "applied_commit": proj.applied_commit,
                        "workspace_removed": proj.workspace_removed,
                    }),
                ),
                None => Response::ok(req.id, serde_json::json!({ "mode": "direct" })),
            },
            Err(e) => err_resp(req.id, "internal", e.to_string()),
        }
    }

    /// remove_workspace(spec 155):force 透传;拒绝场景原样透传
    fn handle_remove_workspace(&mut self, req: &Request) -> Response {
        let task_id = match self.parse_task(req) {
            Ok(id) => id,
            Err(resp) => return resp,
        };
        let force = req
            .params
            .get("force")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        match self.svc.open_task(&task_id) {
            Ok((ledger, _proj, _)) => {
                match zealous_engine::workspace::remove(&self.repo_root, &ledger, force) {
                    Ok(info) => Response::ok(
                        req.id,
                        serde_json::json!({
                            "branch": info.branch,
                            "path": info.path.display().to_string(),
                        }),
                    ),
                    Err(e) => err_resp(req.id, "workspace", e.to_string()),
                }
            }
            Err(e) => err_resp(req.id, "internal", e.to_string()),
        }
    }
}
