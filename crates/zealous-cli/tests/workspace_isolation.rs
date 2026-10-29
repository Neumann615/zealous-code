//! Worktree 隔离集成测试(spec 155):
//! CLI:task new → run --isolate(mock)→ workspace → apply(ff-only)。
//! serve:run_task isolate:true → decide → get_workspace → apply_task。
//!
//! 断言:主工作区文件在运行期间不变、rollback commit 落在任务分支、
//! 账本在主仓库 .zealous/tasks、apply 后主工作区 fast-forward、
//! 事件 seq 连续、apply 事件落账、无密钥泄漏。

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

fn unique_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zealous-ws155-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn chat_body(content: &str) -> String {
    serde_json::json!({
        "choices": [{ "message": { "content": content } }]
    })
    .to_string()
}

fn mock_endpoint(script: Vec<String>) -> String {
    let counter = AtomicU32::new(0);
    let script = Mutex::new(script);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || loop {
        let Ok((mut stream, _)) = listener.accept() else {
            break;
        };
        let mut raw: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let n = match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            raw.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&raw);
            if let Some(h) = text.find("\r\n\r\n") {
                let len = text
                    .split("\r\n")
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if raw.len() >= h + 4 + len {
                    break;
                }
            }
        }
        let idx = counter.fetch_add(1, Ordering::Relaxed) as usize;
        let content = {
            let script = script.lock().unwrap();
            if idx < script.len() {
                script[idx].clone()
            } else {
                "guard".into()
            }
        };
        let body = chat_body(&content);
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .as_bytes(),
        );
        let _ = stream.shutdown(std::net::Shutdown::Write);
    });
    format!("http://{addr}/v1")
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
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

fn base_repo(tag: &str) -> std::path::PathBuf {
    let dir = unique_dir(tag);
    std::fs::write(dir.join(".gitignore"), ".zealous/\n").unwrap();
    std::fs::write(dir.join("note.txt"), "OLD-VALUE\n").unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    dir
}

