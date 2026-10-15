//! 工具执行器:账本事件 + 风险闸门 + 实际执行的单点汇聚(spec 30)。
//!
//! 事件顺序(每个工具调用固定四步,审计可回放):
//! `ToolCallRequested → (T1+: ApprovalRequested → ApprovalDecided) → ToolCallCompleted`
//!
//! 两条硬边界(spec 30):
//! - Worker 只能调用 T0 工具(Worker 无写入权);
//! - 未知工具不执行(未知即最高风险,直接以失败结果回给模型)。

use std::path::PathBuf;
use std::sync::Arc;

use zealous_core::{AgentRef, AgentRunId, ApprovalDecision, EventPayload, RiskTier, ToolCallId};

use crate::approval::ApprovalSink;
use crate::ledger::Ledger;
use crate::tools::{ToolContext, ToolError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    pub ok: bool,
    /// 回给模型的文本:成功为输出摘要,失败/被拒为可解释原因
    pub text: String,
}

#[derive(Debug)]
pub enum ExecutorError {
    /// 账本写入失败:审计链断裂,必须向调用方冒泡(不允许静默继续执行)
    Ledger(zealous_core::FoldError, String),
    LedgerIo(crate::ledger::LedgerError),
}

impl std::fmt::Display for ExecutorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecutorError::Ledger(e, what) => write!(f, "executor ledger({what}): {e:?}"),
            ExecutorError::LedgerIo(e) => write!(f, "executor ledger io: {e}"),
        }
    }
}

pub struct ToolExecutor {
    repo_root: PathBuf,
    registry: Vec<Box<dyn crate::tools::Tool>>,
    sink: Arc<dyn ApprovalSink>,
}

impl ToolExecutor {
    pub fn new(
        repo_root: impl Into<PathBuf>,
        registry: Vec<Box<dyn crate::tools::Tool>>,
        sink: Arc<dyn ApprovalSink>,
    ) -> Self {
        Self {
            repo_root: repo_root.into(),
            registry,
            sink,
        }
    }

    fn find_tool(&self, name: &str) -> Option<&dyn crate::tools::Tool> {
        self.registry
            .iter()
            .find(|t| t.name() == name)
            .map(|b| &**b)
    }

