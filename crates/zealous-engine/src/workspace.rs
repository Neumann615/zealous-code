//! 任务 worktree 隔离(spec 155,opt-in)。
//!
//! 主仓库事件账本是唯一事实源:worktree 只作为执行环境,不保存任务
//! 事件账本;所有 workspace 状态来自事件投影。apply 仅 fast-forward,
//! 不做自动三方合并;模型不获得任何 worktree 管理能力。

use crate::ledger::Ledger;
use crate::worktree;
use std::path::{Path, PathBuf};
use zealous_core::{AgentRef, EventPayload, TaskId, TaskStatus};

pub const WORKSPACE_KIND: &str = "git-worktree";
/// 分支命名(spec 155):zealous/<task-id>,由 TaskId 派生
pub fn branch_name(task_id: &TaskId) -> String {
    format!("zealous/{}", task_id.as_str())
}
/// worktree 路径(spec 155):<repo-root>/.zealous/worktrees/<task-id>
pub fn worktree_path(repo_root: &Path, task_id: &TaskId) -> PathBuf {
    repo_root
        .join(".zealous")
        .join("worktrees")
        .join(task_id.as_str())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceError {
    DetachedHead,
    /// 任务已有 direct 运行历史,禁止切换 isolated(spec 155)
    DirectHistoryExists,
    /// 任务无 isolated workspace,却请求 apply/remove
    NotIsolated,
    TaskNotCompleted,
    BranchExists(String),
    PathExists(PathBuf),
    /// worktree 缺失且分支缺失(spec 155:拒绝恢复,不自动重建)
    WorktreeAndBranchMissing {
        branch: String,
    },
    BranchMismatch {
        expected: String,
        actual: String,
    },
    BaseCommitMissing {
        base: String,
    },
    DirtyWorktree {
        details: String,
    },
    BaseBranchMoved {
        base_commit: String,
        current_head: String,
    },
    NotOnBaseBranch {
        expected: String,
        actual: String,
    },
    NotFastForward,
    /// 任务分支缺失
    BranchMissing {
        branch: String,
    },
    /// 未 applied 且分支未包含,默认不允许清理
    NotApplied,
    /// 已应用过(幂等场景由调用方区分)
    AlreadyRemoved,
    Git(String),
    Ledger(String),
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkspaceError::DetachedHead => {
                write!(f, "detached HEAD:isolated 运行需要位于普通分支上")
            }
            WorkspaceError::DirectHistoryExists => {
                write!(f, "任务已有 direct 运行历史,禁止切换为 isolated 模式")
            }
            WorkspaceError::NotIsolated => write!(f, "任务不是 isolated 模式"),
            WorkspaceError::TaskNotCompleted => write!(f, "任务尚未 Completed,不能 apply"),
            WorkspaceError::BranchExists(b) => write!(f, "分支已存在: {b}"),
            WorkspaceError::PathExists(p) => write!(f, "路径已存在: {}", p.display()),
            WorkspaceError::WorktreeAndBranchMissing { branch } => write!(
                f,
                "worktree 与分支 {branch} 均缺失,拒绝恢复(避免丢失历史执行结果)"
            ),
            WorkspaceError::BranchMismatch { expected, actual } => {
                write!(f, "worktree 分支不符:期望 {expected},实际 {actual}")
            }
            WorkspaceError::BaseCommitMissing { base } => {
                write!(f, "任务分支不包含 base commit {base}(历史被改写?)")
            }
            WorkspaceError::DirtyWorktree { details } => {
                write!(f, "工作区存在用户未提交改动:\n{details}\n写入已拒绝。")
            }
            WorkspaceError::BaseBranchMoved {
                base_commit,
                current_head,
            } => write!(
                f,
                "主分支已前进(HEAD {current_head} ≠ base {base_commit}),请手动 merge/rebase"
            ),
            WorkspaceError::NotOnBaseBranch { expected, actual } => {
                write!(f, "当前分支 {actual} 不是 base 分支 {expected}")
            }
            WorkspaceError::NotFastForward => {
                write!(f, "任务分支与 base 分支分叉,无法 fast-forward")
            }
            WorkspaceError::BranchMissing { branch } => write!(f, "任务分支缺失: {branch}"),
            WorkspaceError::NotApplied => write!(
                f,
                "workspace 尚未 applied 且任务分支未包含于 base 分支,默认不允许清理"
            ),
            WorkspaceError::AlreadyRemoved => write!(f, "workspace 已清理"),
            WorkspaceError::Git(s) => write!(f, "git: {s}"),
            WorkspaceError::Ledger(s) => write!(f, "ledger: {s}"),
        }
    }
}

