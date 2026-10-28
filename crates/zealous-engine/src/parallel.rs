//! Parallel task Alpha (spec 157).
//!
//! At most two isolated tasks run concurrently. Task facts remain in their own
//! ledgers; the batch order is runtime input and is not persisted as a second
//! source of truth.

use crate::approval::ApprovalRequestInfo;
use crate::lease::RunLease;
use crate::orchestrator::Orchestrator;
use crate::progress::SharedSink;
use crate::provider::Provider;
use crate::task_service::TaskService;
use crate::tools::builtin_registry;
use crate::workspace;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use zealous_core::{ApprovalDecision, TaskId, TaskStatus};

pub const MAX_PARALLEL_TASKS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationStrategy {
    FastForwardOnly,
    Merge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnConflictStrategy {
    Abort,
    Workspace,
}

impl OnConflictStrategy {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "abort" => Ok(Self::Abort),
            "workspace" => Ok(Self::Workspace),
            other => Err(format!(
                "unknown on-conflict strategy {other:?}; expected abort or workspace"
            )),
        }
    }
}

impl IntegrationStrategy {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "ff-only" => Ok(Self::FastForwardOnly),
            "merge" => Ok(Self::Merge),
            other => Err(format!(
                "unknown integrate strategy {other:?}; expected ff-only or merge"
            )),
        }
    }
}

pub fn validate_limit(max: usize) -> Result<(), String> {
    if max == 0 {
        Err("--max must be at least 1".into())
    } else if max > MAX_PARALLEL_TASKS {
        Err(format!(
            "Alpha supports at most {MAX_PARALLEL_TASKS} parallel tasks"
        ))
    } else {
        Ok(())
    }
}

pub type ParallelSelectResult = Result<(Vec<TaskId>, Vec<(TaskId, String)>), String>;

pub fn select_runnable(svc: &TaskService, max: usize) -> ParallelSelectResult {
    validate_limit(max)?;
    let (mut runnable, not_runnable) = crate::queue::select_runnable(svc)?;
    runnable.truncate(max);
    Ok((runnable, not_runnable))
}

pub fn validate_explicit(svc: &TaskService, ids: &[TaskId]) -> Result<(), (usize, TaskId, String)> {
    if ids.len() > MAX_PARALLEL_TASKS {
        let id = ids.first().expect("non-empty checked").clone();
        return Err((
            0,
            id,
            format!("Alpha supports at most {MAX_PARALLEL_TASKS} tasks"),
        ));
    }
    crate::queue::validate_explicit(svc, ids)
}

/// Approval requests are serialized by the engine wrapper, and every request is
/// explicitly bound to its task id before reaching the frontend.
pub trait ParallelApprovalSink: Send + Sync {
    fn decide(&self, task_id: &TaskId, info: &ApprovalRequestInfo) -> ApprovalDecision;
}

pub struct AutoParallelApprove;

impl ParallelApprovalSink for AutoParallelApprove {
    fn decide(&self, _task_id: &TaskId, _info: &ApprovalRequestInfo) -> ApprovalDecision {
        ApprovalDecision::Approve
    }
}

struct TaskApprovalSink {
    task_id: TaskId,
    sink: Arc<Mutex<Arc<dyn ParallelApprovalSink>>>,
}

impl crate::approval::ApprovalSink for TaskApprovalSink {
    fn decide(&self, info: &ApprovalRequestInfo) -> ApprovalDecision {
        let sink = self
            .sink
            .lock()
            .expect("parallel approval sink poisoned")
            .clone();
        sink.decide(&self.task_id, info)
    }
}

struct TaskProgress {
    task_id: TaskId,
    inner: Option<SharedSink>,
}

impl crate::progress::ProgressSink for TaskProgress {
    fn progress(&self, line: &str) {
        if let Some(inner) = &self.inner {
            inner.progress(&format!("[{}] {}", self.task_id.as_str(), line));
        }
    }

    fn important(&self, line: &str) {
        if let Some(inner) = &self.inner {
            inner.important(&format!("[{}] {}", self.task_id.as_str(), line));
        }
    }
}

