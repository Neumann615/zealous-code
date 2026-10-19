//! spec 141 集成测试:运行副产物不阻塞 T1 写入链路。
//!
//! 干净 Git 仓库 → 制造 `__pycache__/test.pyc` → edit_file(审批通过)
//! → 目标文件修改成功、自动生成 rollback commit、副产物仍 untracked、
//! commit 只包含目标文件。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use zealous_core::{AgentRef, AgentRunId, EventPayload, TaskStatus};
use zealous_engine::approval::AutoApprove;
use zealous_engine::executor::ToolExecutor;
use zealous_engine::ledger::Ledger;
use zealous_engine::tools::builtin_registry;

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} failed");
}

fn porcelain(dir: &Path) -> String {
    let out = std::process::Command::new("git")
        .args(["status", "--porcelain", "-uall"])
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// 干净 Git 仓库 + src/main.rs 初始 commit
fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zealous-sp141-int-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() { zealous }\n").unwrap();
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "-A"]);
    git(
        &dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    dir
}

fn started_task(repo: &Path) -> Ledger {
    let task_id = zealous_core::TaskId::generate();
    let ledger = Ledger::create(repo, &task_id).unwrap();
    ledger
        .append(
            AgentRef::User,
            EventPayload::TaskCreated {
                title: "sp141".into(),
                repo_root: repo.display().to_string(),
            },
        )
        .unwrap();
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
}

#[test]
fn artifact_does_not_block_edit_and_apply_commits_only_target() {
    let dir = fixture("apply");
    // 运行副产物:先跑测试留下的 __pycache__/test.pyc
    std::fs::create_dir_all(dir.join("__pycache__")).unwrap();
    std::fs::write(dir.join("__pycache__/test.pyc"), "bytecode").unwrap();

    let ledger = started_task(&dir);
    let exec = ToolExecutor::new(&dir, builtin_registry(), Arc::new(AutoApprove));
    let run = AgentRunId::generate();
    ledger
        .append(
            AgentRef::Coordinator,
            EventPayload::AgentRunStarted {
                run_id: run.clone(),
                role: zealous_core::AgentRole::Coordinator,
            },
        )
        .unwrap();

    // 审批通过(AutoApprove)后写入必须成功,不被纯副产物阻塞
    let out = exec
        .run_tool(
            &ledger,
            &run,
            AgentRef::Coordinator,
            r#"{"path":"src/main.rs","old_text":"zealous","new_text":"zealous2"}"#,
            "edit_file",
        )
        .unwrap();
    assert!(out.ok, "纯副产物不得阻塞 edit_file: {}", out.text);

    // 目标文件修改成功
    let body = std::fs::read_to_string(dir.join("src/main.rs")).unwrap();
    assert!(body.contains("zealous2"), "{body}");

    // 自动生成 rollback commit(zealous: edit …)
    let log = std::process::Command::new("git")
        .args(["log", "-1", "--pretty=format:%s", "--name-only"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&log.stdout);
    assert!(log.contains("zealous: edit src"), "{log}");
    assert!(log.contains("src/main.rs"), "{log}");
    // commit 只包含目标文件,不含副产物
    assert!(!log.contains("__pycache__"), "commit 不得含副产物: {log}");
    assert!(!log.contains("test.pyc"), "commit 不得含副产物: {log}");

    // 副产物仍为 untracked,未被删除/stage/commit
    let st = porcelain(&dir);
    assert!(st.contains("?? __pycache__/test.pyc"), "{st}");
    assert!(st.contains("test.pyc"), "{st}");
    // commit 后工作区除副产物外干净
    assert!(!st.contains("src/main.rs"), "目标文件应已提交: {st}");
}