impl std::error::Error for WorkspaceError {}

fn git(dir: &Path, args: &[&str]) -> Result<String, WorkspaceError> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| WorkspaceError::Git(format!("spawn git: {e}")))?;
    if !out.status.success() {
        return Err(WorkspaceError::Git(
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

#[derive(Debug)]
pub struct Prepared {
    pub branch: String,
    pub path: PathBuf,
    pub base_branch: String,
    pub base_commit: String,
}

/// 首次 isolated run:创建任务专属 worktree 并落账(spec 155)。
/// 主工作区存在用户 WIP 不阻塞——WIP 原样保留,worktree 基于 base_commit。
pub fn prepare(
    task_id: &TaskId,
    repo_root: &Path,
    ledger: &Ledger,
) -> Result<Prepared, WorkspaceError> {
    let base_branch = git(repo_root, &["branch", "--show-current"])?
        .trim()
        .to_string();
    if base_branch.is_empty() {
        return Err(WorkspaceError::DetachedHead);
    }
    let base_commit = git(repo_root, &["rev-parse", "HEAD"])?.trim().to_string();
    let branch = branch_name(task_id);
    let path = worktree_path(repo_root, task_id);
    if git_ok(
        repo_root,
        &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
    ) {
        return Err(WorkspaceError::BranchExists(branch));
    }
    if path.exists() {
        return Err(WorkspaceError::PathExists(path));
    }
    git(
        repo_root,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            path.to_str()
                .ok_or_else(|| WorkspaceError::Git("worktree path is not valid unicode".into()))?,
            &base_commit,
        ],
    )?;
    ledger
        .append(
            AgentRef::Coordinator,
            EventPayload::ExecutionWorkspacePrepared {
                kind: WORKSPACE_KIND.into(),
                branch: branch.clone(),
                path: path.display().to_string(),
                base_branch: base_branch.clone(),
                base_commit: base_commit.clone(),
            },
        )
        .map_err(|e| WorkspaceError::Ledger(e.to_string()))?;
    Ok(Prepared {
        branch,
        path,
        base_branch,
        base_commit,
    })
}

/// resume 校验(spec 155):worktree 存在、是 worktree、分支正确、
/// 包含 base_commit、无用户未提交改动(RuntimeArtifact 不阻塞)。
/// worktree 缺失但分支存在 → 重新 attach;两者均缺失 → 拒绝。
pub fn validate_resume(
    repo_root: &Path,
    ws: &zealous_core::projection::ExecutionWorkspaceView,
) -> Result<PathBuf, WorkspaceError> {
    let path = PathBuf::from(&ws.path);
    if !path.exists() {
        if git_ok(
            repo_root,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/heads/{}", ws.branch),
            ],
        ) {
            git(
                repo_root,
                &[
                    "worktree",
                    "add",
                    path.to_str().ok_or_else(|| {
                        WorkspaceError::Git("worktree path is not valid unicode".into())
                    })?,
                    &ws.branch,
                ],
            )?;
        } else {
            return Err(WorkspaceError::WorktreeAndBranchMissing {
                branch: ws.branch.clone(),
            });
        }
    }
    if git(&path, &["rev-parse", "--is-inside-work-tree"])
        .map(|s| s.trim().to_string())
        .as_deref()
        != Ok("true")
    {
        return Err(WorkspaceError::WorktreeAndBranchMissing {
            branch: ws.branch.clone(),
        });
    }
    let current = git(&path, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    if current != ws.branch {
        return Err(WorkspaceError::BranchMismatch {
            expected: ws.branch.clone(),
            actual: current,
        });
    }
    if !git_ok(
        &path,
        &["merge-base", "--is-ancestor", &ws.base_commit, &ws.branch],
    ) {
        return Err(WorkspaceError::BaseCommitMissing {
            base: ws.base_commit.clone(),
        });
    }
    let state = worktree::read_state(&path).map_err(WorkspaceError::Git)?;
    if state.has_user_changes() {
        let details = state
            .of(worktree::EntryKind::UserChange)
            .iter()
            .take(5)
            .map(|e| format!("\n  {}", e.raw))
            .collect::<String>();
        return Err(WorkspaceError::DirtyWorktree { details });
    }
    Ok(path)
}

/// 运行根解析(spec 155 模式规则):
/// - isolated 任务(已有 workspace):无论是否带 flag,都继续使用
///   worktree(禁止退回 direct);
/// - 非 isolated 任务 + isolate=true:无 direct 运行历史时 prepare,
///   有历史则拒绝;
/// - 其余:主工作区(direct,行为不变)。
pub fn ensure_run_root(
    repo_root: &Path,
    ledger: &Ledger,
    task_id: &TaskId,
    isolate: bool,
) -> Result<PathBuf, WorkspaceError> {
    let proj = ledger
        .fold_now()
        .map_err(|e| WorkspaceError::Ledger(e.to_string()))?;
    match &proj.execution_workspace {
        Some(ws) => validate_resume(repo_root, ws),
        None => {
            if isolate {
                if !proj.runs.is_empty() {
                    return Err(WorkspaceError::DirectHistoryExists);
                }
                Ok(prepare(task_id, repo_root, ledger)?.path)
            } else {
                Ok(repo_root.to_path_buf())
            }
        }
    }
}

#[derive(Debug)]
pub struct ApplyOutcome {
    pub branch: String,
    pub applied_commit: String,
    /// 重复 apply:返回已应用状态,不重复执行(spec 155 幂等)
    pub already: bool,
}

/// 应用回主工作区(spec 155):全部允许条件满足后 `git merge --ff-only`。
pub fn apply(repo_root: &Path, ledger: &Ledger) -> Result<ApplyOutcome, WorkspaceError> {
    let proj = ledger
        .fold_now()
        .map_err(|e| WorkspaceError::Ledger(e.to_string()))?;
    let ws = proj
        .execution_workspace
        .clone()
        .ok_or(WorkspaceError::NotIsolated)?;
    if let Some(applied) = &proj.applied_commit {
        return Ok(ApplyOutcome {
            branch: ws.branch,
            applied_commit: applied.clone(),
            already: true,
        });
    }
    if proj.status != TaskStatus::Completed {
        return Err(WorkspaceError::TaskNotCompleted);
    }
    let current_branch = git(repo_root, &["branch", "--show-current"])?
        .trim()
        .to_string();
    if current_branch != ws.base_branch {
        return Err(WorkspaceError::NotOnBaseBranch {
            expected: ws.base_branch.clone(),
            actual: current_branch,
        });
    }
    let head = git(repo_root, &["rev-parse", "HEAD"])?.trim().to_string();
    if head != ws.base_commit {
        return Err(WorkspaceError::BaseBranchMoved {
            base_commit: ws.base_commit.clone(),
            current_head: head,
        });
    }
    let state = worktree::read_state(repo_root).map_err(WorkspaceError::Git)?;
    if state.has_user_changes() {
        let details = state
            .of(worktree::EntryKind::UserChange)
            .iter()
            .take(5)
            .map(|e| format!("\n  {}", e.raw))
            .collect::<String>();
        return Err(WorkspaceError::DirtyWorktree { details });
    }
    if !git_ok(
        repo_root,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/heads/{}", ws.branch),
        ],
    ) {
        return Err(WorkspaceError::BranchMissing {
            branch: ws.branch.clone(),
        });
    }
    if !git_ok(
        repo_root,
        &[
            "merge-base",
            "--is-ancestor",
            &ws.base_commit,
            &format!("refs/heads/{}", ws.branch),
        ],
    ) {
        return Err(WorkspaceError::NotFastForward);
    }
    git(repo_root, &["merge", "--ff-only", &ws.branch])?;
    let applied_commit = git(repo_root, &["rev-parse", "HEAD"])?.trim().to_string();
    ledger
        .append(
            AgentRef::User,
            EventPayload::ExecutionWorkspaceApplied {
                branch: ws.branch.clone(),
                applied_commit: applied_commit.clone(),
            },
        )
        .map_err(|e| WorkspaceError::Ledger(e.to_string()))?;
    Ok(ApplyOutcome {
        branch: ws.branch,
        applied_commit,
        already: false,
    })
}

#[derive(Debug)]
pub struct RemovedInfo {
    pub branch: String,
    pub path: PathBuf,
}

/// 清理 workspace(spec 155):默认仅允许 applied 或分支 tip 已包含于
/// base branch;worktree 有用户未提交改动时需 force。
pub fn remove(
    repo_root: &Path,
    ledger: &Ledger,
    force: bool,
) -> Result<RemovedInfo, WorkspaceError> {
    let proj = ledger
        .fold_now()
        .map_err(|e| WorkspaceError::Ledger(e.to_string()))?;
    let ws = proj
        .execution_workspace
        .clone()
        .ok_or(WorkspaceError::NotIsolated)?;
    if proj.workspace_removed {
        return Err(WorkspaceError::AlreadyRemoved);
    }
    let applied = proj.applied_commit.is_some();
    let contained = git_ok(
        repo_root,
        &[
            "merge-base",
            "--is-ancestor",
            &format!("refs/heads/{}", ws.branch),
            &format!("refs/heads/{}", ws.base_branch),
        ],
    );
    if !applied && !contained {
        return Err(WorkspaceError::NotApplied);
    }
    let path = PathBuf::from(&ws.path);
    if path.exists() {
        let state = worktree::read_state(&path).map_err(WorkspaceError::Git)?;
        if state.has_user_changes() && !force {
            let details = state
                .of(worktree::EntryKind::UserChange)
                .iter()
                .take(5)
                .map(|e| format!("\n  {}", e.raw))
                .collect::<String>();
            return Err(WorkspaceError::DirtyWorktree { details });
        }
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        git(
            repo_root,
            &[
                args.as_slice(),
                &[path.to_str().ok_or_else(|| {
                    WorkspaceError::Git("worktree path is not valid unicode".into())
                })?],
            ]
            .concat(),
        )?;
    }
    git(repo_root, &["branch", "-d", &ws.branch])?;
    ledger
        .append(
            AgentRef::User,
            EventPayload::ExecutionWorkspaceRemoved {
                branch: ws.branch.clone(),
                path: ws.path.clone(),
            },
        )
        .map_err(|e| WorkspaceError::Ledger(e.to_string()))?;
    Ok(RemovedInfo {
        branch: ws.branch,
        path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into()
    }

    fn task_repo(tag: &str) -> (PathBuf, TaskId, Ledger) {
        let dir = std::env::temp_dir().join(format!(
            "zealous-ws-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join(".gitignore"), ".zealous/\n").unwrap();
        std::fs::write(dir.join("base.txt"), "base\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "-m", "init"]);
        let task_id = TaskId::generate();
        let ledger = Ledger::create(&dir, &task_id).unwrap();
        ledger
            .append(
                AgentRef::User,
                EventPayload::TaskCreated {
                    title: "t".into(),
                    repo_root: dir.display().to_string(),
                },
            )
            .unwrap();
        (dir, task_id, ledger)
    }

    fn commit_in_worktree(ws_path: &Path, msg: &str) {
        std::fs::write(ws_path.join("feat.txt"), format!("{msg}\n")).unwrap();
        git(ws_path, &["add", "-A"]);
        git(ws_path, &["commit", "-q", "-m", msg]);
    }

    // ---------- prepare ----------

    #[test]
    fn prepare_clean_repo_creates_worktree_and_event() {
        let (dir, tid, ledger) = task_repo("prep-clean");
        let prepared = prepare(&tid, &dir, &ledger).unwrap();
        assert_eq!(prepared.branch, format!("zealous/{}", tid.as_str()));
        assert!(prepared.path.join(".git").exists());
        assert_eq!(prepared.base_branch, "main");
        let proj = ledger.fold_now().unwrap();
        let ws = proj.execution_workspace.as_ref().unwrap();
        assert_eq!(ws.branch, prepared.branch);
        assert_eq!(ws.status, "ready");
        // 主仓库工作区干净:worktree 位于 ignored 的 .zealous/ 下
        let st = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(st.stdout.is_empty(), "主仓库 status 应干净");
    }

    #[test]
    fn prepare_with_user_wip_still_works() {
        let (dir, tid, ledger) = task_repo("prep-wip");
        std::fs::write(dir.join("wip.txt"), "user wip").unwrap();
        let prepared = prepare(&tid, &dir, &ledger).unwrap();
        assert!(prepared.path.join(".git").exists());
        // 用户 WIP 原样
        assert_eq!(
            std::fs::read_to_string(dir.join("wip.txt")).unwrap(),
            "user wip"
        );
    }

    #[test]
    fn prepare_detached_head_rejected() {
        let (dir, tid, ledger) = task_repo("prep-detached");
        let sha = git(&dir, &["rev-parse", "HEAD"]).trim().to_string();
        git(&dir, &["checkout", "-q", &sha]); // detached
        let err = prepare(&tid, &dir, &ledger).unwrap_err();
        assert_eq!(err, WorkspaceError::DetachedHead);
    }

    #[test]
    fn prepare_existing_branch_rejected() {
        let (dir, tid, ledger) = task_repo("prep-branch");
        git(&dir, &["branch", &branch_name(&tid)]);
        let err = prepare(&tid, &dir, &ledger).unwrap_err();
        assert!(matches!(err, WorkspaceError::BranchExists(_)), "{err:?}");
    }

    #[test]
    fn prepare_existing_path_rejected() {
        let (dir, tid, ledger) = task_repo("prep-path");
        let path = worktree_path(&dir, &tid);
        std::fs::create_dir_all(&path).unwrap();
        let err = prepare(&tid, &dir, &ledger).unwrap_err();
        assert!(matches!(err, WorkspaceError::PathExists(_)), "{err:?}");
    }

    // ---------- resume / ensure_run_root ----------

    #[test]
    fn ensure_run_root_direct_history_rejects_isolate() {
        let (dir, tid, ledger) = task_repo("run-hist");
        ledger
            .append(
                AgentRef::Coordinator,
                EventPayload::AgentRunStarted {
                    run_id: zealous_core::AgentRunId::generate(),
                    role: zealous_core::AgentRole::Coordinator,
                },
            )
            .unwrap();
        let err = ensure_run_root(&dir, &ledger, &tid, true).unwrap_err();
        assert_eq!(err, WorkspaceError::DirectHistoryExists);
        // 默认 direct 不受影响
        let root = ensure_run_root(&dir, &ledger, &tid, false).unwrap();
        assert_eq!(root, dir);
    }

    #[test]
    fn ensure_run_root_isolated_task_auto_continues_without_flag() {
        let (dir, tid, ledger) = task_repo("run-auto");
        let ws_path = ensure_run_root(&dir, &ledger, &tid, true).unwrap();
        // 不带 flag 再跑:自动继续 worktree(禁止退回 direct)
        let root = ensure_run_root(&dir, &ledger, &tid, false).unwrap();
        assert_eq!(root, ws_path);
    }

    #[test]
    fn resume_rejects_dirty_worktree_but_allows_runtime_artifacts() {
        let (dir, tid, ledger) = task_repo("resume-dirty");
        let ws = prepare(&tid, &dir, &ledger).unwrap();
        std::fs::write(ws.path.join("user.txt"), "wip").unwrap();
        let err = validate_resume(&dir, &{
            let proj = ledger.fold_now().unwrap();
            proj.execution_workspace.clone().unwrap()
        })
        .unwrap_err();
        assert!(
            matches!(err, WorkspaceError::DirtyWorktree { .. }),
            "{err:?}"
        );
        // RuntimeArtifact 不阻塞
        std::fs::remove_file(ws.path.join("user.txt")).unwrap();
        std::fs::create_dir_all(ws.path.join("__pycache__")).unwrap();
        std::fs::write(ws.path.join("__pycache__/x.pyc"), "b").unwrap();
        validate_resume(&dir, &{
            let proj = ledger.fold_now().unwrap();
            proj.execution_workspace.clone().unwrap()
        })
        .unwrap();
    }

    #[test]
    fn resume_missing_worktree_reattaches_missing_both_rejects() {
        let (dir, tid, ledger) = task_repo("resume-missing");
        let ws = prepare(&tid, &dir, &ledger).unwrap();
        // 分支保留,worktree 移除 → re-attach
        git(&dir, &["worktree", "remove", ws.path.to_str().unwrap()]);
        assert!(!ws.path.exists());
        let root = ensure_run_root(&dir, &ledger, &tid, false).unwrap();
        assert!(root.join(".git").exists());
        // worktree 与分支均缺失 → 拒绝
        git(&dir, &["worktree", "remove", ws.path.to_str().unwrap()]);
        git(&dir, &["branch", "-D", &ws.branch]);
        let err = ensure_run_root(&dir, &ledger, &tid, true).unwrap_err();
        assert!(
            matches!(err, WorkspaceError::WorktreeAndBranchMissing { .. }),
            "{err:?}"
        );
    }

    // ---------- apply ----------

    fn run_and_complete(dir: &Path, tid: &TaskId, ledger: &Ledger) -> Prepared {
        let ws = prepare(tid, dir, ledger).unwrap();
        // Queued → Running → Completed(合法链)
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
        ws
    }

    #[test]
    fn apply_fast_forward_success() {
        let (dir, tid, ledger) = task_repo("apply-ff");
        let ws = run_and_complete(&dir, &tid, &ledger);
        commit_in_worktree(&ws.path, "feat: isolated change");
        let out = apply(&dir, &ledger).unwrap();
        assert!(!out.already);
        // 主工作区获得任务分支 commit
        let log = git(&dir, &["log", "--oneline"]);
        assert!(log.contains("feat: isolated change"), "{log}");
        // 主工作区原文件保留
        assert!(dir.join("base.txt").exists());
        // 重复 apply 幂等
        let out2 = apply(&dir, &ledger).unwrap();
        assert!(out2.already);
        assert_eq!(out2.applied_commit, out.applied_commit);
    }

    #[test]
    fn apply_empty_branch_marks_applied() {
        let (dir, tid, ledger) = task_repo("apply-empty");
        run_and_complete(&dir, &tid, &ledger);
        let out = apply(&dir, &ledger).unwrap();
        assert!(!out.already);
        let proj = ledger.fold_now().unwrap();
        assert_eq!(
            proj.applied_commit,
            Some(proj.execution_workspace.unwrap().base_commit)
        );
    }

    #[test]
    fn apply_rejects_main_moved_user_change_wrong_branch_and_incomplete() {
        // 主分支前进
        let (dir, tid, ledger) = task_repo("apply-moved");
        let ws = run_and_complete(&dir, &tid, &ledger);
        commit_in_worktree(&ws.path, "feat: isolated");
        std::fs::write(dir.join("main.txt"), "main moved").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "-m", "main moved"]);
        let err = apply(&dir, &ledger).unwrap_err();
        assert!(
            matches!(err, WorkspaceError::BaseBranchMoved { .. }),
            "{err:?}"
        );
        // 清掉 main 前进(回退到 base)→ UserChange 拒绝
        git(&dir, &["reset", "-q", "--hard", &ws.base_commit]);
        std::fs::write(dir.join("user.txt"), "wip").unwrap();
        let err = apply(&dir, &ledger).unwrap_err();
        assert!(
            matches!(err, WorkspaceError::DirtyWorktree { .. }),
            "{err:?}"
        );
        std::fs::remove_file(dir.join("user.txt")).unwrap();
        // 未 Completed 拒绝(新任务)
        let (dir2, tid2, ledger2) = task_repo("apply-incomplete");
        prepare(&tid2, &dir2, &ledger2).unwrap();
        ledger2
            .append(
                AgentRef::User,
                EventPayload::TaskStatusChanged {
                    from: TaskStatus::Queued,
                    to: TaskStatus::Running,
                    reason: None,
                },
            )
            .unwrap();
        let err = apply(&dir2, &ledger2).unwrap_err();
        assert_eq!(err, WorkspaceError::TaskNotCompleted);
        // 非 isolated 任务拒绝
        let (dir3, _tid3, ledger3) = task_repo("apply-direct");
        let err = apply(&dir3, &ledger3).unwrap_err();
        assert_eq!(err, WorkspaceError::NotIsolated);
        let _ = dir; // used above
    }

    // ---------- remove ----------

    #[test]
    fn remove_requires_applied_or_contained_and_respects_dirty() {
        let (dir, tid, ledger) = task_repo("rm");
        let ws = run_and_complete(&dir, &tid, &ledger);
        commit_in_worktree(&ws.path, "feat: not applied yet");
        // 未 applied 且分支未包含 → 拒绝
        let err = remove(&dir, &ledger, false).unwrap_err();
        assert_eq!(err, WorkspaceError::NotApplied);
        // apply 后可 remove
        apply(&dir, &ledger).unwrap();
        std::fs::write(ws.path.join("dirty.txt"), "d").unwrap();
        let err = remove(&dir, &ledger, false).unwrap_err();
        assert!(
            matches!(err, WorkspaceError::DirtyWorktree { .. }),
            "{err:?}"
        );
        remove(&dir, &ledger, true).unwrap();
        assert!(!ws.path.exists());
        let proj = ledger.fold_now().unwrap();
        assert!(proj.workspace_removed);
        // 分支已删
        assert!(!git_ok(
            &dir,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/heads/{}", ws.branch)
            ]
        ));
    }
}
