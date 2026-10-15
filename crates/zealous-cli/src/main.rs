//! `zealous` CLI:V1 命令面(spec 00:启动/状态/恢复/审批/事件日志)。
//!
//! TUI(ratatui)与 app-server 模式是 cli crate 的后续形态(spec 50);
//! 当前为无 TUI 的最小可用命令面,审批经 stdin 交互完成。

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use zealous_core::{ApprovalDecision, TaskStatus};
use zealous_engine::approval::ApprovalSink;
use zealous_engine::executor::ToolExecutor;
use zealous_engine::orchestrator::Orchestrator;
use zealous_engine::provider::{OpenAiCompatProvider, ProviderConfig};
use zealous_engine::task_service::{CreateOutcome, TaskService};
use zealous_engine::tools::builtin_registry;

#[derive(Parser)]
#[command(name = "zealous", version, about = "本地优先的多 Agent 编程助手")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 任务操作(须在 Git 仓库内执行)
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
}

#[derive(Subcommand)]
enum TaskCmd {
    /// 创建任务并立即校验 Provider 配置
    New {
        /// 一句话描述工程任务
        title: String,
    },
    /// 列出全部任务
    List,
    /// 查看任务投影状态
    Status { task_id: String },
    /// 运行或恢复任务(审批经 stdin 交互;--yes/--no 用于脚本化)
    Run {
        task_id: String,
        /// 全部审批自动通过(仅限你完全信任的场合)
        #[arg(long)]
        yes: bool,
        /// 全部审批自动拒绝
        #[arg(long)]
        no: bool,
    },
    /// 查看任务事件账本(可审计)
    Events { task_id: String },
}

/// stdin 交互审批:完整展示将执行的动作,y/回车=批准,其余=拒绝
struct StdinApproval {
    auto: Option<ApprovalDecision>,
}

impl ApprovalSink for StdinApproval {
    fn decide(&self, call_id: &str, summary: &str) -> ApprovalDecision {
        if let Some(d) = self.auto {
            println!("[approval auto-{:?}] {call_id}: {summary}", d);
            return d;
        }
        println!("\n=== 审批请求 {call_id} ===\n{summary}\n批准? [y/N] ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_ok() {
            let t = line.trim().to_ascii_lowercase();
            if t == "y" || t == "yes" {
                ApprovalDecision::Approve
            } else {
                ApprovalDecision::Reject
            }
        } else {
            ApprovalDecision::Reject
        }
    }
}

fn repo_root() -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cwd: {e}"))?;
    if !cwd.join(".git").exists() {
        return Err("当前目录不是 Git 仓库(.git 不存在);任务必须在 Git 仓库内发起".into());
    }
    Ok(cwd)
}

fn svc() -> Result<TaskService, String> {
    Ok(TaskService::new(repo_root()?))
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::Task { cmd } => match cmd {
            TaskCmd::New { title } => cmd_new(&title),
            TaskCmd::List => cmd_list(),
            TaskCmd::Status { task_id } => cmd_status(&task_id),
            TaskCmd::Run { task_id, yes, no } => cmd_run(&task_id, yes, no),
            TaskCmd::Events { task_id } => cmd_events(&task_id),
        },
    };
    std::process::exit(code);
}

fn cmd_new(title: &str) -> i32 {
    let svc = match svc() {
        Ok(s) => s,
        Err(e) => return err(e),
    };
    match svc.create_task(title) {
        Ok(CreateOutcome::Ready { task_id, .. }) => {
            println!("task created: {task_id}");
            println!("next: zealous task run {task_id}");
            0
        }
        Ok(CreateOutcome::FailedAtCreation { task_id, reason }) => {
            println!("task created but failed immediately: {task_id}");
            println!("reason: {reason}");
            println!("修复配置后仍可 `zealous task run {task_id}` 恢复(账本已留档)");
            1
        }
        Err(e) => err(e.to_string()),
    }
}

fn cmd_list() -> i32 {
    let (items, skipped) = match svc().and_then(|s| s.list_tasks().map_err(|e| e.to_string())) {
        Ok(x) => x,
        Err(e) => return err(e),
    };
    if items.is_empty() && skipped.is_empty() {
        println!("(no tasks)");
        return 0;
    }
    for s in &items {
        println!("{:<40} {:?} {}", s.task_id.as_str(), s.status, s.title);
    }
    for (id, why) in &skipped {
        println!("{:<40} CORRUPT {}", id.as_str(), why);
    }
    0
}

