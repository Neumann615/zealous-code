//! engine worker thread(spec 90):任务启动与结果回传。
//! 进度/审批/结果三类事实经单一 mpsc 事件流进入 TUI;
//! 审批决策由 TUI 经独立通道注入,abort 与 TUI 共享。

use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};

use zealous_core::{TaskId, TaskStatus};
use zealous_engine::approval::ApprovalSink;
use zealous_engine::orchestrator::Orchestrator;
use zealous_engine::progress::ProgressSink;
use zealous_engine::provider::{OpenAiCompatProvider, ProviderConfig};
use zealous_engine::task_service::TaskService;
use zealous_engine::tools::builtin_registry;

use super::approval::ApprovalRequest;

/// 发往 TUI 的统一事件
#[derive(Debug)]
pub enum TuiEvent {
    Approval(ApprovalRequest),
    /// 当前 run 结束(Completed/Failed/Interrupted…)
    Finished(TaskStatus),
    /// 启动期失败(配置/打开任务等),不进入编排
    Failure(String),
    /// spec 156:队列/后台进度行
    Progress(String),
    /// spec 156:队列结束(汇总文本)
    QueueFinished(String),
}

/// 启动一次运行/恢复;立即返回,结果经 event_tx 回传。
/// 决策经 decision_rx 注入;abort 由 TUI 持有并在中断/退出时置位。
pub fn spawn_run(
    svc: TaskService,
    task_id: TaskId,
    event_tx: mpsc::Sender<TuiEvent>,
    approval_sink: Arc<dyn ApprovalSink>,
    progress_sink: Arc<dyn ProgressSink>,
    abort: Arc<AtomicBool>,
    isolate: bool,
) {
    std::thread::spawn(move || {
        let config = match ProviderConfig::resolve() {
            Ok(c) => c,
            Err(e) => {
                let _ = event_tx.send(TuiEvent::Failure(format!(
                    "provider 配置无效: {e}(可用 .env 或 ZEALOUS_* 环境变量)"
                )));
                return;
            }
        };
        let (ledger, proj, _) = match svc.open_task(&task_id) {
            Ok(x) => x,
            Err(e) => {
                let _ = event_tx.send(TuiEvent::Failure(format!("打开任务失败: {e}")));
                return;
            }
        };
        let ledger = Arc::new(ledger);

        // spec 156:全局 run lease(线程持有至结束,Drop 释放)
        let _run_lease = match zealous_engine::lease::try_acquire(
            svc.repo_root(),
            task_id.as_str(),
            if isolate { "isolated" } else { "direct" },
        ) {
            Ok(l) => l,
            Err(e) => {
                let _ = event_tx.send(TuiEvent::Failure(format!("already_running: {e}")));
                return;
            }
        };

        // spec 155:运行根解析(模式规则落账;isolated 校验/重建在此完成)
        let run_root = match zealous_engine::workspace::ensure_run_root(
            svc.repo_root(),
            &ledger,
            &task_id,
            isolate,
        ) {
            Ok(r) => r,
            Err(e) => {
                let _ = event_tx.send(TuiEvent::Failure(format!("workspace: {e}")));
                return;
            }
        };
        let mut prompt = proj.title.clone();
        if run_root != svc.repo_root().to_path_buf() {
            prompt = format!(
                "[isolated workspace] 本次运行在任务专属 git worktree 中执行;\n主工作区的未提交改动在此不可见,不得尝试访问或修改主工作区。\n\n{prompt}"
            );
        }

        let executor = Arc::new(zealous_engine::executor::ToolExecutor::new(
            run_root,
            builtin_registry(),
            approval_sink,
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

        let status = match orchestrator.run_task(&ledger, &prompt) {
            Ok(s) => s,
            Err(e) => {
                let _ = event_tx.send(TuiEvent::Failure(format!("编排失败: {e}")));
                return;
            }
        };
        let _ = event_tx.send(TuiEvent::Finished(status));
    });
}

/// spec 156:后台串行队列(A 确认后调用);进度/结果经 event_tx 回传。
pub fn spawn_queue(
    svc: TaskService,
    task_ids: Vec<TaskId>,
    event_tx: mpsc::Sender<TuiEvent>,
    progress_sink: Arc<dyn ProgressSink>,
    abort: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let config = match ProviderConfig::resolve() {
            Ok(c) => c,
            Err(e) => {
                let _ = event_tx.send(TuiEvent::Failure(format!("provider 配置无效: {e}")));
                return;
            }
        };
        // 全局 run lease(队列全程持有)
        let lease = match zealous_engine::lease::try_acquire(svc.repo_root(), "queue", "queue") {
            Ok(l) => l,
            Err(e) => {
                let _ = event_tx.send(TuiEvent::Failure(format!("already_running: {e}")));
                return;
            }
        };
        let model = config.model.clone();
        let provider: Arc<dyn zealous_engine::provider::Provider> =
            Arc::new(zealous_engine::provider::OpenAiCompatProvider::new(config));
        let (approval_sink, request_rx, _decision) =
            crate::tui::approval::channel("queue".into(), abort.clone());
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
        let progress: Arc<dyn ProgressSink> = Arc::new(NotifProgressSink2 {
            tx: event_tx.clone(),
        });
        let mut hooks = zealous_engine::queue::QueueHooks {
            on_task_started: Box::new(|tid, i, total| {
                progress_sink.progress(&format!(
                    "[{i}/{total}] queue task {} 开始(isolated)",
                    tid.as_str()
                ));
            }),
            on_task_finished: Box::new(|tid, status| {
                progress_sink.progress(&format!("queue task {} 结束: {status:?}", tid.as_str()));
            }),
            on_apply_finished: Box::new(|tid, ok, detail| {
                let mark = if ok { "成功" } else { "失败" };
                progress_sink.progress(&format!(
                    "queue task {} apply {mark}: {detail}",
                    tid.as_str()
                ));
            }),
            confirm_apply: Box::new(|_| true),
        };
        let summary = zealous_engine::queue::run_serial(
            svc.repo_root(),
            &svc,
            provider,
            model.clone(),
            24,
            &task_ids,
            true,
            Arc::new(approval_sink) as Arc<dyn zealous_engine::approval::ApprovalSink>,
            Some(progress),
            abort,
            &lease,
            &mut hooks,
        );
        let _ = event_tx.send(TuiEvent::QueueFinished(format!(
            "completed={} applied={} stopped={}",
            summary.completed.len(),
            summary.applied.len(),
            summary.stopped_reason
        )));
    });
}

/// 进度 → TuiEvent::Progress 桥
struct NotifProgressSink2 {
    tx: mpsc::Sender<TuiEvent>,
}

impl ProgressSink for NotifProgressSink2 {
    fn progress(&self, line: &str) {
        let _ = self.tx.send(TuiEvent::Progress(line.to_string()));
    }
    fn important(&self, line: &str) {
        let _ = self.tx.send(TuiEvent::Progress(line.to_string()));
    }
}