fn find_ledger(repo: &std::path::Path) -> Option<std::path::PathBuf> {
    let tasks = repo.join(".zealous").join("tasks");
    if let Ok(entries) = std::fs::read_dir(&tasks) {
        for e in entries.flatten() {
            let p = e.path().join("events.jsonl");
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

fn ledger_text(repo: &std::path::Path) -> String {
    let p = find_ledger(repo).expect("ledger");
    std::fs::read_to_string(&p).unwrap()
}

fn run_task(repo: &std::path::Path, task_id: &str, endpoint: &str, isolate: bool) -> String {
    let zealous = env!("CARGO_BIN_EXE_zealous");
    let mut args = vec!["task", "run", task_id];
    if isolate {
        args.push("--isolate");
    }
    args.push("--yes");
    let out = Command::new(zealous)
        .args(&args)
        .current_dir(repo)
        .env("ZEALOUS_BASE_URL", endpoint)
        .env("ZEALOUS_API_KEY", "e2e-secret-key")
        .env("ZEALOUS_MODEL", "mock-model")
        .output()
        .expect("spawn task run");
    assert!(
        out.status.success(),
        "task run failed: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stdout).into()
}

#[test]
fn cli_isolated_run_and_apply() {
    let repo = base_repo("cli");
    let script = vec![
        r#"{"action":"plan","plan":"edit note.txt in worktree","workers":[]}"#.into(),
        r#"{"action":"tool","tool":"edit_file","args":{"path":"note.txt","old_text":"OLD-VALUE","new_text":"NEW-VALUE"}}"#.into(),
        r#"{"action":"message","text":"done"}"#.into(),
    ];
    let endpoint = mock_endpoint(script);

    // task new
    let zealous = env!("CARGO_BIN_EXE_zealous");
    let out = Command::new(zealous)
        .args(["task", "new", "ws cli task"])
        .current_dir(&repo)
        .env("ZEALOUS_BASE_URL", &endpoint)
        .env("ZEALOUS_API_KEY", "e2e-secret-key")
        .env("ZEALOUS_MODEL", "mock-model")
        .output()
        .unwrap();
    assert!(out.status.success());
    let task_id = String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("task created: "))
        .unwrap()
        .trim()
        .to_string();

    // run --isolate --yes(mock)
    let run_out = run_task(&repo, &task_id, &endpoint, true);
    assert!(run_out.contains("Completed"), "{run_out}");

    // 主工作区 note.txt 不变(修改在 worktree 中)
    assert_eq!(
        std::fs::read_to_string(repo.join("note.txt"))
            .unwrap()
            .trim_end(),
        "OLD-VALUE"
    );
    // 任务分支有 rollback commit;主分支没有
    let branch_log = git(&repo, &["log", "--oneline", &format!("zealous/{task_id}")]);
    assert!(
        branch_log.contains("zealous: edit note.txt"),
        "{branch_log}"
    );
    let main_log = git(&repo, &["log", "--oneline", "main"]);
    assert!(!main_log.contains("zealous: edit note.txt"));
    // 账本在主仓库 .zealous/tasks
    assert!(find_ledger(&repo).is_some());
    let text = ledger_text(&repo);
    for needle in ["e2e-secret-key", "Authorization", "Bearer "] {
        assert!(!text.contains(needle), "账本泄漏: {needle}");
    }

    // task workspace:字段来自投影
    let out = Command::new(zealous)
        .args(["task", "workspace", &task_id])
        .current_dir(&repo)
        .output()
        .unwrap();
    let ws_text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(ws_text.contains("mode:           isolated"), "{ws_text}");
    assert!(ws_text.contains(&format!("zealous/{task_id}")), "{ws_text}");

    // apply --yes:主工作区 fast-forward
    let out = Command::new(zealous)
        .args(["task", "apply", &task_id, "--yes"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "apply failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("note.txt"))
            .unwrap()
            .trim_end(),
        "NEW-VALUE"
    );
    let main_log = git(&repo, &["log", "--oneline", "main"]);
    assert!(main_log.contains("zealous: edit note.txt"));

    // 事件 seq 连续 + apply 事件落账
    let text = ledger_text(&repo);
    let events: Vec<serde_json::Value> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    for (i, ev) in events.iter().enumerate() {
        assert_eq!(ev["seq"].as_u64().unwrap(), (i + 1) as u64, "seq 跳号");
    }
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "ExecutionWorkspacePrepared"),
        "缺少 Prepared 事件"
    );
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "ExecutionWorkspaceApplied"),
        "缺少 Applied 事件"
    );

    // 重复 apply 幂等
    let out = Command::new(zealous)
        .args(["task", "apply", &task_id, "--yes"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "重复 apply 应幂等成功");
    assert!(String::from_utf8_lossy(&out.stdout).contains("已应用过"));
}

// ---------- app-server 流 ----------

struct ServeProc {
    #[allow(dead_code)]
    child: Child,
    stdin: BufWriter,
    lines_rx: std::sync::mpsc::Receiver<String>,
}

type BufWriter = std::io::BufWriter<std::process::ChildStdin>;

fn spawn_serve(repo: &std::path::Path, endpoint: &str) -> ServeProc {
    let zealous = env!("CARGO_BIN_EXE_zealous");
    let mut child = Command::new(zealous)
        .args(["serve"])
        .current_dir(repo)
        .env("ZEALOUS_BASE_URL", endpoint)
        .env("ZEALOUS_API_KEY", "serve-secret-key")
        .env("ZEALOUS_MODEL", "mock-model")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            match line {
                Ok(l) => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
    ServeProc {
        child,
        stdin: std::io::BufWriter::new(stdin),
        lines_rx: rx,
    }
}

impl ServeProc {
    fn send(&mut self, v: &serde_json::Value) {
        serde_json::to_writer(&mut self.stdin, v).unwrap();
        self.stdin.write_all(b"\n").unwrap();
        self.stdin.flush().unwrap();
    }

    fn wait_response(
        &mut self,
        id: u64,
        deadline: Instant,
        collect: &mut Vec<serde_json::Value>,
    ) -> serde_json::Value {
        loop {
            assert!(Instant::now() < deadline, "等待响应 id={id} 超时");
            let line = self
                .lines_rx
                .recv_timeout(Duration::from_millis(500))
                .expect("响应超时");
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            if v.get("id").and_then(|x| x.as_u64()) == Some(id) {
                return v;
            }
            collect.push(v);
        }
    }
}

static SERVE_ISOLATED_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn serve_isolated_run_get_workspace_apply() {
    let _serial_guard = SERVE_ISOLATED_TEST_LOCK.lock().unwrap();
    let repo = base_repo("serve");
    let script = vec![
        r#"{"action":"plan","plan":"edit note.txt","workers":[]}"#.into(),
        r#"{"action":"tool","tool":"edit_file","args":{"path":"note.txt","old_text":"OLD-VALUE","new_text":"NEW-VALUE"}}"#.into(),
        r#"{"action":"message","text":"done"}"#.into(),
    ];
    let endpoint = mock_endpoint(script);
    let mut proc = spawn_serve(&repo, &endpoint);
    let mut notes: Vec<serde_json::Value> = Vec::new();
    let mut id = 0u64;
    let deadline = Instant::now() + Duration::from_secs(60);

    id += 1;
    proc.send(&serde_json::json!({"id": id, "cmd": "initialize", "params": {"version": 1}}));
    let r = proc.wait_response(id, deadline, &mut notes);
    assert_eq!(r["ok"], true);
    assert_eq!(r["result"]["capabilities"]["isolated_worktree"], true);

    id += 1;
    proc.send(&serde_json::json!({"id": id, "cmd": "create_task", "params": {"title": "ws serve", "brief": "edit note.txt"}}));
    let r = proc.wait_response(id, deadline, &mut notes);
    let task_id = r["result"]["task_id"].as_str().unwrap().to_string();

    id += 1;
    proc.send(&serde_json::json!({"id": id, "cmd": "run_task", "params": {"task_id": task_id, "isolate": true}}));
    let r = proc.wait_response(id, deadline, &mut notes);
    assert_eq!(r["ok"], true);

    // 等审批 → 批准 → Completed
    let mut saw_approval = false;
    loop {
        assert!(Instant::now() < deadline, "等待 run_finished 超时");
        let line = proc
            .lines_rx
            .recv_timeout(Duration::from_millis(500))
            .expect("通知流超时");
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        if v["event"] == "approval_requested" {
            saw_approval = true;
            let call = v["params"]["call_id"].as_str().unwrap().to_string();
            id += 1;
            proc.send(&serde_json::json!({"id": id, "cmd": "decide_approval", "params": {"task_id": task_id, "call_id": call, "decision": "approve"}}));
        }
        if v["event"] == "run_finished" {
            assert_eq!(v["params"]["status"], "Completed");
            break;
        }
    }
    assert!(saw_approval, "缺少审批请求");

    // get_workspace
    id += 1;
    proc.send(
        &serde_json::json!({"id": id, "cmd": "get_workspace", "params": {"task_id": task_id}}),
    );
    let r = proc.wait_response(id, deadline, &mut notes);
    assert_eq!(r["result"]["mode"], "isolated");
    assert_eq!(r["result"]["status"], "ready");

    // apply_task
    id += 1;
    proc.send(&serde_json::json!({"id": id, "cmd": "apply_task", "params": {"task_id": task_id}}));
    let r = proc.wait_response(id, deadline, &mut notes);
    assert_eq!(r["ok"], true, "{}", r);

    // get_task Completed + 主工作区 fast-forward
    id += 1;
    proc.send(&serde_json::json!({"id": id, "cmd": "get_task", "params": {"task_id": task_id}}));
    let r = proc.wait_response(id, deadline, &mut notes);
    assert_eq!(r["result"]["status"], "Completed");
    assert_eq!(
        std::fs::read_to_string(repo.join("note.txt"))
            .unwrap()
            .trim_end(),
        "NEW-VALUE"
    );

    // 事件连续 + 无泄漏
    let mut ledger = None;
    let tasks = repo.join(".zealous").join("tasks");
    for e in std::fs::read_dir(&tasks).unwrap().flatten() {
        let p = e.path().join("events.jsonl");
        if p.exists() {
            ledger = Some(std::fs::read_to_string(p).unwrap());
        }
    }
    let text = ledger.unwrap();
    for needle in ["serve-secret-key", "Authorization", "Bearer "] {
        assert!(!text.contains(needle), "账本泄漏: {needle}");
    }
    let events: Vec<serde_json::Value> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    for (i, ev) in events.iter().enumerate() {
        assert_eq!(ev["seq"].as_u64().unwrap(), (i + 1) as u64, "seq 跳号");
    }
    assert!(events
        .iter()
        .any(|e| e["type"] == "ExecutionWorkspaceApplied"));

    // shutdown
    id += 1;
    proc.send(&serde_json::json!({"id": id, "cmd": "shutdown", "params": {}}));
    let r = proc.wait_response(id, deadline, &mut notes);
    assert_eq!(r["ok"], true);
}

#[test]
fn cli_parallel_two_tasks_same_base_and_apply_conflict() {
    let repo = base_repo("parallel-cli");
    let zealous = env!("CARGO_BIN_EXE_zealous");
    let endpoint = stage_mock_endpoint();
    let mut ids = Vec::new();
    for title in ["parallel one", "parallel two"] {
        let out = Command::new(zealous)
            .args(["task", "new", title])
            .current_dir(&repo)
            .env("ZEALOUS_BASE_URL", &endpoint)
            .env("ZEALOUS_API_KEY", "e2e-secret-key")
            .env("ZEALOUS_MODEL", "mock-model")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        ids.push(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .find_map(|l| l.strip_prefix("task created: "))
                .unwrap()
                .trim()
                .to_string(),
        );
    }

    let out = Command::new(zealous)
        .args([
            "parallel",
            "run",
            ids[0].as_str(),
            ids[1].as_str(),
            "--auto-apply",
            "--yes",
        ])
        .current_dir(&repo)
        .env("ZEALOUS_BASE_URL", &endpoint)
        .env("ZEALOUS_API_KEY", "e2e-secret-key")
        .env("ZEALOUS_MODEL", "mock-model")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    // Alpha semantics: both complete from one base; first applies, second rejects
    // because the base moved. Exit 1 is intentional.
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("apply 失败"), "{text}");
    assert!(
        std::fs::read_to_string(repo.join("note.txt"))
            .unwrap()
            .contains("NEW-VALUE"),
        "first task should fast-forward"
    );
    assert!(repo.join(".zealous/worktrees").join(&ids[1]).exists());

    for id in &ids {
        let out = Command::new(zealous)
            .args(["task", "status", id])
            .current_dir(&repo)
            .output()
            .unwrap();
        let status = String::from_utf8_lossy(&out.stdout);
        assert!(status.contains("status: Completed"), "{status}");
    }
    let text = ledger_text(&repo);
    for needle in ["e2e-secret-key", "Authorization", "Bearer "] {
        assert!(!text.contains(needle), "账本泄漏: {needle}");
    }
}

/// State-independent mock: infer the next action from prior conversation state,
/// so two concurrent tasks can share one endpoint without a global call order.
fn stage_mock_endpoint() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || loop {
        let Ok((mut stream, _)) = listener.accept() else {
            break;
        };
        let mut raw: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let n = match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            raw.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&raw);
            if let Some(h) = text.find("\r\n\r\n") {
                let len = text
                    .split("\r\n")
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if raw.len() >= h + 4 + len {
                    break;
                }
            }
        }
        let request = String::from_utf8_lossy(&raw).to_string();
        let content = if request.contains("tool `edit_file` result") {
            r#"{"action":"message","text":"done"}"#
        } else if request.contains("edit shared note") {
            r#"{"action":"tool","tool":"edit_file","args":{"path":"note.txt","old_text":"OLD-VALUE","new_text":"NEW-VALUE"}}"#
        } else {
            r#"{"action":"plan","plan":"edit shared note","workers":[]}"#
        };
        let body = chat_body(content);
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .as_bytes(),
        );
        let _ = stream.shutdown(std::net::Shutdown::Write);
    });
    format!("http://{addr}/v1")
}