pub type OnTaskStartedFn<'a> = Box<dyn FnMut(&TaskId, usize, usize) + 'a>;
pub type OnTaskFinishedFn<'a> = Box<dyn FnMut(&TaskId, TaskStatus) + 'a>;
pub type OnApplyStartedFn<'a> = Box<dyn FnMut(&TaskId) + 'a>;
pub type OnApplyFinishedFn<'a> = Box<dyn FnMut(&TaskId, bool, String) + 'a>;
pub type OnMergeStartedFn<'a> = Box<dyn FnMut(&TaskId) + 'a>;
pub type OnMergeFinishedFn<'a> = Box<dyn FnMut(&TaskId, bool, String) + 'a>;
pub type OnMergeConflictFn<'a> = Box<dyn FnMut(&TaskId, Vec<String>) + 'a>;
pub type ConfirmApplyFn<'a> = Box<dyn FnMut(&TaskId) -> bool + 'a>;

pub struct ParallelHooks<'a> {
    pub on_task_started: OnTaskStartedFn<'a>,
    pub on_task_finished: OnTaskFinishedFn<'a>,
    pub on_apply_started: OnApplyStartedFn<'a>,
    pub on_apply_finished: OnApplyFinishedFn<'a>,
    pub on_merge_started: OnMergeStartedFn<'a>,
    pub on_merge_finished: OnMergeFinishedFn<'a>,
    pub on_merge_conflict: OnMergeConflictFn<'a>,
    pub confirm_apply: ConfirmApplyFn<'a>,
}

#[derive(Debug, Default, Clone)]
pub struct ParallelSummary {
    pub base_commit: String,
    pub completed: Vec<TaskId>,
    pub failed: Vec<TaskId>,
    pub interrupted: Vec<TaskId>,
    pub applied: Vec<TaskId>,
    pub merged: Vec<TaskId>,
    pub merge_workspaces: Vec<TaskId>,
    pub merge_conflicts: Vec<(TaskId, Vec<String>)>,
    pub stopped_reason: String,
}

struct PreparedTask {
    id: TaskId,
    title: String,
    ledger: Arc<crate::ledger::Ledger>,
    base_commit: String,
}

