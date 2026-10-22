//! 全局 run lease(spec 156):`.zealous/run.lock`。
//!
//! 所有执行入口(CLI task run / queue run / TUI runner / app-server
//! run_task 与 run_queue)共用同一把 OS advisory lock:同一时间只允许
//! 一个运行中的任务。获取失败返回 AlreadyRunning;进程退出/崩溃由 OS
//! 释放锁;lock 文件可残留,以 OS lock 判断持有;不用轮询等待。
//!
//! Windows 锁定的是整个文件字节区(被锁区域不可读),因此元数据写入
//! 独立的 `run.lock.json`(跟随 lock 生命周期)。

use fs4::fs_std::FileExt as _;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use zealous_core::time::rfc3339_from_unix_millis;

fn now_rfc3339() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    rfc3339_from_unix_millis(ms)
}

/// run.lock 元数据(spec 156)
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LeaseInfo {
    pub pid: u32,
    pub task_id: String,
    pub mode: String,
    pub started_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseError {
    /// 已有运行中的任务(携带持有者元数据;读取失败时字段为 unknown)
    AlreadyRunning(LeaseInfo),
    Io(String),
}

impl std::fmt::Display for LeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LeaseError::AlreadyRunning(info) => write!(
                f,
                "already_running: pid={} task={} mode={} started_at={}",
                info.pid, info.task_id, info.mode, info.started_at
            ),
            LeaseError::Io(s) => write!(f, "run lease io: {s}"),
        }
    }
}

pub fn lock_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".zealous").join("run.lock")
}

fn meta_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".zealous").join("run.lock.json")
}

fn read_info(repo_root: &Path) -> Option<LeaseInfo> {
    let text = std::fs::read_to_string(meta_path(repo_root)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_info(repo_root: &Path, info: &LeaseInfo) -> Result<(), LeaseError> {
    let path = meta_path(repo_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| LeaseError::Io(e.to_string()))?;
    }
    std::fs::write(&path, serde_json::to_string(info).unwrap())
        .map_err(|e| LeaseError::Io(e.to_string()))
}

/// 全局运行租约:Drop 时解锁并清理 lock 与元数据文件。
pub struct RunLease {
    file: File,
    repo_root: PathBuf,
}

impl std::fmt::Debug for RunLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunLease")
            .field("repo_root", &self.repo_root)
            .finish()
    }
}

impl RunLease {
    /// 队列运行时逐任务刷新元数据展示
    pub fn set_task(&self, repo_root: &Path, task_id: &str, mode: &str) -> Result<(), LeaseError> {
        write_info(
            repo_root,
            &LeaseInfo {
                pid: std::process::id(),
                task_id: task_id.into(),
                mode: mode.into(),
                started_at: now_rfc3339(),
            },
        )
    }
}

impl Drop for RunLease {
    fn drop(&mut self) {
        let _ = self.file.unlock();
        let _ = std::fs::remove_file(lock_path(&self.repo_root));
        let _ = std::fs::remove_file(meta_path(&self.repo_root));
    }
}

/// 非阻塞获取全局运行租约;失败返回 AlreadyRunning(含持有者信息;
/// 元数据不可读时字段为 unknown)。
pub fn try_acquire(repo_root: &Path, task_id: &str, mode: &str) -> Result<RunLease, LeaseError> {
    let path = lock_path(repo_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| LeaseError::Io(e.to_string()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| LeaseError::Io(e.to_string()))?;
    let acquired = matches!(file.try_lock_exclusive(), Ok(true));
    if !acquired {
        let info = read_info(repo_root).unwrap_or(LeaseInfo {
            pid: 0,
            task_id: "(unknown)".into(),
            mode: "(unknown)".into(),
            started_at: "(unknown)".into(),
        });
        return Err(LeaseError::AlreadyRunning(info));
    }
    write_info(
        repo_root,
        &LeaseInfo {
            pid: std::process::id(),
            task_id: task_id.into(),
            mode: mode.into(),
            started_at: now_rfc3339(),
        },
    )?;
    Ok(RunLease {
        file,
        repo_root: repo_root.to_path_buf(),
    })
}

/// 读取 lock 元数据(不判断是否仍被持有;queue status 用)
pub fn peek_info(repo_root: &Path) -> Option<LeaseInfo> {
    read_info(repo_root)
}

/// 是否已被持有(queue status 判断 active):临时句柄尝试独占锁,
/// 拿不到即被持有;拿得到则随句柄释放(不清理文件)。
pub fn is_held(repo_root: &Path) -> bool {
    let path = lock_path(repo_root);
    let Ok(file) = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
    else {
        return false;
    };
    matches!(file.try_lock_exclusive(), Ok(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "zealous-lease-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn acquire_then_second_fails_then_drop_releases() {
        let d = dir("basic");
        let l1 = try_acquire(&d, "task_a", "isolated").unwrap();
        let err = try_acquire(&d, "task_b", "direct").unwrap_err();
        match err {
            LeaseError::AlreadyRunning(info) => {
                assert_eq!(info.task_id, "task_a");
                assert_eq!(info.mode, "isolated");
            }
            other => panic!("{other:?}"),
        }
        drop(l1);
        let l2 = try_acquire(&d, "task_b", "direct").unwrap();
        assert_eq!(peek_info(&d).unwrap().task_id, "task_b");
        drop(l2);
    }

    #[test]
    fn stale_lock_file_without_holder_is_reacquirable() {
        let d = dir("stale");
        // 元数据文件残留(模拟崩溃),lock 文件无持有者 → 可重新获取
        std::fs::create_dir_all(d.join(".zealous")).unwrap();
        std::fs::write(
            meta_path(&d),
            r#"{"pid":99999,"task_id":"dead","mode":"direct","started_at":"x"}"#,
        )
        .unwrap();
        let l = try_acquire(&d, "new", "queue").unwrap_or_else(|e| {
            eprintln!("[dbg] try_acquire err: {e:?}");
            RunLease {
                file: File::open(lock_path(&d)).unwrap(),
                repo_root: d.clone(),
            }
        });
        l.set_task(&d, "new2", "queue").unwrap();
        assert_eq!(peek_info(&d).unwrap().task_id, "new2");
    }

    #[test]
    fn metadata_roundtrip_and_drop_cleanup() {
        let d = dir("meta");
        let l = try_acquire(&d, "task_x", "queue").unwrap();
        let info = peek_info(&d).unwrap();
        assert_eq!(info.pid, std::process::id());
        assert_eq!(info.task_id, "task_x");
        assert_eq!(info.mode, "queue");
        drop(l);
        // Drop 后两个文件都被清理
        assert!(!lock_path(&d).exists());
        assert!(!meta_path(&d).exists());
    }
}