#[test]
fn cli_parallel_merge_strategy_integrates_both_tasks() {
    let repo = base_repo("parallel-merge-cli");
    std::fs::write(repo.join("left.txt"), "LEFT\n").unwrap();
    std::fs::write(repo.join("right.txt"), "RIGHT\n").unwrap();
    git(&repo, &["add", "left.txt", "right.txt"]);
    git(&repo, &["commit", "-q", "-m", "merge targets"]);
    let endpoint = parallel_merge_endpoint();
    let zealous = env!("CARGO_BIN_EXE_zealous");
    let mut ids = Vec::new();
    for title in ["parallel merge left", "parallel merge right"] {
        let out = Command::new(zealous)
            .args(["task", "new", title])
            .current_dir(&repo)
            .env("ZEALOUS_BASE_URL", &endpoint)
            .env("ZEALOUS_API_KEY", "e2e-secret-key")
            .env("ZEALOUS_MODEL", "mock-model")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        ids.push(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .find_map(|l| l.strip_prefix("task created: "))
                .unwrap()
                .trim()
                .to_string(),
        );
    }

    let out = Command::new(zealous)
        .args([
            "parallel",
            "run",
            ids[0].as_str(),
            ids[1].as_str(),
            "--auto-apply",
            "--integrate",
            "merge",
            "--yes",
        ])
        .current_dir(&repo)
        .env("ZEALOUS_BASE_URL", &endpoint)
        .env("ZEALOUS_API_KEY", "e2e-secret-key")
        .env("ZEALOUS_MODEL", "mock-model")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{text}");
    assert!(text.contains("merged 1"), "{text}");
    assert!(std::fs::read_to_string(repo.join("left.txt"))
        .unwrap()
        .contains("LEFT-DONE"));
    assert!(std::fs::read_to_string(repo.join("right.txt"))
        .unwrap()
        .contains("RIGHT-DONE"));
    let merges = git(&repo, &["log", "--oneline", "--merges"]);
    assert!(merges.contains(&format!("zealous/{}", ids[1])), "{merges}");
    for id in &ids {
        let out = Command::new(zealous)
            .args(["task", "workspace", id])
            .current_dir(&repo)
            .output()
            .unwrap();
        let ws = String::from_utf8_lossy(&out.stdout);
        assert!(ws.contains("status:         applied"), "{ws}");
    }
    let second = std::fs::read_to_string(
        repo.join(".zealous/tasks")
            .join(&ids[1])
            .join("events.jsonl"),
    )
    .unwrap();
    assert!(second.contains("ExecutionWorkspaceMerged"), "{second}");
    assert!(!second.contains("e2e-secret-key"), "{second}");
}

