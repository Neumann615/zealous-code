//! 串行任务队列(spec 156,opt-in;全部 isolated;单并发)。
//!
//! 队列顺序是运行时输入,不新增事件类型/事实源:单任务事实仍在各自
//! 事件账本,队列状态从任务投影与 run lease 派生。

use crate::approval::ApprovalSink;
use crate::lease::RunLease;
use crate::orchestrator::Orchestrator;
use crate::progress::SharedSink;
use crate::provider::Provider;
use crate::task_service::TaskService;
use crate::tools::builtin_registry;
use crate::workspace;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use zealous_core::{TaskId, TaskStatus};

/// --all 模式筛选结果
pub type SelectResult = Result<(Vec<TaskId>, Vec<(TaskId, String)>), String>;

/// 队列事件钩子类型(简化 clippy type_complexity)
pub type OnTaskStartedFn<'a> = Box<dyn FnMut(&TaskId, usize, usize) + 'a>;
pub type OnTaskFinishedFn<'a> = Box<dyn FnMut(&TaskId, TaskStatus) + 'a>;
pub type OnApplyFinishedFn<'a> = Box<dyn FnMut(&TaskId, bool, String) + 'a>;
pub type ConfirmApplyFn<'a> = Box<dyn FnMut(&TaskId) -> bool + 'a>;

/// 单任务可入队判定(spec 156):Queued + 无任何 run + 无 isolated workspace
pub fn check_runnable(proj: &zealous_core::projection::TaskProjection) -> Result<(), String> {
    if proj.status != TaskStatus::Queued {
        return Err(format!("status {:?}", proj.status));
    }
    if !proj.runs.is_empty() {
        return Err("已有 direct 运行历史".into());
    }
    if proj.execution_workspace.is_some() {
        return Err("已有 isolated workspace".into());
    }
    Ok(())
}

/// --all 模式筛选:可运行(TaskId 升序)+ 不可运行原因
pub fn select_runnable(svc: &TaskService) -> SelectResult {
    let (summaries, skipped) = svc.list_tasks().map_err(|e| e.to_string())?;
    let mut runnable = Vec::new();
    let mut not_runnable: Vec<(TaskId, String)> = skipped
        .iter()
        .map(|(id, e)| (id.clone(), format!("ledger corrupt: {e}")))
        .collect();
    for s in &summaries {
        if s.status != TaskStatus::Queued {
            not_runnable.push((s.task_id.clone(), format!("status {:?}", s.status)));
            continue;
        }
        match svc.open_task(&s.task_id) {
            Ok((_l, proj, _)) => match check_runnable(&proj) {
                Ok(()) => runnable.push(s.task_id.clone()),
                Err(reason) => not_runnable.push((s.task_id.clone(), reason)),
            },
            Err(e) => not_runnable.push((s.task_id.clone(), format!("ledger corrupt: {e}"))),
        }
    }
    Ok((runnable, not_runnable))
}

/// 显式模式启动校验:任一任务不可入队即失败,不执行任何任务
pub fn validate_explicit(svc: &TaskService, ids: &[TaskId]) -> Result<(), (usize, TaskId, String)> {
    for (i, id) in ids.iter().enumerate() {
        match svc.open_task(id) {
            Err(e) => return Err((i, id.clone(), e.to_string())),
            Ok((_l, proj, _)) => {
                if let Err(reason) = check_runnable(&proj) {
                    return Err((i, id.clone(), reason));
                }
            }
        }
    }
    Ok(())
}

/// 队列前端钩子(进度/通知/交互确认)
pub struct QueueHooks<'a> {
    pub on_task_started: OnTaskStartedFn<'a>,
    pub on_task_finished: OnTaskFinishedFn<'a>,
    pub on_apply_finished: OnApplyFinishedFn<'a>,
    /// auto_apply=false 时的交互确认;返回 false 队列停止
    pub confirm_apply: ConfirmApplyFn<'a>,
}

#[derive(Debug, Default, Clone)]
pub struct QueueSummary {
    pub completed: Vec<TaskId>,
    pub applied: Vec<TaskId>,
    /// 停止原因;全部成功应用时为空
    pub stopped_reason: String,
}

