//! channel 化进度(spec 90):编排器事实经此进入 TUI;
//! progress 与 important 分流,TUI 决定渲染策略。

use std::sync::mpsc;

use zealous_engine::progress::ProgressSink;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressMsg {
    Progress(String),
    Important(String),
}

/// engine 侧 sink
pub struct ChannelProgressSink {
    tx: mpsc::Sender<ProgressMsg>,
}

impl ProgressSink for ChannelProgressSink {
    fn progress(&self, line: &str) {
        let _ = self.tx.send(ProgressMsg::Progress(line.to_string()));
    }
    fn important(&self, line: &str) {
        let _ = self.tx.send(ProgressMsg::Important(line.to_string()));
    }
}

/// TUI 侧接收端
pub type ProgressRx = mpsc::Receiver<ProgressMsg>;

pub fn channel() -> (ChannelProgressSink, ProgressRx) {
    let (tx, rx) = mpsc::channel();
    (ChannelProgressSink { tx }, rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_and_important_are_routed_separately() {
        let (sink, rx) = channel();
        sink.progress("turn 1");
        sink.important("等待审批");
        sink.progress("turn 2");

        assert_eq!(rx.recv().unwrap(), ProgressMsg::Progress("turn 1".into()));
        assert_eq!(
            rx.recv().unwrap(),
            ProgressMsg::Important("等待审批".into())
        );
        assert_eq!(rx.recv().unwrap(), ProgressMsg::Progress("turn 2".into()));
    }
}
