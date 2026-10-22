//! `zealous` CLI:V1 命令面(spec 00:启动/状态/恢复/审批/事件日志)。
//!
//! TUI(ratatui)与 app-server 模式是 cli crate 的后续形态(spec 50);
//! 当前为无 TUI 的最小可用命令面,审批经 stdin 交互完成。

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::{Parser, Subcommand};
mod serve;
mod tui;
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
    /// 可视化任务工作台(spec 90)
    Tui,
    /// app-server:stdio JSON 协议(spec 100)
    Serve,
    /// 配置检查(Git 仓库 / Provider 配置;不回显密钥)
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// 检查 Git 仓库与 OpenAI-compatible 配置
    Check,
}

#[derive(Subcommand)]
enum TaskCmd {
    /// 创建任务并立即校验 Provider 配置
    New {
        /// 一句话描述工程任务
        title: String,
        /// 任务简报文件(须位于当前 Git 仓库内,spec 80 §1)
        #[arg(long = "file")]
        brief_file: Option<String>,
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
        /// 只保留审批请求、错误与最终结果(spec 80 §5)
        #[arg(long)]
        quiet: bool,
        /// 在任务专属 worktree 中执行(spec 155,opt-in)
        #[arg(long)]
        isolate: bool,
    },
    /// 查看任务事件账本(可审计)
    Events { task_id: String },
    /// 将已完成 isolated 任务 fast-forward 应用回主工作区(spec 155)
    Apply {
        task_id: String,
        /// 跳过确认(仅限你已确认变更内容)
        #[arg(long)]
        yes: bool,
    },
    /// 查看 isolated 任务 workspace 状态(spec 155)
    Workspace { task_id: String },
    /// 清理 isolated 任务 worktree 与分支(spec 155)
    WorkspaceRemove {
        task_id: String,
        /// worktree 有用户未提交改动时强制删除
        #[arg(long)]
        force: bool,
        /// 跳过确认
        #[arg(long)]
        yes: bool,
    },
}

/// stdin 交互审批:完整展示将执行的动作,y/回车=批准,其余=拒绝
/// 审批 sink:后台线程读 stdin + channel,decide 以 100ms 轮询消费;
/// abort 置位后立即拒绝并返回(spec 60 §3:Ctrl-C 不被阻塞的审批卡住)。
struct StdinApproval {
    auto: Option<ApprovalDecision>,
    rx: Mutex<mpsc::Receiver<String>>,
    abort: Arc<AtomicBool>,
}