#[allow(clippy::too_many_arguments)]
pub fn run_parallel(
    repo_root: &Path,
    svc: &TaskService,
    providers: Vec<Arc<dyn Provider>>,
    model: String,
    max_turns: u64,
    task_ids: &[TaskId],
    auto_apply: bool,
    integration: IntegrationStrategy,
    on_conflict: OnConflictStrategy,
    approval_sink: Arc<dyn ParallelApprovalSink>,
    progress: Option<SharedSink>,
    abort: Arc<AtomicBool>,
    lease: &RunLease,
    hooks: &mut ParallelHooks<'_>,
) -> ParallelSummary {
    let mut summary = ParallelSummary::default();
    if task_ids.is_empty()
        || task_ids.len() > MAX_PARALLEL_TASKS
        || providers.len() != task_ids.len()
    {
        summary.stopped_reason = format!(
            "invalid parallel batch: tasks={} providers={} max={MAX_PARALLEL_TASKS}",
            task_ids.len(),
            providers.len()
        );
        return summary;
    }

    // Prepare all worktrees before any model execution. All tasks necessarily use
    // the same base because preparation does not move the main HEAD.
    let mut prepared = Vec::new();
    for (index, id) in task_ids.iter().enumerate() {
        let (ledger, projection, _) = match svc.open_task(id) {
            Ok(value) => value,
            Err(e) => {
                summary.stopped_reason = format!("open task {id} failed: {e}");
                return summary;
            }
        };
        if let Err(reason) = crate::queue::check_runnable(&projection) {
            summary.stopped_reason = format!("task {id} is not runnable: {reason}");
            return summary;
        }
        let ledger = Arc::new(ledger);
        match workspace::ensure_run_root(repo_root, &ledger, id, true) {
            Ok(run_root) => {
                let worktree_commit = git_head(&run_root).unwrap_or_default();
                if index == 0 {
                    summary.base_commit = worktree_commit.clone();
                } else if worktree_commit != summary.base_commit {
                    summary.stopped_reason = format!(
                        "parallel base mismatch: expected {} got {worktree_commit}",
                        summary.base_commit
                    );
                    return summary;
                }
                prepared.push(PreparedTask {
                    id: id.clone(),
                    title: projection.title,
                    ledger,
                    base_commit: worktree_commit,
                });
            }
            Err(e) => {
                summary.stopped_reason = format!("workspace prepare for {id} failed: {e}");
                return summary;
            }
        }
    }
    let base_ids = prepared.iter().map(|p| p.id.clone()).collect::<Vec<_>>();
    let _ = lease.set_parallel(repo_root, &base_ids, &base_ids);

    // Shared mutex serializes approval requests in arrival order. The wrapper adds
    // the task binding while keeping the existing executor ApprovalSink contract.
    let shared_approval = Arc::new(Mutex::new(approval_sink));
    let mut handles = Vec::new();
    for (index, item) in prepared.into_iter().enumerate() {
        (hooks.on_task_started)(&item.id, index + 1, task_ids.len());
        let provider = providers[index].clone();
        let model = model.clone();
        let abort = abort.clone();
        let sink = Arc::new(TaskApprovalSink {
            task_id: item.id.clone(),
            sink: shared_approval.clone(),
        });
        let progress: Option<SharedSink> = Some(Arc::new(TaskProgress {
            task_id: item.id.clone(),
            inner: progress.clone(),
        }));
        let run_root = workspace::worktree_path(repo_root, &item.id);
        let executor = Arc::new(crate::executor::ToolExecutor::new(
            run_root,
            builtin_registry(),
            sink,
        ));
        let orchestrator = Orchestrator {
            provider,
            executor,
            abort,
            max_turns,
            model,
            progress,
        };
        // The prompt carries the task requirement captured during preparation.
        let prompt = format!(
            "[parallel isolated workspace] This task runs in its own git worktree. \
             Do not access or modify the main workspace or another task's worktree.\n\n{}",
            item.title
        );
        let ledger = item.ledger;
        handles.push(std::thread::spawn(move || {
            let result = orchestrator.run_task(&ledger, &prompt);
            let status = match result {
                Ok(status) => status,
                Err(_) => TaskStatus::Failed,
            };
            (item.id, status, ledger, item.base_commit)
        }));
    }

    let mut finished = Vec::new();
    for handle in handles {
        match handle.join() {
            Ok(value) => finished.push(value),
            Err(_) => {
                summary.stopped_reason = "parallel worker panicked".into();
            }
        }
    }
    let _ = lease.set_parallel(repo_root, &base_ids, &[]);

    for (id, status, ledger, _base) in finished {
        (hooks.on_task_finished)(&id, status);
        match status {
            TaskStatus::Completed => summary.completed.push(id.clone()),
            TaskStatus::Interrupted => summary.interrupted.push(id.clone()),
            _ => summary.failed.push(id.clone()),
        }
        if status != TaskStatus::Completed {
            summary.stopped_reason =
                format!("task {id} ended as {status:?}; its worktree is retained");
            continue;
        }
        let confirmed = auto_apply || (hooks.confirm_apply)(&id);
        if !confirmed {
            summary.stopped_reason =
                format!("user rejected apply for {id}; later tasks are stopped");
            continue;
        }
        let current_head = git_head(repo_root).unwrap_or_default();
        let should_merge =
            integration == IntegrationStrategy::Merge && current_head != summary.base_commit;
        if should_merge {
            if on_conflict == OnConflictStrategy::Workspace {
                match crate::merge_workspace::start(repo_root, &id, &ledger) {
                    Ok(crate::merge_workspace::StartOutcome::Integrated {
                        branch,
                        merge_commit,
                        task_tip,
                        ..
                    }) => {
                        (hooks.on_merge_started)(&id);
                        (hooks.on_merge_finished)(
                            &id,
                            true,
                            format!("{branch} @ {merge_commit} (task tip {task_tip})"),
                        );
                        summary.applied.push(id.clone());
                        summary.merged.push(id);
                    }
                    Ok(crate::merge_workspace::StartOutcome::Conflicted {
                        branch,
                        main_head,
                        task_tip,
                        files,
                        ..
                    }) => {
                        (hooks.on_merge_conflict)(&id, files.clone());
                        summary.merge_workspaces.push(id.clone());
                        summary.merge_conflicts.push((id.clone(), files.clone()));
                        summary.stopped_reason = format!(
                            "merge conflict retained in workspace for {id}: {} @ {} (task tip {}); files: {}",
                            branch,
                            main_head,
                            task_tip,
                            files.join(", ")
                        );
                        break;
                    }
                    Err(e) => {
                        let detail = e.to_string();
                        (hooks.on_merge_finished)(&id, false, detail.clone());
                        summary.stopped_reason = format!(
                            "merge workspace failed for {id}: {detail}; later tasks are stopped"
                        );
                        break;
                    }
                }
            } else {
                (hooks.on_merge_started)(&id);
                match workspace::merge(repo_root, &ledger) {
                    Ok(outcome) => {
                        (hooks.on_merge_finished)(
                            &id,
                            true,
                            format!(
                                "{} @ {} (task tip {})",
                                outcome.branch, outcome.merge_commit, outcome.task_tip
                            ),
                        );
                        summary.applied.push(id.clone());
                        summary.merged.push(id);
                    }
                    Err(e) => {
                        let detail = e.to_string();
                        (hooks.on_merge_finished)(&id, false, detail.clone());
                        summary.stopped_reason =
                            format!("merge failed for {id}: {detail}; later tasks are stopped");
                        break;
                    }
                }
            }
        } else {
            (hooks.on_apply_started)(&id);
            match workspace::apply(repo_root, &ledger) {
                Ok(outcome) => {
                    (hooks.on_apply_finished)(
                        &id,
                        true,
                        format!(
                            "{} @ {}{}",
                            outcome.branch,
                            outcome.applied_commit,
                            if outcome.already {
                                " (already applied)"
                            } else {
                                ""
                            }
                        ),
                    );
                    summary.applied.push(id);
                }
                Err(e) => {
                    let detail = e.to_string();
                    (hooks.on_apply_finished)(&id, false, detail.clone());
                    summary.stopped_reason =
                        format!("apply failed for {id}: {detail}; later tasks are stopped");
                    break;
                }
            }
        }
    }

    if summary.failed.is_empty()
        && summary.interrupted.is_empty()
        && summary.stopped_reason.is_empty()
        && summary.applied.len() != summary.completed.len()
    {
        summary.stopped_reason = "not all completed tasks were applied".into();
    }
    summary
}

