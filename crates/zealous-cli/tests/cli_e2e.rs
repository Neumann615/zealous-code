//! 真实端到端测试(spec 60 §6):本地 mock `/chat/completions` server
//! + 真实 `zealous` 子进程 + stdin 自动审批,验证
//!   HTTP Provider → 审批 → 工具执行 → 事件账本 的完整链路。
//!

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

fn unique_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zealous-e2e-{tag}-{}-{}",
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

/// 按请求顺序返回脚本的 mock endpoint(模型只决定"做什么",
/// 账本/工具/审批全部走真实代码)
fn mock_endpoint(script: Vec<String>) -> String {
    static NEXT: Mutex<()> = Mutex::new(());
    let counter = AtomicU32::new(0);
    let script: Mutex<Vec<String>> = Mutex::new(script);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || loop {
        let Ok((mut stream, _)) = listener.accept() else {
            break;
        };
        let mut buf = [0u8; 16384];
        let _ = stream.read(&mut buf);
        let _next = NEXT.lock();
        let idx = counter.fetch_add(1, Ordering::Relaxed) as usize;
        let content = {
            let script = script.lock().unwrap();
            if idx < script.len() {
                script[idx].clone()
            } else {
                "done guard".into()
            }
        };
        let body = chat_body(&content);
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
        // 优雅关闭,避免 RST 丢响应(spec 60 前期修复)
        let _ = stream.shutdown(std::net::Shutdown::Write);
        let mut sink = [0u8; 4096];
        while let Ok(n) = stream.read(&mut sink) {
            if n == 0 {
                break;
            }
        }
    });
    format!("http://{addr}/v1")
}

fn wait_exit(mut child: std::process::Child, secs: u64) -> std::process::ExitStatus {
    for _ in 0..(secs * 20) {
        match child.try_wait().expect("try_wait") {
            Some(status) => return status,
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
    let _ = child.kill();
    panic!("zealous 进程 {}s 未退出", secs);
}

#[test]
fn e2e_real_binary_plan_write_approve_complete() {
    // ---- 夹具:真实 git 仓库 + 初始 commit(脏工作区保护要求)----
    let repo = unique_dir("repo");
    let run = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?} failed: {:?}",
            args,
            out.stderr
        );
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

    // ---- mock endpoint:plan → read → write → message(spec 70 要求 read 步骤)----
    let endpoint = mock_endpoint(vec![
        r#"{"action":"plan","plan":"read README then create hello.txt","workers":[]}"#.into(),
        r#"{"action":"tool","tool":"read_file","args":{"path":"README.md"}}"#.into(),
        r#"{"action":"tool","tool":"write_file","args":{"path":"hello.txt","content":"hello from e2e"}}"#.into(),
        r#"{"action":"message","text":"e2e done"}"#.into(),
    ]);

    // ---- 通过真实二进制创建任务 ----
    let zealous = env!("CARGO_BIN_EXE_zealous");
    let out = Command::new(zealous)
        .args(["task", "new", "e2e: create hello.txt"])
        .current_dir(&repo)
        .env("ZEALOUS_BASE_URL", &endpoint)
        .env("ZEALOUS_API_KEY", "e2e-secret-key")
        .env("ZEALOUS_MODEL", "mock-model")
        .output()
        .expect("spawn task new");
    assert!(
        out.status.success(),
        "task new: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let task_id = stdout
        .lines()
        .find_map(|l| l.strip_prefix("task created: "))
        .expect("task id in stdout")
        .trim()
        .to_string();
    assert!(task_id.starts_with("task_"));

    // ---- 运行任务:stdin 自动输入审批 y(真实交互路径)----
    let mut child = Command::new(zealous)
        .args(["task", "run", &task_id])
        .current_dir(&repo)
        .env("ZEALOUS_BASE_URL", &endpoint)
        .env("ZEALOUS_API_KEY", "e2e-secret-key")
        .env("ZEALOUS_MODEL", "mock-model")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn task run");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        // 稍等审批提示出现再输入;提前写入也成立(行缓冲由 reader 消费)
        std::thread::sleep(std::time::Duration::from_millis(400));
        stdin.write_all(b"y\n").expect("write approval");
        stdin.flush().unwrap();
    }
    let status = wait_exit(child, 30);
    assert!(status.success(), "task run exit: {status}");

    // ---- 断言:文件修改存在 ----
    let hello = std::fs::read_to_string(repo.join("hello.txt")).unwrap();
    assert_eq!(hello, "hello from e2e");
    // 自动 commit 作为回退点(spec 60 §4)
    let log = Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&log.stdout);
    assert!(log.contains("zealous: apply hello.txt"), "git log: {log}");

    // ---- 断言:账本健康 + 事件链完整 + 密钥不落账 ----
    let ledger_path = repo
        .join(".zealous")
        .join("tasks")
        .join(&task_id)
        .join("events.jsonl");
    let text = std::fs::read_to_string(&ledger_path).unwrap();
    for needle in ["e2e-secret-key", "Authorization", "Bearer "] {
        assert!(!text.contains(needle), "账本泄漏敏感串: {needle}");
    }
    let events: Vec<serde_json::Value> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    // seq 连续
    for (i, ev) in events.iter().enumerate() {
        assert_eq!(ev["seq"].as_u64().unwrap(), (i + 1) as u64, "seq 跳号");
    }
    let types: Vec<&str> = events.iter().filter_map(|e| e["type"].as_str()).collect();
    for expected in [
        "TaskCreated",
        "TaskPlanUpdated",
        "ToolCallRequested",
        "ApprovalRequested",
        "ApprovalDecided",
        "ToolCallCompleted",
        "TaskStatusChanged",
    ] {
        assert!(types.contains(&expected), "缺少事件 {expected}: {types:?}");
    }
    // spec 70:脚本必须包含 read_file 步骤(先读后写)
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "ToolCallRequested" && e["tool"] == "read_file"),
        "缺少 read_file 调用"
    );
    let last = events.last().unwrap();
    assert_eq!(last["type"], "TaskStatusChanged");
    assert_eq!(last["to"], "Completed");
}

