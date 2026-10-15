//! 工具层(spec 30):风险分级固定表、路径越界统一判定、内置工具实现。
//!
//! 分级原则就高不就低;分级是代码内固定策略,V1 不暴露配置面。
//! 路径越界判定集中在这里(`resolve_in_repo`),不依赖各工具自觉。

use std::path::{Path, PathBuf};

use zealous_core::RiskTier;

/// 单个工具的执行入参与出参。`args` 由 JSON 反序列化,失败即请求非法。
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub repo_root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub summary: String,
}

#[derive(Debug)]
pub enum ToolError {
    /// 参数缺失/非法(不产生副作用,不算风险事件)
    BadArgs(String),
    /// 执行失败(结果回给模型,任务不因此失败)
    Failed(String),
}

pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    /// 固定分级表(spec 30):T0 只读自动执行;T1 写入审批;T2 高危审批
    fn risk(&self) -> RiskTier;
    /// 审批摘要:T1/T2 挂起时必须完整展示将发生什么
    fn summarize(&self, raw_args: &str) -> String;
    fn execute(&self, raw_args: &str, ctx: &ToolContext) -> Result<ToolOutput, ToolError>;
}

/// 事件账本里展示用的参数摘要截断上限
fn truncate_for_display(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// 路径越界判定(spec 30):仓库根解析后做前缀校验,symlink 解析后的
/// 真实路径同样校验。返回规范化后的仓库内路径。
pub fn resolve_in_repo(repo_root: &Path, raw: &str) -> Result<PathBuf, ToolError> {
    let rel = Path::new(raw);
    if rel.is_absolute() {
        return Err(ToolError::BadArgs(format!(
            "路径越界:必须使用相对路径(仓库内),收到绝对路径 {raw}"
        )));
    }
    let joined = repo_root.join(rel);
    // canonicalize 解析 .. 与 symlink;目标可以不存在(write 场景),此时
    // 对"最近存在的祖先"做 canonicalize,其余部分纯文本拼接后再校验前缀
    let (existing, suffix) = nearest_existing_ancestor(&joined);
    let canon_root = repo_root
        .canonicalize()
        .map_err(|e| ToolError::Failed(format!("仓库根不可解析: {e}")))?;
    let canon = existing
        .canonicalize()
        .map_err(|e| ToolError::Failed(format!("路径不可解析: {e}")))?;
    let full = if suffix.as_os_str().is_empty() {
        canon.clone()
    } else {
        canon.join(&suffix)
    };
    // 真实路径与规范化路径都必须落在仓库根内
    for p in [&full, &canon] {
        if !p.starts_with(&canon_root) {
            return Err(ToolError::BadArgs(format!(
                "路径越界:解析后 {} 不在仓库 {} 内",
                p.display(),
                canon_root.display()
            )));
        }
    }
    Ok(full)
}

/// 沿路径向上找最近的存在祖先;返回(存在的祖先, 相对剩余部分)
fn nearest_existing_ancestor(p: &Path) -> (PathBuf, PathBuf) {
    let mut cur = p.to_path_buf();
    let mut suffix = PathBuf::new();
    loop {
        if cur.exists() {
            return (cur, suffix);
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                suffix = Path::new(name).join(&suffix);
                cur = parent.to_path_buf();
            }
            _ => return (cur, suffix),
        }
    }
}

// ---------- 参数解析小工具 ----------

fn arg_str(raw: &str, key: &str) -> Result<String, ToolError> {
    let v: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| ToolError::BadArgs(format!("bad json: {e}")))?;
    v.get(key)
        .and_then(|x| x.as_str())
        .map(String::from)
        .ok_or_else(|| ToolError::BadArgs(format!("missing string arg: {key}")))
}

// ---------- T0:read_file ----------

struct ReadFile;

impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn risk(&self) -> RiskTier {
        RiskTier::T0
    }
    fn summarize(&self, raw_args: &str) -> String {
        truncate_for_display(raw_args, 200)
    }
    fn execute(&self, raw_args: &str, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = resolve_in_repo(&ctx.repo_root, &arg_str(raw_args, "path")?)?;
        const MAX: u64 = 512 * 1024;
        let meta = std::fs::metadata(&path).map_err(|e| ToolError::Failed(format!("stat: {e}")))?;
        if meta.len() > MAX {
            return Err(ToolError::Failed(format!(
                "file too large: {} bytes (max {MAX})",
                meta.len()
            )));
        }
        let text =
            std::fs::read_to_string(&path).map_err(|e| ToolError::Failed(format!("read: {e}")))?;
        Ok(ToolOutput { summary: text })
    }
}

