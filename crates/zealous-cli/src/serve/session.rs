//! 连接状态与请求路由(spec 100 + 110 §2/§3):会话不拥有任务状态,
//! 事实来自 TaskService(投影/账本);engine 是唯一写入者。

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};

use zealous_core::{ApprovalDecision, TaskId};
use zealous_engine::provider::ProviderConfig;
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

pub struct Session {
    pub svc: TaskService,
    pub repo_root: PathBuf,
    pub initialized: bool,
    pub subscription: Option<Subscription>,
    pub run: Option<RunState>,
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
            "shutdown" => {
                self.wait_worker();
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
        let handles = runner::spawn_worker(
            self.svc.clone(),
            self.repo_root.clone(),
            task_id.clone(),
            self.notif_tx.clone(),
            Arc::new(AtomicBool::new(false)),
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

        // task 绑定:必须匹配当前运行任务
        let Some(run) = &self.run else {
            return err_resp(req.id, "no_pending_approval", "当前没有运行中的任务");
        };
        let param_task = req
            .params
            .get("task_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if param_task != run.task_id.as_str() {
            return err_resp(
                req.id,
                "no_pending_approval",
                format!("task_id 不匹配当前运行任务: {param_task}"),
            );
        }

        // call_id 绑定:必须与当前 pending 一致(未知/重复/过期均拒绝)
        let param_call = req
            .params
            .get("call_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
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

    /// worker 是否已结束;结束后回收 join
    pub fn worker_finished(&mut self) -> bool {
        let Some(run) = &self.run else { return true };
        let finished = run
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
}