fn git_head(root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lease::try_acquire;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::MutexGuard;

    struct ProviderEnv {
        _guard: MutexGuard<'static, ()>,
    }

    impl Drop for ProviderEnv {
        fn drop(&mut self) {
            for key in ["ZEALOUS_BASE_URL", "ZEALOUS_API_KEY", "ZEALOUS_MODEL"] {
                std::env::remove_var(key);
            }
        }
    }

    fn provider_env() -> ProviderEnv {
        let guard = crate::test_support::ENV_LOCK.lock().unwrap();
        std::env::set_var("ZEALOUS_BASE_URL", "https://api.example.invalid/v1");
        std::env::set_var("ZEALOUS_API_KEY", "test-key");
        std::env::set_var("ZEALOUS_MODEL", "test-model");
        ProviderEnv { _guard: guard }
    }

    fn repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zealous-parallel-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".gitignore"), ".zealous/\n").unwrap();
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
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

    fn task(svc: &TaskService, name: &str) -> TaskId {
        match svc.create_task(name).unwrap() {
            crate::task_service::CreateOutcome::Ready { task_id, .. } => task_id,
            other => panic!("{other:?}"),
        }
    }

    fn hooks() -> ParallelHooks<'static> {
        ParallelHooks {
            on_task_started: Box::new(|_, _, _| {}),
            on_task_finished: Box::new(|_, _| {}),
            on_apply_started: Box::new(|_| {}),
            on_apply_finished: Box::new(|_, _, _| {}),
            on_merge_started: Box::new(|_| {}),
            on_merge_finished: Box::new(|_, _, _| {}),
            on_merge_conflict: Box::new(|_, _| {}),
            confirm_apply: Box::new(|_| true),
        }
    }

    fn provider(tool_path: &str, old: &str, new: &str) -> Arc<dyn Provider> {
        Arc::new(crate::provider::MockProvider::scripted(vec![
            Ok(r#"{"action":"plan","plan":"edit","workers":[]}"#.into()),
            Ok(format!(
                r#"{{"action":"tool","tool":"edit_file","args":{{"path":"{tool_path}","old_text":"{old}","new_text":"{new}"}}}}"#
            )),
            Ok(r#"{"action":"message","text":"done"}"#.into()),
        ]))
    }

    fn failing_provider() -> Arc<dyn Provider> {
        Arc::new(crate::provider::MockProvider::scripted(vec![
            Err("parallel injected failure".into()),
            Err("parallel injected failure".into()),
            Err("parallel injected failure".into()),
        ]))
    }

    #[test]
    fn limit_selection_and_explicit_validation() {
        let _provider_env = provider_env();
        assert!(validate_limit(0).is_err());
        assert!(validate_limit(3).is_err());
        let dir = repo("select");
        let svc = TaskService::new(dir);
        let a = task(&svc, "a");
        let b = task(&svc, "b");
        let c = task(&svc, "c");
        let (selected, _) = select_runnable(&svc, 2).unwrap();
        assert_eq!(selected, vec![a.clone(), b.clone()]);
        assert!(validate_explicit(&svc, &[a.clone(), b.clone()]).is_ok());
        assert!(validate_explicit(&svc, &[a, b, c]).is_err());
    }

    #[test]
    fn two_tasks_share_base_then_second_apply_rejects() {
        let _provider_env = provider_env();
        let dir = repo("apply");
        let svc = TaskService::new(dir.clone());
        let a = task(&svc, "A");
        let b = task(&svc, "B");
        let lease = try_acquire(&dir, "parallel", "parallel").unwrap();
        let mut h = hooks();
        let summary = run_parallel(
            &dir,
            &svc,
            vec![
                provider("a.txt", "a", "a-done"),
                provider("b.txt", "b", "b-done"),
            ],
            "mock".into(),
            16,
            &[a.clone(), b.clone()],
            true,
            IntegrationStrategy::FastForwardOnly,
            OnConflictStrategy::Abort,
            Arc::new(AutoParallelApprove),
            None,
            Arc::new(AtomicBool::new(false)),
            &lease,
            &mut h,
        );
        assert_eq!(summary.completed, vec![a.clone(), b.clone()]);
        assert_eq!(summary.applied, vec![a.clone()]);
        assert!(
            !summary.stopped_reason.is_empty(),
            "{}",
            summary.stopped_reason
        );
        assert!(std::fs::read_to_string(dir.join("a.txt"))
            .unwrap()
            .contains("a-done"));
        assert!(std::fs::read_to_string(dir.join("b.txt"))
            .unwrap()
            .contains("b"));
        assert!(workspace::worktree_path(&dir, &b).exists());
    }

    #[test]
    fn merge_strategy_integrates_both_independent_tasks() {
        let _provider_env = provider_env();
        let dir = repo("integrate-merge");
        let svc = TaskService::new(dir.clone());
        let a = task(&svc, "A");
        let b = task(&svc, "B");
        let lease = try_acquire(&dir, "parallel", "parallel").unwrap();
        let mut h = hooks();
        let summary = run_parallel(
            &dir,
            &svc,
            vec![
                provider("a.txt", "a", "a-done"),
                provider("b.txt", "b", "b-done"),
            ],
            "mock".into(),
            16,
            &[a.clone(), b.clone()],
            true,
            IntegrationStrategy::Merge,
            OnConflictStrategy::Abort,
            Arc::new(AutoParallelApprove),
            None,
            Arc::new(AtomicBool::new(false)),
            &lease,
            &mut h,
        );
        assert!(
            summary.stopped_reason.is_empty(),
            "{}",
            summary.stopped_reason
        );
        assert_eq!(summary.completed, vec![a.clone(), b.clone()]);
        assert_eq!(summary.applied, vec![a.clone(), b.clone()]);
        assert_eq!(summary.merged, vec![b.clone()]);
        assert!(std::fs::read_to_string(dir.join("a.txt"))
            .unwrap()
            .contains("a-done"));
        assert!(std::fs::read_to_string(dir.join("b.txt"))
            .unwrap()
            .contains("b-done"));
    }

    #[test]
    fn workspace_conflict_strategy_retains_merge_workspace() {
        let _provider_env = provider_env();
        let dir = repo("on-conflict-workspace");
        let svc = TaskService::new(dir.clone());
        let a = task(&svc, "A");
        let b = task(&svc, "B");
        let lease = try_acquire(&dir, "parallel", "parallel").unwrap();
        let mut h = hooks();
        let summary = run_parallel(
            &dir,
            &svc,
            vec![
                provider("a.txt", "a", "a-left"),
                provider("a.txt", "a", "a-right"),
            ],
            "mock".into(),
            16,
            &[a.clone(), b.clone()],
            true,
            IntegrationStrategy::Merge,
            OnConflictStrategy::Workspace,
            Arc::new(AutoParallelApprove),
            None,
            Arc::new(AtomicBool::new(false)),
            &lease,
            &mut h,
        );
        assert_eq!(summary.completed, vec![a.clone(), b.clone()]);
        assert_eq!(summary.applied, vec![a.clone()]);
        assert_eq!(summary.merge_workspaces, vec![b.clone()]);
        assert_eq!(
            summary.merge_conflicts,
            vec![(b.clone(), vec!["a.txt".to_string()])]
        );
        assert!(
            !summary.stopped_reason.is_empty(),
            "{}",
            summary.stopped_reason
        );
        assert!(std::fs::read_to_string(dir.join("a.txt"))
            .unwrap()
            .contains("a-left"));
        assert!(crate::merge_workspace::merge_path(&dir, &b).exists());
        let (_ledger, proj, _) = svc.open_task(&b).unwrap();
        assert_eq!(proj.merge_conflicts, vec!["a.txt".to_string()]);
        assert_eq!(proj.merge_workspace.as_ref().unwrap().status, "conflicted");
    }

    #[test]
    fn failure_retains_worktree_and_completed_task_applies() {
        let _provider_env = provider_env();
        let dir = repo("failure");
        let svc = TaskService::new(dir.clone());
        let a = task(&svc, "A");
        let b = task(&svc, "B");
        let lease = try_acquire(&dir, "parallel", "parallel").unwrap();
        let mut h = hooks();
        let summary = run_parallel(
            &dir,
            &svc,
            vec![failing_provider(), provider("b.txt", "b", "b-done")],
            "mock".into(),
            16,
            &[a.clone(), b.clone()],
            true,
            IntegrationStrategy::FastForwardOnly,
            OnConflictStrategy::Abort,
            Arc::new(AutoParallelApprove),
            None,
            Arc::new(AtomicBool::new(false)),
            &lease,
            &mut h,
        );
        assert_eq!(summary.completed, vec![b.clone()]);
        assert_eq!(summary.failed, vec![a.clone()]);
        assert_eq!(summary.applied, vec![b.clone()]);
        assert!(workspace::worktree_path(&dir, &a).exists());
        assert!(std::fs::read_to_string(dir.join("b.txt"))
            .unwrap()
            .contains("b-done"));
    }

    #[test]
    fn lease_metadata_contains_parallel_tasks() {
        let _provider_env = provider_env();
        let dir = repo("lease");
        let svc = TaskService::new(dir.clone());
        let a = task(&svc, "A");
        let b = task(&svc, "B");
        let lease = try_acquire(&dir, "parallel", "parallel").unwrap();
        lease
            .set_parallel(&dir, &[a.clone(), b.clone()], std::slice::from_ref(&a))
            .unwrap();
        let info = crate::lease::peek_info(&dir).unwrap();
        assert_eq!(info.mode, "parallel");
        assert_eq!(
            info.task_ids,
            vec![a.as_str().to_string(), b.as_str().to_string()]
        );
        assert_eq!(info.active_task_ids, vec![a.as_str().to_string()]);
    }
}
