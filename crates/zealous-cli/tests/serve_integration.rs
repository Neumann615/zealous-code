//! serve 协议集成测试(spec 100):真实 `zealous serve` 子进程
//! + mock OpenAI-compatible endpoint,走完整协议流:
//!
//! initialize → create_task → run_task → approval → run_finished → get_task
//!
//! 断言:文件修改、审批决策进账本、事件 seq 连续、输出不含密钥。

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Mutex;
use std::time::{Duration, Instant};

fn unique_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zealous-serve-{tag}-{}-{}",
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

/// 按请求顺序返回脚本的 mock endpoint
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

struct ServeProc {
    child: Child,
    stdin: ChildStdin,
    /// stdout 行(读线程 → channel)
    lines_rx: Receiver<String>,
}

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
        .expect("spawn zealous serve");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = mpsc::channel();
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
        stdin,
        lines_rx: rx,
    }
}

impl ServeProc {
    fn send(&mut self, value: &serde_json::Value) {
        writeln!(self.stdin, "{}", value).expect("write stdin");
        self.stdin.flush().expect("flush stdin");
    }

    /// 读到与 id 匹配的响应;途经的通知交由 collect 回调
    fn wait_response(
        &self,
        lines_rx: &Receiver<String>,
        id: u64,
        deadline: Instant,
        mut collect: impl FnMut(serde_json::Value),
    ) -> serde_json::Value {
        loop {
            assert!(Instant::now() < deadline, "等待响应 id={id} 超时");
            let Ok(line) = lines_rx.recv_timeout(Duration::from_millis(500)) else {
                continue;
            };
            let v: serde_json::Value = serde_json::from_str(&line).expect("合法 JSON 行");
            if v.get("id").and_then(|x| x.as_u64()) == Some(id) {
                return v;
            }
            collect(v);
        }
    }
}

