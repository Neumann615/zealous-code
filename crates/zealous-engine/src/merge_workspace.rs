//! Merge conflict workspace Alpha (spec 159).
//!
//! Conflicts are materialized in a dedicated worktree; the main workspace and
//! the original task worktree remain untouched. Resolution is manual and must
//! be staged by the user before continue.

use crate::ledger::Ledger;
use crate::worktree;
use std::path::{Path, PathBuf};
use zealous_core::{AgentRef, EventPayload, TaskId, TaskStatus};

pub const MERGE_WORKSPACE_KIND: &str = "git-merge-worktree";

pub fn merge_branch(task_id: &TaskId) -> String {
    format!("zealous-merge/{}", task_id.as_str())
}

pub fn merge_path(repo_root: &Path, task_id: &TaskId) -> PathBuf {
    repo_root
        .join(".zealous")
        .join("merges")
        .join(task_id.as_str())
}

#[derive(Debug)]
pub enum MergeWorkspaceError {
    NotIsolated,
    TaskNotCompleted,
    AlreadyIntegrated,
    ActiveWorkspaceExists,
    WorkspaceMissing(PathBuf),
    BranchExists(String),
    BranchMissing(String),
    PathExists(PathBuf),
    NotConflicted,
    InvalidStatus(String),
    UnresolvedFiles(Vec<String>),
    MainMoved { expected: String, actual: String },
    NotOnBaseBranch { expected: String, actual: String },
    DirtyMain { details: String },
    DirtyWorkspace { details: String },
    FastForwardAvailable,
    BaseNotAncestorOfHead,
    NotFastForward,
    AlreadyIntegratedBranch,
    Git(String),
    Ledger(String),
}

impl std::fmt::Display for MergeWorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotIsolated => write!(f, "任务不是 isolated 模式"),
            Self::TaskNotCompleted => write!(f, "任务尚未 Completed"),
            Self::AlreadyIntegrated => write!(f, "任务已 applied/merged"),
            Self::ActiveWorkspaceExists => write!(f, "已有活跃 merge workspace"),
            Self::WorkspaceMissing(p) => write!(f, "merge workspace 不存在: {}", p.display()),
            Self::BranchExists(b) => write!(f, "merge 分支已存在: {b}"),
            Self::BranchMissing(b) => write!(f, "merge 分支缺失: {b}"),
            Self::PathExists(p) => write!(f, "merge workspace 路径已存在: {}", p.display()),
            Self::NotConflicted => write!(f, "merge workspace 不处于 conflicted 状态"),
            Self::InvalidStatus(s) => write!(f, "merge workspace 状态非法: {s}"),
            Self::UnresolvedFiles(files) => {
                write!(f, "unresolved conflict files: {}", files.join(", "))
            }
            Self::MainMoved { expected, actual } => {
                write!(f, "主 HEAD 已移动:期望 {expected},实际 {actual}")
            }
            Self::NotOnBaseBranch { expected, actual } => {
                write!(f, "当前分支 {actual} 不是 base 分支 {expected}")
            }
            Self::DirtyMain { details } => {
                write!(f, "主工作区存在用户未提交改动:\n{details}")
            }
            Self::DirtyWorkspace { details } => {
                write!(f, "merge workspace 存在用户额外修改:\n{details}")
            }
            Self::FastForwardAvailable => write!(f, "主 HEAD 仍等于任务 base,请使用 task apply"),
            Self::BaseNotAncestorOfHead => write!(f, "主 HEAD 不是任务 base commit 的后代"),
            Self::NotFastForward => write!(f, "任务分支与主分支历史关系不满足三方合并"),
            Self::AlreadyIntegratedBranch => write!(f, "任务分支已包含于主分支"),
            Self::Git(s) => write!(f, "git: {s}"),
            Self::Ledger(s) => write!(f, "ledger: {s}"),
        }
    }
}

impl std::error::Error for MergeWorkspaceError {}

