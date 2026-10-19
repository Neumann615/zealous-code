//! 工作区状态分类(spec 141):`git status --porcelain -uall` 三分类,
//! 把 Python 运行副产物与用户改动、zealous 运行数据区分开。
//!
//! 确定性规则:
//! - untracked(`??`)且路径在 `.zealous/` 下 → `ZealousRuntimeData`
//! - untracked(`??`)且命中 `__pycache__/` 或 `*.pyc` → `RuntimeArtifact`
//! - 其余(含 tracked 的 .pyc/__pycache__ 修改)→ `UserChange`
//! - Git quoted path 解析后再判断,不做字符串前缀猜测。

use std::path::Path;

/// 工作区条目分类(spec 141)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// `.zealous/` 运行数据(账本/提案),不视为用户改动
    ZealousRuntimeData,
    /// Python 运行副产物(untracked `__pycache__/` / `*.pyc`),不阻塞写入
    RuntimeArtifact,
    /// 用户改动:阻塞写入守卫
    UserChange,
}

/// 一条 porcelain 记录(保留原始行,便于工具输出"原始状态仍可见")
#[derive(Debug, Clone)]
pub struct WorktreeEntry {
    /// 原始 porcelain 行
    pub raw: String,
    /// 解析(unquote)后的路径
    pub path: String,
    /// 是否 untracked(`??`)
    pub untracked: bool,
    pub kind: EntryKind,
}

/// `git status --porcelain` 的分类结果
#[derive(Debug, Clone, Default)]
pub struct WorktreeState {
    pub entries: Vec<WorktreeEntry>,
}

impl WorktreeState {
    pub fn of(&self, kind: EntryKind) -> Vec<&WorktreeEntry> {
        self.entries.iter().filter(|e| e.kind == kind).collect()
    }

    pub fn has_user_changes(&self) -> bool {
        self.entries.iter().any(|e| e.kind == EntryKind::UserChange)
    }
}

/// 分类 porcelain 输出(不执行 git;纯函数,便于测试)
pub fn classify_porcelain(porcelain: &str) -> WorktreeState {
    let entries = porcelain
        .lines()
        .filter(|l| l.trim().len() >= 3)
        .map(|line| {
            let untracked = line.starts_with("??");
            let path_raw = line.get(3..).unwrap_or("");
            let path = parse_porcelain_path(path_raw);
            let kind = classify_path(&path, untracked);
            WorktreeEntry {
                raw: line.to_string(),
                path,
                untracked,
                kind,
            }
        })
        .collect();
    WorktreeState { entries }
}