fn cmd_status(task_id: &str) -> i32 {
    let svc = match svc() {
        Ok(s) => s,
        Err(e) => return err(e),
    };
    let tid = match zealous_core::TaskId::parse(task_id) {
        Ok(t) => t,
        Err(e) => return err(e.to_string()),
    };
    match svc.open_task(&tid) {
        Ok((_, proj, report)) => {
            if report.torn_tail {
                println!("[warn] 账本存在残缺尾行(上次进程中断残留),已忽略");
            }
            println!("task:   {}", proj.task_id.as_str());
            println!("title:  {}", proj.title);
            println!("status: {:?}", proj.status);
            if let Some(plan) = &proj.plan {
                println!("plan:   {plan}");
            }
            println!("runs:   {}", proj.runs.len());
            println!("pending approvals: {}", proj.pending_approvals.len());
            for c in &proj.tool_calls {
                println!("  call {} {:?} {:?}", c.call_id, c.tool, c.state);
            }
            if let Some((scope, msg)) = &proj.last_error {
                println!("last error [{scope}]: {msg}");
            }
            0
        }
        Err(e) => err(e.to_string()),
    }
}

fn cmd_run(task_id: &str, yes: bool, no: bool) -> i32 {
    let svc = match svc() {
        Ok(s) => s,
        Err(e) => return err(e),
    };
    let tid = match zealous_core::TaskId::parse(task_id) {
        Ok(t) => t,
        Err(e) => return err(e.to_string()),
    };
    let (ledger, proj, _) = match svc.open_task(&tid) {
        Ok(x) => x,
        Err(e) => return err(e.to_string()),
    };
    let ledger = Arc::new(ledger);
    if proj.pending_approvals.is_empty() && matches!(proj.status, TaskStatus::Running) {
        println!("任务已在运行中(可能有挂起审批未处理;如异常请查看 events)");
    }

    let config = match ProviderConfig::resolve() {
        Ok(c) => c,
        Err(e) => {
            return err(format!(
                "provider 配置无效(可用 env ZEALOUS_BASE_URL/API_KEY/MODEL): {e}"
            ))
        }
    };
    println!(
        "provider: {} model={} (key 不回显)",
        config.base_url, config.model
    );
    let model = config.model.clone();
    let provider = Arc::new(OpenAiCompatProvider::new(config));
    let auto = if yes {
        Some(ApprovalDecision::Approve)
    } else if no {
        Some(ApprovalDecision::Reject)
    } else {
        None
    };
    let sink: Arc<dyn ApprovalSink> = Arc::new(StdinApproval { auto });
    let executor = Arc::new(ToolExecutor::new(
        repo_root().unwrap(),
        builtin_registry(),
        sink,
    ));
    let orch = Orchestrator {
        provider,
        executor,
        abort: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        max_turns: 24,
        model,
    };
    let title = proj.title.clone();
    match orch.run_task(&ledger, &title) {
        Ok(status) => {
            println!("task finished: {status:?}");
            status_code(&status)
        }
        Err(e) => err(format!("{e}")),
    }
}

fn cmd_events(task_id: &str) -> i32 {
    let repo = match repo_root() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let tid = match zealous_core::TaskId::parse(task_id) {
        Ok(t) => t,
        Err(e) => return err(e.to_string()),
    };
    let path = zealous_engine::ledger::events_path(&zealous_engine::ledger::task_dir(&repo, &tid));
    match zealous_engine::ledger::read_events(&path) {
        Ok((events, report)) => {
            if report.torn_tail {
                println!("[warn] 尾行残缺已忽略(崩溃残留)");
            }
            for ev in &events {
                println!(
                    "{:>4} {} [{}] {}",
                    ev.seq,
                    ev.ts,
                    ev.agent,
                    serde_json::to_string(&ev.payload).unwrap_or_default()
                );
            }
            0
        }
        Err(e) => err(e.to_string()),
    }
}

fn status_code(s: &TaskStatus) -> i32 {
    match s {
        TaskStatus::Completed => 0,
        _ => 1,
    }
}

fn err(msg: impl std::fmt::Display) -> i32 {
    eprintln!("error: {msg}");
    1
}
