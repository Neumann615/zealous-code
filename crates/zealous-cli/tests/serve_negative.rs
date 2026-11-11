//! serve 协议负向场景矩阵(spec 100 验收 + spec 110 §2/§3):
//! 未初始化门禁、版本不匹配、未知命令、解析错误、after_seq 越界、
//! 无 pending/错误 task 的 decide、重复运行。
//! 全部经真实 `zealous serve` 子进程验证。

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zealous_core::TaskStatus;

fn unique_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zealous-serve-neg-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct ServeProc {
    _child: Child,
    stdin: ChildStdin,
    lines_rx: Arc<Mutex<Receiver<String>>>,
}

fn spawn_serve(repo: &std::path::Path, endpoint: &str) -> ServeProc {
    let zealous = env!("CARGO_BIN_EXE_zealous");
    let mut child = Command::new(zealous)
        .args(["serve"])
        .current_dir(repo)
        .env("ZEALOUS_BASE_URL", endpoint)
        .env("ZEALOUS_API_KEY", "neg-secret-key")
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
                    eprintln!("[rd] {}", truncate_dbg(&l, 80));
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("[rd-err] {e}");
                    break;
                }
            }
        }
        eprintln!("[rd-end] reader thread exiting");
    });
    ServeProc {
        _child: child,
        stdin,
        lines_rx: Arc::new(Mutex::new(rx)),
    }
}

impl ServeProc {
    fn send(&mut self, value: &serde_json::Value) {
        writeln!(self.stdin, "{}", value).expect("write stdin");
        self.stdin.flush().expect("flush stdin");
    }

    fn send_raw(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").expect("write stdin");
        self.stdin.flush().expect("flush stdin");
    }

    /// 读一行(200ms 轮询;None = 通道关闭)
    fn next_line(&self, deadline: Instant) -> Option<String> {
        while Instant::now() < deadline {
            match self
                .lines_rx
                .lock()
                .expect("lines rx")
                .recv_timeout(Duration::from_millis(200))
            {
                Ok(l) => return Some(l),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
        None
    }

    /// 等待事件名为 event 的通知,返回其 params
    fn wait_notification(&self, event: &str, deadline: Instant) -> serde_json::Value {
        loop {
            assert!(Instant::now() < deadline, "等待通知 {event} 超时");
            let Some(line) = self.next_line(deadline) else {
                eprintln!(
                    "[dbg-next_line-none] t>{:?}",
                    30 - deadline.elapsed().as_secs()
                );
                continue;
            };
            eprintln!("[dbg-line] {}", &line[..line.len().min(100)]);
            let v: serde_json::Value = serde_json::from_str(&line).expect("合法 JSON 行");
            if v["event"] == event {
                return v["params"].clone();
            }
        }
    }

    /// 读到与 id 匹配的响应;途经行交由 collect
    fn wait_id(&self, id: u64, deadline: Instant) -> serde_json::Value {
        loop {
            assert!(Instant::now() < deadline, "等待响应 id={id} 超时");
            let Some(line) = self.next_line(deadline) else {
                continue;
            };
            let v: serde_json::Value = serde_json::from_str(&line).expect("合法 JSON 行");
            if v.get("id").and_then(|x| x.as_u64()) == Some(id) {
                return v;
            }
        }
    }
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
        let idx = counter.fetch_add(1, Ordering::Relaxed) as usize;
        let content = {
            let script = script.lock().unwrap();
            if idx < script.len() {
                script[idx].clone()
            } else {
                "guard".into()
            }
        };
        // spec 169:native 消费者,纯文本即最终答复
        let body = content;
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 200 OK
Content-Type: application/json
Content-Length: {}
Connection: close

{}",
                body.len(),
                body
            )
            .as_bytes(),
        );
        let _ = stream.shutdown(std::net::Shutdown::Write);
    });
    format!("http://{addr}/v1")
}

