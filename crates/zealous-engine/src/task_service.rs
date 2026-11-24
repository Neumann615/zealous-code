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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskSummary {
    pub task_id: TaskId,
    pub title: String,
    pub status: TaskStatus,
}

/// 列表结果:(可用任务, 跳过的损坏账本及其原因)
pub type TaskListing = (Vec<TaskSummary>, Vec<(TaskId, String)>);

#[derive(Clone)]
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

    /// brief 为文本(spec 100:app-server create_task 直接携带文本,
    /// 客户端不允许自行写账本)
    pub fn create_task_with_brief_text(
        &self,
        title: &str,
        brief: Option<String>,
    ) -> Result<CreateOutcome, ServiceError> {
        if !Self::is_git_repo(&self.repo_root) {
            return Err(ServiceError::NotGitRepo(self.repo_root.clone()));
        }
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

/// spec 173:聊天会话列表项(/resume 用)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSession {
    pub task_id: TaskId,
    /// 首条用户输入(截断)作为标题;没有对话时退回任务标题
    pub title: String,
    /// 用户轮数(ChatUserTurn 条数)
    pub turns: u64,
    /// 最近一条账本事件的时间(RFC3339)
    pub last_ts: String,
}

impl TaskService {
// ---- 聊天会话(spec 165)----

/// 创建聊天任务:普通创建 + ChatSessionStarted 标记
pub fn create_chat_task(
    &self,
    title: &str,
    brief: Option<String>,
) -> Result<CreateOutcome, ServiceError> {
    let outcome = self.create_task_with_brief_text(title, brief)?;
    if let CreateOutcome::Ready { task_id, .. } = &outcome {
        let (ledger, _, _) = self.open_task(task_id)?;
        ledger.append(AgentRef::User, EventPayload::ChatSessionStarted {})?;
    }
    Ok(outcome)
}

/// 聊天会话列表,按最近活动倒序。单个账本损坏的会话跳过。
pub fn chat_sessions(&self) -> Result<Vec<ChatSession>, ServiceError> {
    let (items, _) = self.list_tasks()?;
    let mut out = Vec::new();
    for s in items {
        let Ok((evs, _)) = self.events(&s.task_id) else {
            continue;
        };
        if !evs
            .iter()
            .any(|e| matches!(&e.payload, EventPayload::ChatSessionStarted {}))
        {
            continue;
        }
        let title = evs
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::ChatUserTurn { text } => Some(text.clone()),
                _ => None,
            })
            .map(|t| t.chars().take(48).collect::<String>())
            .unwrap_or_else(|| s.title.clone());
        let turns = evs
            .iter()
            .filter(|e| matches!(&e.payload, EventPayload::ChatUserTurn { .. }))
            .count() as u64;
        let last_ts = evs.last().map(|e| e.ts.clone()).unwrap_or_default();
        out.push(ChatSession {
            task_id: s.task_id,
            title,
            turns,
            last_ts,
        });
    }
    out.sort_by(|a, b| {
        b.last_ts
            .cmp(&a.last_ts)
            .then_with(|| b.task_id.as_str().cmp(a.task_id.as_str()))
    });
    Ok(out)
}

/// 最近活动的聊天会话(spec 165;启动恢复用)
pub fn latest_chat_task(&self) -> Result<Option<TaskId>, ServiceError> {
    Ok(self.chat_sessions()?.into_iter().next().map(|s| s.task_id))
}

/// 压缩对话史 (user, assistant)。只含最近一次 ChatCompacted 之后的轮次
/// (摘要取代此前全部轮次;摘要另见 chat_summary)。
pub fn chat_history(&self, task_id: &TaskId) -> Result<Vec<(String, String)>, ServiceError> {
    let (evs, _) = self.events(task_id)?;
    let mut pairs = Vec::new();
    let mut last_user: Option<String> = None;
    for e in evs {
        match &e.payload {
            EventPayload::ChatCompacted { .. } => {
                pairs.clear();
                last_user = None;
            }
            EventPayload::ChatUserTurn { text } => last_user = Some(text.clone()),
            EventPayload::MessageCompleted { text, .. } => {
                if let Some(u) = last_user.take() {
                    pairs.push((u, text.clone()));
                }
            }
            _ => {}
        }
    }
    Ok(pairs)
}

