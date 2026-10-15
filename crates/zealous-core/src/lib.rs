//! 领域模型与事件定义:纯逻辑、无 IO,UI/编排状态全部由事件投影推导。
//! 对应 spec: `docs/spec/10-domain-model.md`、`docs/spec/20-event-ledger.md`。

pub mod events;
pub mod ids;
pub mod projection;
pub mod time;
pub mod transition;

pub use events::{
    AgentRef, AgentRole, AgentRunStatus, ApprovalDecision, EventPayload, RiskTier, TaskEvent,
    TaskStatus, EVENT_SCHEMA_VERSION, MAX_WORKERS,
};
pub use ids::{AgentRunId, TaskId, ToolCallId};
pub use projection::{FoldError, TaskProjection};

// 状态机流转的唯一裁决在 `transition::can_transition`;
// 账本健康检查(seq 连续、孤立引用)在 `projection::fold`。
pub use transition::can_transition;