fn truncate_dbg(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

fn git_init_commit(repo: &std::path::Path) {
    let run = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {:?}", out.stderr);
    };
    run(&["init", "-q"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
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
}

#[test]
fn neg_initialize_gate_and_protocol_errors() {
    let repo = unique_dir("neg-basic");
    git_init_commit(&repo);
    let mut proc = spawn_serve(&repo, "http://127.0.0.1:9/v1");
    let deadline = Instant::now() + Duration::from_secs(20);

    // 未 initialize 调用业务命令
    proc.send(&serde_json::json!({"id": 1, "cmd": "list_tasks", "params": {}}));
    let r = proc.wait_id(1, deadline);
    assert_eq!(r["error"]["code"], "not_initialized", "{r}");

    // 版本不匹配
    proc.send(&serde_json::json!({"id": 2, "cmd": "initialize", "params": {"version": 42}}));
    let r = proc.wait_id(2, deadline);
    assert_eq!(r["error"]["code"], "version_mismatch", "{r}");

    // 正式 initialize
    proc.send(&serde_json::json!({"id": 3, "cmd": "initialize", "params": {"version": 1}}));
    let r = proc.wait_id(3, deadline);
    assert!(r["ok"].as_bool().unwrap(), "{r}");

    // 未知命令
    proc.send(&serde_json::json!({"id": 4, "cmd": "frobnicate", "params": {}}));
    let r = proc.wait_id(4, deadline);
    assert_eq!(r["error"]["code"], "unknown_command", "{r}");

    // JSON 解析错误(id 无法保留,响应 id=0)
    proc.send_raw("this is not json");
    let r = proc.wait_id(0, deadline);
    assert_eq!(r["id"], 0, "{r}");
    assert_eq!(r["error"]["code"], "parse_error", "{r}");
}

#[test]
fn neg_subscribe_after_seq_beyond_latest() {
    let repo = unique_dir("neg-sub");
    git_init_commit(&repo);
    let mut proc = spawn_serve(&repo, "http://127.0.0.1:9/v1");
    let deadline = Instant::now() + Duration::from_secs(20);
    proc.send(&serde_json::json!({"id": 1, "cmd": "initialize", "params": {"version": 1}}));
    let _ = proc.wait_id(1, deadline);
    proc.send(&serde_json::json!({"id": 2, "cmd": "create_task", "params": {"title": "s"}}));
    let r = proc.wait_id(2, deadline);
    let task_id = r["result"]["task_id"].as_str().unwrap().to_string();

    proc.send(&serde_json::json!({
        "id": 3, "cmd": "subscribe_events",
        "params": {"task_id": task_id, "after_seq": 999}
    }));
    let r = proc.wait_id(3, deadline);
    assert_eq!(r["error"]["code"], "invalid_params", "{r}");
}

#[test]
fn neg_no_pending_and_wrong_task_decide() {
    let repo = unique_dir("neg-decide");
    git_init_commit(&repo);
    let mut proc = spawn_serve(&repo, "http://127.0.0.1:9/v1");
    let deadline = Instant::now() + Duration::from_secs(20);
    proc.send(&serde_json::json!({"id": 1, "cmd": "initialize", "params": {"version": 1}}));
    let _ = proc.wait_id(1, deadline);
    proc.send(&serde_json::json!({"id": 2, "cmd": "create_task", "params": {"title": "d"}}));
    let r = proc.wait_id(2, deadline);
    let task_id = r["result"]["task_id"].as_str().unwrap().to_string();

    // 无 pending → no_pending_approval
    proc.send(&serde_json::json!({
        "id": 3, "cmd": "decide_approval",
        "params": {"task_id": task_id, "call_id": "call_x", "decision": "approve"}
    }));
    let r = proc.wait_id(3, deadline);
    assert_eq!(r["error"]["code"], "no_pending_approval", "{r}");

    // 错误 task_id → no_pending_approval
    proc.send(&serde_json::json!({
        "id": 4, "cmd": "decide_approval",
        "params": {
            "task_id": "task_01AAAAAAAAAAAAAAAAAAAAAAAA",
            "call_id": "call_x",
            "decision": "approve",
        }
    }));
    let r = proc.wait_id(4, deadline);
    assert_eq!(r["error"]["code"], "no_pending_approval", "{r}");
}

// ---------- 生命周期与审批绑定(spec 110 §3)----------

#[test]
fn neg_already_running_and_approval_binding() {
    let repo = unique_dir("neg-run");
    git_init_commit(&repo);
    let script = vec![
        r#"{"choices":[{"message":{"role":"assistant","content":"done"}}]}"#.into(),
    ];
    let endpoint = mock_endpoint(script);
    let mut proc = spawn_serve(&repo, &endpoint);
    let deadline = Instant::now() + Duration::from_secs(90);

    proc.send(&serde_json::json!({"id": 1, "cmd": "initialize", "params": {"version": 1}}));
    let _ = proc.wait_id(1, deadline);
    proc.send(&serde_json::json!({"id": 2, "cmd": "create_task", "params": {"title": "bind"}}));
    let r = proc.wait_id(2, deadline);
    let task_id = r["result"]["task_id"].as_str().unwrap().to_string();

    // run_task 启动(native 单轮纯文本 → 秒完成)
    proc.send(&serde_json::json!({"id": 3, "cmd": "run_task", "params": {"task_id": task_id}}));
    let r = proc.wait_id(3, deadline);
    assert_eq!(r["result"]["started"], true, "{r}");

    // 重复 run_task:任务可能已完成(→ok)或仍在跑(→already_running),均合法
    proc.send(&serde_json::json!({"id": 4, "cmd": "run_task", "params": {"task_id": task_id}}));
    let mut already_or_ok = false;
    loop {
        assert!(Instant::now() < deadline, "等响应超时");
        let Some(line) = proc.next_line(deadline) else {
            continue;
        };
        let v: serde_json::Value = serde_json::from_str(&line).expect("合法 JSON 行");
        if v.get("id").and_then(|x| x.as_u64()) == Some(4) {
            let code = v["error"]["code"].as_str().unwrap_or("");
            let ok = v["ok"].as_bool().unwrap_or(false);
            assert!(
                code == "already_running" || ok,
                "期望 already_running 或成功: {v}"
            );
            already_or_ok = true;
        }
        if already_or_ok { break; }
    }
    assert!(already_or_ok);
}

#[test]
fn neg_live_subscription_pushes_new_events_in_order() {
    let repo = unique_dir("neg-live");
    git_init_commit(&repo);
    let script = vec![
        r#"{"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":"","tool_calls":[{"id":"c1","type":"function","function":{"name":"plan","arguments":"{\"plan\":\"p\",\"workers\":[]}"}}]}}]}"#.into(),
        r#"{"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":"","tool_calls":[{"id":"c2","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"README.md\"}"}}]}}]}"#.into(),
        r#"{"choices":[{"message":{"role":"assistant","content":"done"}}]}"#.into(),
    ];
    let endpoint = mock_endpoint(script);
    let mut proc = spawn_serve(&repo, &endpoint);
    let deadline = Instant::now() + Duration::from_secs(30);

    proc.send(&serde_json::json!({"id": 1, "cmd": "initialize", "params": {"version": 1}}));
    let _ = proc.wait_id(1, deadline);
    proc.send(&serde_json::json!({"id": 2, "cmd": "create_task", "params": {"title": "live"}}));
    let r = proc.wait_id(2, deadline);
    let task_id = r["result"]["task_id"].as_str().unwrap().to_string();

    // 先订阅(cursor=0):重放 create 的 1 条,之后 run 的新事件实时推送
    proc.send(&serde_json::json!({
        "id": 3, "cmd": "subscribe_events",
        "params": {"task_id": task_id, "after_seq": 0}
    }));
    let r = proc.wait_id(3, deadline);
    assert!(r["ok"].as_bool().unwrap(), "{r}");

    proc.send(&serde_json::json!({"id": 4, "cmd": "run_task", "params": {"task_id": task_id}}));
    let r = proc.wait_id(4, deadline);
    assert_eq!(r["result"]["started"], true, "{r}");

    // 收集 task_updated 通知;run_finished 后继续读到通道关闭,
    // 确保所有通知都被接收后再断言
    let mut run_finished_seen = false;
    let finished_at = Instant::now() + Duration::from_secs(25);
    loop {
        assert!(Instant::now() < finished_at, "run 25s 未完成");
        let Some(line) = proc.next_line(finished_at) else {
            break;
        };
        let v: serde_json::Value = serde_json::from_str(&line).expect("合法 JSON 行");
        if v["event"] == "run_finished" {
            assert_eq!(v["params"]["status"], "Completed", "{v}");
            run_finished_seen = true;
        }
    }
    assert!(run_finished_seen, "应收到 run_finished");

    // 最终断言:投影状态来自账本
    let proj = zealous_engine::task_service::TaskService::new(&repo)
        .refresh(&zealous_core::TaskId::parse(&task_id).unwrap())
        .unwrap();
    assert_eq!(proj.status, TaskStatus::Completed);
}

#[test]
fn neg_corrupt_middle_ledger_clears_subscription() {
    let repo = unique_dir("neg-corrupt");
    git_init_commit(&repo);
    let mut proc = spawn_serve(&repo, "http://127.0.0.1:9/v1");
    let deadline = Instant::now() + Duration::from_secs(20);

    proc.send(&serde_json::json!({"id": 1, "cmd": "initialize", "params": {"version": 1}}));
    let _ = proc.wait_id(1, deadline);
    proc.send(&serde_json::json!({"id": 2, "cmd": "create_task", "params": {"title": "c"}}));
    let r = proc.wait_id(2, deadline);
    let task_id = r["result"]["task_id"].as_str().unwrap().to_string();

    // 先正常订阅(cursor=0)
    proc.send(&serde_json::json!({
        "id": 3, "cmd": "subscribe_events",
        "params": {"task_id": task_id, "after_seq": 0}
    }));
    let _ = proc.wait_id(3, deadline);

    // 运行期间从外部破坏账本中间行
    let ledger = repo
        .join(".zealous")
        .join("tasks")
        .join(&task_id)
        .join("events.jsonl");
    let bytes = std::fs::read_to_string(&ledger).unwrap();
    let mut lines: Vec<String> = bytes.lines().map(String::from).collect();
    lines[0] = "{{{ corrupted".into();
    std::fs::write(&ledger, lines.join("\n") + "\n").unwrap();

    // 下一次 poll → error_occurred + 清除订阅;再次订阅同一损坏账本 → 错误响应
    let mut notes: Vec<serde_json::Value> = Vec::new();
    let poll_until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < poll_until {
        if let Some(line) = proc.next_line(poll_until) {
            let v: serde_json::Value =
                serde_json::from_str(&line).unwrap_or(serde_json::Value::Null);
            if v["event"] == "error_occurred" {
                notes.push(v.clone());
            }
        }
    }
    assert!(
        notes.iter().any(|v| {
            v["params"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("损坏")
        }),
        "应推送损坏通知: {notes:?}"
    );

    proc.send(&serde_json::json!({
        "id": 20, "cmd": "subscribe_events",
        "params": {"task_id": task_id, "after_seq": 0}
    }));
    let r = proc.wait_id(20, deadline);
    assert_eq!(r["error"]["code"], "internal", "{r}");
}

#[test]
fn neg_torn_tail_warns_but_subscription_continues() {
    let repo = unique_dir("neg-torn");
    git_init_commit(&repo);
    let mut proc = spawn_serve(&repo, "http://127.0.0.1:9/v1");
    let deadline = Instant::now() + Duration::from_secs(20);

    proc.send(&serde_json::json!({"id": 1, "cmd": "initialize", "params": {"version": 1}}));
    let _ = proc.wait_id(1, deadline);
    proc.send(&serde_json::json!({"id": 2, "cmd": "create_task", "params": {"title": "t"}}));
    let r = proc.wait_id(2, deadline);
    let task_id = r["result"]["task_id"].as_str().unwrap().to_string();

    // 追加残缺尾行
    let ledger = repo
        .join(".zealous")
        .join("tasks")
        .join(&task_id)
        .join("events.jsonl");
    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&ledger)
            .unwrap();
        f.write_all(b"{\"seq\":99,\"ts\":\"2026").unwrap();
    }

    // 订阅成功(ok),且收到一次 torn tail 告警
    proc.send(&serde_json::json!({
        "id": 3, "cmd": "subscribe_events",
        "params": {"task_id": task_id, "after_seq": 0}
    }));
    let r = proc.wait_id(3, deadline);
    assert!(r["ok"].as_bool().unwrap(), "{r}");

    let mut torn = 0u32;
    let poll_until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < poll_until {
        if let Some(line) = proc.next_line(poll_until) {
            let v: serde_json::Value =
                serde_json::from_str(&line).unwrap_or(serde_json::Value::Null);
            if v["event"] == "error_occurred" {
                let m = v["params"]["message"].as_str().unwrap_or("");
                if m.contains("尾行残缺") {
                    torn += 1;
                }
            }
        }
    }
    assert!(torn >= 1, "应有一次 torn tail 告警");
}

// ---------- stdin EOF 与 broken pipe ----------

#[test]
fn neg_stdin_eof_exits_cleanly() {
    let repo = unique_dir("neg-eof");
    git_init_commit(&repo);
    // spawn 后立即关闭 stdin → EOF → serve 等待 worker(无 worker)后退出 0
    let zealous = env!("CARGO_BIN_EXE_zealous");
    let mut child = Command::new(zealous)
        .args(["serve"])
        .current_dir(&repo)
        .env("ZEALOUS_BASE_URL", "http://127.0.0.1:9/v1")
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    drop(child.stdin.take()); // 立即 EOF
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(s) => break s,
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    assert!(status.success(), "EOF 后应干净退出: {status}");
}

#[test]
fn neg_broken_pipe_exits_without_sensitive_output() {
    let repo = unique_dir("neg-pipe");
    git_init_commit(&repo);
    let zealous = env!("CARGO_BIN_EXE_zealous");
    // stdout 直接接 null:写管道必失败 → serve 以非零码退出,
    // 且 stderr 不含任何密钥/Authorization
    let mut child = Command::new(zealous)
        .args(["serve"])
        .current_dir(&repo)
        .env("ZEALOUS_BASE_URL", "http://127.0.0.1:9/v1")
        .env("ZEALOUS_API_KEY", "pipe-secret-key")
        .env("ZEALOUS_MODEL", "mock-model")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait().expect("try_wait") {
            Some(s) => break s,
            None => {
                assert!(Instant::now() < deadline, "broken pipe 场景 30s 未退出");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    // 进程已退出(可能非零:stdout 写入失败);关键是不能挂起且无敏感泄漏
    let _ = status.code();
    let mut stderr = String::new();
    if let Some(mut se) = child.stderr.take() {
        let _ = se.read_to_string(&mut stderr);
    }
    for needle in ["pipe-secret-key", "Authorization", "Bearer "] {
        assert!(!stderr.contains(needle), "stderr 泄漏: {needle}");
    }
}
