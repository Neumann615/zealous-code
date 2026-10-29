//! AI-assisted merge resolution Alpha (spec 160).
//!
//! The resolver is deliberately separate from the task orchestrator: it may
//! read only the known unresolved files and has no tool execution capability.

use crate::ledger::Ledger;
use crate::provider::{ChatMessage, ChatRequest, Provider};
use std::path::{Path, PathBuf};
use zealous_core::{AgentRef, ApprovalDecision, EventPayload, ResolutionId, TaskStatus};

pub const MAX_RESOLUTION_FILES: usize = 8;
pub const MAX_FILE_CONTEXT_BYTES: usize = 256 * 1024;
pub const MAX_TOTAL_CONTEXT_BYTES: usize = 768 * 1024;
pub const MAX_RESOLUTION_REPAIRS: u32 = 2;

#[derive(Debug)]
pub enum ResolutionError {
    NotConflicted,
    NoUnresolvedFiles,
    PendingProposal,
    ProposalNotFound,
    InvalidProposal(String),
    TooManyFiles { count: usize },
    FileTooLarge { path: String, bytes: usize },
    TotalTooLarge { bytes: usize },
    Io(String),
    Git(String),
    Provider(String),
    Ledger(String),
}

impl std::fmt::Display for ResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConflicted => write!(f, "merge workspace 不处于 conflicted 状态"),
            Self::NoUnresolvedFiles => write!(f, "没有 unresolved conflict files"),
            Self::PendingProposal => write!(f, "已有 pending resolution proposal"),
            Self::ProposalNotFound => write!(f, "resolution proposal 不存在或已过期"),
            Self::InvalidProposal(s) => write!(f, "resolution proposal invalid: {s}"),
            Self::TooManyFiles { count } => {
                write!(
                    f,
                    "unresolved files {count} exceeds limit {MAX_RESOLUTION_FILES}"
                )
            }
            Self::FileTooLarge { path, bytes } => write!(
                f,
                "conflict file {path} context {bytes} bytes exceeds limit {MAX_FILE_CONTEXT_BYTES}"
            ),
            Self::TotalTooLarge { bytes } => write!(
                f,
                "total conflict context {bytes} bytes exceeds limit {MAX_TOTAL_CONTEXT_BYTES}"
            ),
            Self::Io(s) => write!(f, "io: {s}"),
            Self::Git(s) => write!(f, "git: {s}"),
            Self::Provider(s) => write!(f, "provider: {s}"),
            Self::Ledger(s) => write!(f, "ledger: {s}"),
        }
    }
}

impl std::error::Error for ResolutionError {}

