//! 审批闸门(spec 30):T1/T2 工具执行前必须取得用户决策。
//!
//! `ApprovalSink` 是阻塞接口:编排层在独立执行线程上调用,CLI 以通道
//! 注入决策;测试用 AutoApprove/AutoReject 替身。V1 只做逐请求
//! Approve/Reject,不做"始终允许"记忆(避免静默扩权)。

use zealous_core::{ApprovalDecision, RiskTier};

/// 审批请求信息(spec 110 §3):通知与绑定校验都需要 tool/risk
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequestInfo {
    pub call_id: String,
    pub tool: String,
    pub risk: RiskTier,
    pub summary: String,
}

pub trait ApprovalSink: Send + Sync {
    /// 阻塞直至决策返回;`info` 必须完整展示将执行的动作
    /// (call_id/tool/risk/summary,spec 110 §3)
    fn decide(&self, info: &ApprovalRequestInfo) -> ApprovalDecision;
}

/// 测试替身:一律批准
pub struct AutoApprove;

impl ApprovalSink for AutoApprove {
    fn decide(&self, _req: &ApprovalRequestInfo) -> ApprovalDecision {
        ApprovalDecision::Approve
    }
}

/// 测试替身:一律拒绝
pub struct AutoReject;

impl ApprovalSink for AutoReject {
    fn decide(&self, _req: &ApprovalRequestInfo) -> ApprovalDecision {
        ApprovalDecision::Reject
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sinks_return_their_decision() {
        let req = ApprovalRequestInfo {
            call_id: "c".into(),
            tool: "read_file".into(),
            risk: zealous_core::RiskTier::T0,
            summary: "s".into(),
        };
        assert_eq!(AutoApprove.decide(&req), ApprovalDecision::Approve);
        assert_eq!(AutoReject.decide(&req), ApprovalDecision::Reject);
    }
}
