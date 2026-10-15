//! channel 化审批(spec 90):engine 的 ApprovalSink 把请求发往 TUI,
//! 阻塞等待决策;abort 置位或通道断开时 fail closed(Reject)。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use zealous_core::ApprovalDecision;
use zealous_engine::approval::ApprovalSink;

/// 发往 TUI 的审批请求
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub call_id: String,
    pub summary: String,
}

const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// engine 侧 sink(阻塞等待决策)
pub struct ChannelApprovalSink {
    request_tx: mpsc::Sender<ApprovalRequest>,
    decision_rx: Mutex<mpsc::Receiver<ApprovalDecision>>,
    abort: Arc<AtomicBool>,
}

impl ChannelApprovalSink {
    pub fn new(
        request_tx: mpsc::Sender<ApprovalRequest>,
        decision_rx: mpsc::Receiver<ApprovalDecision>,
        abort: Arc<AtomicBool>,
    ) -> Self {
        Self {
            request_tx,
            decision_rx: Mutex::new(decision_rx),
            abort,
        }
    }
}

impl ApprovalSink for ChannelApprovalSink {
    fn decide(&self, call_id: &str, summary: &str) -> ApprovalDecision {
        // 请求通道断开(TUI 已退出)= fail closed
        if self
            .request_tx
            .send(ApprovalRequest {
                call_id: call_id.to_string(),
                summary: summary.to_string(),
            })
            .is_err()
        {
            return ApprovalDecision::Reject;
        }
        loop {
            if self.abort.load(Ordering::Relaxed) {
                // x/Ctrl-C:按中断语义拒绝(spec 90)
                return ApprovalDecision::Reject;
            }
            match self
                .decision_rx
                .lock()
                .expect("decision rx poisoned")
                .try_recv()
            {
                Ok(decision) => return decision,
                Err(mpsc::TryRecvError::Empty) => std::thread::sleep(POLL_INTERVAL),
                // 决策通道断开 = fail closed
                Err(mpsc::TryRecvError::Disconnected) => return ApprovalDecision::Reject,
            }
        }
    }
}

/// 建立审批通道(每次运行一对);返回:
/// (engine 侧 sink, TUI 收请求, TUI 发决策)
pub fn channel(
    abort: Arc<AtomicBool>,
) -> (
    ChannelApprovalSink,
    mpsc::Receiver<ApprovalRequest>,
    mpsc::Sender<ApprovalDecision>,
) {
    let (request_tx, request_rx) = mpsc::channel();
    let (decision_tx, decision_rx) = mpsc::channel();
    (
        ChannelApprovalSink::new(request_tx, decision_rx, abort),
        request_rx,
        decision_tx,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_roundtrip() {
        let abort = Arc::new(AtomicBool::new(false));
        let (sink, request_rx, decision_tx) = channel(abort);
        let sink = Arc::new(sink);
        let h = {
            let sink = sink.clone();
            std::thread::spawn(move || sink.decide("call_1", "write hello"))
        };
        // TUI 侧:转发线程模式 —— 收请求、发决策
        let req = request_rx.recv().unwrap();
        assert_eq!(req.call_id, "call_1");
        decision_tx.send(ApprovalDecision::Approve).unwrap();
        assert_eq!(h.join().unwrap(), ApprovalDecision::Approve);
    }

    #[test]
    fn abort_returns_reject() {
        let abort = Arc::new(AtomicBool::new(false));
        let (sink, request_rx, decision_tx) = channel(abort.clone());
        let sink = Arc::new(sink);
        let h = {
            let sink = sink.clone();
            std::thread::spawn(move || sink.decide("call", "summary"))
        };
        abort.store(true, Ordering::Relaxed);
        let _ = (request_rx, decision_tx);
        assert_eq!(h.join().unwrap(), ApprovalDecision::Reject);
    }

    #[test]
    fn disconnected_channel_fails_closed() {
        let abort = Arc::new(AtomicBool::new(false));
        let (sink, request_rx, decision_tx) = channel(abort);
        // TUI 侧先全部断开
        drop(request_rx);
        drop(decision_tx);
        let sink = Arc::new(sink);
        assert_eq!(sink.decide("call", "summary"), ApprovalDecision::Reject);
    }
}