/// 最近一次 /compact 生成的对话摘要(没有压缩过返回 None)
pub fn chat_summary(&self, task_id: &TaskId) -> Result<Option<String>, ServiceError> {
    let (evs, _) = self.events(task_id)?;
    Ok(evs.iter().rev().find_map(|e| match &e.payload {
        EventPayload::ChatCompacted { summary, .. } => Some(summary.clone()),
        _ => None,
    }))
}

/// 全量聊天转录重建,顺序:true=用户,false=助手(压缩不清空转录显示)
pub fn chat_transcript(&self, task_id: &TaskId) -> Result<Vec<(bool, String)>, ServiceError> {
    let (evs, _) = self.events(task_id)?;
    let mut out = Vec::new();
    for e in evs {
        match &e.payload {
            EventPayload::ChatUserTurn { text } => out.push((true, text.clone())),
            EventPayload::MessageCompleted { text, .. } if !text.is_empty() => {
                out.push((false, text.clone()))
            }
            _ => {}
        }
    }
    Ok(out)
}

/// 把压缩摘要作为账本事实落账(spec 173)。可追加在任意状态之后
/// (投影不变,只影响对话史的重建)。
pub fn append_chat_compacted(
    &self,
    task_id: &TaskId,
    summary: &str,
    turns: u64,
) -> Result<(), ServiceError> {
    let (ledger, _, _) = self.open_task(task_id)?;
    ledger
        .append(
            AgentRef::User,
            EventPayload::ChatCompacted {
                summary: summary.trim().to_string(),
                turns,
            },
        )
        .map(|_| ())
        .map_err(ServiceError::Ledger)
}

}

