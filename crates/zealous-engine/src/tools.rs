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

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolError::BadArgs(m) => f.write_str(m),
            ToolError::Failed(m) => f.write_str(m),
        }
    }
}

pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    /// 固定分级表(spec 30):T0 只读自动执行;T1 写入审批;T2 高危审批
    fn risk(&self) -> RiskTier;
    /// 参数模板(稳定契约,进模型 manifest,spec 60 §5)
    fn args_template(&self) -> &'static str;
    /// 一句话语义说明(进模型 manifest)
    fn description(&self) -> &'static str;
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

fn args_object(raw: &str) -> Result<serde_json::Value, ToolError> {
    serde_json::from_str(raw).map_err(|e| ToolError::BadArgs(format!("bad json: {e}")))
}

fn opt_str(raw: &str, key: &str) -> Result<Option<String>, ToolError> {
    Ok(args_object(raw)?
        .get(key)
        .and_then(|x| x.as_str())
        .map(String::from))
}

fn opt_u64(raw: &str, key: &str) -> Result<Option<u64>, ToolError> {
    Ok(args_object(raw)?.get(key).and_then(|x| x.as_u64()))
}

fn opt_bool(raw: &str, key: &str) -> Result<Option<bool>, ToolError> {
    Ok(args_object(raw)?.get(key).and_then(|x| x.as_bool()))
}

/// diff/长输出统一上限:64 KiB,超限按字符边界截断并显式标记(spec 80 §2)
const GIT_OUTPUT_CAP: usize = 64 * 1024;

fn cap_output(s: String) -> String {
    if s.len() <= GIT_OUTPUT_CAP {
        return s;
    }
    let mut cut = GIT_OUTPUT_CAP;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}\n[truncated: 输出超过 {} 字节,已截断]",
        &s[..cut],
        GIT_OUTPUT_CAP
    )
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
    fn args_template(&self) -> &'static str {
        "{\"path\": \"relative/path\"}"
    }
    fn description(&self) -> &'static str {
        "读取仓库内文本文件(<=512KB),返回文件全文"
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
    fn args_template(&self) -> &'static str {
        "{\"path\": \"relative/dir\"}"
    }
    fn description(&self) -> &'static str {
        "列出目录条目,d 前缀为目录、f 前缀为文件"
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
    fn args_template(&self) -> &'static str {
        "{\"query\": \"substring\"}"
    }
    fn description(&self) -> &'static str {
        "在仓库文本文件中做子串搜索(跳过 .git/target 等),返回 file:line: 片段"
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
    fn args_template(&self) -> &'static str {
        "{}"
    }
    fn description(&self) -> &'static str {
        "返回 git status --porcelain 输出(只读)"
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

// ---------- T0:git_log ----------

struct GitLog;

impl Tool for GitLog {
    fn name(&self) -> &'static str {
        "git_log"
    }
    fn risk(&self) -> RiskTier {
        RiskTier::T0
    }
    fn args_template(&self) -> &'static str {
        "{\"limit\": 20, \"path\": \"optional/path\"}"
    }
    fn description(&self) -> &'static str {
        "查看提交历史(默认 20 条,上限 100,可限定路径)"
    }
    fn summarize(&self, raw_args: &str) -> String {
        truncate_for_display(raw_args, 200)
    }
    fn execute(&self, raw_args: &str, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        // spec 80 §2:默认 20 条,上限 100;可限定路径(仓库内守卫同其他工具)
        let limit = opt_u64(raw_args, "limit")?.unwrap_or(20).clamp(1, 100);
        let mut args = vec!["log".to_string(), "-n".to_string(), limit.to_string()];
        if let Some(p) = opt_str(raw_args, "path")? {
            let canon = resolve_in_repo(&ctx.repo_root, &p)?;
            args.push("--".into());
            args.push(rel_to_repo(&ctx.repo_root, &canon));
        }
        let out = std::process::Command::new("git")
            .args(&args)
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

// ---------- T0:git_diff ----------

struct GitDiff;

impl Tool for GitDiff {
    fn name(&self) -> &'static str {
        "git_diff"
    }
    fn risk(&self) -> RiskTier {
        RiskTier::T0
    }
    fn args_template(&self) -> &'static str {
        "{\"path\": \"optional/path\", \"staged\": false}"
    }
    fn description(&self) -> &'static str {
        "查看未提交改动(工作区或 staged),输出上限 64KiB"
    }
    fn summarize(&self, raw_args: &str) -> String {
        truncate_for_display(raw_args, 200)
    }
    fn execute(&self, raw_args: &str, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let staged = opt_bool(raw_args, "staged")?.unwrap_or(false);
        let mut args = vec!["diff".to_string()];
        if staged {
            args.push("--staged".into());
        }
        if let Some(p) = opt_str(raw_args, "path")? {
            let canon = resolve_in_repo(&ctx.repo_root, &p)?;
            args.push("--".into());
            args.push(rel_to_repo(&ctx.repo_root, &canon));
        }
        let out = std::process::Command::new("git")
            .args(&args)
            .current_dir(&ctx.repo_root)
            .output()
            .map_err(|e| ToolError::Failed(format!("spawn git: {e}")))?;
        if !out.status.success() {
            return Err(ToolError::Failed(
                String::from_utf8_lossy(&out.stderr).into(),
            ));
        }
        Ok(ToolOutput {
            summary: cap_output(String::from_utf8_lossy(&out.stdout).into()),
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
    fn args_template(&self) -> &'static str {
        "{\"path\": \"relative/path\", \"content\": \"full content\"}"
    }
    fn description(&self) -> &'static str {
        "写入主工作区文件(T1:需审批;要求工作区干净;成功后自动 commit 作为回退点)"
    }
    fn summarize(&self, raw_args: &str) -> String {
        let path = arg_str(raw_args, "path").unwrap_or_default();
        let content_len = raw_args.len();
        format!("write_file path={path} args_bytes={content_len}")
    }
    fn execute(&self, raw_args: &str, ctx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let path = resolve_in_repo(&ctx.repo_root, &arg_str(raw_args, "path")?)?;
        let content = arg_str(raw_args, "content")?;
        assert_clean_worktree(&ctx.repo_root)?;
        apply_transactional(&ctx.repo_root, &path, &content)?;
        Ok(ToolOutput {
            summary: format!("written {} (committed as rollback point)", path.display()),
        })
    }
}