impl StdinApproval {
    /// 真实 stdin 模式:auto 时无需读 stdin,不启动读线程
    fn new(auto: Option<ApprovalDecision>, abort: Arc<AtomicBool>) -> Self {
        let (tx, rx) = mpsc::channel();
        if auto.is_none() {
            std::thread::spawn(move || {
                let mut line = String::new();
                loop {
                    line.clear();
                    match std::io::stdin().read_line(&mut line) {
                        Ok(0) | Err(_) => {
                            let _ = tx.send("__eof__".into());
                            return;
                        }
                        Ok(_) => {
                            if tx.send(line.trim().to_string()).is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        }
        Self {
            auto,
            rx: Mutex::new(rx),
            abort,
        }
    }

    fn parse_decision(line: &str) -> Option<ApprovalDecision> {
        let t = line.trim().to_ascii_lowercase();
        match t.as_str() {
            "y" | "yes" => Some(ApprovalDecision::Approve),
            "n" | "no" => Some(ApprovalDecision::Reject),
            _ => None,
        }
    }
}

impl ApprovalSink for StdinApproval {
    fn decide(&self, info: &zealous_engine::approval::ApprovalRequestInfo) -> ApprovalDecision {
        let (call_id, summary) = (info.call_id.as_str(), info.summary.as_str());
        if let Some(d) = self.auto {
            println!(
                "[approval auto-{:?}] {call_id}(工具 {} 风险 {:?}): {summary}",
                d, info.tool, info.risk
            );
            return d;
        }
        println!(
            "\n=== 审批请求 {call_id}(工具 {} 风险 {:?})===\n{summary}\n批准? [y/N](Ctrl-C 中断任务)",
            info.tool, info.risk
        );
        let _ = std::io::stdout().flush();
        let rx = self.rx.lock().unwrap();
        loop {
            if self.abort.load(Ordering::Relaxed) {
                println!("[interrupted] 审批按拒绝收尾");
                return ApprovalDecision::Reject;
            }
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(line) => {
                    if line.as_str() == "__eof__" {
                        return ApprovalDecision::Reject;
                    }
                    match Self::parse_decision(&line) {
                        Some(d) => return d,
                        // 无效输入:重新提示
                        None => {
                            print!("请输入 y 或 n: ");
                            let _ = std::io::stdout().flush();
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return ApprovalDecision::Reject,
            }
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

/// 加载当前目录下的 `.env`(存在时):KEY=VALUE,已存在的进程环境变量
/// 优先、不被覆盖。密钥因此可放在项目根且不入库(.gitignore 已忽略 .env)。
fn load_dotenv() {
    let Ok(text) = std::fs::read_to_string(".env") else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        if k.is_empty() {
            continue;
        }
        let mut v = v.trim();
        if v.len() >= 2
            && ((v.starts_with('"') && v.ends_with('"'))
                || (v.starts_with('\'') && v.ends_with('\'')))
        {
            v = &v[1..v.len() - 1];
        }
        if std::env::var_os(k).is_none() {
            std::env::set_var(k, v);
        }
    }
}

fn main() {
    load_dotenv();
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::Tui => match repo_root() {
            Ok(root) => tui::run_command(root),
            Err(e) => err(e),
        },
        Cmd::Serve => match repo_root() {
            Ok(root) => serve::run_command(root),
            Err(e) => err(e),
        },
        Cmd::Task { cmd } => match cmd {
            TaskCmd::New { title, brief_file } => cmd_new(&title, brief_file.as_deref()),
            TaskCmd::List => cmd_list(),
            TaskCmd::Status { task_id } => cmd_status(&task_id),
            TaskCmd::Run {
                task_id,
                yes,
                no,
                quiet,
                isolate,
            } => cmd_run(&task_id, yes, no, quiet, isolate),
            TaskCmd::Events { task_id } => cmd_events(&task_id),
            TaskCmd::Apply { task_id, yes } => cmd_apply(&task_id, yes),
            TaskCmd::Workspace { task_id } => cmd_workspace(&task_id),
            TaskCmd::WorkspaceRemove {
                task_id,
                force,
                yes,
            } => cmd_workspace_remove(&task_id, force, yes),
        },
        Cmd::Config {
            cmd: ConfigCmd::Check,
        } => cmd_config_check(),
    };
    std::process::exit(code);
}

fn cmd_config_check() -> i32 {
    let mut ok = true;
    match repo_root() {
        Ok(root) => println!("[ok] git repo: {}", root.display()),
        Err(e) => {
            println!("[fail] {e}");
            ok = false;
        }
    }
    match ProviderConfig::resolve() {
        Ok(cfg) => {
            println!(
                "[ok] base_url: {}(应指向 OpenAI-compatible 根路径,如 https://example/v1)",
                cfg.base_url
            );
            println!("[ok] model: {}", cfg.model);
            println!(
                "[ok] api_key: configured (len={}, 不回显)",
                cfg.api_key.len()
            );
            println!(
                "[ok] timeout={}s max_retries={}",
                cfg.timeout_secs, cfg.max_retries
            );
        }
        Err(e) => {
            println!("[fail] provider 配置:{e}");
            println!("       设置 env ZEALOUS_BASE_URL / ZEALOUS_API_KEY / ZEALOUS_MODEL,");
            println!("       或写入 ~/.zealous/config.json(env 优先)");
            ok = false;
        }
    }
    if ok {
        println!("config check: all good");
        0
    } else {
        1
    }
}

fn cmd_new(title: &str, brief_file: Option<&str>) -> i32 {
    let svc = match svc() {
        Ok(s) => s,
        Err(e) => return err(e),
    };
    let brief_path = brief_file.map(std::path::PathBuf::from);
    match svc.create_task_with_brief(title, brief_path.as_deref()) {
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
            if let Some(brief) = &proj.brief {
                let summary: String = brief.chars().take(80).collect();
                let ellipsis = if brief.chars().count() > 80 {
                    "…"
                } else {
                    ""
                };
                println!("brief:  {summary}{ellipsis}");
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

/// CLI 进度 sink:默认全量,--quiet 只保留 important(审批/错误/最终)
struct CliProgress {
    quiet: bool,
}

impl zealous_engine::progress::ProgressSink for CliProgress {
    fn progress(&self, line: &str) {
        if !self.quiet {
            println!("{line}");
        }
    }
    fn important(&self, line: &str) {
        println!("{line}");
    }
}

fn cmd_run(task_id: &str, yes: bool, no: bool, quiet: bool, isolate: bool) -> i32 {
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
    // spec 155:运行根解析(模式规则落账;isolated 校验/重建在此完成)
    let run_root = match zealous_engine::workspace::ensure_run_root(
        repo_root().unwrap_or_default().as_path(),
        &ledger,
        &tid,
        isolate,
    ) {
        Ok(r) => r,
        Err(e) => return err(format!("workspace: {e}")),
    };
    if run_root != repo_root().unwrap_or_default() {
        let ws = proj.execution_workspace.as_ref();
        if let Some(ws) = ws {
            println!(
                "[isolated] worktree: {}\n[isolated] branch: {} (base {} @ {})",
                ws.path, ws.branch, ws.base_branch, ws.base_commit
            );
        }
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
    let abort = Arc::new(AtomicBool::new(false));
    // spec 60 §3:handler 只设标志,不做 IO;审批轮询与轮次边界响应 abort
    if let Err(e) = ctrlc::set_handler({
        let abort = abort.clone();
        move || abort.store(true, Ordering::Relaxed)
    }) {
        return err(format!("install ctrl-c handler: {e}"));
    }
    let sink: Arc<dyn ApprovalSink> = Arc::new(StdinApproval::new(auto, abort.clone()));
    let executor = Arc::new(ToolExecutor::new(
        run_root.clone(),
        builtin_registry(),
        sink,
    ));
    let orch = Orchestrator {
        provider,
        executor,
        abort,
        max_turns: 24,
        model,
        progress: Some(Arc::new(CliProgress { quiet })),
    };
    let mut title = proj.title.clone();
    if run_root != repo_root().unwrap_or_default() {
        // spec 155:isolated 运行必须向模型声明执行环境边界
        title = format!(
            "[isolated workspace] 本次运行在任务专属 git worktree 中执行;\n主工作区的未提交改动在此不可见,不得尝试访问或修改主工作区。\n\n{title}"
        );
    }
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

fn git_out(dir: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("spawn git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into())
}

fn cmd_apply(task_id: &str, yes: bool) -> i32 {
    let repo = match repo_root() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let tid = match zealous_core::TaskId::parse(task_id) {
        Ok(t) => t,
        Err(e) => return err(e.to_string()),
    };
    let svc = match svc() {
        Ok(s) => s,
        Err(e) => return err(e),
    };
    let (ledger, proj, _) = match svc.open_task(&tid) {
        Ok(x) => x,
        Err(e) => return err(e.to_string()),
    };
    let ledger = std::sync::Arc::new(ledger);
    let ws = match &proj.execution_workspace {
        Some(w) => w.clone(),
        None => return err("任务不是 isolated 模式"),
    };
    if let Some(applied) = &proj.applied_commit {
        println!("已应用过(幂等): {applied}");
        return 0;
    }
    if proj.status != TaskStatus::Completed {
        return err("任务尚未 Completed,不能 apply");
    }
    // 预览(spec 155:base / tip / commits / 变更文件)
    let tip = match git_out(&repo, &["rev-parse", &format!("refs/heads/{}", ws.branch)]) {
        Ok(t) => t.trim().to_string(),
        Err(e) => return err(e),
    };
    println!("base:   {} @ {}", ws.base_branch, ws.base_commit);
    println!("branch: {} @ {}", ws.branch, tip);
    println!("将进入主分支的 commits:");
    match git_out(
        &repo,
        &[
            "log",
            "--oneline",
            &format!("{}..{}", ws.base_commit, ws.branch),
        ],
    ) {
        Ok(l) => print!("{}", l),
        Err(e) => return err(e),
    }
    println!("变更文件:");
    match git_out(
        &repo,
        &[
            "diff",
            "--name-status",
            &format!("{}..{}", ws.base_commit, ws.branch),
        ],
    ) {
        Ok(l) => print!("{}", l),
        Err(e) => return err(e),
    }
    if !yes {
        print!("确认应用? [y/N] ");
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return err("读取确认失败");
        }
        let t = line.trim().to_ascii_lowercase();
        if t != "y" && t != "yes" {
            println!("已取消");
            return 0;
        }
    }
    match zealous_engine::workspace::apply(&repo, &ledger) {
        Ok(out) => {
            println!(
                "applied: {} @ {}{}",
                out.branch,
                out.applied_commit,
                if out.already { " (already)" } else { "" }
            );
            0
        }
        Err(e) => err(format!("apply: {e}")),
    }
}

fn cmd_workspace(task_id: &str) -> i32 {
    let repo = match repo_root() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let tid = match zealous_core::TaskId::parse(task_id) {
        Ok(t) => t,
        Err(e) => return err(e.to_string()),
    };
    let svc = match svc() {
        Ok(s) => s,
        Err(e) => return err(e),
    };
    let (_ledger, proj, _) = match svc.open_task(&tid) {
        Ok(x) => x,
        Err(e) => return err(e.to_string()),
    };
    let ws = match &proj.execution_workspace {
        Some(w) => w,
        None => return err("任务不是 isolated 模式"),
    };
    println!("mode:           isolated ({})", ws.kind);
    println!("path:           {}", ws.path);
    println!("branch:         {}", ws.branch);
    println!("base_branch:    {}", ws.base_branch);
    println!("base_commit:    {}", ws.base_commit);
    println!("current_commit: {}", ws.current_commit);
    println!("status:         {}", ws.status);
    if let Some(applied) = &proj.applied_commit {
        println!("applied_commit: {applied}");
    }
    println!("workspace_removed: {}", proj.workspace_removed);
    // 活分支 tip(来自 git 事实,仅展示)
    if let Ok(tip) = git_out(&repo, &["rev-parse", &format!("refs/heads/{}", ws.branch)]) {
        println!("branch_tip:     {}", tip.trim());
    }
    0
}

fn cmd_workspace_remove(task_id: &str, force: bool, yes: bool) -> i32 {
    let repo = match repo_root() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let tid = match zealous_core::TaskId::parse(task_id) {
        Ok(t) => t,
        Err(e) => return err(e.to_string()),
    };
    let svc = match svc() {
        Ok(s) => s,
        Err(e) => return err(e),
    };
    let (ledger, proj, _) = match svc.open_task(&tid) {
        Ok(x) => x,
        Err(e) => return err(e.to_string()),
    };
    let ledger = std::sync::Arc::new(ledger);
    let ws = match &proj.execution_workspace {
        Some(w) => w.clone(),
        None => return err("任务不是 isolated 模式"),
    };
    if !yes {
        println!("将删除 worktree {} 与分支 {}。", ws.path, ws.branch);
        if force {
            println!("警告:--force 将丢弃 worktree 中的未提交改动!");
        }
        print!("确认删除? [y/N] ");
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return err("读取确认失败");
        }
        let t = line.trim().to_ascii_lowercase();
        if t != "y" && t != "yes" {
            println!("已取消");
            return 0;
        }
    }
    match zealous_engine::workspace::remove(&repo, &ledger, force) {
        Ok(info) => {
            println!("removed: {} ({})", info.branch, info.path.display());
            0
        }
        Err(e) => err(format!("workspace-remove: {e}")),
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

#[cfg(test)]
impl StdinApproval {
    fn with_rx(rx: mpsc::Receiver<String>, abort: Arc<AtomicBool>) -> Self {
        Self {
            auto: None,
            rx: Mutex::new(rx),
            abort,
        }
    }
}

#[cfg(test)]
mod approval_tests {
    use super::*;

    /// spec 60 §3:abort 置位后,阻塞中的审批等待必须立即以 Reject 退出
    #[test]
    fn abort_unblocks_pending_approval() {
        let abort = Arc::new(AtomicBool::new(false));
        let (_tx, rx) = mpsc::channel();
        let sink = StdinApproval::with_rx(rx, abort.clone());
        let sink = Arc::new(sink);
        let s2 = sink.clone();
        let handle = std::thread::spawn(move || {
            let info = zealous_engine::approval::ApprovalRequestInfo {
                call_id: "c1".into(),
                tool: "shell".into(),
                risk: zealous_core::RiskTier::T2,
                summary: "shell: echo".into(),
            };
            s2.decide(&info)
        });
        std::thread::sleep(Duration::from_millis(150));
        assert!(!handle.is_finished(), "无决策且无 abort 时应保持阻塞");
        abort.store(true, Ordering::Relaxed);
        let d = handle.join().unwrap();
        assert_eq!(d, ApprovalDecision::Reject);
    }

    #[test]
    fn stdin_line_parses_to_decision() {
        let abort = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let sink = StdinApproval::with_rx(rx, abort.clone());
        let info = zealous_engine::approval::ApprovalRequestInfo {
            call_id: "c".into(),
            tool: "shell".into(),
            risk: zealous_core::RiskTier::T2,
            summary: "s".into(),
        };
        tx.send("y".into()).unwrap();
        assert_eq!(sink.decide(&info), ApprovalDecision::Approve);
        let (tx2, rx2) = mpsc::channel();
        let sink2 = StdinApproval::with_rx(rx2, abort.clone());
        tx2.send("n".into()).unwrap();
        assert_eq!(sink2.decide(&info), ApprovalDecision::Reject);
    }
}