#[test]
fn serve_protocol_full_flow_with_mock_model() {
    let repo = unique_dir("repo");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {:?}", out.stderr);
    };
    git(&["init", "-q"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    git(&["add", "-A"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "-m",
        "init",
    ]);

    let endpoint = mock_endpoint(vec![
        r#"{"action":"plan","plan":"read then edit README","workers":[]}"#.into(),
        r#"{"action":"tool","tool":"read_file","args":{"path":"README.md"}}"#.into(),
        r#"{"action":"tool","tool":"edit_file","args":{"path":"README.md","old_text":"fixture","new_text":"edited by serve"}}"#.into(),
        r#"{"action":"message","text":"serve flow done"}"#.into(),
    ]);

    let mut proc = spawn_serve(&repo, &endpoint);
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut notifications: Vec<serde_json::Value> = Vec::new();
    let mut next_id = 1u64;

    // ---- initialize ----
    let id = next_id;
    next_id += 1;
    proc.send(&serde_json::json!({"id": id, "cmd": "initialize", "params": {"version": 1}}));
    let resp = proc.wait_response(&proc.lines_rx, id, deadline, |n| notifications.push(n));
    assert!(resp["ok"].as_bool().unwrap(), "initialize: {resp}");

    // ---- 版本不匹配:新会话语义,显式失败 ----
    let id = next_id;
    next_id += 1;
    proc.send(&serde_json::json!({"id": id, "cmd": "initialize", "params": {"version": 99}}));
    let resp = proc.wait_response(&proc.lines_rx, id, deadline, |n| notifications.push(n));
    assert_eq!(resp["error"]["code"], "version_mismatch", "{resp}");

    // ---- create_task ----
    let id = next_id;
    next_id += 1;
    proc.send(&serde_json::json!({
        "id": id, "cmd": "create_task",
        "params": {"title": "serve: edit README", "brief": "把 fixture 改成 edited by serve"}
    }));
    let resp = proc.wait_response(&proc.lines_rx, id, deadline, |n| notifications.push(n));
    let task_id = resp["result"]["task_id"].as_str().unwrap().to_string();

    // ---- subscribe_events(after_seq=0 重放)----
    let id = next_id;
    next_id += 1;
    proc.send(&serde_json::json!({
        "id": id, "cmd": "subscribe_events",
        "params": {"task_id": task_id, "after_seq": 0}
    }));
    let resp = proc.wait_response(&proc.lines_rx, id, deadline, |n| notifications.push(n));
    assert!(resp["result"]["replayed"].as_u64().unwrap() >= 2, "{resp}");

    // ---- run_task ----
    let id = next_id;
    next_id += 1;
    proc.send(&serde_json::json!({
        "id": id, "cmd": "run_task", "params": {"task_id": task_id}
    }));
    let resp = proc.wait_response(&proc.lines_rx, id, deadline, |n| notifications.push(n));
    assert_eq!(resp["result"]["started"], true, "{resp}");

    // ---- 等审批通知 → decide_approval(approve)----
    let mut approved = false;
    loop {
        assert!(Instant::now() < deadline, "run 60s 未完成");
        let Ok(line) = proc.lines_rx.recv_timeout(Duration::from_millis(500)) else {
            continue;
        };
        let v: serde_json::Value = serde_json::from_str(&line).expect("合法 JSON 行");
        if v.get("event").and_then(|e| e.as_str()) == Some("approval_requested") {
            // 断言摘要含 exact old/new + 绑定要素(spec 110 §3)
            let params = &v["params"];
            let summary = params["summary"].as_str().unwrap();
            assert!(summary.contains("fixture"), "{summary}");
            assert!(summary.contains("edited by serve"), "{summary}");
            assert!(params["task_id"].as_str().is_some());
            assert!(params["tool"].as_str().is_some());
            assert!(params["risk"].as_str().is_some());
            let id = next_id;
            next_id += 1;
            proc.send(&serde_json::json!({
                "id": id, "cmd": "decide_approval",
                "params": {
                    "task_id": params["task_id"].clone(),
                    "call_id": params["call_id"].clone(),
                    "decision": "approve",
                }
            }));
            approved = true;
        }
        if v.get("event").and_then(|e| e.as_str()) == Some("run_finished") {
            assert_eq!(v["params"]["status"], "Completed", "{v}");
            break;
        }
        notifications.push(v);
    }
    assert!(approved, "应收到审批请求通知");

    // ---- get_task:最终状态来自投影 ----
    let id = next_id;
    next_id += 1;
    proc.send(&serde_json::json!({
        "id": id, "cmd": "get_task", "params": {"task_id": task_id}
    }));
    let resp = proc.wait_response(&proc.lines_rx, id, deadline, |n| notifications.push(n));
    assert_eq!(resp["result"]["status"], "Completed", "{resp}");

    // ---- 断言:文件修改 + 审批进账本 + seq 连续 ----
    assert_eq!(
        std::fs::read_to_string(repo.join("README.md")).unwrap(),
        "edited by serve\n"
    );
    let ledger = std::fs::read_to_string(
        repo.join(".zealous")
            .join("tasks")
            .join(&task_id)
            .join("events.jsonl"),
    )
    .unwrap();
    let events: Vec<serde_json::Value> = ledger
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    for (i, e) in events.iter().enumerate() {
        assert_eq!(e["seq"].as_u64().unwrap(), (i + 1) as u64, "seq 跳号");
    }
    assert!(
        events.iter().any(|e| e["type"] == "ApprovalDecided"),
        "审批决策应进入账本"
    );
    // 脱敏:输出与账本不含密钥
    for needle in ["serve-secret-key", "Authorization", "Bearer "] {
        assert!(!ledger.contains(needle), "账本泄漏: {needle}");
    }

    // ---- shutdown:等待 worker 后退出 ----
    let id = next_id;
    proc.send(&serde_json::json!({"id": id, "cmd": "shutdown", "params": {}}));
    let resp = proc.wait_response(&proc.lines_rx, id, deadline, |n| notifications.push(n));
    assert!(resp["ok"].as_bool().unwrap());
    let status = {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match proc.child.try_wait().expect("try_wait") {
                Some(s) => break s,
                None => {
                    assert!(Instant::now() < deadline, "shutdown 后 serve 15s 未退出");
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    };
    assert!(status.success(), "serve 退出码: {status}");

    // 密钥不进任何输出(serve 的 stdout 行已全部经过断言通道)
    assert!(
        !notifications
            .iter()
            .any(|n| n.to_string().contains("serve-secret-key")),
        "通知泄漏密钥"
    );
}

/// P1 回归:run_finished 后同一会话必须可再次 run_task(续跑)。
/// 此前 RunState 只在 shutdown/EOF 回收,二次 run_task 恒撞 already_running,
/// 中断/恢复与 provider 失败续跑都走不通。
#[test]
fn run_task_again_after_finish_in_same_session() {
    let repo = unique_dir("resume");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {:?}", out.stderr);
    };
    git(&["init", "-q"]);
    std::fs::write(repo.join("note.txt"), "hello\n").unwrap();
    git(&["add", "-A"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "-m",
        "init",
    ]);

    // 每次 run 两拍:规划轮 Message(单 Agent 方案)→ 执行轮 Message(结束)
    let msg = r#"{"action":"message","text":"run finished ok"}"#;
    let endpoint = mock_endpoint(vec![msg.into(), msg.into(), msg.into(), msg.into()]);

    let mut proc = spawn_serve(&repo, &endpoint);
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut notifications: Vec<serde_json::Value> = Vec::new();
    let mut next_id = 1u64;

    let id = next_id;
    next_id += 1;
    proc.send(&serde_json::json!({"id": id, "cmd": "initialize", "params": {"version": 1}}));
    let resp = proc.wait_response(&proc.lines_rx, id, deadline, |n| notifications.push(n));
    assert!(resp["ok"].as_bool().unwrap(), "initialize: {resp}");

    let id = next_id;
    next_id += 1;
    proc.send(&serde_json::json!({
        "id": id, "cmd": "create_task",
        "params": {"title": "resume same session", "brief": "say ok twice"}
    }));
    let resp = proc.wait_response(&proc.lines_rx, id, deadline, |n| notifications.push(n));
    let task_id = resp["result"]["task_id"].as_str().unwrap().to_string();

    let wait_finished = |proc: &mut ServeProc,
                         notifications: &mut Vec<serde_json::Value>,
                         next_id: &mut u64|
     -> String {
        let id = *next_id;
        *next_id += 1;
        proc.send(&serde_json::json!({
            "id": id, "cmd": "run_task", "params": {"task_id": task_id}
        }));
        let resp = proc.wait_response(&proc.lines_rx, id, deadline, |n| notifications.push(n));
        assert_eq!(
            resp["result"]["started"], true,
            "run_task 应启动(结束后必须可续跑,不能 already_running): {resp}"
        );
        loop {
            assert!(Instant::now() < deadline, "run_finished 60s 未到");
            let Ok(line) = proc.lines_rx.recv_timeout(Duration::from_millis(500)) else {
                continue;
            };
            let v: serde_json::Value = serde_json::from_str(&line).expect("合法 JSON 行");
            if v.get("event").and_then(|e| e.as_str()) == Some("run_finished") {
                return v["params"]["status"].as_str().unwrap().to_string();
            }
            notifications.push(v);
        }
    };

    // 第一轮结束后,同会话再次 run_task = 续跑(P1 修复的核心断言)
    let s1 = wait_finished(&mut proc, &mut notifications, &mut next_id);
    assert_eq!(s1, "Completed", "run 1");
    let s2 = wait_finished(&mut proc, &mut notifications, &mut next_id);
    assert_eq!(s2, "Completed", "run 2(续跑)");

    // 续跑语义:账本含 TaskResumed、seq 连续
    let ledger = std::fs::read_to_string(
        repo.join(".zealous")
            .join("tasks")
            .join(&task_id)
            .join("events.jsonl"),
    )
    .unwrap();
    let events: Vec<serde_json::Value> = ledger
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    for (i, e) in events.iter().enumerate() {
        assert_eq!(e["seq"].as_u64().unwrap(), (i + 1) as u64, "seq 跳号");
    }
    assert!(
        events.iter().any(|e| e["type"] == "TaskResumed"),
        "第二次 run_task 应落 TaskResumed"
    );

    let id = next_id;
    next_id += 1;
    proc.send(&serde_json::json!({"id": id, "cmd": "shutdown", "params": {}}));
    let resp = proc.wait_response(&proc.lines_rx, id, deadline, |n| notifications.push(n));
    assert!(resp["ok"].as_bool().unwrap());
    let _ = next_id;
}

#[test]
fn serve_parallel_two_tasks_approvals_and_status() {
    let repo = unique_dir("parallel");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {:?}", out.stderr);
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), ".zealous/\n").unwrap();
    std::fs::write(repo.join("one.txt"), "ONE-OLD\n").unwrap();
    std::fs::write(repo.join("two.txt"), "TWO-OLD\n").unwrap();
    git(&["add", "-A"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "-m",
        "init",
    ]);

    let endpoint = parallel_stage_endpoint();
    let mut proc = spawn_serve(&repo, &endpoint);
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut next_id = 1u64;

    let mut notifications: Vec<serde_json::Value> = Vec::new();
    macro_rules! request {
        ($cmd:expr, $params:expr) => {{
            let id = next_id;
            next_id += 1;
            proc.send(&serde_json::json!({"id": id, "cmd": $cmd, "params": $params}));
            let resp =
                proc.wait_response(&proc.lines_rx, id, deadline, |n| notifications.push(n));
            assert!(resp["ok"].as_bool().unwrap(), "{} failed: {resp}", $cmd);
            resp
        }};
    }

    request!("initialize", serde_json::json!({"version": 1}));
    let mut task_ids = Vec::new();
    for title in ["parallel one", "parallel two"] {
        let resp = request!("create_task", serde_json::json!({"title": title}));
        task_ids.push(resp["result"]["task_id"].as_str().unwrap().to_string());
    }
    let run_resp = request!(
        "run_parallel",
        serde_json::json!({"task_ids": task_ids, "auto_apply": true})
    );
    assert_eq!(run_resp["result"]["total"], 2, "{run_resp}");
    assert!(run_resp["result"]["base_commit"].as_str().is_some());

    // Engine serializes approvals, but an approval can race ahead of the
    // run_parallel response. Replay captured notifications before live stdout.
    let mut captured = std::mem::take(&mut notifications);
    let mut decided = 0;
    while decided < 2 {
        let v = if let Some(v) = captured.pop() {
            v
        } else if let Some(v) = notifications.pop() {
            v
        } else {
            loop {
                assert!(Instant::now() < deadline, "parallel approval timeout");
                let Ok(line) = proc.lines_rx.recv_timeout(Duration::from_millis(200)) else {
                    continue;
                };
                let v: serde_json::Value = serde_json::from_str(&line).unwrap();
                if v["event"].as_str() == Some("approval_requested") {
                    break v;
                }
            }
        };
        assert_eq!(v["event"].as_str(), Some("approval_requested"), "{v}");
        let id = next_id;
        next_id += 1;
        proc.send(&serde_json::json!({
            "id": id,
            "cmd": "decide_approval",
            "params": {
                "task_id": v["params"]["task_id"],
                "call_id": v["params"]["call_id"],
                "decision": "approve"
            }
        }));
        let resp = proc.wait_response(&proc.lines_rx, id, deadline, |n| notifications.push(n));
        assert!(resp["ok"].as_bool().unwrap(), "approval: {resp}");
        decided += 1;
    }

    let v = notifications
        .iter()
        .position(|v| {
            v["event"].as_str() == Some("progress")
                && v["params"]["level"].as_str() == Some("parallel_finished")
        })
        .map(|i| notifications.remove(i))
        .unwrap_or_else(|| loop {
            assert!(Instant::now() < deadline, "parallel_finished timeout");
            let Ok(line) = proc.lines_rx.recv_timeout(Duration::from_millis(200)) else {
                continue;
            };
            let v: serde_json::Value = serde_json::from_str(&line).unwrap();
            if v["event"].as_str() == Some("progress")
                && v["params"]["level"].as_str() == Some("parallel_finished")
            {
                break v;
            }
        });
    let line = v["params"]["line"].as_str().unwrap_or_default();
    assert!(line.contains("integrated=1"), "{line}");

    let status = request!("get_parallel_status", serde_json::json!({}));
    assert_eq!(status["result"]["total"], 2, "{status}");
    assert_eq!(
        status["result"]["completed"].as_array().map(Vec::len),
        Some(2),
        "{status}"
    );
    assert_eq!(
        status["result"]["applied"].as_array().map(Vec::len),
        Some(1),
        "{status}"
    );
    for id in &task_ids {
        let task = request!("get_task", serde_json::json!({"task_id": id}));
        assert_eq!(task["result"]["status"], "Completed", "{task}");
    }
    assert!(repo.join(".zealous/worktrees").join(&task_ids[1]).exists());
    assert!(std::fs::read_to_string(repo.join("one.txt"))
        .unwrap()
        .contains("ONE-DONE"));
    assert!(std::fs::read_to_string(repo.join("two.txt"))
        .unwrap()
        .contains("TWO-OLD"));

    // spec 158:app-server 显式三方合并第二个共享 base 的 Completed task。
    let merge = request!("merge_task", serde_json::json!({"task_id": task_ids[1]}));
    assert_eq!(merge["result"]["merged"], true, "{merge}");
    assert!(std::fs::read_to_string(repo.join("two.txt"))
        .unwrap()
        .contains("TWO-DONE"));
    let merged_task = request!("get_task", serde_json::json!({"task_id": task_ids[1]}));
    assert_eq!(
        merged_task["result"]["applied_strategy"], "ThreeWayMerge",
        "{merged_task}"
    );

    let shutdown_id = next_id;
    proc.send(&serde_json::json!({"id": shutdown_id, "cmd": "shutdown", "params": {}}));
    let resp = proc.wait_response(&proc.lines_rx, shutdown_id, deadline, |_| {});
    assert!(resp["ok"].as_bool().unwrap(), "{resp}");
}