fn parallel_merge_endpoint() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || loop {
        let Ok((mut stream, _)) = listener.accept() else {
            break;
        };
        let mut raw: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let n = match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            raw.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&raw);
            if let Some(h) = text.find("\r\n\r\n") {
                let len = text
                    .split("\r\n")
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if raw.len() >= h + 4 + len {
                    break;
                }
            }
        }
        let request = String::from_utf8_lossy(&raw).to_string();
        let left = request.contains("parallel merge left");
        let content = if request.contains("tool `edit_file` result") {
            r#"{"action":"message","text":"done"}"#
        } else if request.contains("edit target") {
            if left {
                r#"{"action":"tool","tool":"edit_file","args":{"path":"left.txt","old_text":"LEFT","new_text":"LEFT-DONE"}}"#
            } else {
                r#"{"action":"tool","tool":"edit_file","args":{"path":"right.txt","old_text":"RIGHT","new_text":"RIGHT-DONE"}}"#
            }
        } else if left {
            r#"{"action":"plan","plan":"edit target left","workers":[]}"#
        } else {
            r#"{"action":"plan","plan":"edit target right","workers":[]}"#
        };
        let body = chat_body(content);
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .as_bytes(),
        );
        let _ = stream.shutdown(std::net::Shutdown::Write);
    });
    format!("http://{addr}/v1")
}