fn git(dir: &Path, args: &[&str]) -> Result<String, ResolutionError> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| ResolutionError::Git(format!("spawn git: {e}")))?;
    if !out.status.success() {
        return Err(ResolutionError::Git(
            String::from_utf8_lossy(&out.stderr).into(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into())
}

fn git_optional(dir: &Path, args: &[&str]) -> Result<String, ResolutionError> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| ResolutionError::Git(format!("spawn git: {e}")))?;
    if !out.status.success() {
        return Ok("missing".into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into())
}

fn append(ledger: &Ledger, payload: EventPayload) -> Result<(), ResolutionError> {
    ledger
        .append(AgentRef::Coordinator, payload)
        .map(|_| ())
        .map_err(|e| ResolutionError::Ledger(e.to_string()))
}

fn error(ledger: &Ledger, message: String) -> ResolutionError {
    let returned = ResolutionError::InvalidProposal(message.clone());
    let _ = append(
        ledger,
        EventPayload::ErrorOccurred {
            scope: "merge_resolution".into(),
            message,
        },
    );
    returned
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictContext {
    pub path: String,
    pub base: String,
    pub ours: String,
    pub theirs: String,
    pub working: String,
}

pub fn unresolved_files(workspace: &Path) -> Result<Vec<String>, ResolutionError> {
    let output = git(workspace, &["diff", "--name-only", "--diff-filter=U"])?;
    let mut files = Vec::new();
    for line in output.lines() {
        let path = line.trim().replace('\\', "/");
        if !path.is_empty() && !files.contains(&path) {
            files.push(path);
        }
    }
    Ok(files)
}

fn safe_workspace_path(workspace: &Path, path: &str) -> Result<PathBuf, ResolutionError> {
    if path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains("..")
        || Path::new(path).is_absolute()
    {
        return Err(ResolutionError::InvalidProposal(format!(
            "illegal path {path:?}"
        )));
    }
    let resolved = workspace.join(path);
    if !resolved.starts_with(workspace) {
        return Err(ResolutionError::InvalidProposal(format!(
            "path escapes workspace: {path}"
        )));
    }
    Ok(resolved)
}

pub fn collect_contexts(workspace: &Path) -> Result<Vec<ConflictContext>, ResolutionError> {
    let files = unresolved_files(workspace)?;
    if files.is_empty() {
        return Err(ResolutionError::NoUnresolvedFiles);
    }
    if files.len() > MAX_RESOLUTION_FILES {
        return Err(ResolutionError::TooManyFiles { count: files.len() });
    }
    let mut contexts = Vec::new();
    let mut total = 0usize;
    for path in files {
        let file = safe_workspace_path(workspace, &path)?;
        let working = std::fs::read_to_string(&file)
            .map_err(|e| ResolutionError::Io(format!("read working {path}: {e}")))?;
        let base = git_optional(workspace, &["show", &format!(":1:{path}")])?;
        let ours = git_optional(workspace, &["show", &format!(":2:{path}")])?;
        let theirs = git_optional(workspace, &["show", &format!(":3:{path}")])?;
        let bytes = base.len() + ours.len() + theirs.len() + working.len();
        if bytes > MAX_FILE_CONTEXT_BYTES {
            return Err(ResolutionError::FileTooLarge { path, bytes });
        }
        total += bytes;
        contexts.push(ConflictContext {
            path,
            base,
            ours,
            theirs,
            working,
        });
    }
    if total > MAX_TOTAL_CONTEXT_BYTES {
        return Err(ResolutionError::TotalTooLarge { bytes: total });
    }
    Ok(contexts)
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
enum ProposalMode {
    #[serde(rename = "resolved")]
    Resolved,
    #[serde(rename = "manual")]
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposalFileDto {
    path: String,
    mode: ProposalMode,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    rationale: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposalDto {
    action: String,
    summary: String,
    files: Vec<ProposalFileDto>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalFile {
    Resolved {
        path: String,
        content: String,
        rationale: String,
    },
    Manual {
        path: String,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub resolution_id: ResolutionId,
    pub summary: String,
    pub files: Vec<ProposalFile>,
}

impl Proposal {
    pub fn resolved_files(&self) -> Vec<String> {
        self.files
            .iter()
            .filter_map(|f| match f {
                ProposalFile::Resolved { path, .. } => Some(path.clone()),
                ProposalFile::Manual { .. } => None,
            })
            .collect()
    }

    pub fn manual_files(&self) -> Vec<String> {
        self.files
            .iter()
            .filter_map(|f| match f {
                ProposalFile::Resolved { .. } => None,
                ProposalFile::Manual { path, .. } => Some(path.clone()),
            })
            .collect()
    }

    pub fn all_files(&self) -> Vec<String> {
        self.files
            .iter()
            .map(|f| match f {
                ProposalFile::Resolved { path, .. } | ProposalFile::Manual { path, .. } => {
                    path.clone()
                }
            })
            .collect()
    }
}

fn parse_body(raw: &str, contexts: &[ConflictContext]) -> Result<ProposalDto, String> {
    let trimmed = raw.trim();
    let body = if let Some(rest) = trimmed.strip_prefix("```") {
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        let rest = rest.trim_start_matches(['\r', '\n']);
        let end = rest.rfind("```").ok_or("fenced block 未闭合")?;
        rest[..end].trim()
    } else {
        trimmed
    };
    if !body.starts_with('{') || !body.ends_with('}') {
        return Err("输出必须是恰好一个 JSON object".into());
    }
    let dto: ProposalDto = serde_json::from_str(body).map_err(|e| format!("json 解析失败: {e}"))?;
    if dto.action != "resolve" {
        return Err(format!("action 必须为 resolve,实际 {:?}", dto.action));
    }
    if dto.summary.trim().is_empty() {
        return Err("summary 不能为空".into());
    }
    let expected: Vec<String> = contexts.iter().map(|c| c.path.clone()).collect();
    let actual: Vec<String> = dto.files.iter().map(|f| f.path.clone()).collect();
    if dto.files.len() != expected.len() {
        return Err(format!(
            "files 数量必须为 {},实际 {}",
            expected.len(),
            dto.files.len()
        ));
    }
    if actual != expected {
        return Err(format!(
            "files path 集合不匹配: expected {expected:?}, actual {actual:?}"
        ));
    }
    for file in &dto.files {
        match file.mode {
            ProposalMode::Resolved => {
                let content = file
                    .content
                    .as_ref()
                    .ok_or_else(|| format!("resolved 文件 {} 缺少 content", file.path))?;
                if file
                    .rationale
                    .as_deref()
                    .unwrap_or_default()
                    .trim()
                    .is_empty()
                {
                    return Err(format!("resolved 文件 {} 缺少 rationale", file.path));
                }
                for marker in ["<<<<<<<", "=======", ">>>>>>>"] {
                    if content.contains(marker) {
                        return Err(format!(
                            "resolved 文件 {} content 含 conflict marker {marker:?}",
                            file.path
                        ));
                    }
                }
                let working = &contexts
                    .iter()
                    .find(|c| c.path == file.path)
                    .expect("path checked")
                    .working;
                if content == working {
                    return Err(format!(
                        "resolved 文件 {} content 与 working conflict 内容相同",
                        file.path
                    ));
                }
            }
            ProposalMode::Manual => {
                if file.content.is_some() || file.rationale.is_some() {
                    return Err(format!(
                        "manual 文件 {} 不允许 content/rationale",
                        file.path
                    ));
                }
                if file.reason.as_deref().unwrap_or_default().trim().is_empty() {
                    return Err(format!("manual 文件 {} 缺少 reason", file.path));
                }
            }
        }
    }
    Ok(dto)
}

fn to_dto(raw: &str, contexts: &[ConflictContext]) -> Result<Proposal, String> {
    let dto = parse_body(raw, contexts)?;
    let files = dto
        .files
        .into_iter()
        .map(|file| match file.mode {
            ProposalMode::Resolved => ProposalFile::Resolved {
                path: file.path,
                content: file.content.expect("validated"),
                rationale: file.rationale.expect("validated"),
            },
            ProposalMode::Manual => ProposalFile::Manual {
                path: file.path,
                reason: file.reason.expect("validated"),
            },
        })
        .collect();
    Ok(Proposal {
        resolution_id: ResolutionId::generate(),
        summary: dto.summary,
        files,
    })
}

fn prompt(contexts: &[ConflictContext]) -> String {
    let schema = r#"{"action":"resolve","summary":"...","files":[{"path":"...","mode":"resolved","content":"complete file","rationale":"..."},{"path":"...","mode":"manual","reason":"..."}]}"#;
    let mut out = String::new();
    out.push_str(
        "You are resolving Git merge conflicts. Return exactly one JSON object and no prose.\n",
    );
    out.push_str("Rules: files must exactly match the unresolved paths; use mode=resolved with complete content and rationale, or mode=manual with reason; do not output diffs; do not retain conflict markers; do not choose ours/theirs blindly.\n");
    out.push_str("Schema: ");
    out.push_str(schema);
    out.push('\n');
    for c in contexts {
        out.push_str(&format!(
            "\n=== FILE {} ===\nBASE:\n{}\nOURS:\n{}\nTHEIRS:\n{}\nWORKING CONFLICT:\n{}\n",
            c.path, c.base, c.ours, c.theirs, c.working
        ));
    }
    out
}

fn repair_prompt(previous: &str, message: &str) -> String {
    format!(
        "Your previous resolution output was invalid.\nError: {message}\nPrevious output:\n{previous}\n\nReturn exactly one corrected JSON object matching the schema. No prose."
    )
}

pub fn propose(
    workspace: &Path,
    ledger: &Ledger,
    provider: &dyn Provider,
    model: &str,
) -> Result<Proposal, ResolutionError> {
    let proj = ledger
        .fold_now()
        .map_err(|e| ResolutionError::Ledger(e.to_string()))?;
    if proj.status != TaskStatus::Completed {
        return Err(ResolutionError::NotConflicted);
    }
    let merge_ws = proj
        .merge_workspace
        .as_ref()
        .filter(|w| w.status == "conflicted")
        .ok_or(ResolutionError::NotConflicted)?;
    if proj
        .merge_resolution
        .as_ref()
        .is_some_and(|r| r.status == "proposed")
    {
        return Err(ResolutionError::PendingProposal);
    }
    let contexts = collect_contexts(Path::new(&merge_ws.path))?;
    let files = contexts.iter().map(|c| c.path.clone()).collect();
    let resolution_id = ResolutionId::generate();
    append(
        ledger,
        EventPayload::MergeResolutionStarted {
            resolution_id: resolution_id.clone(),
            files,
        },
    )?;
    let _ = workspace;

    let mut messages = vec![ChatMessage::user(prompt(&contexts))];
    let mut parsed = None;
    for attempt in 0..=MAX_RESOLUTION_REPAIRS {
        let request = ChatRequest {
            system: Some("You are a strict JSON merge-resolution assistant.".into()),
            messages: messages.clone(),
        };
        let response = provider
            .complete(model, &request)
            .map_err(|e| error(ledger, e.to_string()))?;
        let last_raw = response.text;
        match to_dto(&last_raw, &contexts) {
            Ok(mut proposal) => {
                proposal.resolution_id = resolution_id.clone();
                parsed = Some(proposal);
                break;
            }
            Err(message) if attempt < MAX_RESOLUTION_REPAIRS => {
                messages.push(ChatMessage::assistant(last_raw.clone()));
                messages.push(ChatMessage::user(repair_prompt(&last_raw, &message)));
            }
            Err(message) => {
                return Err(error(ledger, format!("repair exhausted: {message}")));
            }
        }
    }
    let proposal = parsed.expect("success path sets proposal");
    append(
        ledger,
        EventPayload::MergeResolutionProposed {
            resolution_id: resolution_id.clone(),
            summary: proposal.summary.clone(),
            resolved_files: proposal.resolved_files(),
            manual_files: proposal.manual_files(),
        },
    )?;
    Ok(proposal)
}

fn workspace_from_ledger(
    ledger: &Ledger,
) -> Result<(PathBuf, zealous_core::MergeResolutionView), ResolutionError> {
    let proj = ledger
        .fold_now()
        .map_err(|e| ResolutionError::Ledger(e.to_string()))?;
    let merge_ws = proj
        .merge_workspace
        .as_ref()
        .filter(|w| w.status == "conflicted")
        .ok_or(ResolutionError::NotConflicted)?;
    let resolution = proj
        .merge_resolution
        .as_ref()
        .filter(|r| r.status == "proposed")
        .ok_or(ResolutionError::ProposalNotFound)?;
    Ok((PathBuf::from(&merge_ws.path), resolution.clone()))
}

pub fn reject(
    ledger: &Ledger,
    resolution_id: &ResolutionId,
    reason: Option<String>,
) -> Result<(), ResolutionError> {
    let proj = ledger
        .fold_now()
        .map_err(|e| ResolutionError::Ledger(e.to_string()))?;
    let resolution = proj
        .merge_resolution
        .as_ref()
        .filter(|r| r.resolution_id == *resolution_id && r.status == "proposed")
        .ok_or(ResolutionError::ProposalNotFound)?;
    let _ = resolution;
    append(
        ledger,
        EventPayload::MergeResolutionDecided {
            resolution_id: resolution_id.clone(),
            decision: ApprovalDecision::Reject,
            reason,
        },
    )
    .map_err(|e| ResolutionError::Ledger(e.to_string()))
}

pub fn approve(
    ledger: &Ledger,
    resolution_id: &ResolutionId,
) -> Result<(Vec<String>, Vec<String>), ResolutionError> {
    let (workspace, view) = workspace_from_ledger(ledger)?;
    if view.resolution_id != *resolution_id {
        return Err(ResolutionError::ProposalNotFound);
    }
    let current = unresolved_files(&workspace)?;
    let mut expected = view.resolved_files.clone();
    expected.extend(view.manual_files.clone());
    expected.sort();
    if current != expected {
        return Err(ResolutionError::InvalidProposal(format!(
            "unresolved files changed: expected {expected:?}, actual {current:?}"
        )));
    }
    // Proposal content is not persisted in the ledger. The caller passes the
    // user-visible object into `apply_proposal` after revalidating paths.
    Err(ResolutionError::InvalidProposal(
        "internal proposal content is not persisted; use apply_proposal".into(),
    ))
}

pub fn apply_proposal(
    workspace: &Path,
    ledger: &Ledger,
    proposal: &Proposal,
) -> Result<(Vec<String>, Vec<String>), ResolutionError> {
    let proj = ledger
        .fold_now()
        .map_err(|e| ResolutionError::Ledger(e.to_string()))?;
    let merge_ws = proj
        .merge_workspace
        .as_ref()
        .filter(|w| w.status == "conflicted")
        .ok_or(ResolutionError::NotConflicted)?;
    if Path::new(&merge_ws.path) != workspace {
        return Err(ResolutionError::NotConflicted);
    }
    let resolution = proj
        .merge_resolution
        .as_ref()
        .filter(|r| r.resolution_id == proposal.resolution_id && r.status == "proposed")
        .ok_or(ResolutionError::ProposalNotFound)?;
    let mut expected = resolution.resolved_files.clone();
    expected.extend(resolution.manual_files.clone());
    expected.sort();
    let current = unresolved_files(workspace)?;
    if current != expected {
        return Err(ResolutionError::InvalidProposal(format!(
            "unresolved files changed: expected {expected:?}, actual {current:?}"
        )));
    }
    let mut staged = Vec::new();
    for file in &proposal.files {
        if let ProposalFile::Resolved { path, content, .. } = file {
            let target = safe_workspace_path(workspace, path)?;
            if content.contains("<<<<<<<")
                || content.contains("=======")
                || content.contains(">>>>>>>")
            {
                return Err(ResolutionError::InvalidProposal(format!(
                    "{path} content contains conflict markers"
                )));
            }
            std::fs::write(&target, content.as_bytes())
                .map_err(|e| ResolutionError::Io(format!("write {path}: {e}")))?;
            git(workspace, &["add", path])?;
            staged.push(path.clone());
        }
    }
    append(
        ledger,
        EventPayload::MergeResolutionDecided {
            resolution_id: proposal.resolution_id.clone(),
            decision: ApprovalDecision::Approve,
            reason: None,
        },
    )?;
    append(
        ledger,
        EventPayload::MergeResolutionApplied {
            resolution_id: proposal.resolution_id.clone(),
            staged_files: staged.clone(),
        },
    )?;
    let manual = proposal.manual_files();
    Ok((staged, manual))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(path: &str, working: &str) -> ConflictContext {
        ConflictContext {
            path: path.into(),
            base: "base".into(),
            ours: "ours".into(),
            theirs: "theirs".into(),
            working: working.into(),
        }
    }

    #[test]
    fn valid_resolved_and_manual_proposal_parses() {
        let contexts = vec![context("a.md", "<<<<<<<\n"), context("b.rs", "<<<<<<<\n")];
        let raw = serde_json::json!({
            "action": "resolve",
            "summary": "semantic merge",
            "files": [
                {"path":"a.md","mode":"resolved","content":"resolved\n","rationale":"combine"},
                {"path":"b.rs","mode":"manual","reason":"too complex"}
            ]
        })
        .to_string();
        let proposal = to_dto(&raw, &contexts).unwrap();
        assert_eq!(proposal.resolved_files(), vec!["a.md".to_string()]);
        assert_eq!(proposal.manual_files(), vec!["b.rs".to_string()]);
    }

    #[test]
    fn proposal_rejects_missing_extra_and_unknown_paths() {
        let contexts = vec![context("a.md", "conflict")];
        let base = serde_json::json!({
            "action":"resolve","summary":"s",
            "files":[{"path":"a.md","mode":"manual","reason":"manual"}]
        })
        .to_string();
        assert!(to_dto(&base, &contexts).is_ok());
        let missing = serde_json::json!({"action":"resolve","summary":"s","files":[]}).to_string();
        assert!(to_dto(&missing, &contexts).is_err());
        let extra = serde_json::json!({
        "action":"resolve","summary":"s",
        "files":[
            {"path":"a.md","mode":"manual","reason":"x"},
            {"path":"z","mode":"manual","reason":"x"}
        ]})
        .to_string();
        assert!(to_dto(&extra, &contexts).is_err());
        let unknown_path = serde_json::json!({
            "action":"resolve","summary":"s",
            "files":[{"path":"z","mode":"manual","reason":"x"}]
        })
        .to_string();
        assert!(to_dto(&unknown_path, &contexts).is_err());
    }

    #[test]
    fn proposal_rejects_markers_same_content_and_unknown_fields() {
        let contexts = vec![context("a.md", "working")];
        let marker = serde_json::json!({
            "action":"resolve","summary":"s",
            "files":[{"path":"a.md","mode":"resolved","content":"<<<<<<<\n","rationale":"x"}]
        })
        .to_string();
        assert!(to_dto(&marker, &contexts).is_err());
        let same = serde_json::json!({
            "action":"resolve","summary":"s",
            "files":[{"path":"a.md","mode":"resolved","content":"working","rationale":"x"}]
        })
        .to_string();
        assert!(to_dto(&same, &contexts).is_err());
        let unknown = r#"{"action":"resolve","summary":"s","files":[{"path":"a.md","mode":"manual","reason":"x","extra":1}]}"#;
        assert!(to_dto(unknown, &contexts).is_err());
    }

    #[test]
    fn repair_round_recovers_invalid_json() {
        let contexts = vec![context("a.md", "conflict")];
        let good = serde_json::json!({
            "action":"resolve","summary":"s",
            "files":[{"path":"a.md","mode":"manual","reason":"manual"}]
        })
        .to_string();
        let provider =
            crate::provider::MockProvider::scripted(vec![Ok("not json".into()), Ok(good)]);
        let dir = std::env::temp_dir();
        let ledger = Ledger::create(&dir, &zealous_core::TaskId::generate()).unwrap();
        // propose needs a real conflicted merge workspace; here only provider
        // repair semantics are tested through direct calls.
        let mut messages = vec![ChatMessage::user(prompt(&contexts))];
        let mut result = None;
        for _ in 0..=MAX_RESOLUTION_REPAIRS {
            let req = ChatRequest {
                system: None,
                messages: messages.clone(),
            };
            let text = provider.complete("mock", &req).unwrap().text;
            if let Ok(mut p) = to_dto(&text, &contexts) {
                p.resolution_id = ResolutionId::generate();
                result = Some(p);
                break;
            }
            messages.push(ChatMessage::assistant(text));
            messages.push(ChatMessage::user(repair_prompt("invalid", "invalid json")));
        }
        assert!(result.is_some());
        let _ = ledger;
    }
}
