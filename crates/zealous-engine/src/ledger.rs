//! 事件账本持久化(spec 20):每 Task 一个 `events.jsonl`,追加只写,单写入者。
//!
//! V1 单进程内所有 Agent 共用一个进程,Mutex 即串行队列;每次 append 以
//! 单次完整行写入并 flush,崩溃最坏留下一条残缺尾行(读取时丢弃并告警)。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use zealous_core::{
    projection, time, AgentRef, EventPayload, FoldError, TaskEvent, TaskId, TaskProjection,
    EVENT_SCHEMA_VERSION,
};

#[derive(Debug)]
pub enum LedgerError {
    Io(io::Error),
    Json(serde_json::Error),
    /// 账本中间损坏:解析失败(非末行)或 seq 跳号等
    Corrupt {
        line: usize,
        detail: String,
    },
    Fold(FoldError),
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerError::Io(e) => write!(f, "ledger io: {e}"),
            LedgerError::Json(e) => write!(f, "ledger json: {e}"),
            LedgerError::Corrupt { line, detail } => {
                write!(f, "ledger corrupt at line {line}: {detail}")
            }
            LedgerError::Fold(e) => write!(f, "ledger fold: {e:?}"),
        }
    }
}

impl std::error::Error for LedgerError {}

impl From<io::Error> for LedgerError {
    fn from(e: io::Error) -> Self {
        LedgerError::Io(e)
    }
}

/// 读取报告:`torn_tail` 为真表示末行残缺已被丢弃(进程崩溃残留),
/// 这是恢复路径的正常形态;账本中间残缺则是 Corrupt 错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReadReport {
    pub events_read: u64,
    pub torn_tail: bool,
}

/// task 目录布局:`<repo_root>/.zealous/tasks/<task_id>/`
pub fn task_dir(repo_root: &Path, task_id: &TaskId) -> PathBuf {
    repo_root.join(".zealous").join("tasks").join(&task_id.0)
}

pub fn events_path(task_dir: &Path) -> PathBuf {
    task_dir.join("events.jsonl")
}

pub struct Ledger {
    task_id: TaskId,
    path: PathBuf,
    // (文件句柄, 下一个 seq)——Mutex 即 spec 20 的"串行队列",V1 单写入者
    inner: Mutex<(File, u64)>,
}

// File 不可 derive Debug;账本调试只需要 id 与路径
impl std::fmt::Debug for Ledger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ledger")
            .field("task_id", &self.task_id)
            .field("path", &self.path)
            .finish()
    }
}

