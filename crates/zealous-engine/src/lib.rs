//! 编排与执行内核:事件账本写入、Coordinator/Worker 编排、工具注册表与
//! 风险分级、审批流、OpenAI-compatible Provider(`Provider` trait 隔离)。
//! 对应 spec: `docs/spec/20-event-ledger.md`、`docs/spec/30-permissions.md`、
//! `docs/spec/50-architecture.md`。
//!
//! 本 crate 是未来桌面 app 复用的内核主体;UI 一律为薄客户端。

pub mod approval;
pub mod executor;
pub mod lease;
pub mod ledger;
pub mod merge_workspace;
pub mod orchestrator;
pub mod parallel;
pub mod progress;
pub mod provider;
pub mod queue;
pub mod task_service;
pub mod tools;
pub mod workspace;
pub mod worktree;

/// 测试专用:ProviderConfig::resolve 读取进程级环境变量,
/// 触碰环境的测试必须持有此锁串行执行,避免并行测试互相污染。
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Mutex;
    pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());
}
