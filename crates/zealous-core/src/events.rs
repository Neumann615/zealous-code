//! 事件信封与载荷定义(spec 20)。
//!
//! 所有对外可见的 Agent 行为都必须表达为 `TaskEvent`;`type` 标签用 PascalCase,
//! 与 spec 20 示例一致。新增事件只能新增枚举值,禁止复用旧类型。

use crate::ids::{AgentRunId, TaskId, ToolCallId};
use serde::{Deserialize, Serialize};

/// 账本结构版本;载荷结构不兼容变更时递增(spec 20:`v` 字段)。
pub const EVENT_SCHEMA_VERSION: u32 = 1;

/// Worker 数量上限(spec 10 拓扑约束:1 Coordinator + ≤3 Worker)。
pub const MAX_WORKERS: u8 = 3;

/// 事件主体:用户、Coordinator 或编号 Worker(1 起)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRef {
    User,
    Coordinator,
    /// 1..=MAX_WORKERS
    Worker(u8),
}

impl AgentRef {
    pub fn as_str(&self) -> String {
        match self {
            AgentRef::User => "user".into(),
            AgentRef::Coordinator => "coordinator".into(),
            AgentRef::Worker(n) => format!("worker-{n}"),
        }
    }

    pub fn parse_ref(s: &str) -> Option<Self> {
        match s {
            "user" => Some(AgentRef::User),
            "coordinator" => Some(AgentRef::Coordinator),
            _ => s
                .strip_prefix("worker-")
                .and_then(|n| n.parse::<u8>().ok())
                .filter(|n| (1..=MAX_WORKERS).contains(n))
                .map(AgentRef::Worker),
        }
    }

    /// Agent 角色与编号在 Run 开始时绑定,投影据此还原拓扑。
    pub fn role(&self) -> AgentRole {
        match self {
            AgentRef::Coordinator => AgentRole::Coordinator,
            AgentRef::Worker(n) => AgentRole::Worker(*n),
            AgentRef::User => AgentRole::Coordinator, // 不应出现;归入协调侧避免 newtype 爆炸
        }
    }
}

impl std::fmt::Display for AgentRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_str())
    }
}

impl Serialize for AgentRef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.as_str())
    }
}

impl std::str::FromStr for AgentRef {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        AgentRef::parse_ref(s).ok_or_else(|| format!("invalid agent ref: {s}"))
    }
}

impl<'de> Deserialize<'de> for AgentRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse::<AgentRef>().map_err(serde::de::Error::custom)
    }
}

/// Agent 在拓扑中的角色(spec 10:固定 1 Coordinator + ≤MAX_WORKERS Worker)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Coordinator,
    Worker(u8),
}

/// Task 状态机(spec 10)。流转合法性由 `transition::can_transition` 唯一裁决。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum TaskStatus {
    Queued,
    Running,
    Interrupted,
    Completed,
    Failed,
    Cancelled,
}

/// AgentRun 状态:一次连续执行;恢复/重试总是开启新 Run。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum AgentRunStatus {
    Running,
    Completed,
    Failed,
    Killed,
}

/// 风险分级(spec 30):T0 自动,T1/T2 需审批。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskTier {
    T0,
    T1,
    T2,
}

impl std::fmt::Display for RiskTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            RiskTier::T0 => "T0",
            RiskTier::T1 => "T1",
            RiskTier::T2 => "T2",
        };
        f.write_str(s)
    }
}

/// 审批决策(spec 30:V1 只做逐请求 Approve/Reject,不做"始终允许")。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum ApprovalDecision {
    Approve,
    Reject,
}

/// 事件载荷。`args_json`/`output_summary` 保持字符串形态:工具参数与输出
/// 在 V1 不做深 schema 约束,账本只保证完整性与可审计。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "PascalCase")]
pub enum EventPayload {
    TaskCreated {
        title: String,
        repo_root: String,
    },
    TaskStatusChanged {
        from: TaskStatus,
        to: TaskStatus,
        reason: Option<String>,
    },
    TaskPlanUpdated {
        plan: String,
    },
    AgentRunStarted {
        run_id: AgentRunId,
        role: AgentRole,
    },
    AgentRunEnded {
        run_id: AgentRunId,
        status: AgentRunStatus,
        reason: Option<String>,
    },
    TurnStarted {
        run_id: AgentRunId,
        turn: u64,
    },
    TurnCompleted {
        run_id: AgentRunId,
        turn: u64,
    },
    MessageCompleted {
        run_id: AgentRunId,
        turn: u64,
        text: String,
    },
    ToolCallRequested {
        call_id: ToolCallId,
        run_id: AgentRunId,
        tool: String,
        args_json: String,
        risk: RiskTier,
    },
    ApprovalRequested {
        call_id: ToolCallId,
        summary: String,
    },
    ApprovalDecided {
        call_id: ToolCallId,
        decision: ApprovalDecision,
    },
    ToolCallCompleted {
        call_id: ToolCallId,
        ok: bool,
        output_summary: String,
    },
    TaskInterrupted {
        reason: String,
    },
    TaskResumed {
        run_id: AgentRunId,
    },
    ErrorOccurred {
        scope: String,
        message: String,
    },
}

/// 统一事件信封(spec 20):`seq` 由写入器分配并保证 task 内连续;`agent` 记事件主体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskEvent {
    pub seq: u64,
    /// RFC3339 UTC,毫秒精度
    pub ts: String,
    pub v: u32,
    pub task_id: TaskId,
    pub agent: AgentRef,
    #[serde(flatten)]
    pub payload: EventPayload,
}

impl TaskEvent {
    pub fn new(
        seq: u64,
        ts: String,
        task_id: TaskId,
        agent: AgentRef,
        payload: EventPayload,
    ) -> Self {
        Self {
            seq,
            ts,
            v: EVENT_SCHEMA_VERSION,
            task_id,
            agent,
            payload,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_ref_string_roundtrip() {
        for r in [
            AgentRef::User,
            AgentRef::Coordinator,
            AgentRef::Worker(1),
            AgentRef::Worker(MAX_WORKERS),
        ] {
            assert_eq!(AgentRef::parse_ref(&r.as_str()), Some(r));
        }
        assert_eq!(AgentRef::parse_ref("worker-0"), None);
        assert_eq!(AgentRef::parse_ref("worker-4"), None);
    }

    #[test]
    fn payload_json_has_pascal_type_tag() {
        let e = TaskEvent::new(
            1,
            "2026-10-15T00:00:00.000Z".into(),
            TaskId::generate(),
            AgentRef::User,
            EventPayload::TaskCreated {
                title: "t".into(),
                repo_root: "C:/repo".into(),
            },
        );
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""type":"TaskCreated""#));
        assert!(json.contains(r#""v":1"#));
        let back: TaskEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e);
    }
}
