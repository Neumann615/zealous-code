//! `zealous serve` stdio 生命周期(spec 100):
//! stdin 一行 JSON 请求 → session 路由 → stdout 一行 JSON 响应/通知。
//! 同步 IO + 线程 + channel;broken pipe 干净退出;EOF 等待 worker。

pub mod protocol;
pub mod session;

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use protocol::{parse_error_response, parse_request, ErrorResponse, Notification, Response};
use session::Session;

pub fn run_command(repo_root: PathBuf) -> i32 {
    let (notif_tx, notif_rx) = mpsc::channel::<Notification>();
    let (request_tx, request_rx) = mpsc::channel::<String>();

    // stdin 读取线程:逐行送入请求通道;EOF 时发送 None 语义(通道关闭)
    let _stdin_reader = std::thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(l) => {
                    if request_tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        // request_tx 随线程结束 drop → 主循环据此感知 EOF
    });

    let mut session = Session::new(repo_root, notif_tx);
    let stdout = io::stdout();
    let mut out = stdout.lock();

    fn write_line(out: &mut dyn Write, line: &str) -> bool {
        // broken pipe → 返回 false,主循环干净退出(不产生敏感输出)
        out.write_all(line.as_bytes()).is_ok()
            && out.write_all(b"\n").is_ok()
            && out.flush().is_ok()
    }

    loop {
        // 1) 先冲刷通知(保持顺序:通知先于后续响应)
        while let Ok(note) = notif_rx.try_recv() {
            let Ok(json) = serde_json::to_string(&note) else {
                break;
            };
            if !write_line(&mut out, &json) {
                return 1;
            }
        }

        // 2) 请求(非阻塞轮询,通知得以在无请求时持续推送)
        match request_rx.try_recv() {
            Ok(line) => {
                let resp: Response = match parse_request(&line) {
                    Ok(req) => session.handle(req),
                    Err(_) => parse_error_response(&line),
                };
                let Ok(json) = serde_json::to_string(&resp) else {
                    continue;
                };
                if !write_line(&mut out, &json) {
                    return 1;
                }
                // shutdown:等待 worker(join),随后进程退出;
                // stdin 读取线程是 IO 管道,随进程结束,不 join(否则
                // 客户端不关 stdin 会永远挂起)
                if resp.ok && line.contains("\"shutdown\"") {
                    session.worker_finished();
                    return 0;
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                // stdin EOF:等待当前 worker 完成
                session.worker_finished();
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }

        std::thread::sleep(Duration::from_millis(10));
    }

    // EOF 后继续冲刷剩余通知直到通道关闭
    while let Ok(note) = notif_rx.try_recv() {
        if let Ok(json) = serde_json::to_string(&note) {
            let _ = write_line(&mut out, &json);
        }
    }
    0
}

// 保留 ErrorResponse 导入使用(parse_error_response 内部使用)
#[allow(dead_code)]
fn _assert_types(_: ErrorResponse, _: Response, _: Notification) {}