impl Ledger {
    /// 新建账本(Task 创建时调用);目录不存在则创建。
    pub fn create(repo_root: &Path, task_id: &TaskId) -> Result<Self, LedgerError> {
        let dir = task_dir(repo_root, task_id);
        fs::create_dir_all(&dir)?;
        let path = events_path(&dir);
        if path.exists() {
            return Err(LedgerError::Corrupt {
                line: 0,
                detail: format!("账本已存在: {}", path.display()),
            });
        }
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)?;
        Ok(Self {
            task_id: task_id.clone(),
            path,
            inner: Mutex::new((file, 1)),
        })
    }

    /// 打开既有账本:读取全量事件、校验连续性(丢弃残缺尾行),
    /// 返回账本(写入位置续接)与投影。恢复路径(A5)依赖此方法。
    pub fn open(
        repo_root: &Path,
        task_id: &TaskId,
    ) -> Result<(Self, TaskProjection, ReadReport), LedgerError> {
        let dir = task_dir(repo_root, task_id);
        let path = events_path(&dir);
        let (events, report) = read_events(&path)?;
        let projection = projection::fold(task_id, &events).map_err(LedgerError::Fold)?;
        let next_seq = projection.last_event_seq + 1;
        let file = OpenOptions::new().append(true).open(&path)?;
        Ok((
            Self {
                task_id: task_id.clone(),
                path,
                inner: Mutex::new((file, next_seq)),
            },
            projection,
            report,
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    /// 追加一个事件:分配 seq、生成时间戳、整行单次写入。返回分配的 seq。
    pub fn append(&self, agent: AgentRef, payload: EventPayload) -> Result<u64, LedgerError> {
        let mut guard = self.inner.lock().expect("ledger mutex poisoned");
        let (file, next_seq) = &mut *guard;
        let seq = *next_seq;
        let event = TaskEvent::new(seq, now_rfc3339(), self.task_id.clone(), agent, payload);
        let mut line = serde_json::to_vec(&event).map_err(LedgerError::Json)?;
        line.push(b'\n');
        // 单次 write 整行 + flush:崩溃安全的边界
        file.write_all(&line)?;
        file.flush()?;
        *next_seq = seq + 1;
        Ok(seq)
    }

    /// 同步读取当前账本并折叠投影(用于 CLI status 等只读命令)。
    pub fn fold_now(&self) -> Result<TaskProjection, LedgerError> {
        let (events, _) = read_events(&self.path)?;
        projection::fold(&self.task_id, &events).map_err(LedgerError::Fold)
    }
}

fn now_rfc3339() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    time::rfc3339_from_unix_millis(millis)
}

/// 读取 events.jsonl:seq 连续性在这里保证(信封层),残缺尾行丢弃并报告。
pub fn read_events(path: &Path) -> Result<(Vec<TaskEvent>, ReadReport), LedgerError> {
    let mut report = ReadReport::default();
    if !path.exists() {
        return Ok((Vec::new(), report));
    }
    let bytes = fs::read(path)?;
    if bytes.is_empty() {
        return Ok((Vec::new(), report));
    }

    // 残缺尾行:文件最后一个字节不是 \n,说明最后一次 append 中断
    let (complete, torn) = if *bytes.last().unwrap() == b'\n' {
        (&bytes[..], None)
    } else {
        let split = bytes
            .iter()
            .rposition(|&b| b == b'\n')
            .map(|p| p + 1)
            .unwrap_or(0);
        report.torn_tail = true;
        (&bytes[..split], Some(&bytes[split..]))
    };
    let _ = torn; // 尾行内容仅用于调试,恢复不使用

    let mut events = Vec::new();
    for (i, line) in complete.split(|&b| b == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let event: TaskEvent = serde_json::from_slice(line).map_err(|e| LedgerError::Corrupt {
            line: i + 1,
            detail: format!("json 解析失败: {e}"),
        })?;
        // 信封层 seq 连续性(与 fold 的校验互为冗余,这里先行给出更精确的行号)
        let expected = events.last().map(|e: &TaskEvent| e.seq + 1).unwrap_or(1);
        if event.seq != expected {
            return Err(LedgerError::Corrupt {
                line: i + 1,
                detail: format!("seq 跳号: 期望 {expected}, 实际 {}", event.seq),
            });
        }
        if event.v != EVENT_SCHEMA_VERSION {
            return Err(LedgerError::Corrupt {
                line: i + 1,
                detail: format!("不支持的账本版本 v{}", event.v),
            });
        }
        events.push(event);
    }
    report.events_read = events.len() as u64;
    Ok((events, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zealous_core::{TaskStatus, ToolCallId};

    fn temp_repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zealous-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn created_payload() -> EventPayload {
        EventPayload::TaskCreated {
            title: "t".into(),
            repo_root: "C:/repo".into(),
        }
    }

    #[test]
    fn append_then_reopen_continues_seq_and_folds() {
        let repo = temp_repo();
        let tid = TaskId::generate();
        {
            let ledger = Ledger::create(&repo, &tid).unwrap();
            ledger.append(AgentRef::User, created_payload()).unwrap();
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
        }
        let (ledger, proj, report) = Ledger::open(&repo, &tid).unwrap();
        assert_eq!(report.events_read, 2);
        assert!(!report.torn_tail);
        assert_eq!(proj.status, TaskStatus::Running);
        // 重开后 seq 续接
        let seq = ledger
            .append(
                AgentRef::User,
                EventPayload::TaskPlanUpdated { plan: "p".into() },
            )
            .unwrap();
        assert_eq!(seq, 3);
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn torn_tail_is_dropped_and_reported() {
        let repo = temp_repo();
        let tid = TaskId::generate();
        {
            let ledger = Ledger::create(&repo, &tid).unwrap();
            ledger.append(AgentRef::User, created_payload()).unwrap();
        }
        // 模拟崩溃残缺:追加不完整 JSON(无换行)
        let mut f = OpenOptions::new()
            .append(true)
            .open(events_path(&task_dir(&repo, &tid)))
            .unwrap();
        f.write_all(br#"{"seq":2,"ts":"2026"#).unwrap();
        drop(f);

        let (ledger, proj, report) = Ledger::open(&repo, &tid).unwrap();
        assert!(report.torn_tail);
        assert_eq!(report.events_read, 1);
        assert_eq!(proj.status, TaskStatus::Queued);
        // 残缺行被丢弃后,新事件续写 seq=2
        let seq = ledger
            .append(
                AgentRef::User,
                EventPayload::TaskPlanUpdated { plan: "p".into() },
            )
            .unwrap();
        assert_eq!(seq, 2);
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn mid_file_corruption_is_an_error() {
        let repo = temp_repo();
        let tid = TaskId::generate();
        let ledger = Ledger::create(&repo, &tid).unwrap();
        ledger.append(AgentRef::User, created_payload()).unwrap();
        ledger
            .append(
                AgentRef::User,
                EventPayload::TaskPlanUpdated { plan: "p".into() },
            )
            .unwrap();
        drop(ledger);
        // 篡改中间行:破坏 json(非末行 → Corrupt 而非 torn tail)
        let path = events_path(&task_dir(&repo, &tid));
        let bytes = fs::read(&path).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let mut lines: Vec<String> = text.lines().map(String::from).collect();
        lines[0] = "{{{ not json".into();
        fs::write(&path, lines.join("\n") + "\n").unwrap();

        let err = Ledger::open(&repo, &tid).unwrap_err();
        assert!(matches!(err, LedgerError::Corrupt { line: 1, .. }));
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn secrets_never_hit_the_ledger() {
        // spec 40 公共断言:密钥不得出现在账本(信封与载荷均无凭据字段,
        // 这里以 grep 语义断言兜底,防止未来误加字段)
        let repo = temp_repo();
        let tid = TaskId::generate();
        let ledger = Ledger::create(&repo, &tid).unwrap();
        ledger
            .append(
                AgentRef::Coordinator,
                EventPayload::ErrorOccurred {
                    scope: "provider".into(),
                    message: "connect timeout".into(),
                },
            )
            .unwrap();
        drop(ledger);
        let text = fs::read_to_string(events_path(&task_dir(&repo, &tid))).unwrap();
        for needle in ["api_key", "Authorization", "Bearer", "sk-"] {
            assert!(!text.contains(needle), "账本泄漏敏感串: {needle}");
        }
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn tool_call_id_roundtrip_through_ledger() {
        // A5 前置:含 ToolCall 的事件经落盘-重读-投影,call 状态一致
        let repo = temp_repo();
        let tid = TaskId::generate();
        let call = ToolCallId::generate();
        let run = zealous_core::AgentRunId::generate();
        {
            let ledger = Ledger::create(&repo, &tid).unwrap();
            ledger.append(AgentRef::User, created_payload()).unwrap();
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
                    AgentRef::Coordinator,
                    EventPayload::AgentRunStarted {
                        run_id: run.clone(),
                        role: zealous_core::AgentRole::Coordinator,
                    },
                )
                .unwrap();
            ledger
                .append(
                    AgentRef::Coordinator,
                    EventPayload::ToolCallRequested {
                        call_id: call.clone(),
                        run_id: run.clone(),
                        tool: "read_file".into(),
                        args_json: "{\"path\":\"a.rs\"}".into(),
                        risk: zealous_core::RiskTier::T0,
                    },
                )
                .unwrap();
        }
        let (_, proj, _) = Ledger::open(&repo, &tid).unwrap();
        assert_eq!(proj.tool_calls.len(), 1);
        assert_eq!(proj.tool_calls[0].call_id, call.0);
        let _ = fs::remove_dir_all(repo);
    }
}
