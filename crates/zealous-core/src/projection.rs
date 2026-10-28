//! 事件投影(spec 20:UI/任务视图 = fold(events),投影不可回写)。
//!
//! fold 同时是账本的健康检查器:seq 跳号、非法状态流转、孤立引用都在这里显式报错,
//! 不允许静默吞掉(spec 20 崩溃恢复前提)。

use crate::events::{
    AgentRef, AgentRole, AgentRunStatus, ApprovalDecision, EventPayload, RiskTier, TaskEvent,
    TaskStatus, MAX_WORKERS,
};
use crate::ids::{AgentRunId, TaskId};
use crate::transition::can_transition;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum ToolCallState {
    /// 已请求,尚未判定(T0 直接执行 / T1+ 等审批)
    Requested,
    AwaitingApproval,
    Approved,
    Completed {
        ok: bool,
    },
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ToolCallView {
    pub call_id: String,
    pub run_id: String,
    pub tool: String,
    pub args_json: String,
    pub risk: RiskTier,
    pub state: ToolCallState,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RunView {
    pub run_id: String,
    pub role: AgentRole,
    pub status: Option<AgentRunStatus>,
    pub turns_completed: u64,
    pub last_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PendingApproval {
    pub call_id: String,
    pub summary: String,
}

/// spec 158:workspace 集成策略(只来自事件投影)
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum AppliedStrategy {
    FastForward,
    ThreeWayMerge,
}

/// spec 155:isolated 运行的工作区视图(只来自事件投影)
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExecutionWorkspaceView {
    pub kind: String,
    pub branch: String,
    pub path: String,
    pub base_branch: String,
    pub base_commit: String,
    pub current_commit: String,
    /// ready / applied / removed
    pub status: String,
}

/// spec 159:冲突解决工作区视图(只来自事件投影)
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MergeWorkspaceView {
    pub branch: String,
    pub path: String,
    pub main_head: String,
    pub task_branch: String,
    pub task_tip: String,
    /// conflicted / resolved / integrated / aborted
    pub status: String,
    pub merge_commit: Option<String>,
}

/// Task 的完整投影:所有 UI/恢复判定只读这里,不读原始事件。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskProjection {
    pub task_id: TaskId,
    pub title: String,
    pub repo_root: String,
    pub status: TaskStatus,
    pub plan: Option<String>,
    /// 任务简报(spec 80)
    pub brief: Option<String>,
    pub runs: Vec<RunView>,
    pub tool_calls: Vec<ToolCallView>,
    /// 待决策审批(即阻塞中的 T1/T2 调用)
    pub pending_approvals: Vec<PendingApproval>,
    pub last_error: Option<(String, String)>,
    pub interrupted_count: u64,
    pub last_event_seq: u64,
    /// spec 155:isolated 运行的工作区状态
    pub execution_workspace: Option<ExecutionWorkspaceView>,
    /// spec 155:已应用回主工作区的 commit
    pub applied_commit: Option<String>,
    /// spec 158:集成策略;旧账本在事件补齐前保持 None
    pub applied_strategy: Option<AppliedStrategy>,
    /// spec 158:三方合并生成的 merge commit
    pub merge_commit: Option<String>,
    /// spec 159:冲突解决工作区状态
    pub merge_workspace: Option<MergeWorkspaceView>,
    /// spec 159:待处理冲突文件;Resolved/Aborted/Integrated 后清空
    pub merge_conflicts: Vec<String>,
    /// spec 155:workspace 是否已清理
    pub workspace_removed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldError {
    /// seq 跳号或非 1 起:账本损坏(spec 20:显式报错,不静默)
    SeqGap { expected: u64, found: u64 },
    /// 首个事件不是 TaskCreated
    MissingTaskCreated,
    /// 事件 task_id 与请求投影的 task 不符
    TaskMismatch { event_task: String },
    /// 非法状态流转(写入器缺陷或账本被篡改)
    InvalidTransition {
        seq: u64,
        from: TaskStatus,
        to: TaskStatus,
    },
    /// 事件引用了不存在的 run / 非法事件顺序
    OrphanReference { seq: u64, detail: String },
    /// Worker 编号越界
    InvalidWorker { seq: u64, worker: u8 },
}

fn run_index(runs: &[RunView], run_id: &AgentRunId) -> Option<usize> {
    runs.iter().position(|r| r.run_id == run_id.0)
}

fn tool_index(calls: &[ToolCallView], call_id: &str) -> Option<usize> {
    calls.iter().position(|c| c.call_id == call_id)
}

/// 从事件流重建投影。事件必须来自同一 task 且 seq 从 1 连续。
pub fn fold(task_id: &TaskId, events: &[TaskEvent]) -> Result<TaskProjection, FoldError> {
    let mut proj = TaskProjection {
        task_id: task_id.clone(),
        title: String::new(),
        repo_root: String::new(),
        status: TaskStatus::Queued,
        plan: None,
        brief: None,
        runs: Vec::new(),
        tool_calls: Vec::new(),
        pending_approvals: Vec::new(),
        last_error: None,
        interrupted_count: 0,
        last_event_seq: 0,
        execution_workspace: None,
        applied_commit: None,
        applied_strategy: None,
        merge_commit: None,
        merge_workspace: None,
        merge_conflicts: Vec::new(),
        workspace_removed: false,
    };

    let mut seen_runs: HashSet<String> = HashSet::new();
    let mut seen_calls: HashSet<String> = HashSet::new();

    for event in events {
        // task 归属校验:跨 task 事件混入即是账本损坏
        if event.task_id != *task_id {
            return Err(FoldError::TaskMismatch {
                event_task: event.task_id.0.clone(),
            });
        }
        // seq 连续性(信封层,先于载荷)
        let expected = proj.last_event_seq + 1;
        if event.seq != expected {
            return Err(FoldError::SeqGap {
                expected,
                found: event.seq,
            });
        }
        proj.last_event_seq = event.seq;

        match &event.payload {
            EventPayload::TaskCreated { title, repo_root } => {
                if proj.last_event_seq != 1 {
                    return Err(FoldError::MissingTaskCreated);
                }
                proj.title = title.clone();
                proj.repo_root = repo_root.clone();
            }
            EventPayload::TaskStatusChanged { from, to, .. } => {
                if *from != proj.status {
                    return Err(FoldError::InvalidTransition {
                        seq: event.seq,
                        from: *from,
                        to: *to,
                    });
                }
                if !can_transition(*from, *to) {
                    return Err(FoldError::InvalidTransition {
                        seq: event.seq,
                        from: *from,
                        to: *to,
                    });
                }
                proj.status = *to;
            }
            EventPayload::TaskPlanUpdated { plan } => proj.plan = Some(plan.clone()),
            EventPayload::TaskBriefAttached { brief } => proj.brief = Some(brief.clone()),
            EventPayload::AgentRunStarted { run_id, role } => {
                if let AgentRole::Worker(n) = role {
                    if !(1..=MAX_WORKERS).contains(n) {
                        return Err(FoldError::InvalidWorker {
                            seq: event.seq,
                            worker: *n,
                        });
                    }
                }
                let rid = run_id.0.clone();
                if !seen_runs.insert(rid.clone()) {
                    return Err(FoldError::OrphanReference {
                        seq: event.seq,
                        detail: format!("run {rid} 重复开始"),
                    });
                }
                proj.runs.push(RunView {
                    run_id: rid,
                    role: *role,
                    status: Some(AgentRunStatus::Running),
                    turns_completed: 0,
                    last_message: None,
                });
            }
            EventPayload::AgentRunEnded { run_id, status, .. } => {
                let idx =
                    run_index(&proj.runs, run_id).ok_or_else(|| FoldError::OrphanReference {
                        seq: event.seq,
                        detail: format!("run {} 未开始即结束", run_id.0),
                    })?;
                if proj.runs[idx].status != Some(AgentRunStatus::Running) {
                    return Err(FoldError::OrphanReference {
                        seq: event.seq,
                        detail: format!("run {} 已结束却被再次结束", run_id.0),
                    });
                }
                proj.runs[idx].status = Some(*status);
            }
            EventPayload::TurnStarted { run_id, turn } => {
                let idx = active_run(&proj, run_id, event.seq)?;
                if *turn != proj.runs[idx].turns_completed + 1 {
                    return Err(FoldError::OrphanReference {
                        seq: event.seq,
                        detail: format!("run {} turn 编号跳号: {}", run_id.0, turn),
                    });
                }
            }
            EventPayload::TurnCompleted { run_id, turn } => {
                let idx = active_run(&proj, run_id, event.seq)?;
                if *turn != proj.runs[idx].turns_completed + 1 {
                    return Err(FoldError::OrphanReference {
                        seq: event.seq,
                        detail: format!("run {} turn {} 完成顺序错误", run_id.0, turn),
                    });
                }
                proj.runs[idx].turns_completed += 1;
            }
            EventPayload::MessageCompleted { run_id, text, .. } => {
                let idx = active_run(&proj, run_id, event.seq)?;
                proj.runs[idx].last_message = Some(text.clone());
            }
            EventPayload::ToolCallRequested {
                call_id,
                run_id,
                tool,
                args_json,
                risk,
            } => {
                active_run(&proj, run_id, event.seq)?;
                let cid = call_id.0.clone();
                if !seen_calls.insert(cid.clone()) {
                    return Err(FoldError::OrphanReference {
                        seq: event.seq,
                        detail: format!("call {cid} 重复请求"),
                    });
                }
                proj.tool_calls.push(ToolCallView {
                    call_id: cid,
                    run_id: run_id.0.clone(),
                    tool: tool.clone(),
                    args_json: args_json.clone(),
                    risk: *risk,
                    state: ToolCallState::Requested,
                });
            }
            EventPayload::ApprovalRequested { call_id, summary } => {
                let idx = tool_index(&proj.tool_calls, &call_id.0).ok_or_else(|| {
                    FoldError::OrphanReference {
                        seq: event.seq,
                        detail: format!("call {} 未请求即请求审批", call_id.0),
                    }
                })?;
                if proj.tool_calls[idx].state != ToolCallState::Requested {
                    return Err(FoldError::OrphanReference {
                        seq: event.seq,
                        detail: format!(
                            "call {} 状态 {} 下不能进入审批",
                            call_id.0, "非 Requested"
                        ),
                    });
                }
                proj.tool_calls[idx].state = ToolCallState::AwaitingApproval;
                proj.pending_approvals.push(PendingApproval {
                    call_id: call_id.0.clone(),
                    summary: summary.clone(),
                });
            }
            EventPayload::ApprovalDecided { call_id, decision } => {
                let idx = tool_index(&proj.tool_calls, &call_id.0).ok_or_else(|| {
                    FoldError::OrphanReference {
                        seq: event.seq,
                        detail: format!("call {} 未知审批决策", call_id.0),
                    }
                })?;
                if proj.tool_calls[idx].state != ToolCallState::AwaitingApproval {
                    return Err(FoldError::OrphanReference {
                        seq: event.seq,
                        detail: format!("call {} 不在等待审批状态", call_id.0),
                    });
                }
                proj.tool_calls[idx].state = match decision {
                    ApprovalDecision::Approve => ToolCallState::Approved,
                    ApprovalDecision::Reject => ToolCallState::Rejected,
                };
                proj.pending_approvals.retain(|p| p.call_id != call_id.0);
            }
            EventPayload::ToolCallCompleted {
                call_id,
                ok,
                output_summary,
            } => {
                let idx = tool_index(&proj.tool_calls, &call_id.0).ok_or_else(|| {
                    FoldError::OrphanReference {
                        seq: event.seq,
                        detail: format!("call {} 未请求即完成", call_id.0),
                    }
                })?;
                match proj.tool_calls[idx].state {
                    // T0 直接执行路径,或 T1+ 审批通过后执行
                    ToolCallState::Requested | ToolCallState::Approved => {}
                    _ => {
                        return Err(FoldError::OrphanReference {
                            seq: event.seq,
                            detail: format!("call {} 状态不允许完成", call_id.0),
                        })
                    }
                }
                proj.tool_calls[idx].state = ToolCallState::Completed { ok: *ok };
                let _ = output_summary;
            }
            EventPayload::TaskInterrupted { .. } => {
                proj.interrupted_count += 1;
                // spec 30:中断时挂起审批一律按拒绝收尾
                for call in &mut proj.tool_calls {
                    if call.state == ToolCallState::AwaitingApproval {
                        call.state = ToolCallState::Rejected;
                    }
                }
                proj.pending_approvals.clear();
            }
            EventPayload::TaskResumed { run_id } => {
                if seen_runs.contains(&run_id.0) {
                    return Err(FoldError::OrphanReference {
                        seq: event.seq,
                        detail: format!("resume 引用已存在的 run {}", run_id.0),
                    });
                }
                // resume 事件后必须紧跟新 AgentRunStarted;此处仅记账,run 由后续事件建立
                let _ = run_id;
            }
            EventPayload::ErrorOccurred { scope, message } => {
                proj.last_error = Some((scope.clone(), message.clone()));
            }
            EventPayload::MergeWorkspacePrepared {
                branch,
                path,
                main_head,
                task_branch,
                task_tip,
            } => {
                if let Some(existing) = &proj.merge_workspace {
                    if existing.status != "aborted" {
                        return Err(FoldError::OrphanReference {
                            seq: event.seq,
                            detail: "merge workspace 重复准备".into(),
                        });
                    }
                }
                proj.merge_workspace = Some(MergeWorkspaceView {
                    branch: branch.clone(),
                    path: path.clone(),
                    main_head: main_head.clone(),
                    task_branch: task_branch.clone(),
                    task_tip: task_tip.clone(),
                    status: "conflicted".into(),
                    merge_commit: None,
                });
                proj.merge_conflicts.clear();
            }
            EventPayload::MergeConflictPending { files } => match &mut proj.merge_workspace {
                Some(ws) if ws.status == "conflicted" => {
                    proj.merge_conflicts = files.clone();
                }
                _ => {
                    return Err(FoldError::OrphanReference {
                        seq: event.seq,
                        detail: "conflict 引用非 conflicted merge workspace".into(),
                    })
                }
            },
            EventPayload::MergeWorkspaceResolved { merge_commit } => {
                match &mut proj.merge_workspace {
                    Some(ws) if ws.status == "conflicted" => {
                        ws.status = "resolved".into();
                        ws.merge_commit = Some(merge_commit.clone());
                        proj.merge_conflicts.clear();
                    }
                    _ => {
                        return Err(FoldError::OrphanReference {
                            seq: event.seq,
                            detail: "resolve 引用非 conflicted merge workspace".into(),
                        })
                    }
                }
            }
            EventPayload::MergeWorkspaceAborted => match &mut proj.merge_workspace {
                Some(ws) if ws.status == "conflicted" => {
                    ws.status = "aborted".into();
                    ws.merge_commit = None;
                    proj.merge_conflicts.clear();
                }
                _ => {
                    return Err(FoldError::OrphanReference {
                        seq: event.seq,
                        detail: "abort 引用非 conflicted merge workspace".into(),
                    })
                }
            },
            EventPayload::ExecutionWorkspacePrepared {
                kind,
                branch,
                path,
                base_branch,
                base_commit,
            } => {
                if proj.execution_workspace.is_some() {
                    return Err(FoldError::OrphanReference {
                        seq: event.seq,
                        detail: "workspace 重复准备".into(),
                    });
                }
                proj.execution_workspace = Some(ExecutionWorkspaceView {
                    kind: kind.clone(),
                    branch: branch.clone(),
                    path: path.clone(),
                    base_branch: base_branch.clone(),
                    base_commit: base_commit.clone(),
                    current_commit: base_commit.clone(),
                    status: "ready".into(),
                });
            }
            EventPayload::ExecutionWorkspaceApplied {
                branch,
                applied_commit,
            } => match &mut proj.execution_workspace {
                Some(ws) if ws.branch == *branch => {
                    ws.current_commit = applied_commit.clone();
                    ws.status = "applied".into();
                    proj.applied_commit = Some(applied_commit.clone());
                    proj.applied_strategy = Some(AppliedStrategy::FastForward);
                }
                _ => {
                    return Err(FoldError::OrphanReference {
                        seq: event.seq,
                        detail: "apply 引用未准备的 workspace 或分支不匹配".into(),
                    })
                }
            },
            EventPayload::ExecutionWorkspaceMerged {
                branch,
                base_commit,
                task_tip,
                merge_commit,
                conflicted,
            } => match &mut proj.execution_workspace {
                Some(ws)
                    if ws.branch == *branch && ws.base_commit == *base_commit && !conflicted =>
                {
                    ws.current_commit = merge_commit.clone();
                    ws.status = "applied".into();
                    proj.applied_commit = Some(merge_commit.clone());
                    proj.applied_strategy = Some(AppliedStrategy::ThreeWayMerge);
                    proj.merge_commit = Some(merge_commit.clone());
                    if let Some(merge_ws) = &mut proj.merge_workspace {
                        if merge_ws.status == "resolved" {
                            merge_ws.status = "integrated".into();
                        }
                    }
                    proj.merge_conflicts.clear();
                    let _ = task_tip;
                }
                _ => {
                    return Err(FoldError::OrphanReference {
                        seq: event.seq,
                        detail: "merge 引用未准备的 workspace、base 不匹配或声明冲突".into(),
                    })
                }
            },
            EventPayload::ExecutionWorkspaceRemoved { branch, .. } => {
                match &mut proj.execution_workspace {
                    Some(ws) if ws.branch == *branch => {
                        ws.status = "removed".into();
                        proj.workspace_removed = true;
                    }
                    _ => {
                        return Err(FoldError::OrphanReference {
                            seq: event.seq,
                            detail: "remove 引用未准备的 workspace 或分支不匹配".into(),
                        })
                    }
                }
            }
        }
    }

    // TaskCreated 必须是首事件
    if !events.is_empty() && proj.title.is_empty() && proj.repo_root.is_empty() {
        return Err(FoldError::MissingTaskCreated);
    }

    Ok(proj)
}

fn active_run(proj: &TaskProjection, run_id: &AgentRunId, seq: u64) -> Result<usize, FoldError> {
    let idx = run_index(&proj.runs, run_id).ok_or_else(|| FoldError::OrphanReference {
        seq,
        detail: format!("引用未开始的 run {}", run_id.0),
    })?;
    if proj.runs[idx].status != Some(AgentRunStatus::Running) {
        return Err(FoldError::OrphanReference {
            seq,
            detail: format!("run {} 已结束,不接受 turn/message 事件", run_id.0),
        });
    }
    Ok(idx)
}

#[cfg(test)]
mod spec155_tests {
    use super::*;
    use crate::events::EventPayload;
    use crate::ids::TaskId;

    fn ev(tid: &TaskId, seq: u64, payload: EventPayload) -> TaskEvent {
        TaskEvent::new(
            seq,
            "2026-10-22T00:00:00.000Z".into(),
            tid.clone(),
            AgentRef::Coordinator,
            payload,
        )
    }

    #[test]
    fn workspace_lifecycle_fold() {
        let tid = TaskId::generate();
        let mut events = vec![ev(
            &tid,
            1,
            EventPayload::TaskCreated {
                title: "t".into(),
                repo_root: "r".into(),
            },
        )];
        events.push(ev(
            &tid,
            2,
            EventPayload::ExecutionWorkspacePrepared {
                kind: "git-worktree".into(),
                branch: "zealous/t1".into(),
                path: "r/.zealous/worktrees/t1".into(),
                base_branch: "main".into(),
                base_commit: "abc".into(),
            },
        ));
        let proj = fold(&tid, &events).unwrap();
        let ws = proj.execution_workspace.as_ref().unwrap();
        assert_eq!(ws.status, "ready");
        assert_eq!(ws.current_commit, "abc");
        assert_eq!(proj.applied_commit, None);
        assert!(!proj.workspace_removed);

        events.push(ev(
            &tid,
            3,
            EventPayload::ExecutionWorkspaceApplied {
                branch: "zealous/t1".into(),
                applied_commit: "def".into(),
            },
        ));
        let proj = fold(&tid, &events).unwrap();
        assert_eq!(proj.applied_commit.as_deref(), Some("def"));
        assert_eq!(proj.applied_strategy, Some(AppliedStrategy::FastForward));
        assert_eq!(proj.execution_workspace.as_ref().unwrap().status, "applied");

        events.push(ev(
            &tid,
            4,
            EventPayload::ExecutionWorkspaceRemoved {
                branch: "zealous/t1".into(),
                path: "r/.zealous/worktrees/t1".into(),
            },
        ));
        let proj = fold(&tid, &events).unwrap();
        assert!(proj.workspace_removed);
        assert_eq!(proj.execution_workspace.as_ref().unwrap().status, "removed");
    }

    #[test]
    fn workspace_merge_fold() {
        let tid = TaskId::generate();
        let events = vec![
            ev(
                &tid,
                1,
                EventPayload::TaskCreated {
                    title: "t".into(),
                    repo_root: "r".into(),
                },
            ),
            ev(
                &tid,
                2,
                EventPayload::ExecutionWorkspacePrepared {
                    kind: "git-worktree".into(),
                    branch: "zealous/t1".into(),
                    path: "r/.zealous/worktrees/t1".into(),
                    base_branch: "main".into(),
                    base_commit: "abc".into(),
                },
            ),
            ev(
                &tid,
                3,
                EventPayload::ExecutionWorkspaceMerged {
                    branch: "zealous/t1".into(),
                    base_commit: "abc".into(),
                    task_tip: "task".into(),
                    merge_commit: "merge".into(),
                    conflicted: false,
                },
            ),
        ];
        let proj = fold(&tid, &events).unwrap();
        assert_eq!(proj.applied_commit.as_deref(), Some("merge"));
        assert_eq!(proj.applied_strategy, Some(AppliedStrategy::ThreeWayMerge));
        assert_eq!(proj.merge_commit.as_deref(), Some("merge"));
        assert_eq!(proj.execution_workspace.as_ref().unwrap().status, "applied");
    }

    #[test]
    fn merge_workspace_lifecycle_fold() {
        let tid = TaskId::generate();
        let events = vec![
            ev(
                &tid,
                1,
                EventPayload::TaskCreated {
                    title: "t".into(),
                    repo_root: "r".into(),
                },
            ),
            ev(
                &tid,
                2,
                EventPayload::ExecutionWorkspacePrepared {
                    kind: "git-worktree".into(),
                    branch: "zealous/t1".into(),
                    path: "r/.zealous/worktrees/t1".into(),
                    base_branch: "main".into(),
                    base_commit: "base".into(),
                },
            ),
            ev(
                &tid,
                3,
                EventPayload::MergeWorkspacePrepared {
                    branch: "zealous-merge/t1".into(),
                    path: "r/.zealous/merges/t1".into(),
                    main_head: "main".into(),
                    task_branch: "zealous/t1".into(),
                    task_tip: "task".into(),
                },
            ),
            ev(
                &tid,
                4,
                EventPayload::MergeConflictPending {
                    files: vec!["shared.md".into()],
                },
            ),
        ];
        let proj = fold(&tid, &events).unwrap();
        let merge_ws = proj.merge_workspace.as_ref().unwrap();
        assert_eq!(merge_ws.status, "conflicted");
        assert_eq!(proj.merge_conflicts, vec!["shared.md".to_string()]);

        let events = [
            events,
            vec![
                ev(
                    &tid,
                    5,
                    EventPayload::MergeWorkspaceResolved {
                        merge_commit: "merge".into(),
                    },
                ),
                ev(
                    &tid,
                    6,
                    EventPayload::ExecutionWorkspaceMerged {
                        branch: "zealous/t1".into(),
                        base_commit: "base".into(),
                        task_tip: "task".into(),
                        merge_commit: "merge".into(),
                        conflicted: false,
                    },
                ),
            ],
        ]
        .concat();
        let proj = fold(&tid, &events).unwrap();
        assert_eq!(proj.merge_workspace.unwrap().status, "integrated");
        assert!(proj.merge_conflicts.is_empty());
        assert_eq!(proj.applied_strategy, Some(AppliedStrategy::ThreeWayMerge));
    }

    #[test]
    fn workspace_apply_branch_mismatch_is_orphan() {
        let tid = TaskId::generate();
        let events = vec![
            ev(
                &tid,
                1,
                EventPayload::TaskCreated {
                    title: "t".into(),
                    repo_root: "r".into(),
                },
            ),
            ev(
                &tid,
                2,
                EventPayload::ExecutionWorkspacePrepared {
                    kind: "git-worktree".into(),
                    branch: "zealous/t1".into(),
                    path: "p".into(),
                    base_branch: "main".into(),
                    base_commit: "abc".into(),
                },
            ),
            ev(
                &tid,
                3,
                EventPayload::ExecutionWorkspaceApplied {
                    branch: "zealous/other".into(),
                    applied_commit: "def".into(),
                },
            ),
        ];
        assert!(matches!(
            fold(&tid, &events),
            Err(FoldError::OrphanReference { .. })
        ));
    }
}

/// 便捷:直接从 AgentRef 构造事件(写入器用)。
pub fn event_for(
    task_id: TaskId,
    agent: AgentRef,
    seq: u64,
    ts: String,
    payload: EventPayload,
) -> TaskEvent {
    TaskEvent::new(seq, ts, task_id, agent, payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ToolCallId;

    fn task() -> TaskId {
        TaskId("task_t".into())
    }

    fn ev(seq: u64, agent: AgentRef, payload: EventPayload) -> TaskEvent {
        TaskEvent::new(
            seq,
            "2026-10-15T00:00:00.000Z".into(),
            task(),
            agent,
            payload,
        )
    }

    fn base_events() -> Vec<TaskEvent> {
        vec![ev(
            1,
            AgentRef::User,
            EventPayload::TaskCreated {
                title: "demo".into(),
                repo_root: "C:/repo".into(),
            },
        )]
    }

    #[test]
    fn fold_happy_path_with_approval() {
        let run = AgentRunId("run_1".into());
        let call = ToolCallId::generate();
        let mut events = base_events();
        events.push(ev(
            2,
            AgentRef::User,
            EventPayload::TaskStatusChanged {
                from: TaskStatus::Queued,
                to: TaskStatus::Running,
                reason: None,
            },
        ));
        events.push(ev(
            3,
            AgentRef::Coordinator,
            EventPayload::AgentRunStarted {
                run_id: run.clone(),
                role: AgentRole::Coordinator,
            },
        ));
        events.push(ev(
            4,
            AgentRef::Coordinator,
            EventPayload::ToolCallRequested {
                call_id: call.clone(),
                run_id: run.clone(),
                tool: "write_file".into(),
                args_json: "{}".into(),
                risk: RiskTier::T1,
            },
        ));
        events.push(ev(
            5,
            AgentRef::Coordinator,
            EventPayload::ApprovalRequested {
                call_id: call.clone(),
                summary: "write main.rs".into(),
            },
        ));
        let proj = fold(&task(), &events).unwrap();
        assert_eq!(proj.pending_approvals.len(), 1);

        events.push(ev(
            6,
            AgentRef::User,
            EventPayload::ApprovalDecided {
                call_id: call.clone(),
                decision: ApprovalDecision::Approve,
            },
        ));
        events.push(ev(
            7,
            AgentRef::Coordinator,
            EventPayload::ToolCallCompleted {
                call_id: call.clone(),
                ok: true,
                output_summary: "written".into(),
            },
        ));
        let proj = fold(&task(), &events).unwrap();
        assert_eq!(proj.pending_approvals.len(), 0);
        assert_eq!(
            proj.tool_calls[0].state,
            ToolCallState::Completed { ok: true }
        );
    }

    #[test]
    fn fold_rejects_seq_gap() {
        let mut events = base_events();
        events[0].seq = 2;
        assert_eq!(
            fold(&task(), &events),
            Err(FoldError::SeqGap {
                expected: 1,
                found: 2
            })
        );
    }

    #[test]
    fn fold_rejects_illegal_transition() {
        let mut events = base_events();
        events.push(ev(
            2,
            AgentRef::User,
            EventPayload::TaskStatusChanged {
                from: TaskStatus::Queued,
                to: TaskStatus::Completed,
                reason: None,
            },
        ));
        assert!(matches!(
            fold(&task(), &events),
            Err(FoldError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn interrupt_rejects_pending_approvals() {
        let run = AgentRunId("run_1".into());
        let call = ToolCallId::generate();
        let mut events = base_events();
        events.push(ev(
            2,
            AgentRef::User,
            EventPayload::TaskStatusChanged {
                from: TaskStatus::Queued,
                to: TaskStatus::Running,
                reason: None,
            },
        ));
        events.push(ev(
            3,
            AgentRef::Coordinator,
            EventPayload::AgentRunStarted {
                run_id: run.clone(),
                role: AgentRole::Coordinator,
            },
        ));
        events.push(ev(
            4,
            AgentRef::Coordinator,
            EventPayload::ToolCallRequested {
                call_id: call.clone(),
                run_id: run.clone(),
                tool: "shell".into(),
                args_json: "{}".into(),
                risk: RiskTier::T2,
            },
        ));
        events.push(ev(
            5,
            AgentRef::Coordinator,
            EventPayload::ApprovalRequested {
                call_id: call.clone(),
                summary: "rm -rf".into(),
            },
        ));
        events.push(ev(
            6,
            AgentRef::User,
            EventPayload::TaskInterrupted {
                reason: "sigint".into(),
            },
        ));
        events.push(ev(
            7,
            AgentRef::User,
            EventPayload::TaskStatusChanged {
                from: TaskStatus::Running,
                to: TaskStatus::Interrupted,
                reason: Some("sigint".into()),
            },
        ));
        let proj = fold(&task(), &events).unwrap();
        assert_eq!(proj.status, TaskStatus::Interrupted);
        assert_eq!(proj.pending_approvals.len(), 0);
        assert_eq!(proj.tool_calls[0].state, ToolCallState::Rejected);
    }
}
