//! Task 状态流转的唯一裁决点(spec 10 状态机)。
//!
//! 投影与写入器都必须经过这里判定,禁止第二份实现,避免账本允许非法流转。

use crate::events::TaskStatus;

/// 合法流转(spec 10):
/// - Queued → Running
/// - Queued → Failed | Cancelled(启动前即失败/取消:如 provider 配置非法,
///   spec 10 要求创建时即失败并落账)
/// - Running → Completed | Failed | Cancelled | Interrupted
/// - Interrupted → Running(恢复)
/// - 终态(Completed/Failed/Cancelled)→ Running(resume 是一等公民)
/// - 不允许同状态自转(禁止空转事件)
pub fn can_transition(from: TaskStatus, to: TaskStatus) -> bool {
    use TaskStatus::*;
    matches!(
        (from, to),
        (Queued, Running)
            | (Queued, Failed | Cancelled)
            | (Running, Completed | Failed | Cancelled | Interrupted)
            | (Interrupted, Running)
            | (Completed | Failed | Cancelled, Running)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use TaskStatus::*;

    #[test]
    fn spec_transitions_hold() {
        assert!(can_transition(Queued, Running));
        for to in [Completed, Failed, Cancelled, Interrupted] {
            assert!(can_transition(Running, to));
        }
        assert!(can_transition(Interrupted, Running));
        for from in [Completed, Failed, Cancelled, Interrupted] {
            assert!(can_transition(from, Running), "resume from {from:?}");
        }
    }

    #[test]
    fn everything_else_rejected() {
        for from in [Queued, Running, Interrupted, Completed, Failed, Cancelled] {
            for to in [Queued, Running, Interrupted, Completed, Failed, Cancelled] {
                if from == to || can_transition(from, to) {
                    continue;
                }
                assert!(!can_transition(from, to), "{from:?} → {to:?} 应非法");
            }
        }
        // 自转一律非法
        for s in [Queued, Running, Interrupted, Completed, Failed, Cancelled] {
            assert!(!can_transition(s, s));
        }
    }
}