#[test]
fn cli_merge_workspace_conflict_continue_flow() {
    let repo = base_repo("merge-workspace-cli");
    let endpoint = conflicting_parallel_endpoint();
    let zealous = env!("CARGO_BIN_EXE_zealous");
    let mut ids = Vec::new();
    for title in ["conflict left", "conflict right"] {
        let out = Command::new(zealous)
            .args(["task", "new", title])
            .current_dir(&repo)
            .env("ZEALOUS_BASE_URL", &endpoint)
            .env("ZEALOUS_API_KEY", "e2e-secret-key")
            .env("ZEALOUS_MODEL", "mock-model")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        ids.push(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .find_map(|l| l.strip_prefix("task created: "))
                .unwrap()
                .trim()
                .to_string(),
        );
    }

    let out = Command::new(zealous)
        .args([
            "parallel",
            "run",
            ids[0].as_str(),
            ids[1].as_str(),
            "--auto-apply",
            "--yes",
        ])
        .current_dir(&repo)
        .env("ZEALOUS_BASE_URL", &endpoint)
        .env("ZEALOUS_API_KEY", "e2e-secret-key")
        .env("ZEALOUS_MODEL", "mock-model")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("note.txt"))
            .unwrap()
            .trim_end(),
        "LEFT"
    );

    let out = Command::new(zealous)
        .args(["task", "merge-workspace", ids[1].as_str()])
        .current_dir(&repo)
        .output()
        .unwrap();
    let started = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{started}");
    assert!(started.contains("status:        conflicted"), "{started}");
    assert!(started.contains("note.txt"), "{started}");
    let workspace = repo.join(".zealous").join("merges").join(&ids[1]);
    assert!(workspace.exists());
    assert_eq!(git(&repo, &["status", "--porcelain"]).trim(), "");

    let status = Command::new(zealous)
        .args(["task", "merge-status", ids[1].as_str()])
        .current_dir(&repo)
        .output()
        .unwrap();
    let status_text = String::from_utf8_lossy(&status.stdout).into_owned();
    assert!(
        status_text.contains("status:        conflicted"),
        "{status_text}"
    );
    assert!(status_text.contains("unresolved files:"), "{status_text}");
    assert!(status_text.contains("note.txt"), "{status_text}");

    let continue_before = Command::new(zealous)
        .args(["task", "merge-continue", ids[1].as_str(), "--yes"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert_eq!(continue_before.status.code(), Some(1));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&continue_before.stdout),
        String::from_utf8_lossy(&continue_before.stderr)
    );
    assert!(
        combined.contains("unresolved conflict files: note.txt"),
        "{combined}"
    );

    std::fs::write(workspace.join("note.txt"), "USER-RESOLVED\n").unwrap();
    git(&workspace, &["add", "note.txt"]);
    let out = Command::new(zealous)
        .args(["task", "merge-continue", ids[1].as_str(), "--yes"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let continued = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{continued}");
    assert_eq!(
        std::fs::read_to_string(repo.join("note.txt"))
            .unwrap()
            .trim_end(),
        "USER-RESOLVED"
    );
    assert!(!workspace.exists());
    assert!(repo.join(".zealous/worktrees").join(&ids[1]).exists());
    assert_eq!(git(&repo, &["status", "--porcelain"]).trim(), "");

    let ledger = std::fs::read_to_string(
        repo.join(".zealous/tasks")
            .join(&ids[1])
            .join("events.jsonl"),
    )
    .unwrap();
    for needle in [
        "MergeWorkspacePrepared",
        "MergeConflictPending",
        "MergeWorkspaceResolved",
        "ExecutionWorkspaceMerged",
    ] {
        assert!(ledger.contains(needle), "missing {needle}");
    }
    assert!(!ledger.contains("e2e-secret-key"), "{ledger}");
}

fn conflicting_parallel_endpoint() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || loop {
        let Ok((mut stream, _)) = listener.accept() else {
            break;
        };
        let mut raw: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let n = match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            raw.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&raw);
            if let Some(h) = text.find("\r\n\r\n") {
                let len = text
                    .split("\r\n")
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if raw.len() >= h + 4 + len {
                    break;
                }
            }
        }
        let request = String::from_utf8_lossy(&raw).to_string();
        if request.contains("WORKING CONFLICT") {
            let proposal = serde_json::json!({
                "action":"resolve",
                "summary":"Resolve the same-line conflict by preserving both intents.",
                "files":[{"path":"note.txt","mode":"resolved","content":"MODEL-RESOLVED\n","rationale":"combine both requested values"}]
            }).to_string();
            let body = chat_body(&proposal);
            let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes());
            let _ = stream.shutdown(std::net::Shutdown::Write);
            continue;
        }
        let left = request.contains("conflict left");
        let content = if request.contains("tool `edit_file` result") {
            r#"{"action":"message","text":"done"}"#
        } else if request.contains("edit conflict") {
            if left {
                r#"{"action":"tool","tool":"edit_file","args":{"path":"note.txt","old_text":"OLD-VALUE","new_text":"LEFT"}}"#
            } else {
                r#"{"action":"tool","tool":"edit_file","args":{"path":"note.txt","old_text":"OLD-VALUE","new_text":"RIGHT"}}"#
            }
        } else if left {
            r#"{"action":"plan","plan":"edit conflict left","workers":[]}"#
        } else {
            r#"{"action":"plan","plan":"edit conflict right","workers":[]}"#
        };
        let body = chat_body(content);
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .as_bytes(),
        );
        let _ = stream.shutdown(std::net::Shutdown::Write);
    });
    format!("http://{addr}/v1")
}

