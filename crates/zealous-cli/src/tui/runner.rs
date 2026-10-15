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

        let executor = Arc::new(zealous_engine::executor::ToolExecutor::new(
            svc.repo_root().to_path_buf(),
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

        let status = match orchestrator.run_task(&ledger, &proj.title) {
            Ok(s) => s,
            Err(e) => {
                let _ = event_tx.send(TuiEvent::Failure(format!("编排失败: {e}")));
                return;
            }
        };
        let _ = event_tx.send(TuiEvent::Finished(status));
    });
}
