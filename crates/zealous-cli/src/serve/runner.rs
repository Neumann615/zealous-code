//! engine worker 与通道桥接(spec 100):run_task 的 worker 线程、
//! 审批/进度通知桥接。事实只来自事件账本与编排器,不创造状态。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

use zealous_core::{ApprovalDecision, TaskId};
use zealous_engine::orchestrator::Orchestrator;
use zealous_engine::progress::ProgressSink;
use zealous_engine::provider::{OpenAiCompatProvider, ProviderConfig};
use zealous_engine::task_service::TaskService;
use zealous_engine::tools::builtin_registry;

use super::protocol::Notification;

/// 当前挂起审批的绑定槽(spec 110 §3):decide_approval 校验 call_id 用
pub type PendingSlot = Arc<Mutex<Option<String>>>;

/// worker 句柄:abort / 决策注入 / pending 绑定 / join
pub struct RunHandles {
    pub abort: Arc<AtomicBool>,
    pub decision_tx: mpsc::Sender<ApprovalDecision>,
    pub pending: PendingSlot,
    pub join: Option<JoinHandle<()>>,
}

impl RunHandles {
    pub fn interrupt(&self) {
        self.abort.store(true, Ordering::Relaxed);
    }
}

/// 启动 run worker;立即返回句柄,结果经通知回传
/// (run_finished 由 worker 按实际结束状态发送)。
pub fn spawn_worker(
    svc: TaskService,
    repo_root: PathBuf,
    task_id: TaskId,
    notif_tx: mpsc::Sender<Notification>,
    abort: Arc<AtomicBool>,
) -> RunHandles {
    let (approval_sink, request_rx, decision_tx) =
        crate::tui::approval::channel(task_id.as_str().to_string(), abort.clone());
    let pending: PendingSlot = Arc::new(Mutex::new(None));

    // 审批请求 → approval_requested 通知 + 绑定槽
    {
        let tx = notif_tx.clone();
        let pending = pending.clone();
        std::thread::spawn(move || {
            for r in request_rx {
                *pending.lock().expect("pending slot") = Some(r.call_id.clone());
                let note = Notification::approval_requested(
                    &r.task_id, &r.call_id, &r.tool, &r.risk, &r.summary,
                );
                if tx.send(note).is_err() {
                    break;
                }
            }
        });
    }

    let progress: Arc<dyn ProgressSink> = Arc::new(NotifProgressSink {
        tx: notif_tx.clone(),
    });
    let executor = Arc::new(zealous_engine::executor::ToolExecutor::new(
        repo_root,
        builtin_registry(),
        Arc::new(approval_sink),
    ));

    let worker_abort = abort.clone();
    let join = std::thread::spawn(move || {
        let config = match ProviderConfig::resolve() {
            Ok(c) => c,
            Err(e) => {
                let _ = notif_tx.send(Notification::error_occurred(&format!(
                    "provider 配置无效: {e}"
                )));
                return;
            }
        };
        let model = config.model.clone();
        let (ledger, proj, _) = match svc.open_task(&task_id) {
            Ok(x) => x,
            Err(e) => {
                let _ = notif_tx.send(Notification::error_occurred(&format!("打开任务失败: {e}")));
                return;
            }
        };
        let ledger = Arc::new(ledger);
        let orchestrator = Orchestrator {
            provider: Arc::new(OpenAiCompatProvider::new(config)),
            executor,
            abort: worker_abort,
            max_turns: 24,
            model,
            progress: Some(progress),
        };
        let status = match orchestrator.run_task(&ledger, &proj.title) {
            Ok(s) => s,
            Err(e) => {
                let _ = notif_tx.send(Notification::error_occurred(&format!("编排失败: {e}")));
                return;
            }
        };
        let _ = notif_tx.send(Notification::run_finished(
            task_id.as_str(),
            &format!("{status:?}"),
        ));
    });

    RunHandles {
        abort,
        decision_tx,
        pending,
        join: Some(join),
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