fn parallel_stage_endpoint() -> String {
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
        let one = request.contains("parallel one");
        let content = if request.contains("tool `edit_file` result") {
            r#"{"action":"message","text":"done"}"#
        } else if request.contains("edit target") {
            if one {
                r#"{"action":"tool","tool":"edit_file","args":{"path":"one.txt","old_text":"ONE-OLD","new_text":"ONE-DONE"}}"#
            } else {
                r#"{"action":"tool","tool":"edit_file","args":{"path":"two.txt","old_text":"TWO-OLD","new_text":"TWO-DONE"}}"#
            }
        } else if one {
            r#"{"action":"plan","plan":"edit target one","workers":[]}"#
        } else {
            r#"{"action":"plan","plan":"edit target two","workers":[]}"#
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
fn serve_merge_workspace_conflict_continue_flow() {
    let repo = unique_dir("merge-ws");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {:?}", out.stderr);
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), ".zealous/\n").unwrap();
    std::fs::write(repo.join("note.txt"), "OLD\n").unwrap();
    git(&["add", "-A"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "-m",
        "init",
    ]);
    let endpoint = serve_conflict_endpoint();
    let mut proc = spawn_serve(&repo, &endpoint);
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut next_id = 1u64;
    let mut notes: Vec<serde_json::Value> = Vec::new();
    macro_rules! request {
        ($cmd:expr, $params:expr) => {{
            let id = next_id;
            next_id += 1;
            proc.send(&serde_json::json!({"id": id, "cmd": $cmd, "params": $params}));
            let resp = proc.wait_response(&proc.lines_rx, id, deadline, |n| notes.push(n));
            resp
        }};
    }
    request!("initialize", serde_json::json!({"version": 1}));
    let mut tasks = Vec::new();
    for title in ["serve conflict left", "serve conflict right"] {
        let resp = request!("create_task", serde_json::json!({"title": title}));
        tasks.push(resp["result"]["task_id"].as_str().unwrap().to_string());
    }
    let run = request!(
        "run_parallel",
        serde_json::json!({"task_ids": tasks, "auto_apply": true})
    );
    assert!(
        run["result"]["parallel_started"].as_bool().unwrap(),
        "{run}"
    );
    for _ in 0..2 {
        let value = notes
            .iter()
            .position(|n| n["event"].as_str() == Some("approval_requested"))
            .map(|i| notes.remove(i));
        let value = if let Some(value) = value {
            value
        } else {
            loop {
                assert!(Instant::now() < deadline, "approval timeout");
                let Ok(line) = proc.lines_rx.recv_timeout(Duration::from_millis(200)) else {
                    continue;
                };
                let value: serde_json::Value = serde_json::from_str(&line).unwrap();
                if value["event"].as_str() == Some("approval_requested") {
                    break value;
                }
            }
        };
        let id = next_id;
        next_id += 1;
        proc.send(&serde_json::json!({
            "id": id,
            "cmd": "decide_approval",
            "params": {
                "task_id": value["params"]["task_id"],
                "call_id": value["params"]["call_id"],
                "decision": "approve"
            }
        }));
        let resp = proc.wait_response(&proc.lines_rx, id, deadline, |n| notes.push(n));
        assert!(resp["ok"].as_bool().unwrap(), "{resp}");
    }

    loop {
        let finished = notes.iter().position(|n| {
            n["event"].as_str() == Some("progress")
                && n["params"]["level"].as_str() == Some("parallel_finished")
        });
        if finished.is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "parallel finish timeout");
        let Ok(line) = proc.lines_rx.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        if value["event"].as_str() == Some("progress")
            && value["params"]["level"].as_str() == Some("parallel_finished")
        {
            break;
        }
    }
    assert_eq!(
        std::fs::read_to_string(repo.join("note.txt"))
            .unwrap()
            .trim_end(),
        "LEFT"
    );

    let started = request!(
        "start_merge_workspace",
        serde_json::json!({"task_id": tasks[1]})
    );
    assert!(
        started["result"]["conflicted"].as_bool().unwrap(),
        "{started}"
    );
    assert_eq!(
        started["result"]["files"],
        serde_json::json!(["note.txt"]),
        "{started}"
    );
    let status = request!("get_merge_status", serde_json::json!({"task_id": tasks[1]}));
    assert_eq!(status["result"]["status"], "conflicted", "{status}");
    assert_eq!(
        status["result"]["unresolved_files"],
        serde_json::json!(["note.txt"]),
        "{status}"
    );
    let unresolved = request!(
        "continue_merge_workspace",
        serde_json::json!({"task_id": tasks[1]})
    );
    assert_eq!(
        unresolved["error"]["code"], "unresolved_conflicts",
        "{unresolved}"
    );

    let proposal = request!(
        "propose_merge_resolution",
        serde_json::json!({"task_id": tasks[1]})
    );
    let resolution_id = proposal["result"]["resolution_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        proposal["result"]["summary"].as_str().is_some(),
        "{proposal}"
    );
    assert_eq!(
        proposal["result"]["files"][0]["mode"], "resolved",
        "{proposal}"
    );
    let approved = request!(
        "approve_merge_resolution",
        serde_json::json!({"task_id": tasks[1], "resolution_id": resolution_id})
    );
    assert_eq!(
        approved["result"]["staged_files"],
        serde_json::json!(["note.txt"]),
        "{approved}"
    );
    let workspace = repo.join(".zealous/merges").join(&tasks[1]);
    assert_eq!(
        std::fs::read_to_string(workspace.join("note.txt"))
            .unwrap()
            .trim_end(),
        "SERVER-AI-RESOLVED"
    );
    let continued = request!(
        "continue_merge_workspace",
        serde_json::json!({"task_id": tasks[1]})
    );
    assert!(
        continued["result"]["merged"].as_bool().unwrap(),
        "{continued}"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("note.txt"))
            .unwrap()
            .trim_end(),
        "SERVER-AI-RESOLVED"
    );
    let task = request!("get_task", serde_json::json!({"task_id": tasks[1]}));
    assert_eq!(
        task["result"]["applied_strategy"], "ThreeWayMerge",
        "{task}"
    );
    assert_eq!(
        task["result"]["merge_workspace"]["status"], "integrated",
        "{task}"
    );
    let shutdown_id = next_id;
    proc.send(&serde_json::json!({"id": shutdown_id, "cmd": "shutdown", "params": {}}));
    let resp = proc.wait_response(&proc.lines_rx, shutdown_id, deadline, |_| {});
    assert!(resp["ok"].as_bool().unwrap(), "{resp}");
}

fn serve_conflict_endpoint() -> String {
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
                "summary":"Resolve by combining both semantic changes.",
                "files":[{"path":"note.txt","mode":"resolved","content":"SERVER-AI-RESOLVED\n","rationale":"combine both sides"}]
            }).to_string();
            let body = chat_body(&proposal);
            let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes());
            let _ = stream.shutdown(std::net::Shutdown::Write);
            continue;
        }
        let left = request.contains("serve conflict left");
        let content = if request.contains("tool `edit_file` result") {
            r#"{"action":"message","text":"done"}"#
        } else if request.contains("edit conflict") {
            if left {
                r#"{"action":"tool","tool":"edit_file","args":{"path":"note.txt","old_text":"OLD","new_text":"LEFT"}}"#
            } else {
                r#"{"action":"tool","tool":"edit_file","args":{"path":"note.txt","old_text":"OLD","new_text":"RIGHT"}}"#
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