/// apply 事务(spec 70 §4):临时文件 → 原子替换 → git add → git commit;
/// git 步骤失败则回滚文件与 index,ok=true 仅在提交成功后产生。
/// 崩溃窗口(替换与 commit 之间)如实声明于 spec 70 §4,不做虚假事务保证。
fn apply_transactional(repo_root: &Path, path: &Path, content: &str) -> Result<(), ToolError> {
    // 回滚素材:原内容(新文件则为 None)
    let original = std::fs::read(path).ok();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ToolError::Failed(format!("mkdir: {e}")))?;
    }
    // 同目录临时文件 + 原子替换(跨盘 rename 会失败,同目录保证同卷)
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| ToolError::BadArgs("path 无文件名".into()))?;
    let tmp = path.with_file_name(format!(".{file_name}.zealous-tmp"));
    std::fs::write(&tmp, content).map_err(|e| ToolError::Failed(format!("write tmp: {e}")))?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(ToolError::Failed(format!("atomic replace: {e}")));
    }

    match commit_applied(repo_root, path) {
        Ok(summary) => {
            let _ = summary;
            Ok(())
        }
        Err(commit_err) => {
            // 回滚:恢复原内容或删除新文件;清理 staged index
            let restore = match &original {
                Some(bytes) => std::fs::write(path, bytes),
                None => std::fs::remove_file(path),
            };
            let rel = rel_to_repo(repo_root, path);
            let reset = std::process::Command::new("git")
                .args(["reset", "-q", "HEAD", "--", &rel])
                .current_dir(repo_root)
                .output();
            match (restore, reset) {
                (Ok(()), Ok(out)) if out.status.success() => Err(ToolError::Failed(format!(
                    "apply 后 git 提交失败,已回滚文件与 index。原因: {commit_err}"
                ))),
                (restore_res, reset_res) => Err(ToolError::Failed(format!(
                    "apply 失败且回滚未完全成功,需要人工处理: commit 原因={commit_err}; \
                     文件恢复={restore_res:?}; index 清理={reset_res:?}"
                ))),
            }
        }
    }
}

/// file 相对仓库根的路径串(git 命令用;file 是 canonicalize 产物,
/// Windows 带 \\?\ 前缀,须对规范化后的根求 strip_prefix)
fn rel_to_repo(repo_root: &Path, file: &Path) -> String {
    let canon_root = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    file.strip_prefix(&canon_root)
        .or_else(|_| file.strip_prefix(repo_root))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| file.display().to_string())
}

/// 把刚写入的文件作为独立回退点提交;作者署名 zealous,与用户提交区分
fn commit_applied(repo_root: &Path, file: &Path) -> Result<String, ToolError> {
    let rel = rel_to_repo(repo_root, file);
    let add = std::process::Command::new("git")
        .args(["add", "--", &rel])
        .current_dir(repo_root)
        .output()
        .map_err(|e| ToolError::Failed(format!("spawn git add: {e}")))?;
    if !add.status.success() {
        return Err(ToolError::Failed(format!(
            "git add 失败: {}",
            String::from_utf8_lossy(&add.stderr)
        )));
    }
    let commit = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=zealous",
            "-c",
            "user.email=zealous@local",
            "commit",
            "-q",
            "-m",
            &format!("zealous: apply {rel}"),
        ])
        .current_dir(repo_root)
        .output()
        .map_err(|e| ToolError::Failed(format!("spawn git commit: {e}")))?;
    if !commit.status.success() {
        return Err(ToolError::Failed(format!(
            "git commit 失败: {}",
            String::from_utf8_lossy(&commit.stderr)
        )));
    }
    let hash = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(repo_root)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    Ok(hash)
}