#[test]
fn cli_ai_resolution_proposal_apply_and_continue() {
    let repo = base_repo("ai-resolution-cli");
    let endpoint = conflicting_parallel_endpoint();
    let zealous = env!("CARGO_BIN_EXE_zealous");
    let mut ids = Vec::new();
    for title in ["conflict left", "conflict right"] {
        let out = Command::new(zealous)
            .args(["task", "new", title])
            .current_dir(&repo)
            .env("ZEALOUS_BASE_URL", &endpoint)
            .env("ZEALOUS_API_KEY", "e2e-secret-key")
            .env("ZEALOUS_MODEL", "mock-model")
            .output()
            .unwrap();
        ids.push(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .find_map(|l| l.strip_prefix("task created: "))
                .unwrap()
                .trim()
                .to_string(),
        );
    }
    let out = Command::new(zealous)
        .args([
            "parallel",
            "run",
            ids[0].as_str(),
            ids[1].as_str(),
            "--auto-apply",
            "--yes",
        ])
        .current_dir(&repo)
        .env("ZEALOUS_BASE_URL", &endpoint)
        .env("ZEALOUS_API_KEY", "e2e-secret-key")
        .env("ZEALOUS_MODEL", "mock-model")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));

    let out = Command::new(zealous)
        .args(["task", "merge-workspace", ids[1].as_str()])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );

    let out = Command::new(zealous)
        .args(["task", "resolve", ids[1].as_str(), "--yes"])
        .current_dir(&repo)
        .env("ZEALOUS_BASE_URL", &endpoint)
        .env("ZEALOUS_API_KEY", "e2e-secret-key")
        .env("ZEALOUS_MODEL", "mock-model")
        .output()
        .unwrap();
    let resolved = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{resolved}");
    assert!(resolved.contains("resolution_id:"), "{resolved}");
    assert!(resolved.contains("MODEL-RESOLVED"), "{resolved}");
    assert!(
        resolved.contains("resolved + staged: note.txt"),
        "{resolved}"
    );
    let workspace = repo.join(".zealous/merges").join(&ids[1]);
    assert!(workspace.exists());
    assert_eq!(
        std::fs::read_to_string(workspace.join("note.txt"))
            .unwrap()
            .trim_end(),
        "MODEL-RESOLVED"
    );
    assert_eq!(git(&repo, &["status", "--porcelain"]).trim(), "");

    let out = Command::new(zealous)
        .args(["task", "merge-continue", ids[1].as_str(), "--yes"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let continued = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{continued}");
    assert_eq!(
        std::fs::read_to_string(repo.join("note.txt"))
            .unwrap()
            .trim_end(),
        "MODEL-RESOLVED"
    );
    assert!(!workspace.exists());
    assert!(repo.join(".zealous/worktrees").join(&ids[1]).exists());
    let ledger = std::fs::read_to_string(
        repo.join(".zealous/tasks")
            .join(&ids[1])
            .join("events.jsonl"),
    )
    .unwrap();
    for needle in [
        "MergeResolutionStarted",
        "MergeResolutionProposed",
        "MergeResolutionDecided",
        "MergeResolutionApplied",
    ] {
        assert!(ledger.contains(needle), "missing {needle}");
    }
    assert!(!ledger.contains("e2e-secret-key"), "{ledger}");
}