/// 读取仓库当前工作区状态并分类。
///
/// 必须用 `-uall`:默认 porcelain 会把 untracked 目录折叠成 `?? pkg/`,
/// 内层 `pkg/__pycache__/` 无从识别;逐文件列出后分类才确定。
pub fn read_state(repo_root: &Path) -> Result<WorktreeState, String> {
    let out = std::process::Command::new("git")
        .args(["status", "--porcelain", "-uall"])
        .current_dir(repo_root)
        .output()
        .map_err(|e| format!("spawn git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git status 失败: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(classify_porcelain(&String::from_utf8_lossy(&out.stdout)))
}

fn classify_path(path: &str, untracked: bool) -> EntryKind {
    // 只有 untracked 才是运行数据/副产物;tracked 修改一律 UserChange
    // (spec 141:已跟踪的 .pyc/__pycache__ 是用户仓库事实,不降级)
    if !untracked {
        return EntryKind::UserChange;
    }
    if is_zealous_runtime(path) {
        return EntryKind::ZealousRuntimeData;
    }
    if is_python_artifact(path) {
        return EntryKind::RuntimeArtifact;
    }
    EntryKind::UserChange
}

fn is_zealous_runtime(path: &str) -> bool {
    let p = path.trim_end_matches(['/', '\\']);
    p == ".zealous" || p.starts_with(".zealous/") || p.starts_with(".zealous\\")
}

/// 任意层级 `__pycache__/` 或 `*.pyc`(spec 141 只认这两类)
fn is_python_artifact(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    // porcelain 里 untracked 目录带尾斜杠;去掉后再拆段
    let trimmed = normalized.trim_end_matches('/');
    if trimmed.split('/').any(|seg| seg == "__pycache__") {
        return true;
    }
    // 目录名以 .pyc 结尾不降级(只认 *.pyc 文件:非目录条目)
    let is_dir_entry = normalized.ends_with('/');
    !is_dir_entry
        && trimmed
            .rsplit('/')
            .next()
            .map(|name| name.ends_with(".pyc"))
            .unwrap_or(false)
}

/// porcelain 路径字段解析:quoted path 的 C 风格转义
/// (`\"` `\\` `\n` `\t` `\r` `\a` `\b` `\f` `\v` 与八进制字节),
/// 解析出字节后按 UTF-8 还原;解析失败则回退原文。
pub fn parse_porcelain_path(field: &str) -> String {
    let field = field.trim();
    // rename:`old -> new`,分类按新路径
    let field = match field.rsplit_once(" -> ") {
        Some((_, new)) => new.trim(),
        None => field,
    };
    if !(field.starts_with('"') && field.ends_with('"') && field.len() >= 2) {
        return field.to_string();
    }
    let inner = &field[1..field.len() - 1];
    let bytes = inner.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let n = bytes[i + 1];
            match n {
                b'"' => {
                    out.push(b'"');
                    i += 2;
                }
                b'\\' => {
                    out.push(b'\\');
                    i += 2;
                }
                b'n' => {
                    out.push(b'\n');
                    i += 2;
                }
                b't' => {
                    out.push(b'\t');
                    i += 2;
                }
                b'r' => {
                    out.push(b'\r');
                    i += 2;
                }
                b'a' => {
                    out.push(0x07);
                    i += 2;
                }
                b'b' => {
                    out.push(0x08);
                    i += 2;
                }
                b'f' => {
                    out.push(0x0c);
                    i += 2;
                }
                b'v' => {
                    out.push(0x0b);
                    i += 2;
                }
                b'0'..=b'7' => {
                    let mut v: u32 = 0;
                    let mut j = i + 1;
                    while j < bytes.len() && j < i + 4 && (b'0'..=b'7').contains(&bytes[j]) {
                        v = v * 8 + u32::from(bytes[j] - b'0');
                        j += 1;
                    }
                    out.push(v as u8);
                    i = j;
                }
                other => {
                    out.push(other);
                    i += 2;
                }
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| inner.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind_of(line: &str) -> EntryKind {
        let st = classify_porcelain(line);
        st.entries[0].kind
    }

    #[test]
    fn untracked_pycache_any_level_is_artifact() {
        assert_eq!(kind_of("?? __pycache__/"), EntryKind::RuntimeArtifact);
        assert_eq!(kind_of("?? a/b/__pycache__/"), EntryKind::RuntimeArtifact);
        assert_eq!(
            kind_of("?? a/__pycache__/mod.cpython-312.pyc"),
            EntryKind::RuntimeArtifact
        );
    }

    #[test]
    fn untracked_pyc_is_artifact_but_pyc_dir_is_not() {
        assert_eq!(kind_of("?? out.pyc"), EntryKind::RuntimeArtifact);
        assert_eq!(kind_of("?? a/b/out.pyc"), EntryKind::RuntimeArtifact);
        // 目录名以 .pyc 结尾不是副产物文件
        assert_eq!(kind_of("?? weird.pyc/"), EntryKind::UserChange);
    }

    #[test]
    fn tracked_pyc_modification_is_user_change() {
        assert_eq!(kind_of(" M out.pyc"), EntryKind::UserChange);
        assert_eq!(kind_of("M  __pycache__/x.pyc"), EntryKind::UserChange);
        assert_eq!(kind_of(" D __pycache__/"), EntryKind::UserChange);
    }

    #[test]
    fn zealous_runtime_data_untracked_only() {
        assert_eq!(kind_of("?? .zealous/"), EntryKind::ZealousRuntimeData);
        assert_eq!(
            kind_of("?? .zealous/tasks/t1/events.jsonl"),
            EntryKind::ZealousRuntimeData
        );
        // tracked 修改不降级
        assert_eq!(kind_of(" M .zealous/config.json"), EntryKind::UserChange);
    }

    #[test]
    fn ordinary_changes_are_user_change() {
        assert_eq!(kind_of(" M src/lib.rs"), EntryKind::UserChange);
        assert_eq!(kind_of("?? new.txt"), EntryKind::UserChange);
        assert_eq!(kind_of("A  staged.txt"), EntryKind::UserChange);
        assert_eq!(kind_of("R  old.txt -> new.txt"), EntryKind::UserChange);
    }

    #[test]
    fn quoted_paths_unquoted_before_classify() {
        // 空格路径(git 转义引号)
        assert_eq!(
            kind_of(r#"?? "my dir/__pycache__/" "#.trim_end()),
            EntryKind::RuntimeArtifact
        );
        assert_eq!(kind_of(r#"?? "has space.pyc""#), EntryKind::RuntimeArtifact);
        // 非 ASCII(octal 转义):"文/__pycache__/" = \346\226\207/__pycache__/
        assert_eq!(
            kind_of(r#"?? "\346\226\207/__pycache__/" "#.trim_end()),
            EntryKind::RuntimeArtifact
        );
        // quoted 普通路径仍是用户改动
        assert_eq!(kind_of(r#"?? "my notes.txt""#), EntryKind::UserChange);
    }

    #[test]
    fn parse_handles_escapes_and_rename() {
        assert_eq!(parse_porcelain_path(r#""a\"b.pyc""#), "a\"b.pyc");
        assert_eq!(parse_porcelain_path(r#""\346\226\207.txt""#), "文.txt");
        assert_eq!(parse_porcelain_path("plain.txt"), "plain.txt");
        assert_eq!(
            parse_porcelain_path("old name.txt -> new name.txt"),
            "new name.txt"
        );
    }

    #[test]
    fn state_sections_and_has_user_changes() {
        let st = classify_porcelain(" M src/lib.rs\n?? __pycache__/\n?? out.pyc\n?? .zealous/\n");
        assert!(st.has_user_changes());
        assert_eq!(st.of(EntryKind::UserChange).len(), 1);
        assert_eq!(st.of(EntryKind::RuntimeArtifact).len(), 2);
        assert_eq!(st.of(EntryKind::ZealousRuntimeData).len(), 1);

        let clean = classify_porcelain("?? __pycache__/\n?? .zealous/\n");
        assert!(!clean.has_user_changes());
    }
}