/// 主工作区写入保护(spec 60 §4):apply 前工作区必须干净,
/// Git commit/status 是安全回退边界(spec 00 硬性前提)。
fn assert_clean_worktree(repo_root: &Path) -> Result<(), ToolError> {
    let out = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo_root)
        .output()
        .map_err(|e| ToolError::Failed(format!("spawn git: {e}")))?;
    if !out.status.success() {
        return Err(ToolError::Failed(format!(
            "git status 失败: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let dirty = String::from_utf8_lossy(&out.stdout);
    // `.zealous/` 是本工具自己的运行时数据(账本/提案),不算用户的脏工作区
    let user_dirty: Vec<&str> = dirty
        .lines()
        .filter(|l| {
            let path = l.get(3..).unwrap_or("").trim_start_matches('"');
            !path.starts_with(".zealous/")
        })
        .collect();
    if !user_dirty.is_empty() {
        let preview: String = user_dirty
            .iter()
            .take(5)
            .map(|l| format!("\n  {l}"))
            .collect();
        return Err(ToolError::Failed(format!(
            "Git 工作区有未提交改动,拒绝写入主工作区。请先 commit/stash:{preview}"
        )));
    }
    Ok(())
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
    fn args_template(&self) -> &'static str {
        "{\"command\": \"command line\"}"
    }
    fn description(&self) -> &'static str {
        "经 cmd /C 执行命令(T2:需审批;完整命令会展示给用户)"
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
        Box::new(GitLog),
        Box::new(GitDiff),
        Box::new(WriteFile),
        Box::new(Shell),
    ]
}

/// 工具 manifest(spec 60 §5):模型提示的稳定契约。
/// 由注册表生成,与实现天然一致;`only` 过滤风险级(如 Worker 仅 T0)。
pub fn tool_manifest(only: Option<RiskTier>) -> String {
    let mut out = String::from(
        "## 可用工具
",
    );
    for t in builtin_registry() {
        if let Some(risk) = only {
            if t.risk() != risk {
                continue;
            }
        }
        let line = format!(
            "- `{}` [{}] 参数: {} — {}\n",
            t.name(),
            t.risk(),
            t.args_template(),
            t.description()
        );
        out.push_str(&line);
    }
    out
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
        // 脏工作区保护要求夹具有初始 commit(spec 60 §4)
        let _ = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .output();
        commit_all(&dir);
        (dir.clone(), ToolContext { repo_root: dir })
    }

    fn commit_all(dir: &Path) {
        let _ = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .output();
        let out = std::process::Command::new("git")
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
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "commit failed: {:?}", out.stderr);
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

    #[test]
    fn dirty_worktree_blocks_write() {
        let (dir, ctx) = fixture();
        // 提交后制造脏状态:未跟踪文件让 porcelain 非空
        std::fs::write(dir.join("untracked.txt"), "dirty").unwrap();
        let write = WriteFile;
        let err = write
            .execute(r#"{"path":"new.txt","content":"x"}"#, &ctx)
            .unwrap_err();
        assert!(
            matches!(err, ToolError::Failed(ref m) if m.contains("拒绝写入")),
            "{err:?}"
        );
        assert!(!dir.join("new.txt").exists());
    }

    #[test]
    fn manifest_covers_all_tools_and_filters_t0() {
        let names: Vec<&str> = builtin_registry().iter().map(|t| t.name()).collect();
        let all = tool_manifest(None);
        for n in &names {
            assert!(all.contains(&format!("`{n}`")), "manifest 缺 {n}");
        }
        assert!(all.contains("T0") && all.contains("T1") && all.contains("T2"));
        let t0 = tool_manifest(Some(RiskTier::T0));
        assert!(t0.contains("`read_file`"));
        assert!(!t0.contains("`write_file`"), "T0 manifest 不应含写工具");
        assert!(!t0.contains("`shell`"));
    }
}

#[cfg(test)]
mod apply_tx_tests {
    use super::*;

    /// 真实 git 仓库 + 初始 commit(spec 60 §4:脏工作区保护与回退点都依赖)
    fn real_git_fixture(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zealous-apply-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        run(&["init", "-q"]);
        std::fs::write(dir.join("README.md"), "fixture\n").unwrap();
        run(&["add", "-A"]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "init",
        ]);
        dir
    }

    #[test]
    fn apply_rolls_back_file_and_index_when_commit_fails() {
        let dir = real_git_fixture("rollback");
        // 让 commit 必然失败:pre-commit hook exit 1
        let hook = dir.join(".git").join("hooks").join("pre-commit");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();

        let ctx = ToolContext {
            repo_root: dir.clone(),
        };
        let write = WriteFile;
        let err = write
            .execute(r#"{"path":"new.txt","content":"x"}"#, &ctx)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Failed(m) if m.contains("已回滚")),
            "{err}"
        );
        assert!(!dir.join("new.txt").exists(), "回滚后新文件不得残留");
        let st = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&st.stdout).trim().is_empty(),
            "回滚后工作区与 index 必须干净"
        );
    }

    #[test]
    fn apply_happy_path_commits_rollback_point() {
        let dir = real_git_fixture("happy");
        let ctx = ToolContext {
            repo_root: dir.clone(),
        };
        let write = WriteFile;
        write
            .execute(r#"{"path":"feature.txt","content":"v1"}"#, &ctx)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("feature.txt")).unwrap(),
            "v1"
        );
        let log = std::process::Command::new("git")
            .args(["log", "--oneline"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&log.stdout).contains("zealous: apply feature.txt"),
            "应有 zealous 回退 commit"
        );
        let st = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&st.stdout).trim().is_empty());
    }
}

