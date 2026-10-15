//! 领域模型与事件定义:纯逻辑、无 IO,UI/编排状态全部由事件投影推导。
//! 对应 spec: `docs/spec/10-domain-model.md`、`docs/spec/20-event-ledger.md`。

/// Task 状态(spec 10 状态机)。只允许由 `TaskStatusChanged` 事件驱动,
/// 禁止在投影之外维护第二份可变状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Queued,
    Running,
    /// 用户中断或进程崩溃后待恢复;resume 开启新 AgentRun
    Interrupted,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    /// 终态任务仍可 resume(spec 10:resume 是一等公民)
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
        )
    }
}

/// AgentRun 状态:一次连续执行;恢复/重试总是开启新 Run,不复用已结束的 Run。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRunStatus {
    Running,
    Completed,
    Failed,
    /// 被用户中断或进程终止
    Killed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_states_are_resumable_but_terminal() {
        assert!(TaskStatus::Completed.is_terminal());
        assert!(TaskStatus::Failed.is_terminal());
        assert!(!TaskStatus::Interrupted.is_terminal());
        assert!(!TaskStatus::Running.is_terminal());
    }
}