/// spec 173:/compact 的模型调用:把对话史(可并入上一次的摘要)压缩成新的摘要。
/// 非流式单轮;摘要为空视为失败(不落账,避免丢上下文)。
pub fn summarize_history(
    provider: &dyn crate::provider::Provider,
    model: &str,
    history: &[(String, String)],
    // 上一次压缩留下的摘要:再次压缩时并入,避免丢失更早的上下文
    prior_summary: Option<&str>,
) -> Result<String, crate::provider::ProviderError> {
    const MAX_PAIRS: usize = 60;
    const MAX_CHARS: usize = 4000;
    let start = history.len().saturating_sub(MAX_PAIRS);
    let mut transcript = String::new();
    if let Some(prior) = prior_summary.map(str::trim).filter(|p| !p.is_empty()) {
        transcript.push_str(&format!("[此前摘要] {}\n", clip(prior, MAX_CHARS)));
    }
    for (i, (u, a)) in history[start..].iter().enumerate() {
        transcript.push_str(&format!(
            "[{}] 用户: {}\n[{}] 助手: {}\n",
            i + 1,
            clip(u, MAX_CHARS),
            i + 1,
            clip(a, MAX_CHARS)
        ));
    }
    let req = crate::provider::ChatRequest {
        system: Some(
            "你是对话压缩器。把下面的编程助手对话历史压缩成给后续助手看的摘要,必须保留:\n\
             1) 用户的目标与约束;2) 已完成的工作与结论(含关键文件路径、命令、数字);\n\
             3) 做过的关键决定及原因;4) 未完成的事项与已知问题。\n\
             直接输出摘要正文(不要前言、不要 Markdown 标题),用中文,控制在 600 字以内。"
                .to_string(),
        ),
        messages: vec![crate::provider::ChatMessage::user(format!(
            "对话历史:\n{transcript}"
        ))],
        tools: Vec::new(),
    };
    let resp = provider.complete(model, &req)?;
    let summary = resp.text.trim();
    if summary.is_empty() {
        return Err(crate::provider::ProviderError::Rejected {
            status: 200,
            summary: "empty summary".into(),
        });
    }
    Ok(summary.to_string())
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
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
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_zealous_env();
        // 显式非法值(env 优先级高于 ~/.zealous/config.json,保证与
        // 本机全局配置解耦的确定性;spec 10 优先级)
        std::env::set_var("ZEALOUS_BASE_URL", "notaurl");
        std::env::set_var("ZEALOUS_API_KEY", "k");
        std::env::set_var("ZEALOUS_MODEL", "m");
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
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

#[cfg(test)]
mod chat_session_tests {
    use super::*;
    use crate::provider::{ChatRequest, ChatResponse, Provider, ProviderError};
    use zealous_core::{AgentRole, AgentRunId};

    /// 建任务要校验 provider 配置:显式设置合法环境变量并串行执行
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::test_support::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ZEALOUS_BASE_URL", "http://127.0.0.1:9/v1");
        std::env::set_var("ZEALOUS_API_KEY", "k");
        std::env::set_var("ZEALOUS_MODEL", "m");
        guard
    }

    fn clear_env() {
        for k in ["ZEALOUS_BASE_URL", "ZEALOUS_API_KEY", "ZEALOUS_MODEL"] {
            std::env::remove_var(k);
        }
    }

    fn repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zealous-chat-sessions-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        dir
    }

    /// 追加一轮有助手答复的对话(ChatUserTurn → Run → MessageCompleted)
    fn push_turn(svc: &TaskService, tid: &TaskId, user: &str, assistant: &str) {
        let (ledger, _, _) = svc.open_task(tid).unwrap();
        let run = AgentRunId::generate();
        let a = |p| ledger.append(AgentRef::User, p).unwrap();
        a(EventPayload::ChatUserTurn { text: user.into() });
        a(EventPayload::AgentRunStarted {
            run_id: run.clone(),
            role: AgentRole::Coordinator,
        });
        a(EventPayload::MessageCompleted {
            run_id: run.clone(),
            turn: 1,
            text: assistant.into(),
        });
        a(EventPayload::AgentRunEnded {
            run_id: run,
            status: zealous_core::AgentRunStatus::Completed,
            reason: None,
        });
    }

    fn chat_task(svc: &TaskService, title: &str) -> TaskId {
        match svc.create_chat_task(title, Some("brief".into())).unwrap() {
            CreateOutcome::Ready { task_id, .. } => task_id,
            other => panic!("expected Ready: {other:?}"),
        }
    }

    #[test]
    fn sessions_list_chat_tasks_only_with_title_and_turns() {
        let _env = env_lock();
        let root = repo("list");
        let svc = TaskService::new(&root);
        let a = chat_task(&svc, "t-a");
        let b = chat_task(&svc, "t-b");
        // 普通任务不是聊天会话
        let plain = match svc.create_task("plain").unwrap() {
            CreateOutcome::Ready { task_id, .. } => task_id,
            other => panic!("expected Ready: {other:?}"),
        };
        push_turn(&svc, &a, "第一问", "第一答");
        push_turn(&svc, &a, "第二问", "第二答");

        let sessions = svc.chat_sessions().unwrap();
        let ids: Vec<_> = sessions.iter().map(|s| s.task_id.as_str()).collect();
        assert!(!ids.contains(&plain.as_str()), "普通任务不应出现在会话列表");
        assert_eq!(ids.len(), 2, "{ids:?}");
        // 最近活动(a)排最前;标题取首条用户输入,轮数按用户输入计
        assert_eq!(sessions[0].task_id, a);
        assert_eq!(sessions[0].title, "第一问");
        assert_eq!(sessions[0].turns, 2);
        assert_eq!(sessions[1].task_id, b);
        assert_eq!(sessions[1].turns, 0);
        // 启动恢复取最近活动的会话
        assert_eq!(svc.latest_chat_task().unwrap(), Some(a));
        clear_env();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn history_resets_after_compaction_and_summary_is_kept() {
        let _env = env_lock();
        let root = repo("compact");
        let svc = TaskService::new(&root);
        let tid = chat_task(&svc, "t");
        push_turn(&svc, &tid, "q1", "a1");
        push_turn(&svc, &tid, "q2", "a2");
        assert_eq!(svc.chat_history(&tid).unwrap().len(), 2);
        assert_eq!(svc.chat_summary(&tid).unwrap(), None);

        svc.append_chat_compacted(&tid, "  前两轮的摘要  ", 2).unwrap();
        // 摘要落账(去首尾空白);此前的对话史被摘要取代
        assert_eq!(svc.chat_summary(&tid).unwrap().as_deref(), Some("前两轮的摘要"));
        assert!(svc.chat_history(&tid).unwrap().is_empty());
        // 账本仍可正常投影(压缩不改变任务状态)
        assert_eq!(svc.refresh(&tid).unwrap().status, TaskStatus::Queued);

        // 压缩之后的新轮次重新累计;再次压缩只保留最新摘要
        push_turn(&svc, &tid, "q3", "a3");
        assert_eq!(svc.chat_history(&tid).unwrap(), vec![("q3".to_string(), "a3".to_string())]);
        svc.append_chat_compacted(&tid, "新摘要", 1).unwrap();
        assert_eq!(svc.chat_summary(&tid).unwrap().as_deref(), Some("新摘要"));
        assert!(svc.chat_history(&tid).unwrap().is_empty());
        clear_env();
        let _ = std::fs::remove_dir_all(root);
    }

    struct MockProvider {
        reply: &'static str,
    }

    impl Provider for MockProvider {
        fn complete(&self, _model: &str, _req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
            Ok(ChatResponse {
                text: self.reply.into(),
                tool_calls: Vec::new(),
                usage: None,
            })
        }
    }

    #[test]
    fn summarize_history_wraps_transcript_and_trims_reply() {
        let provider = MockProvider { reply: "  摘要正文\n" };
        let out = summarize_history(
            &provider,
            "m",
            &[("问".to_string(), "答".to_string())],
            None,
        )
        .unwrap();
        assert_eq!(out, "摘要正文");
        // 空摘要视为失败(调用方不落账,避免丢上下文)
        assert!(summarize_history(&MockProvider { reply: "  " }, "m", &[], None).is_err());
    }

    #[test]
    fn summarize_history_sends_history_in_prompt() {
        use std::sync::Mutex;
        struct Capture {
            reqs: Mutex<Vec<ChatRequest>>,
        }
        impl Provider for Capture {
            fn complete(&self, _model: &str, req: &ChatRequest) -> Result<ChatResponse, ProviderError> {
                self.reqs.lock().unwrap().push(ChatRequest {
                    system: req.system.clone(),
                    messages: req.messages.clone(),
                    tools: Vec::new(),
                });
                Ok(ChatResponse {
                    text: "s".into(),
                    tool_calls: Vec::new(),
                    usage: None,
                })
            }
        }
        let provider = Capture { reqs: Mutex::new(Vec::new()) };
        summarize_history(
            &provider,
            "m",
            &[
                ("第一问".to_string(), "第一答".to_string()),
                ("第二问".to_string(), "第二答".to_string()),
            ],
            Some("更早的摘要"),
        )
        .unwrap();
        let reqs = provider.reqs.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        let system = reqs[0].system.clone().unwrap();
        assert!(system.contains("压缩"), "{system}");
        let body = reqs[0].messages[0].content.clone();
        // 上一次的摘要排在最前,再次压缩不丢更早的上下文
        assert!(body.contains("[此前摘要] 更早的摘要"), "{body}");
        assert!(body.contains("[1] 用户: 第一问") && body.contains("[1] 助手: 第一答"), "{body}");
        assert!(body.contains("[2] 用户: 第二问") && body.contains("[2] 助手: 第二答"), "{body}");
    }
}