#[cfg(test)]
mod git_tools_tests {
    use super::*;

    fn real_repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zealous-git-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {:?}", out.stderr);
        };
        run(&["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "line1\nline2\n").unwrap();
        run(&["add", "-A"]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "commit one",
        ]);
        std::fs::write(dir.join("a.txt"), "line1\nline2 changed\n").unwrap();
        run(&["add", "-A"]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "commit two",
        ]);
        std::fs::write(dir.join("a.txt"), "line1\nline2 changed again\n").unwrap();
        dir
    }

    #[test]
    fn git_log_default_limit_and_path() {
        let dir = real_repo("log");
        let ctx = ToolContext {
            repo_root: dir.clone(),
        };
        let out = GitLog.execute("{}", &ctx).unwrap();
        assert!(out.summary.contains("commit two"));
        assert!(out.summary.contains("commit one"));
        // limit=1:只含最新提交,不含更早的
        let out = GitLog.execute(r#"{"limit":1}"#, &ctx).unwrap();
        assert!(out.summary.contains("commit two"));
        assert!(!out.summary.contains("commit one"), "{}", out.summary);
        // 越界路径拒绝
        let err = GitLog
            .execute(r#"{"path":"../outside"}"#, &ctx)
            .unwrap_err();
        assert!(matches!(err, ToolError::BadArgs(m) if m.contains("越界")));
    }

    #[test]
    fn git_log_limit_clamped_to_100() {
        let dir = real_repo("logcap");
        let ctx = ToolContext {
            repo_root: dir.clone(),
        };
        // limit=99999 被 clamp 到 100(等价合法执行,不报错)
        let out = GitLog.execute(r#"{"limit":99999}"#, &ctx).unwrap();
        assert!(!out.summary.is_empty());
    }

    #[test]
    fn git_diff_worktree_and_staged_and_truncation() {
        let dir = real_repo("diff");
        let ctx = ToolContext {
            repo_root: dir.clone(),
        };
        // 工作区 diff:未暂存改动可见
        let out = GitDiff.execute("{}", &ctx).unwrap();
        assert!(out.summary.contains("line2 changed"), "{}", out.summary);
        // staged diff:未暂存时为空
        let out = GitDiff.execute(r#"{"staged":true}"#, &ctx).unwrap();
        assert!(out.summary.trim().is_empty(), "{}", out.summary);
        // 暂存后可见
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&dir)
            .output()
            .unwrap();
        let out = GitDiff.execute(r#"{"staged":true}"#, &ctx).unwrap();
        assert!(out.summary.contains("line2 changed"));

        // 截断标记:构造 >64KiB 的 diff(git diff 不含 untracked,改已跟踪文件)
        let big = format!("{}\n", "x".repeat(80 * 1024));
        let mut tracked = std::fs::read_to_string(dir.join("a.txt")).unwrap();
        tracked.push_str(&big);
        std::fs::write(dir.join("a.txt"), tracked).unwrap();
        let out = GitDiff.execute("{}", &ctx).unwrap();
        assert!(out.summary.contains("[truncated"), "应显式标记截断");
        assert!(out.summary.len() <= 64 * 1024 + 200);
    }

    #[test]
    fn git_tools_registered_as_t0() {
        let reg = builtin_registry();
        assert_eq!(risk_of(&reg, "git_log"), Some(RiskTier::T0));
        assert_eq!(risk_of(&reg, "git_diff"), Some(RiskTier::T0));
    }
}
