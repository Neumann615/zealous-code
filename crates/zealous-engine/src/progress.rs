//! 进度反馈(spec 80 §5):进度只来自编排器事实,经注入的 sink 输出;
//! engine 内不直接打印业务日志。`progress` 为常规轻量进度,
//! `important` 为审批请求、错误与最终结果(--quiet 模式只保留后者)。

use std::sync::Arc;

pub trait ProgressSink: Send + Sync {
    fn progress(&self, line: &str);
    fn important(&self, line: &str);
}

/// 丢弃全部输出(默认;CLI 注入真实实现)
pub struct NoopSink;

impl ProgressSink for NoopSink {
    fn progress(&self, _line: &str) {}
    fn important(&self, _line: &str) {}
}

/// 测试替身:记录全部输出
#[derive(Default)]
pub struct RecordingSink {
    pub progress: std::sync::Mutex<Vec<String>>,
    pub important: std::sync::Mutex<Vec<String>>,
}

impl ProgressSink for RecordingSink {
    fn progress(&self, line: &str) {
        self.progress.lock().unwrap().push(line.to_string());
    }
    fn important(&self, line: &str) {
        self.important.lock().unwrap().push(line.to_string());
    }
}

/// 共享句柄别名
pub type SharedSink = Arc<dyn ProgressSink>;