    /// 执行一次工具调用,全过程落账。返回给模型的见 `ToolOutcome`。
    ///
    /// 审计链固定:任何路径都先落 `ToolCallRequested`(未知工具按 T2 就高
    /// 归级),再落 `ToolCallCompleted`,保证账本事件序列永远合法可回放。
    #[allow(clippy::too_many_arguments)]
    pub fn run_tool(
        &self,
        ledger: &Ledger,
        run_id: &AgentRunId,
        agent: AgentRef,
        raw_args: &str,
        tool_name: &str,
    ) -> Result<ToolOutcome, ExecutorError> {
        let call_id = ToolCallId::generate();
        let (tool, risk): (Option<&dyn crate::tools::Tool>, RiskTier) =
            match self.find_tool(tool_name) {
                Some(t) => (Some(t), t.risk()),
                // 未知工具:就高不就低(spec 30),不执行
                None => (None, RiskTier::T2),
            };

        ledger
            .append(
                agent.clone(),
                EventPayload::ToolCallRequested {
                    call_id: call_id.clone(),
                    run_id: run_id.clone(),
                    tool: tool_name.to_string(),
                    args_json: raw_args.to_string(),
                    risk,
                },
            )
            .map_err(ExecutorError::LedgerIo)?;

        let fail = |ok: bool, text: String| -> Result<ToolOutcome, ExecutorError> {
            ledger
                .append(
                    agent.clone(),
                    EventPayload::ToolCallCompleted {
                        call_id: call_id.clone(),
                        ok,
                        output_summary: text.clone(),
                    },
                )
                .map_err(ExecutorError::LedgerIo)?;
            Ok(ToolOutcome { ok, text })
        };

        // Worker 边界(spec 30):Worker 只有 T0 工具权
        if matches!(agent, AgentRef::Worker(_)) && risk != RiskTier::T0 {
            return fail(
                false,
                format!("worker 不具备 {risk:?} 工具权(仅 T0);请求的工具: {tool_name}"),
            );
        }

        let Some(tool) = tool else {
            return fail(false, format!("unknown tool: {tool_name}"));
        };

        // T1/T2:审批闸门
        if risk != RiskTier::T0 {
            let summary = tool.summarize(raw_args);
            ledger
                .append(
                    agent.clone(),
                    EventPayload::ApprovalRequested {
                        call_id: call_id.clone(),
                        summary: summary.clone(),
                    },
                )
                .map_err(ExecutorError::LedgerIo)?;
            let decision = self.sink.decide(call_id.as_str(), &summary);
            ledger
                .append(
                    AgentRef::User,
                    EventPayload::ApprovalDecided {
                        call_id: call_id.clone(),
                        decision,
                    },
                )
                .map_err(ExecutorError::LedgerIo)?;
            if decision == ApprovalDecision::Reject {
                // spec 30:Rejected 是 ToolCall 终态,不再追加完成事件;
                // "以失败结果回给模型"由返回值承载,任务不因此失败
                return Ok(ToolOutcome {
                    ok: false,
                    text: "rejected by user".into(),
                });
            }
        }

        let ctx = ToolContext {
            repo_root: self.repo_root.clone(),
        };
        match tool.execute(raw_args, &ctx) {
            Ok(out) => fail(true, out.summary),
            Err(ToolError::BadArgs(m)) => fail(false, format!("invalid args: {m}")),
            Err(ToolError::Failed(m)) => fail(false, format!("tool failed: {m}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::{AutoApprove, AutoReject};
    use crate::tools::builtin_registry;
    use std::path::Path;
    use zealous_core::{TaskId, TaskStatus};

    fn fixture_repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zealous-exec-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("a.txt"), "content-a").unwrap();
        let _ = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .output();
        // 脏工作区保护要求夹具有初始 commit(spec 60 §4)
        let _ = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&dir)
            .output();
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "init",
            ])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "commit failed: {:?}", out.stderr);
        dir
    }

    fn started_task(repo: &Path) -> (TaskId, Ledger) {
        let task_id = TaskId::generate();
        let ledger = Ledger::create(repo, &task_id).unwrap();
        ledger
            .append(
                AgentRef::User,
                EventPayload::TaskCreated {
                    title: "t".into(),
                    repo_root: repo.display().to_string(),
                },
            )
            .unwrap();
        ledger
            .append(
                AgentRef::User,
                EventPayload::TaskStatusChanged {
                    from: TaskStatus::Queued,
                    to: TaskStatus::Running,
                    reason: None,
                },
            )
            .unwrap();
        (task_id, ledger)
    }

    /// 编排器开工的第一步:Run 开始事件(工具调用的 run 引用据此合法)
    fn start_run(
        ledger: &Ledger,
        run: &AgentRunId,
        agent: AgentRef,
        role: zealous_core::AgentRole,
    ) {
        ledger
            .append(
                agent,
                EventPayload::AgentRunStarted {
                    run_id: run.clone(),
                    role,
                },
            )
            .unwrap();
    }

    fn executor(sink: Arc<dyn ApprovalSink>, repo: &PathBuf) -> ToolExecutor {
        ToolExecutor::new(repo, builtin_registry(), sink)
    }

    #[test]
    fn t0_tool_runs_without_approval() {
        let repo = fixture_repo();
        let (_tid, ledger) = started_task(&repo);
        let exec = executor(Arc::new(AutoApprove), &repo);
        let run = AgentRunId::generate();
        start_run(
            &ledger,
            &run,
            AgentRef::Coordinator,
            zealous_core::AgentRole::Coordinator,
        );
        let out = exec
            .run_tool(
                &ledger,
                &run,
                AgentRef::Coordinator,
                r#"{"path":"a.txt"}"#,
                "read_file",
            )
            .unwrap();
        assert!(out.ok);
        assert!(out.text.contains("content-a"));
        let proj = ledger.fold_now().unwrap();
        // T0 四步中无审批两步:Requested → Completed
        assert_eq!(proj.tool_calls.len(), 1);
        assert_eq!(proj.pending_approvals.len(), 0);
    }

    #[test]
    fn t1_tool_requires_approval_and_reject_blocks_write() {
        let repo = fixture_repo();
        let exec = executor(Arc::new(AutoReject), &repo);
        let (_tid, ledger) = started_task(&repo);
        let run = AgentRunId::generate();
        start_run(
            &ledger,
            &run,
            AgentRef::Coordinator,
            zealous_core::AgentRole::Coordinator,
        );
        let out = exec
            .run_tool(
                &ledger,
                &run,
                AgentRef::Coordinator,
                r#"{"path":"new.txt","content":"x"}"#,
                "write_file",
            )
            .unwrap();
        assert!(!out.ok);
        assert_eq!(out.text, "rejected by user");
        assert!(!repo.join("new.txt").exists(), "被拒后文件不得写入");
        let proj = ledger.fold_now().unwrap();
        assert_eq!(
            proj.tool_calls[0].state,
            zealous_core::projection::ToolCallState::Rejected
        );
    }

    #[test]
    fn t1_tool_approved_writes_file() {
        let repo = fixture_repo();
        let exec = executor(Arc::new(AutoApprove), &repo);
        let (_tid, ledger) = started_task(&repo);
        let run = AgentRunId::generate();
        start_run(
            &ledger,
            &run,
            AgentRef::Coordinator,
            zealous_core::AgentRole::Coordinator,
        );
        let out = exec
            .run_tool(
                &ledger,
                &run,
                AgentRef::Coordinator,
                r#"{"path":"new.txt","content":"x"}"#,
                "write_file",
            )
            .unwrap();
        assert!(out.ok);
        assert_eq!(std::fs::read_to_string(repo.join("new.txt")).unwrap(), "x");
    }

    #[test]
    fn worker_cannot_invoke_t1_even_with_approving_sink() {
        let repo = fixture_repo();
        let exec = executor(Arc::new(AutoApprove), &repo);
        let (_tid, ledger) = started_task(&repo);
        let run = AgentRunId::generate();
        start_run(
            &ledger,
            &run,
            AgentRef::Worker(1),
            zealous_core::AgentRole::Worker(1),
        );
        let out = exec
            .run_tool(
                &ledger,
                &run,
                AgentRef::Worker(1),
                r#"{"path":"new.txt","content":"x"}"#,
                "write_file",
            )
            .unwrap();
        assert!(!out.ok);
        assert!(out.text.contains("T0"));
        assert!(!repo.join("new.txt").exists());
        // 未进入审批:账本无 ApprovalRequested
        let proj = ledger.fold_now().unwrap();
        assert_eq!(proj.pending_approvals.len(), 0);
    }

    #[test]
    fn unknown_tool_fails_without_execution() {
        let repo = fixture_repo();
        let exec = executor(Arc::new(AutoApprove), &repo);
        let (_tid, ledger) = started_task(&repo);
        let run = AgentRunId::generate();
        start_run(
            &ledger,
            &run,
            AgentRef::Coordinator,
            zealous_core::AgentRole::Coordinator,
        );
        let out = exec
            .run_tool(
                &ledger,
                &run,
                AgentRef::Coordinator,
                "{}",
                "rm_rf_everything",
            )
            .unwrap();
        assert!(!out.ok);
        assert!(out.text.contains("unknown tool"));
    }
}
