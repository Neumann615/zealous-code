//! 任务服务(spec 10/20):任务的创建、列举与打开(恢复入口)。
//!
//! 硬性前提(spec 00):任务必须在 Git 仓库内发起,创建时校验 `.git` 存在。
//! Provider 配置在创建时校验(spec 10):缺失/非法 → 任务立即落账为
//! `Failed(reason=provider_config_invalid)`,错误原因可从账本读出(A6)。

use std::path::{Path, PathBuf};

use zealous_core::{projection, AgentRef, EventPayload, TaskId, TaskProjection, TaskStatus};

use crate::ledger::{Ledger, LedgerError, ReadReport};
use crate::provider::ProviderConfig;

#[derive(Debug)]
pub enum ServiceError {
    /// 不在 Git 仓库内(spec 00 硬性前提)
    NotGitRepo(PathBuf),
    /// brief 文件问题(spec 80:越界/不存在/不可读)
    Brief(String),
    Io(std::io::Error),
    Ledger(LedgerError),
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceError::NotGitRepo(p) => write!(f, "not a git repo: {}", p.display()),
            ServiceError::Brief(m) => write!(f, "brief: {m}"),
            ServiceError::Io(e) => write!(f, "io: {e}"),
            ServiceError::Ledger(e) => write!(f, "ledger: {e}"),
        }
    }
}

impl std::error::Error for ServiceError {}

impl From<std::io::Error> for ServiceError {
    fn from(e: std::io::Error) -> Self {
        ServiceError::Io(e)
    }
}

impl From<LedgerError> for ServiceError {
    fn from(e: LedgerError) -> Self {
        ServiceError::Ledger(e)
    }
}

/// 创建结果:Ready 表示可以启动编排;FailedAtCreation 的任务已存在且
/// 账本可解释(A6:配置错误的任务也要可审计,而不是静默不创建)。
pub enum CreateOutcome {
    Ready { task_id: TaskId, ledger: Ledger },
    FailedAtCreation { task_id: TaskId, reason: String },
}