// ---------- T0:list_dir ----------

struct ListDir;

impl Tool for ListDir {
    fn name(&self) -> &'static str {
        "list_dir"
    }
    fn risk(&self) -> RiskTier {
        RiskTier::T0
    }
    fn summarize(&self, raw_args: &str) -> String {
        truncate_for_display(raw_args, 200)
    }
    fn execute(&self, raw_args: &str, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = resolve_in_repo(&ctx.repo_root, &arg_str(raw_args, "path")?)?;
        let mut names: Vec<String> = Vec::new();
        for entry in
            std::fs::read_dir(&path).map_err(|e| ToolError::Failed(format!("read_dir: {e}")))?
        {
            let entry = entry.map_err(|e| ToolError::Failed(format!("entry: {e}")))?;
            let kind = if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                "d"
            } else {
                "f"
            };
            names.push(format!("{kind} {}", entry.file_name().to_string_lossy()));
        }
        names.sort();
        Ok(ToolOutput {
            summary: names.join("\n"),
        })
    }
}

// ---------- T0:search_text ----------

struct SearchText;

const SEARCH_SKIP_DIRS: &[&str] = &[".git", ".zealous", "target", "node_modules", "dist"];
const SEARCH_MAX_FILE: u64 = 1024 * 1024;
const SEARCH_MAX_HITS: usize = 100;

impl Tool for SearchText {
    fn name(&self) -> &'static str {
        "search_text"
    }
    fn risk(&self) -> RiskTier {
        RiskTier::T0
    }
    fn summarize(&self, raw_args: &str) -> String {
        truncate_for_display(raw_args, 200)
    }
    fn execute(&self, raw_args: &str, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let needle = arg_str(raw_args, "query")?;
        let root = resolve_in_repo(&ctx.repo_root, ".")?;
        let mut hits: Vec<String> = Vec::new();
        walk(&root, &mut |file: &Path| {
            if hits.len() >= SEARCH_MAX_HITS {
                return;
            }
            let Ok(meta) = std::fs::metadata(file) else {
                return;
            };
            if meta.len() > SEARCH_MAX_FILE {
                return;
            }
            let Ok(text) = std::fs::read_to_string(file) else {
                return;
            };
            for (i, line) in text.lines().enumerate() {
                if line.contains(&needle) {
                    hits.push(format!(
                        "{}:{}: {}",
                        file.strip_prefix(&root).unwrap_or(file).display(),
                        i + 1,
                        truncate_for_display(line.trim(), 120)
                    ));
                    if hits.len() >= SEARCH_MAX_HITS {
                        return;
                    }
                }
            }
        });
        Ok(ToolOutput {
            summary: if hits.is_empty() {
                "(no matches)".into()
            } else {
                format!("{} hits\n{}", hits.len(), hits.join("\n"))
            },
        })
    }
}

fn walk(dir: &Path, f: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            if !SEARCH_SKIP_DIRS.contains(&name.as_str()) {
                walk(&path, f);
            }
        } else {
            f(&path);
        }
    }
}

// ---------- T0:git_status ----------

struct GitStatus;

impl Tool for GitStatus {
    fn name(&self) -> &'static str {
        "git_status"
    }
    fn risk(&self) -> RiskTier {
        RiskTier::T0
    }
    fn summarize(&self, raw_args: &str) -> String {
        truncate_for_display(raw_args, 120)
    }
    fn execute(&self, _raw_args: &str, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let out = std::process::Command::new("git")
            .arg("status")
            .arg("--porcelain")
            .current_dir(&ctx.repo_root)
            .output()
            .map_err(|e| ToolError::Failed(format!("spawn git: {e}")))?;
        if !out.status.success() {
            return Err(ToolError::Failed(
                String::from_utf8_lossy(&out.stderr).into(),
            ));
        }
        Ok(ToolOutput {
            summary: String::from_utf8_lossy(&out.stdout).into(),
        })
    }
}

// ---------- T1:write_file ----------

struct WriteFile;