/// 串行执行(spec 156 §4):逐任务 isolated → Completed → apply
/// (auto 或确认)→ 下一个基于新主 HEAD。Failed/Interrupted/apply
/// 被拒或失败 → 停止,后续保持 Queued,当前 worktree 保留。
#[allow(clippy::too_many_arguments)]
pub fn run_serial(
    repo_root: &Path,
    svc: &TaskService,
    provider: Arc<dyn Provider>,
    model: String,
    max_turns: u64,
    task_ids: &[TaskId],
    auto_apply: bool,
    approval_sink: Arc<dyn ApprovalSink>,
    progress: Option<SharedSink>,
    abort: Arc<AtomicBool>,
    lease: &RunLease,
    hooks: &mut QueueHooks<'_>,
) -> QueueSummary {
    let total = task_ids.len();
    let mut summary = QueueSummary::default();
    for (i, tid) in task_ids.iter().enumerate() {
        (hooks.on_task_started)(tid, i + 1, total);
        let _ = lease.set_task(repo_root, tid.as_str(), "queue");
        let (ledger, proj, _) = match svc.open_task(tid) {
            Ok(x) => x,
            Err(e) => {
                summary.stopped_reason = format!("打开任务失败: {e}");
                break;
            }
        };
        if proj.status != TaskStatus::Queued {
            summary.stopped_reason = format!("任务 {tid} 不再是 Queued({:?})", proj.status);
            break;
        }
        let run_root = match workspace::ensure_run_root(repo_root, &ledger, tid, true) {
            Ok(r) => r,
            Err(e) => {
                summary.stopped_reason = format!("workspace: {e}");
                break;
            }
        };
        let mut prompt = proj.title.clone();
        prompt = format!(
            "[isolated workspace] 本次运行在任务专属 git worktree 中执行;\\n主工作区的未提交改动在此不可见,不得尝试访问或修改主工作区。\\n\\n{prompt}"
        );
        let executor = Arc::new(crate::executor::ToolExecutor::new(
            run_root,
            builtin_registry(),
            approval_sink.clone(),
        ));
        let orchestrator = Orchestrator {
            provider: provider.clone(),
            executor,
            abort: abort.clone(),
            max_turns,
            model: model.clone(),
            progress: progress.clone(),
        };
        let ledger = Arc::new(ledger);
        let status = match orchestrator.run_task(&ledger, &prompt) {
            Ok(s) => s,
            Err(e) => {
                (hooks.on_task_finished)(tid, TaskStatus::Failed);
                summary.stopped_reason = format!("编排失败: {e}");
                break;
            }
        };
        (hooks.on_task_finished)(tid, status);
        if status != TaskStatus::Completed {
            summary.stopped_reason = format!("任务 {tid} 结束状态 {status:?},队列停止");
            break;
        }
        summary.completed.push(tid.clone());
        let confirmed = auto_apply || (hooks.confirm_apply)(tid);
        if !confirmed {
            summary.stopped_reason = "用户拒绝 apply,队列停止".into();
            break;
        }
        match workspace::apply(repo_root, &ledger) {
            Ok(out) => {
                (hooks.on_apply_finished)(
                    tid,
                    true,
                    format!("{} @ {}", out.branch, out.applied_commit),
                );
                summary.applied.push(tid.clone());
            }
            Err(e) => {
                (hooks.on_apply_finished)(tid, false, e.to_string());
                summary.stopped_reason = format!("apply 失败: {e}");
                break;
            }
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lease::try_acquire;
    use std::path::PathBuf;
    use std::process::Command;
    use zealous_core::{AgentRef, AgentRole, AgentRunId, EventPayload};

    fn repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zealous-q-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".gitignore"), ".zealous/\n").unwrap();
        std::fs::write(dir.join("base.txt"), "base\n").unwrap();
        Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(&dir)
            .status()
            .unwrap();
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(&dir)
            .status()
            .unwrap();
        Command::new("git")
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
            .status()
            .unwrap();
        dir
    }

    fn svc_for(dir: &Path) -> TaskService {
        TaskService::new(dir.to_path_buf())
    }

    fn scripted_provider(script: Vec<String>) -> Arc<dyn Provider> {
        Arc::new(crate::provider::MockProvider::scripted(
            script.into_iter().map(Ok).collect(),
        ))
    }

    #[test]
    fn selection_filters_and_orders() {
        let dir = repo("sel");
        let svc = svc_for(&dir);
        // 创建 A B C(创建顺序 = TaskId 升序)
        let mut ids = Vec::new();
        for n in ["a", "b", "c"] {
            let tid = match svc.create_task(n).unwrap() {
                crate::task_service::CreateOutcome::Ready { task_id, .. } => task_id,
                other => panic!("{other:?}"),
            };
            ids.push(tid);
        }
        // B:direct 历史
        let (ledger_b, _, _) = svc.open_task(&ids[1]).unwrap();
        ledger_b
            .append(
                AgentRef::Coordinator,
                EventPayload::AgentRunStarted {
                    run_id: AgentRunId::generate(),
                    role: AgentRole::Coordinator,
                },
            )
            .unwrap();
        // C:isolated workspace(仅事件,无真实 worktree——投影层即可筛选)
        let (ledger_c, _, _) = svc.open_task(&ids[2]).unwrap();
        ledger_c
            .append(
                AgentRef::Coordinator,
                EventPayload::ExecutionWorkspacePrepared {
                    kind: "git-worktree".into(),
                    branch: format!("zealous/{}", ids[2].as_str()),
                    path: "p".into(),
                    base_branch: "main".into(),
                    base_commit: "abc".into(),
                },
            )
            .unwrap();
        let (runnable, not_runnable) = select_runnable(&svc).unwrap();
        assert_eq!(runnable, vec![ids[0].clone()]);
        assert_eq!(not_runnable.len(), 2);
        // 显式校验:A 可入队;B/C 拒绝
        assert!(validate_explicit(&svc, &[ids[0].clone()]).is_ok());
        assert!(validate_explicit(&svc, &[ids[1].clone()]).is_err());
        assert!(validate_explicit(&svc, &[ids[2].clone()]).is_err());
    }

    #[test]
    fn run_serial_two_tasks_sequential_apply() {
        let dir = repo("serial");
        let svc = svc_for(&dir);
        let mut ids = Vec::new();
        for name in ["t1", "t2"] {
            let tid = match svc.create_task(name).unwrap() {
                crate::task_service::CreateOutcome::Ready { task_id, .. } => task_id,
                other => panic!("{other:?}"),
            };
            ids.push(tid);
        }
        let script = vec![
            r#"{"action":"plan","plan":"p1","workers":[]}"#.into(),
            r#"{"action":"tool","tool":"edit_file","args":{"path":"base.txt","old_text":"base","new_text":"t1-edit"}}"#.into(),
            r#"{"action":"message","text":"t1 done"}"#.into(),
            r#"{"action":"plan","plan":"p2","workers":[]}"#.into(),
            r#"{"action":"tool","tool":"edit_file","args":{"path":"base.txt","old_text":"t1-edit","new_text":"t2-edit"}}"#.into(),
            r#"{"action":"message","text":"t2 done"}"#.into(),
        ];
        let provider = scripted_provider(script);
        let lease = try_acquire(&dir, "queue", "queue").unwrap();
        let sink: Arc<dyn ApprovalSink> = Arc::new(crate::approval::AutoApprove);
        let mut hooks = QueueHooks {
            on_task_started: Box::new(|tid, i, total| {
                println!("task {tid} started ({i}/{total})");
            }),
            on_task_finished: Box::new(|_, _| {}),
            on_apply_finished: Box::new(|_, _, _| {}),
            confirm_apply: Box::new(|_| true),
        };
        let summary = run_serial(
            &dir,
            &svc,
            provider,
            "mock".into(),
            24,
            &ids,
            true,
            sink,
            None,
            Arc::new(AtomicBool::new(false)),
            &lease,
            &mut hooks,
        );
        assert!(
            summary.stopped_reason.is_empty(),
            "{}",
            summary.stopped_reason
        );
        assert_eq!(summary.completed.len(), 2);
        assert_eq!(summary.applied.len(), 2);
        // 主分支包含两个 rollback commit(t2-edit 在最上)
        let log = Command::new("git")
            .args(["log", "--oneline"])
            .current_dir(&dir)
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&log.stdout);
        assert_eq!(log.matches("zealous: edit base.txt").count(), 2, "{log}");
        // 主工作区最终内容 = 第二个任务的修改
        let base_txt = std::fs::read_to_string(dir.join("base.txt")).unwrap();
        assert!(base_txt.contains("t2-edit"), "{base_txt}");
        // 顺序保持:HEAD~1 的内容是 t1 的修改
        let prev = Command::new("git")
            .args(["show", "HEAD~1:base.txt"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&prev.stdout).contains("t1-edit"),
            "{}",
            String::from_utf8_lossy(&prev.stdout)
        );
    }
}