/// 读取仓库根 .env(真实 smoke 专用);不打印任何值
fn load_repo_dotenv() -> Vec<(String, String)> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.env");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("real_model_smoke 需要仓库根 .env({path:?}): {e}"));
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                return None;
            }
            let (k, v) = l.split_once('=')?;
            let v = v.trim().trim_matches('"').to_string();
            if v.is_empty() {
                return None;
            }
            Some((k.trim().to_string(), v))
        })
        .collect()
}

/// 真实端点 smoke(spec 70 §5):默认 ignored,显式执行:
///   cargo test --test cli_e2e real_model_smoke -- --ignored --nocapture
/// 配置来自仓库根 .env;不复制 .env 进临时仓库,只注入必要环境变量;
/// 任何输出不得包含密钥。
#[test]
#[ignore = "真实端点 smoke:消耗模型额度,须显式执行(见 spec 70 §5)"]
fn real_model_smoke() {
    let cfg = load_repo_dotenv();
    assert!(
        cfg.iter().any(|(k, _)| k == "ZEALOUS_BASE_URL"),
        ".env 缺 ZEALOUS_BASE_URL"
    );
    let api_key = cfg
        .iter()
        .find(|(k, _)| k == "ZEALOUS_API_KEY")
        .map(|(_, v)| v.clone())
        .expect(".env 缺 ZEALOUS_API_KEY");

    // ---- 夹具:临时 git 仓库 + 初始 commit + README ----
    let repo = unique_dir("smoke");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {:?}", out.stderr);
    };
    git(&["init", "-q"]);
    let readme_first_line = "alpha smoke fixture line one";
    std::fs::write(
        repo.join("README.md"),
        format!("{readme_first_line}\nfixture body\n"),
    )
    .unwrap();
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

    // ---- 创建任务(注入 env,不复制 .env)----
    let zealous = env!("CARGO_BIN_EXE_zealous");
    let with_env = |cmd: &mut Command| {
        for (k, v) in &cfg {
            cmd.env(k, v);
        }
    };
    let mut new_cmd = Command::new(zealous);
    new_cmd
        .args(["task", "new", "smoke: 单一简单任务,不要创建 Worker,直接完成。先读取 README.md,然后创建 alpha.txt,其内容必须恰好等于 README.md 的第一行文字(仅这一行,不得包含 README 其余内容)。完成后用 message 汇报。"])
        .current_dir(&repo)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    with_env(&mut new_cmd);
    let out = new_cmd.output().expect("spawn task new");
    assert!(out.status.success(), "task new failed: {:?}", out.stderr);
    let task_id = String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("task created: "))
        .expect("task id")
        .trim()
        .to_string();

    // ---- 运行:stdin 注入 y 审批写入(spec 70:不用 --yes,走真实审批)----
    let mut run_cmd = Command::new(zealous);
    run_cmd
        .args(["task", "run", &task_id])
        .current_dir(&repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    with_env(&mut run_cmd);
    let mut child = run_cmd.spawn().expect("spawn task run");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        // 审批可能不止一个(写入 + 模型自行追加的验证命令):预写足量 y,
        // 多余行被 reader 消费后无害
        std::thread::sleep(std::time::Duration::from_millis(500));
        for _ in 0..6 {
            stdin.write_all(b"y\n").expect("write approval");
            stdin.flush().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }
    // 先取走管道再等待退出(wait_exit 消费 Child)
    let mut stdout_pipe = child.stdout.take().expect("stdout");
    let mut stderr_pipe = child.stderr.take().expect("stderr");
    let status = wait_exit(child, 600);
    let mut run_stdout = String::new();
    let _ = stdout_pipe.read_to_string(&mut run_stdout);
    let mut run_stderr = String::new();
    let _ = stderr_pipe.read_to_string(&mut run_stderr);

    // 脱敏断言:输出不含凭据(spec 70 §5)
    for output in [&run_stdout, &run_stderr] {
        assert!(!output.contains(&api_key), "输出泄漏 API key");
        assert!(!output.contains("Authorization"), "输出泄漏请求头名");
    }
    assert!(
        status.success(),
        "task run exit: {status}\n{run_stdout}\n{run_stderr}"
    );

    // ---- 断言:产物 + 账本 + 回退 commit ----
    let alpha = std::fs::read_to_string(repo.join("alpha.txt")).unwrap();
    assert_eq!(
        alpha.trim(),
        readme_first_line,
        "alpha.txt 应为 README 首行"
    );

    let ledger_path = repo
        .join(".zealous")
        .join("tasks")
        .join(&task_id)
        .join("events.jsonl");
    let text = std::fs::read_to_string(&ledger_path).unwrap();
    for needle in [&api_key, "Authorization", "Bearer "] {
        assert!(!text.contains(needle), "账本泄漏敏感串: {needle}");
    }
    let events: Vec<serde_json::Value> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    for (i, ev) in events.iter().enumerate() {
        assert_eq!(ev["seq"].as_u64().unwrap(), (i + 1) as u64, "seq 跳号");
    }
    for expected in ["read_file", "write_file"] {
        assert!(
            events
                .iter()
                .any(|e| e["type"] == "ToolCallRequested" && e["tool"] == expected),
            "缺少 {expected} 调用"
        );
    }
    assert_eq!(events.last().unwrap()["to"], "Completed");

    let log = Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&log.stdout).contains("zealous: apply alpha.txt"),
        "缺少 zealous 回退 commit"
    );
}