impl Tool for WriteFile {
    fn name(&self) -> &'static str {
        "write_file"
    }
    fn risk(&self) -> RiskTier {
        RiskTier::T1
    }
    fn summarize(&self, raw_args: &str) -> String {
        let path = arg_str(raw_args, "path").unwrap_or_default();
        let content_len = raw_args.len();
        format!("write_file path={path} args_bytes={content_len}")
    }
    fn execute(&self, raw_args: &str, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = resolve_in_repo(&ctx.repo_root, &arg_str(raw_args, "path")?)?;
        let content = arg_str(raw_args, "content")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ToolError::Failed(format!("mkdir: {e}")))?;
        }
        std::fs::write(&path, content).map_err(|e| ToolError::Failed(format!("write: {e}")))?;
        Ok(ToolOutput {
            summary: format!("written {}", path.display()),
        })
    }
}

// ---------- T2:shell ----------

struct Shell;

impl Tool for Shell {
    fn name(&self) -> &'static str {
        "shell"
    }
    fn risk(&self) -> RiskTier {
        RiskTier::T2
    }
    fn summarize(&self, raw_args: &str) -> String {
        format!(
            "shell: {}",
            truncate_for_display(&arg_str(raw_args, "command").unwrap_or_default(), 300)
        )
    }
    fn execute(&self, raw_args: &str, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let command = arg_str(raw_args, "command")?;
        let out = std::process::Command::new("cmd")
            .args(["/C", &command])
            .current_dir(&ctx.repo_root)
            .output()
            .map_err(|e| ToolError::Failed(format!("spawn: {e}")))?;
        let mut text: String = String::from_utf8_lossy(&out.stdout).into();
        let err = String::from_utf8_lossy(&out.stderr);
        if !err.is_empty() {
            text.push_str("\n[stderr] ");
            text.push_str(&err);
        }
        if !out.status.success() {
            return Err(ToolError::Failed(format!(
                "exit={:?}\n{text}",
                out.status.code()
            )));
        }
        Ok(ToolOutput {
            summary: truncate_for_display(text.trim(), 4000),
        })
    }
}

/// 内置注册表:name → 工具。新工具接入必须在此登记并归级(spec 30)。
pub fn builtin_registry() -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(ReadFile),
        Box::new(ListDir),
        Box::new(SearchText),
        Box::new(GitStatus),
        Box::new(WriteFile),
        Box::new(Shell),
    ]
}

/// 未知工具按 T2 处理(就高不就低);执行层据此决定审批或拒绝。
pub fn risk_of(registry: &[Box<dyn Tool>], name: &str) -> Option<RiskTier> {
    registry.iter().find(|t| t.name() == name).map(|t| t.risk())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (PathBuf, ToolContext) {
        let dir = std::env::temp_dir().join(format!(
            "zealous-tools-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/main.rs"),
            "fn main() { println!(\"zealous\"); }\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        (dir.clone(), ToolContext { repo_root: dir })
    }

    #[test]
    fn read_file_and_traversal_rejected() {
        let (_dir, ctx) = fixture();
        let read = ReadFile;
        let ok = read.execute(r#"{"path":"src/main.rs"}"#, &ctx).unwrap();
        assert!(ok.summary.contains("zealous"));

        // 相对路径逃逸
        let err = read
            .execute(r#"{"path":"../outside.txt"}"#, &ctx)
            .unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(msg) if msg.contains("越界")));

        // 绝对路径直接拒绝
        let err = read
            .execute(r#"{"path":"C:/Windows/win.ini"}"#, &ctx)
            .unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(msg) if msg.contains("越界")));
    }

    #[test]
    fn write_creates_and_respects_guard() {
        let (dir, ctx) = fixture();
        let write = WriteFile;
        write
            .execute(r#"{"path":"docs/new/note.md","content":"hello"}"#, &ctx)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("docs/new/note.md")).unwrap(),
            "hello"
        );
        let err = write
            .execute(r#"{"path":"../evil.txt","content":"x"}"#, &ctx)
            .unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(msg) if msg.contains("越界")));
    }

    #[test]
    fn search_and_list_and_risk_table() {
        let (_dir, ctx) = fixture();
        let hits = SearchText.execute(r#"{"query":"zealous"}"#, &ctx).unwrap();
        // 路径分隔符跨平台:只断言文件名与行号
        assert!(hits.summary.contains("main.rs:1"), "{}", hits.summary);
        let reg = builtin_registry();
        assert_eq!(risk_of(&reg, "read_file"), Some(RiskTier::T0));
        assert_eq!(risk_of(&reg, "write_file"), Some(RiskTier::T1));
        assert_eq!(risk_of(&reg, "shell"), Some(RiskTier::T2));
        assert_eq!(risk_of(&reg, "nope"), None);
    }
}