fn git(dir: &Path, args: &[&str]) -> Result<String, MergeWorkspaceError> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| MergeWorkspaceError::Git(format!("spawn git: {e}")))?;
    if !out.status.success() {
        return Err(MergeWorkspaceError::Git(
            String::from_utf8_lossy(&out.stderr).into(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into())
}

fn git_ok(dir: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn porcelain(dir: &Path) -> Result<Vec<String>, MergeWorkspaceError> {
    Ok(git(dir, &["status", "--porcelain=v1", "-uall"])?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect())
}

fn user_change_details(repo_root: &Path) -> Result<Option<String>, MergeWorkspaceError> {
    let state = worktree::read_state(repo_root).map_err(MergeWorkspaceError::Git)?;
    if state.has_user_changes() {
        let details = state
            .of(worktree::EntryKind::UserChange)
            .iter()
            .take(10)
            .map(|e| format!("\n  {}", e.raw))
            .collect::<String>();
        Ok(Some(details))
    } else {
        Ok(None)
    }
}

fn validate_start_preconditions(
    repo_root: &Path,
    ledger: &Ledger,
) -> Result<(zealous_core::ExecutionWorkspaceView, String), MergeWorkspaceError> {
    let proj = ledger
        .fold_now()
        .map_err(|e| MergeWorkspaceError::Ledger(e.to_string()))?;
    let ws = proj
        .execution_workspace
        .clone()
        .ok_or(MergeWorkspaceError::NotIsolated)?;
    if proj.applied_commit.is_some() {
        return Err(MergeWorkspaceError::AlreadyIntegrated);
    }
    if proj.status != TaskStatus::Completed {
        return Err(MergeWorkspaceError::TaskNotCompleted);
    }
    if let Some(merge_ws) = &proj.merge_workspace {
        if merge_ws.status == "conflicted" || merge_ws.status == "resolved" {
            return Err(MergeWorkspaceError::ActiveWorkspaceExists);
        }
        if merge_ws.status == "integrated" {
            return Err(MergeWorkspaceError::AlreadyIntegrated);
        }
    }
    let current_branch = git(repo_root, &["branch", "--show-current"])?
        .trim()
        .to_string();
    if current_branch != ws.base_branch {
        return Err(MergeWorkspaceError::NotOnBaseBranch {
            expected: ws.base_branch.clone(),
            actual: current_branch,
        });
    }
    let head = git(repo_root, &["rev-parse", "HEAD"])?.trim().to_string();
    if head == ws.base_commit {
        return Err(MergeWorkspaceError::FastForwardAvailable);
    }
    if !git_ok(
        repo_root,
        &["merge-base", "--is-ancestor", &ws.base_commit, "HEAD"],
    ) {
        return Err(MergeWorkspaceError::BaseNotAncestorOfHead);
    }
    if let Some(details) = user_change_details(repo_root)? {
        return Err(MergeWorkspaceError::DirtyMain { details });
    }
    let branch_ref = format!("refs/heads/{}", ws.branch);
    if !git_ok(repo_root, &["rev-parse", "--verify", &branch_ref]) {
        return Err(MergeWorkspaceError::BranchMissing(ws.branch.clone()));
    }
    if !git_ok(
        repo_root,
        &["merge-base", "--is-ancestor", &ws.base_commit, &branch_ref],
    ) {
        return Err(MergeWorkspaceError::NotFastForward);
    }
    if git_ok(
        repo_root,
        &["merge-base", "--is-ancestor", &branch_ref, "HEAD"],
    ) {
        return Err(MergeWorkspaceError::AlreadyIntegratedBranch);
    }
    Ok((ws, head))
}

fn conflicted_files(workspace: &Path) -> Result<Vec<String>, MergeWorkspaceError> {
    let output = git(workspace, &["diff", "--name-only", "--diff-filter=U"])?;
    Ok(output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim().replace('\\', "/"))
        .collect())
}

fn unresolved_files(workspace: &Path) -> Result<Vec<String>, MergeWorkspaceError> {
    let output = git(workspace, &["ls-files", "-u"])?;
    let mut files = Vec::new();
    for line in output.lines() {
        if let Some(path) = line.split('\t').next_back() {
            let path = path.trim().replace('\\', "/");
            if !path.is_empty() && !files.contains(&path) {
                files.push(path);
            }
        }
    }
    Ok(files)
}

fn append(ledger: &Ledger, payload: EventPayload) -> Result<(), MergeWorkspaceError> {
    ledger
        .append(AgentRef::User, payload)
        .map(|_| ())
        .map_err(|e| MergeWorkspaceError::Ledger(e.to_string()))
}

fn remove_merge_worktree(
    repo_root: &Path,
    path: &Path,
    branch: &str,
    force: bool,
) -> Result<(), MergeWorkspaceError> {
    if path.exists() {
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(
            path.to_str()
                .ok_or_else(|| MergeWorkspaceError::Git("invalid unicode path".into()))?,
        );
        git(repo_root, &args)?;
    }
    if git_ok(
        repo_root,
        &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
    ) {
        git(repo_root, &["branch", "-d", branch])?;
    }
    Ok(())
}

#[derive(Debug)]
pub enum StartOutcome {
    Integrated {
        branch: String,
        base_commit: String,
        task_tip: String,
        merge_commit: String,
    },
    Conflicted {
        branch: String,
        path: PathBuf,
        main_head: String,
        task_branch: String,
        task_tip: String,
        files: Vec<String>,
    },
}

pub fn start(
    repo_root: &Path,
    task_id: &TaskId,
    ledger: &Ledger,
) -> Result<StartOutcome, MergeWorkspaceError> {
    let (ws, main_head) = validate_start_preconditions(repo_root, ledger)?;
    let branch = merge_branch(task_id);
    let path = merge_path(repo_root, task_id);
    let branch_ref = format!("refs/heads/{branch}");
    if path.exists() {
        return Err(MergeWorkspaceError::PathExists(path));
    }
    if git_ok(repo_root, &["rev-parse", "--verify", &branch_ref]) {
        return Err(MergeWorkspaceError::BranchExists(branch));
    }
    let task_tip = git(
        repo_root,
        &["rev-parse", &format!("refs/heads/{}", ws.branch)],
    )?
    .trim()
    .to_string();
    git(
        repo_root,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            path.to_str()
                .ok_or_else(|| MergeWorkspaceError::Git("invalid unicode path".into()))?,
            &main_head,
        ],
    )?;
    append(
        ledger,
        EventPayload::MergeWorkspacePrepared {
            branch: branch.clone(),
            path: path.display().to_string(),
            main_head: main_head.to_string(),
            task_branch: ws.branch.clone(),
            task_tip: task_tip.clone(),
        },
    )?;
    let merge_result = std::process::Command::new("git")
        .args(["merge", "--no-ff", &ws.branch])
        .current_dir(&path)
        .output()
        .map_err(|e| MergeWorkspaceError::Git(format!("spawn git merge: {e}")))?;
    if !merge_result.status.success() {
        let files = conflicted_files(&path)?;
        append(
            ledger,
            EventPayload::MergeConflictPending {
                files: files.clone(),
            },
        )?;
        return Ok(StartOutcome::Conflicted {
            branch,
            path,
            main_head: main_head.to_string(),
            task_branch: ws.branch,
            task_tip,
            files,
        });
    }

    let merge_commit = git(&path, &["rev-parse", "HEAD"])?.trim().to_string();
    append(
        ledger,
        EventPayload::MergeWorkspaceResolved {
            merge_commit: merge_commit.clone(),
        },
    )?;
    git(repo_root, &["merge", "--ff-only", &branch])?;
    append(
        ledger,
        EventPayload::ExecutionWorkspaceMerged {
            branch: ws.branch.clone(),
            base_commit: ws.base_commit.clone(),
            task_tip: task_tip.clone(),
            merge_commit: merge_commit.clone(),
            conflicted: false,
        },
    )?;
    remove_merge_worktree(repo_root, &path, &branch, false)?;
    Ok(StartOutcome::Integrated {
        branch: ws.branch,
        base_commit: ws.base_commit,
        task_tip,
        merge_commit,
    })
}

#[derive(Debug, Clone)]
pub struct Status {
    pub branch: String,
    pub path: String,
    pub main_head: String,
    pub task_branch: String,
    pub task_tip: String,
    pub status: String,
    pub merge_commit: Option<String>,
    pub unresolved_files: Vec<String>,
}

pub fn status(repo_root: &Path, ledger: &Ledger) -> Result<Status, MergeWorkspaceError> {
    let proj = ledger
        .fold_now()
        .map_err(|e| MergeWorkspaceError::Ledger(e.to_string()))?;
    let view = proj.merge_workspace.ok_or_else(|| {
        MergeWorkspaceError::WorkspaceMissing(merge_path(repo_root, &proj.task_id))
    })?;
    let path = PathBuf::from(&view.path);
    let unresolved = if view.status == "conflicted" && path.exists() {
        unresolved_files(&path)?
    } else {
        Vec::new()
    };
    Ok(Status {
        branch: view.branch,
        path: view.path,
        main_head: view.main_head,
        task_branch: view.task_branch,
        task_tip: view.task_tip,
        status: view.status,
        merge_commit: view.merge_commit,
        unresolved_files: unresolved,
    })
}

fn validate_continue(
    repo_root: &Path,
    ledger: &Ledger,
) -> Result<
    (
        zealous_core::MergeWorkspaceView,
        zealous_core::ExecutionWorkspaceView,
        PathBuf,
    ),
    MergeWorkspaceError,
> {
    let proj = ledger
        .fold_now()
        .map_err(|e| MergeWorkspaceError::Ledger(e.to_string()))?;
    let view = proj.merge_workspace.clone().ok_or_else(|| {
        MergeWorkspaceError::WorkspaceMissing(merge_path(repo_root, &proj.task_id))
    })?;
    if view.status != "conflicted" {
        return Err(MergeWorkspaceError::InvalidStatus(view.status));
    }
    let ws = proj
        .execution_workspace
        .clone()
        .ok_or(MergeWorkspaceError::NotIsolated)?;
    if proj.applied_commit.is_some() {
        return Err(MergeWorkspaceError::AlreadyIntegrated);
    }
    let path = PathBuf::from(&view.path);
    if !path.exists() {
        return Err(MergeWorkspaceError::WorkspaceMissing(path));
    }
    if !path.join(".git").exists() {
        return Err(MergeWorkspaceError::WorkspaceMissing(path));
    }
    let unresolved = unresolved_files(&path)?;
    if !unresolved.is_empty() {
        return Err(MergeWorkspaceError::UnresolvedFiles(unresolved));
    }
    let current_branch = git(repo_root, &["branch", "--show-current"])?
        .trim()
        .to_string();
    if current_branch != ws.base_branch {
        return Err(MergeWorkspaceError::NotOnBaseBranch {
            expected: ws.base_branch.clone(),
            actual: current_branch,
        });
    }
    let head = git(repo_root, &["rev-parse", "HEAD"])?.trim().to_string();
    if head != view.main_head {
        return Err(MergeWorkspaceError::MainMoved {
            expected: view.main_head.clone(),
            actual: head,
        });
    }
    if let Some(details) = user_change_details(repo_root)? {
        return Err(MergeWorkspaceError::DirtyMain { details });
    }
    Ok((view, ws, path))
}

pub fn continue_merge(repo_root: &Path, ledger: &Ledger) -> Result<String, MergeWorkspaceError> {
    let (view, ws, path) = validate_continue(repo_root, ledger)?;
    git(&path, &["commit", "--no-edit"])?;
    let merge_commit = git(&path, &["rev-parse", "HEAD"])?.trim().to_string();
    append(
        ledger,
        EventPayload::MergeWorkspaceResolved {
            merge_commit: merge_commit.clone(),
        },
    )?;
    git(repo_root, &["merge", "--ff-only", &view.branch])?;
    append(
        ledger,
        EventPayload::ExecutionWorkspaceMerged {
            branch: ws.branch.clone(),
            base_commit: ws.base_commit.clone(),
            task_tip: view.task_tip.clone(),
            merge_commit: merge_commit.clone(),
            conflicted: false,
        },
    )?;
    remove_merge_worktree(repo_root, &path, &view.branch, false)?;
    Ok(merge_commit)
}

pub fn abort(repo_root: &Path, ledger: &Ledger, force: bool) -> Result<(), MergeWorkspaceError> {
    let proj = ledger
        .fold_now()
        .map_err(|e| MergeWorkspaceError::Ledger(e.to_string()))?;
    let view = proj.merge_workspace.clone().ok_or_else(|| {
        MergeWorkspaceError::WorkspaceMissing(merge_path(repo_root, &proj.task_id))
    })?;
    if view.status != "conflicted" {
        return Err(MergeWorkspaceError::InvalidStatus(view.status));
    }
    let path = PathBuf::from(&view.path);
    if !path.exists() {
        return Err(MergeWorkspaceError::WorkspaceMissing(path));
    }
    if !force {
        let expected = proj.merge_conflicts.to_vec();
        let unexpected = porcelain(&path)?
            .into_iter()
            .filter(|raw| {
                let path_part = raw.get(3..).unwrap_or_default().trim().to_string();
                !expected.contains(&path_part)
            })
            .collect::<Vec<_>>();
        if !unexpected.is_empty() {
            return Err(MergeWorkspaceError::DirtyWorkspace {
                details: unexpected
                    .iter()
                    .map(|l| format!("\n  {l}"))
                    .collect::<String>(),
            });
        }
    }
    git(&path, &["merge", "--abort"])?;
    append(ledger, EventPayload::MergeWorkspaceAborted)?;
    remove_merge_worktree(repo_root, &path, &view.branch, force)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into()
    }

    fn repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zealous-merge-ws-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join(".gitignore"), ".zealous/\n").unwrap();
        std::fs::write(dir.join("shared.md"), "value=0\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "-m", "init"]);
        dir
    }

    fn completed_task(dir: &Path, tag: &str) -> (TaskId, Ledger, PathBuf) {
        let task_id = TaskId::generate();
        let ledger = Ledger::create(dir, &task_id).unwrap();
        ledger
            .append(
                AgentRef::User,
                EventPayload::TaskCreated {
                    title: tag.into(),
                    repo_root: dir.display().to_string(),
                },
            )
            .unwrap();
        let prepared = workspace::prepare(&task_id, dir, &ledger).unwrap();
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
        ledger
            .append(
                AgentRef::User,
                EventPayload::TaskStatusChanged {
                    from: TaskStatus::Running,
                    to: TaskStatus::Completed,
                    reason: None,
                },
            )
            .unwrap();
        (task_id, ledger, prepared.path)
    }

    fn commit_all(dir: &Path, msg: &str) {
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", msg]);
    }

    #[test]
    fn start_auto_merge_integrates_and_cleans_workspace() {
        let dir = repo("auto");
        let (tid, ledger, task_path) = completed_task(&dir, "auto");
        std::fs::write(task_path.join("task-file.txt"), "task\n").unwrap();
        commit_all(&task_path, "task change");
        std::fs::write(dir.join("main-file.txt"), "main\n").unwrap();
        commit_all(&dir, "main change");
        let out = start(&dir, &tid, &ledger).unwrap();
        let merge_commit = match out {
            StartOutcome::Integrated { merge_commit, .. } => merge_commit,
            other => panic!("{other:?}"),
        };
        assert!(dir.join("task-file.txt").exists());
        assert!(dir.join("main-file.txt").exists());
        assert_eq!(git(&dir, &["rev-parse", "HEAD"]).trim(), merge_commit);
        assert!(!merge_path(&dir, &tid).exists());
        let proj = ledger.fold_now().unwrap();
        assert_eq!(proj.merge_workspace.unwrap().status, "integrated");
        assert_eq!(
            proj.applied_strategy,
            Some(zealous_core::AppliedStrategy::ThreeWayMerge)
        );
    }

    #[test]
    fn conflict_status_continue_and_task_worktree_retained() {
        let dir = repo("continue");
        let (tid, ledger, task_path) = completed_task(&dir, "conflict");
        std::fs::write(task_path.join("shared.md"), "task\n").unwrap();
        commit_all(&task_path, "task conflict");
        std::fs::write(dir.join("shared.md"), "main\n").unwrap();
        commit_all(&dir, "main conflict");
        let out = start(&dir, &tid, &ledger).unwrap();
        let (path, files) = match out {
            StartOutcome::Conflicted { path, files, .. } => (path, files),
            other => panic!("{other:?}"),
        };
        assert_eq!(files, vec!["shared.md".to_string()]);
        assert!(path.exists());
        assert!(task_path.exists());
        assert_eq!(git(&dir, &["status", "--porcelain"]).trim(), "");
        let stat = status(&dir, &ledger).unwrap();
        assert_eq!(stat.status, "conflicted");
        assert_eq!(stat.unresolved_files, vec!["shared.md".to_string()]);
        let err = continue_merge(&dir, &ledger).unwrap_err();
        assert!(
            matches!(err, MergeWorkspaceError::UnresolvedFiles(_)),
            "{err:?}"
        );
        std::fs::write(path.join("shared.md"), "resolved-by-user\n").unwrap();
        git(&path, &["add", "shared.md"]);
        let merge_commit = continue_merge(&dir, &ledger).unwrap();
        assert_eq!(git(&dir, &["rev-parse", "HEAD"]).trim(), merge_commit);
        assert_eq!(
            std::fs::read_to_string(dir.join("shared.md"))
                .unwrap()
                .trim_end(),
            "resolved-by-user"
        );
        assert!(!path.exists());
        assert!(task_path.exists());
        let proj = ledger.fold_now().unwrap();
        assert!(proj.merge_conflicts.is_empty());
        assert_eq!(proj.applied_commit.as_deref(), Some(merge_commit.as_str()));
    }

    #[test]
    fn abort_cleans_merge_workspace_and_retains_task() {
        let dir = repo("abort");
        let (tid, ledger, task_path) = completed_task(&dir, "abort");
        std::fs::write(task_path.join("shared.md"), "task\n").unwrap();
        commit_all(&task_path, "task conflict");
        std::fs::write(dir.join("shared.md"), "main\n").unwrap();
        commit_all(&dir, "main conflict");
        let out = start(&dir, &tid, &ledger).unwrap();
        let path = match out {
            StartOutcome::Conflicted { path, .. } => path,
            other => panic!("{other:?}"),
        };
        abort(&dir, &ledger, false).unwrap();
        assert!(!path.exists());
        assert!(task_path.exists());
        assert!(!git_ok(
            &dir,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/heads/{}", merge_branch(&tid))
            ]
        ));
        assert_eq!(
            std::fs::read_to_string(dir.join("shared.md")).unwrap(),
            "main\n"
        );
        assert_eq!(git(&dir, &["status", "--porcelain"]).trim(), "");
        let proj = ledger.fold_now().unwrap();
        assert_eq!(proj.merge_workspace.unwrap().status, "aborted");
    }

    #[test]
    fn abort_rejects_extra_workspace_changes_without_force() {
        let dir = repo("dirty-abort");
        let (tid, ledger, task_path) = completed_task(&dir, "dirty");
        std::fs::write(task_path.join("shared.md"), "task\n").unwrap();
        commit_all(&task_path, "task conflict");
        std::fs::write(dir.join("shared.md"), "main\n").unwrap();
        commit_all(&dir, "main conflict");
        let out = start(&dir, &tid, &ledger).unwrap();
        let path = match out {
            StartOutcome::Conflicted { path, .. } => path,
            other => panic!("{other:?}"),
        };
        std::fs::write(path.join("extra.txt"), "user\n").unwrap();
        let err = abort(&dir, &ledger, false).unwrap_err();
        assert!(
            matches!(err, MergeWorkspaceError::DirtyWorkspace { .. }),
            "{err:?}"
        );
        assert!(path.exists());
        abort(&dir, &ledger, true).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn start_rejects_dirty_main_and_fast_forward_available() {
        let dir = repo("preconditions");
        let (tid, ledger, task_path) = completed_task(&dir, "pre");
        std::fs::write(task_path.join("task.txt"), "task\n").unwrap();
        commit_all(&task_path, "task");
        let err = start(&dir, &tid, &ledger).unwrap_err();
        assert!(
            matches!(err, MergeWorkspaceError::FastForwardAvailable),
            "{err:?}"
        );
        std::fs::write(dir.join("main.txt"), "main\n").unwrap();
        commit_all(&dir, "main");
        std::fs::write(dir.join("wip.txt"), "wip\n").unwrap();
        let err = start(&dir, &tid, &ledger).unwrap_err();
        assert!(
            matches!(err, MergeWorkspaceError::DirtyMain { .. }),
            "{err:?}"
        );
    }
}
