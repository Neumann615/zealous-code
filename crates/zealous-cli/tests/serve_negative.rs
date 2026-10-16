//! serve 协议负向场景矩阵(spec 100 验收 + spec 110 §2/§3):
//! 未初始化门禁、版本不匹配、未知命令、解析错误、after_seq 越界、
//! 无 pending/错误 task 的 decide、重复运行。
//! 全部经真实 `zealous serve` 子进程验证。

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
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