impl std::fmt::Debug for CreateOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CreateOutcome::Ready { task_id, .. } => {
                f.debug_struct("Ready").field("task_id", task_id).finish()
            }
            CreateOutcome::FailedAtCreation { task_id, reason } => f
                .debug_struct("FailedAtCreation")
                .field("task_id", task_id)
                .field("reason", reason)
                .finish(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSummary {
    pub task_id: TaskId,
    pub title: String,
    pub status: TaskStatus,
}

/// 列表结果:(可用任务, 跳过的损坏账本及其原因)
pub type TaskListing = (Vec<TaskSummary>, Vec<(TaskId, String)>);

pub struct TaskService {
    repo_root: PathBuf,
}

impl TaskService {
    pub fn new(repo_root: impl Into<PathBuf>) -> Self {
        Self {
            repo_root: repo_root.into(),
        }
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    /// 读取仓库内 brief 文件:越界/不存在/不可读均拒绝(spec 80 §1)
    fn read_brief_in_repo(&self, p: &Path) -> Result<String, ServiceError> {
        let canon_root = self
            .repo_root
            .canonicalize()
            .map_err(|e| ServiceError::Brief(format!("仓库根不可解析: {e}")))?;
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.repo_root.join(p)
        };
        let canon = joined
            .canonicalize()
            .map_err(|e| ServiceError::Brief(format!("brief 文件不可解析: {e}")))?;
        if !canon.starts_with(&canon_root) {
            return Err(ServiceError::Brief(format!(
                "brief 文件越界:{} 不在仓库 {} 内",
                canon.display(),
                canon_root.display()
            )));
        }
        if !canon.is_file() {
            return Err(ServiceError::Brief(format!(
                "brief 路径不是文件: {}",
                canon.display()
            )));
        }
        std::fs::read_to_string(&canon)
            .map_err(|e| ServiceError::Brief(format!("brief 读取失败: {e}")))
    }

    fn is_git_repo(root: &Path) -> bool {
        // .git 可以是目录(普通仓库)或文件(worktree/submodule)
        root.join(".git").exists()
    }

    /// 创建任务并写入 TaskCreated;随后校验 Provider 配置。
    /// 配置非法时不回滚:任务以 Failed 状态留档,原因可解释(spec 10)。
    pub fn create_task(&self, title: &str) -> Result<CreateOutcome, ServiceError> {
        self.create_task_with_brief(title, None)
    }

    /// 带 brief 的创建(spec 80 §1):brief 文件必须位于当前 Git 仓库内。
    pub fn create_task_with_brief(
        &self,
        title: &str,
        brief_path: Option<&Path>,
    ) -> Result<CreateOutcome, ServiceError> {
        if !Self::is_git_repo(&self.repo_root) {
            return Err(ServiceError::NotGitRepo(self.repo_root.clone()));
        }
        let brief = match brief_path {
            Some(p) => Some(self.read_brief_in_repo(p)?),
            None => None,
        };
        let task_id = TaskId::generate();
        let ledger = Ledger::create(&self.repo_root, &task_id)?;
        ledger.append(
            AgentRef::User,
            EventPayload::TaskCreated {
                title: title.to_string(),
                repo_root: self.repo_root.display().to_string(),
            },
        )?;
        if let Some(b) = &brief {
            ledger.append(
                AgentRef::User,
                EventPayload::TaskBriefAttached { brief: b.clone() },
            )?;
        }

        match ProviderConfig::resolve() {
            Ok(_) => Ok(CreateOutcome::Ready { task_id, ledger }),
            Err(e) => {
                // 错误串只含字段级原因,不含凭据(ProviderError::Invalid 的构造保证)
                let reason = e.to_string();
                ledger.append(
                    AgentRef::User,
                    EventPayload::TaskStatusChanged {
                        from: TaskStatus::Queued,
                        to: TaskStatus::Failed,
                        reason: Some(reason.clone()),
                    },
                )?;
                ledger.append(
                    AgentRef::User,
                    EventPayload::ErrorOccurred {
                        scope: "provider".into(),
                        message: reason.clone(),
                    },
                )?;
                Ok(CreateOutcome::FailedAtCreation { task_id, reason })
            }
        }
    }

    /// 打开既有任务(恢复/查看入口):完整校验账本并返回投影。
    pub fn open_task(
        &self,
        task_id: &TaskId,
    ) -> Result<(Ledger, TaskProjection, ReadReport), ServiceError> {
        let (ledger, projection, report) = Ledger::open(&self.repo_root, task_id)?;
        Ok((ledger, projection, report))
    }

    /// 读取任务全量事件(spec 90:TUI 等薄客户端经此审计,
    /// 不直接触碰 .zealous/.../events.jsonl 路径)
    pub fn events(
        &self,
        task_id: &TaskId,
    ) -> Result<(Vec<zealous_core::TaskEvent>, crate::ledger::ReadReport), ServiceError> {
        let path = crate::ledger::events_path(&crate::ledger::task_dir(&self.repo_root, task_id));
        crate::ledger::read_events(&path).map_err(ServiceError::Ledger)
    }

    /// 重新打开任务并返回投影(spec 90:TUI 刷新只重新 fold)
    pub fn refresh(&self, task_id: &TaskId) -> Result<TaskProjection, ServiceError> {
        let (_, projection, _) = self.open_task(task_id)?;
        Ok(projection)
    }

    /// 列出全部任务。单个账本损坏不阻塞列表:跳过并记录原因。
    pub fn list_tasks(&self) -> Result<TaskListing, ServiceError> {
        let root = self.repo_root.join(".zealous").join("tasks");
        if !root.exists() {
            return Ok((Vec::new(), Vec::new()));
        }
        let mut summaries = Vec::new();
        let mut skipped = Vec::new();
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let dir_name = entry.file_name().to_string_lossy().into_owned();
            // 目录名即 ID:非法目录名视为外部污染,跳过(spec 60)
            let task_id = match TaskId::parse(&dir_name) {
                Ok(id) => id,
                Err(_) => continue,
            };
            let path = crate::ledger::events_path(&entry.path());
            match std::fs::read_to_string(&path)
                .map_err(LedgerError::Io)
                .and_then(|text| {
                    // 列表走轻量路径:不做 torn-tail 恢复语义,交给 open_task
                    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
                    let mut events = Vec::new();
                    for (i, line) in lines.iter().enumerate() {
                        let ev: zealous_core::TaskEvent =
                            serde_json::from_str(line).map_err(|e| LedgerError::Corrupt {
                                line: i + 1,
                                detail: e.to_string(),
                            })?;
                        events.push(ev);
                    }
                    projection::fold(&task_id, &events).map_err(LedgerError::Fold)
                }) {
                Ok(proj) => summaries.push(TaskSummary {
                    task_id: task_id.clone(),
                    title: proj.title,
                    status: proj.status,
                }),
                Err(e) => skipped.push((task_id.clone(), e.to_string())),
            }
        }
        summaries.sort_by(|a, b| a.task_id.as_str().cmp(b.task_id.as_str()));
        Ok((summaries, skipped))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ENV_LOCK;

    pub(crate) fn git_repo_fixture() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zealous-svc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        dir
    }

    fn clear_zealous_env() {
        for k in [
            "ZEALOUS_BASE_URL",
            "ZEALOUS_API_KEY",
            "ZEALOUS_MODEL",
            "ZEALOUS_TIMEOUT_SECS",
            "ZEALOUS_MAX_RETRIES",
        ] {
            std::env::remove_var(k);
        }
    }

    #[test]
    fn create_requires_git_repo() {
        let dir = std::env::temp_dir().join(format!("zealous-nogit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let svc = TaskService::new(&dir);
        let err = svc.create_task("t").unwrap_err();
        assert!(matches!(err, ServiceError::NotGitRepo(_)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn invalid_provider_config_fails_task_with_explainable_reason() {
        let _env = ENV_LOCK.lock().unwrap();
        clear_zealous_env();
        let repo = git_repo_fixture();
        let svc = TaskService::new(&repo);
        let outcome = svc.create_task("build feature").unwrap();
        let task_id = match outcome {
            CreateOutcome::FailedAtCreation { task_id, reason } => {
                assert!(reason.contains("provider_config_invalid"), "{reason}");
                task_id
            }
            CreateOutcome::Ready { .. } => panic!("无配置时应创建即失败"),
        };
        // 任务留档且状态可解释(A6)
        let (_, proj, _) = svc.open_task(&task_id).unwrap();
        assert_eq!(proj.status, TaskStatus::Failed);
        let (_, scope_msg) = proj.last_error.unwrap();
        assert!(scope_msg.contains("provider_config_invalid"));
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn valid_config_creates_ready_task_and_lists_it() {
        let _env = ENV_LOCK.lock().unwrap();
        std::env::set_var("ZEALOUS_BASE_URL", "https://api.example.com/v1");
        std::env::set_var("ZEALOUS_API_KEY", "k");
        std::env::set_var("ZEALOUS_MODEL", "m");
        let repo = git_repo_fixture();
        let svc = TaskService::new(&repo);
        let outcome = svc.create_task("write tests").unwrap();
        let task_id = match outcome {
            CreateOutcome::Ready { task_id, .. } => task_id,
            CreateOutcome::FailedAtCreation { .. } => panic!("配置齐全时应 Ready"),
        };
        let (summaries, skipped) = svc.list_tasks().unwrap();
        assert!(skipped.is_empty());
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].task_id, task_id);
        assert_eq!(summaries[0].status, TaskStatus::Queued);
        clear_zealous_env();
        let _ = std::fs::remove_dir_all(repo);
    }
}

#[cfg(test)]
mod brief_tests {
    use super::*;
    use crate::test_support::ENV_LOCK;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }
    use super::tests::git_repo_fixture;

    #[test]
    fn brief_outside_repo_rejected() {
        let _env = env_lock();
        for k in ["ZEALOUS_BASE_URL", "ZEALOUS_API_KEY", "ZEALOUS_MODEL"] {
            std::env::set_var(k, "x");
        }
        let repo = git_repo_fixture();
        let svc = TaskService::new(&repo);
        let outside = std::env::temp_dir().join("zealous-brief-outside.md");
        std::fs::write(&outside, "brief body").unwrap();
        let err = svc.create_task_with_brief("t", Some(&outside)).unwrap_err();
        assert!(
            matches!(&err, ServiceError::Brief(m) if m.contains("越界")),
            "{err:?}"
        );
        assert!(!repo.join("outside.md").exists());
        for k in ["ZEALOUS_BASE_URL", "ZEALOUS_API_KEY", "ZEALOUS_MODEL"] {
            std::env::remove_var(k);
        }
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn brief_in_repo_attaches_and_reaches_projection() {
        let _env = env_lock();
        std::env::set_var("ZEALOUS_BASE_URL", "https://api.example.invalid/v1");
        std::env::set_var("ZEALOUS_API_KEY", "k");
        std::env::set_var("ZEALOUS_MODEL", "m");
        let repo = git_repo_fixture();
        std::fs::write(repo.join("BRIEF.md"), "实现 slugify 并验证\n").unwrap();
        let svc = TaskService::new(&repo);
        let outcome = svc
            .create_task_with_brief("t", Some(Path::new("BRIEF.md")))
            .unwrap();
        let task_id = match outcome {
            CreateOutcome::Ready { task_id, .. } => task_id,
            CreateOutcome::FailedAtCreation { .. } => panic!("配置齐全应 Ready"),
        };
        let (_, proj, _) = svc.open_task(&task_id).unwrap();
        assert_eq!(
            proj.brief.as_deref(),
            Some("实现 slugify 并验证\n"),
            "brief 应进入投影"
        );
        for k in ["ZEALOUS_BASE_URL", "ZEALOUS_API_KEY", "ZEALOUS_MODEL"] {
            std::env::remove_var(k);
        }
        let _ = std::fs::remove_dir_all(repo);
    }
}
