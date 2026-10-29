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
use zealous_engine::provider::{OpenAiCompatProvider, Provider, ProviderConfig};
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
    /// 串行任务队列(spec 156;全部 isolated,单并发)
    Queue {
        #[command(subcommand)]
        cmd: QueueCmd,
    },
    /// 并行任务 Alpha(spec 157;最多两个 isolated tasks)
    Parallel {
        #[command(subcommand)]
        cmd: ParallelCmd,
    },
}

#[derive(Subcommand)]
enum QueueCmd {
    /// 串行执行任务(显式 IDs 按顺序;--all 按 TaskId 升序取全部可运行)
    Run {
        /// 按顺序执行的 task ids(与 --all 互斥)
        task_ids: Vec<String>,
        /// 取全部可运行 Queued 任务
        #[arg(long)]
        all: bool,
        /// 任务 Completed 后自动 fast-forward 应用回主分支
        #[arg(long)]
        auto_apply: bool,
        /// 全部工具审批自动通过
        #[arg(long)]
        yes: bool,
        /// 全部工具审批自动拒绝
        #[arg(long)]
        no: bool,
        /// 只保留审批请求、错误与最终结果
        #[arg(long)]
        quiet: bool,
    },
    /// 查看队列状态(active / runnable / not runnable)
    Status,
}

#[derive(Subcommand)]
enum ParallelCmd {
    /// 并行执行任务(显式 IDs 按顺序;--all 按 TaskId 升序取前 max 个)
    Run {
        /// 按顺序执行并后续按此顺序 apply 的 task ids(与 --all 互斥)
        task_ids: Vec<String>,
        /// 取全部可运行 Queued 任务的前 max 个
        #[arg(long)]
        all: bool,
        /// Alpha 最大并发,默认 2,上限 3
        #[arg(long, default_value_t = 2)]
        max: usize,
        /// 所有任务终态后自动按顺序集成(ff-only 或 merge)
        #[arg(long)]
        auto_apply: bool,
        /// 集成策略:ff-only(默认)或 merge(spec 158)
        #[arg(long, default_value = "ff-only")]
        integrate: String,
        /// merge 冲突策略:abort(默认)或 workspace(spec 159)
        #[arg(long, default_value = "abort")]
        on_conflict: String,
        /// 全部工具审批自动通过
        #[arg(long)]
        yes: bool,
        /// 全部工具审批自动拒绝
        #[arg(long)]
        no: bool,
        /// 只保留审批请求、错误与最终结果
        #[arg(long)]
        quiet: bool,
    },
    /// 冲突停止后按输入顺序恢复集成已完成任务(spec 161)
    Integrate {
        /// 按顺序集成的 task ids
        task_ids: Vec<String>,
        /// 集成策略:ff-only(默认)或 merge
        #[arg(long, default_value = "ff-only")]
        integrate: String,
        /// merge 冲突策略:abort(默认)或 workspace
        #[arg(long, default_value = "abort")]
        on_conflict: String,
    },
    /// 查看并行状态(active / runnable / not runnable)
    Status,
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
    /// 将因主分支前进而无法 fast-forward 的已完成 isolated 任务三方合并回主工作区(spec 158)
    Merge {
        task_id: String,
        /// 跳过确认(仅限你已确认变更内容)
        #[arg(long)]
        yes: bool,
    },
    /// 创建冲突解决工作区;无冲突时自动完成合并并集成(spec 159)
    MergeWorkspace { task_id: String },
    /// 查看冲突解决工作区状态(spec 159)
    MergeStatus { task_id: String },
    /// 用户解决并 stage 冲突后继续合并(spec 159)
    MergeContinue {
        task_id: String,
        /// 跳过确认(仅限已确认 staged 解决结果)
        #[arg(long)]
        yes: bool,
    },
    /// 生成模型辅助冲突解决方案(spec 160)
    Resolve {
        task_id: String,
        /// 自动批准全部模型建议
        #[arg(long)]
        yes: bool,
        /// 自动拒绝模型建议
        #[arg(long)]
        no: bool,
    },
    /// 中止冲突解决工作区(spec 159)
    MergeAbort {
        task_id: String,
        /// merge workspace 有额外修改时强制清理
        #[arg(long)]
        force: bool,
        /// 跳过确认
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
            TaskCmd::Merge { task_id, yes } => cmd_merge(&task_id, yes),
            TaskCmd::MergeWorkspace { task_id } => cmd_merge_workspace(&task_id),
            TaskCmd::MergeStatus { task_id } => cmd_merge_status(&task_id),
            TaskCmd::MergeContinue { task_id, yes } => cmd_merge_continue(&task_id, yes),
            TaskCmd::Resolve { task_id, yes, no } => cmd_resolve(&task_id, yes, no),
            TaskCmd::MergeAbort {
                task_id,
                force,
                yes,
            } => cmd_merge_abort(&task_id, force, yes),
            TaskCmd::Workspace { task_id } => cmd_workspace(&task_id),
            TaskCmd::WorkspaceRemove {
                task_id,
                force,
                yes,
            } => cmd_workspace_remove(&task_id, force, yes),
        },
        Cmd::Queue { cmd } => match cmd {
            QueueCmd::Run {
                task_ids,
                all,
                auto_apply,
                yes,
                no,
                quiet,
            } => cmd_queue_run(task_ids, all, auto_apply, yes, no, quiet),
            QueueCmd::Status => cmd_queue_status(),
        },
        Cmd::Parallel { cmd } => match cmd {
            ParallelCmd::Run {
                task_ids,
                all,
                max,
                auto_apply,
                integrate,
                on_conflict,
                yes,
                no,
                quiet,
            } => cmd_parallel_run(
                task_ids,
                all,
                max,
                auto_apply,
                integrate,
                on_conflict,
                yes,
                no,
                quiet,
            ),
            ParallelCmd::Integrate {
                task_ids,
                integrate,
                on_conflict,
            } => cmd_parallel_integrate(task_ids, integrate, on_conflict),
            ParallelCmd::Status => cmd_parallel_status(),
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
    // spec 156:全局 run lease(单并发防线;Drop 时释放)
    let mode = if isolate { "isolated" } else { "direct" };
    let _run_lease =
        match zealous_engine::lease::try_acquire(&repo_root().unwrap_or_default(), task_id, mode) {
            Ok(l) => l,
            Err(e) => return err(e.to_string()),
        };
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

fn cmd_queue_run(
    task_ids: Vec<String>,
    all: bool,
    auto_apply: bool,
    yes: bool,
    no: bool,
    quiet: bool,
) -> i32 {
    use zealous_engine::queue;
    if !all && task_ids.is_empty() {
        return err("需要显式 task ids 或 --all");
    }
    if all && !task_ids.is_empty() {
        return err("显式 task ids 与 --all 互斥");
    }
    let repo = match repo_root() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let svc = match svc() {
        Ok(s) => s,
        Err(e) => return err(e),
    };
    // 任务解析与筛选
    let ids: Vec<zealous_core::TaskId> = if all {
        let (runnable, not_runnable) = match queue::select_runnable(&svc) {
            Ok(x) => x,
            Err(e) => return err(e),
        };
        if runnable.is_empty() {
            println!("(no runnable queued tasks)");
            return 0;
        }
        if !not_runnable.is_empty() {
            println!("not runnable:");
            for (id, reason) in &not_runnable {
                println!("  {id}  {reason}");
            }
        }
        runnable
    } else {
        let mut parsed = Vec::new();
        for raw in &task_ids {
            match zealous_core::TaskId::parse(raw) {
                Ok(id) => parsed.push(id),
                Err(e) => return err(format!("无效 task id {raw}: {e}")),
            }
        }
        parsed
    };
    // 显式模式:启动前全量校验
    if !all {
        if let Err((i, id, reason)) = queue::validate_explicit(&svc, &ids) {
            return err(format!(
                "queue 启动失败(未执行任何任务): 第 {} 个任务 {id}: {reason}",
                i + 1
            ));
        }
    }
    // Provider 配置
    let config = match ProviderConfig::resolve() {
        Ok(c) => c,
        Err(e) => return err(format!("provider 配置无效: {e}")),
    };
    println!(
        "queue: {} 个任务, isolated, auto_apply={auto_apply}",
        ids.len()
    );
    // 全局 run lease(队列全程持有)
    let lease = match zealous_engine::lease::try_acquire(&repo, "queue", "queue") {
        Ok(l) => l,
        Err(e) => return err(e.to_string()),
    };
    let model = config.model.clone();
    let provider: Arc<dyn Provider> = Arc::new(OpenAiCompatProvider::new(config));
    let auto = if yes {
        Some(ApprovalDecision::Approve)
    } else if no {
        Some(ApprovalDecision::Reject)
    } else {
        None
    };
    let abort = Arc::new(AtomicBool::new(false));
    if let Err(e) = ctrlc::set_handler({
        let abort = abort.clone();
        move || abort.store(true, Ordering::Relaxed)
    }) {
        return err(format!("install ctrl-c handler: {e}"));
    }
    let sink: Arc<dyn ApprovalSink> = Arc::new(StdinApproval::new(auto, abort.clone()));
    let progress: Option<zealous_engine::progress::SharedSink> =
        Some(Arc::new(CliProgress { quiet }));
    // 交互 apply 确认(auto_apply 时跳过)
    let confirm = move |tid: &zealous_core::TaskId| -> bool {
        print!(
            "任务 {} 已完成,将其 fast-forward 应用回主分支? [y/N] ",
            tid.as_str()
        );
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return false;
        }
        let t = line.trim().to_ascii_lowercase();
        t == "y" || t == "yes"
    };
    let mut hooks = queue::QueueHooks {
        on_task_started: Box::new(|tid, i, total| {
            println!("=== [{i}/{total}] task {} 开始(isolated) ===", tid.as_str());
        }),
        on_task_finished: Box::new(|tid, status| {
            println!("=== task {} 结束: {status:?} ===", tid.as_str());
        }),
        on_apply_finished: Box::new(|tid, ok, detail| {
            if ok {
                println!("=== task {} apply 成功: {detail} ===", tid.as_str());
            } else {
                println!("=== task {} apply 失败: {detail} ===", tid.as_str());
            }
        }),
        confirm_apply: Box::new(confirm),
    };
    let summary = queue::run_serial(
        &repo, &svc, provider, model, 24, &ids, auto_apply, sink, progress, abort, &lease,
        &mut hooks,
    );
    println!(
        "queue 完成: applied {} / completed {}",
        summary.applied.len(),
        summary.completed.len()
    );
    if !summary.stopped_reason.is_empty() {
        println!("queue 停止: {}", summary.stopped_reason);
        return 1;
    }
    0
}

/// CLI parallel approval adapter: engine serializes calls and adds task binding;
/// StdinApproval remains responsible for actual user interaction.
struct ParallelStdinApproval {
    inner: Arc<dyn ApprovalSink>,
}

impl zealous_engine::parallel::ParallelApprovalSink for ParallelStdinApproval {
    fn decide(
        &self,
        task_id: &zealous_core::TaskId,
        info: &zealous_engine::approval::ApprovalRequestInfo,
    ) -> ApprovalDecision {
        let mut scoped = info.clone();
        scoped.summary = format!(
            "task: {}\ncall: {}\n{}",
            task_id.as_str(),
            info.call_id,
            info.summary
        );
        self.inner.decide(&scoped)
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_parallel_run(
    task_ids: Vec<String>,
    all: bool,
    max: usize,
    auto_apply: bool,
    integrate: String,
    on_conflict: String,
    yes: bool,
    no: bool,
    quiet: bool,
) -> i32 {
    if all && !task_ids.is_empty() {
        return err("parallel 启动失败: --all 与显式 task ids 互斥");
    }
    if !all && task_ids.is_empty() {
        return err("parallel 启动失败: 需要显式 task ids 或 --all");
    }
    if let Err(e) = zealous_engine::parallel::validate_limit(max) {
        return err(format!("parallel 启动失败: {e}"));
    }
    if !all && task_ids.len() > max {
        return err(format!(
            "parallel 启动失败: 显式 task ids 数量 {} 超过 --max {max}",
            task_ids.len()
        ));
    }
    let integration = match zealous_engine::parallel::IntegrationStrategy::parse(&integrate) {
        Ok(strategy) => strategy,
        Err(e) => return err(format!("parallel 启动失败: {e}")),
    };
    let on_conflict_strategy =
        match zealous_engine::parallel::OnConflictStrategy::parse(&on_conflict) {
            Ok(strategy) => strategy,
            Err(e) => return err(format!("parallel 启动失败: {e}")),
        };
    let repo = match repo_root() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let svc = match svc() {
        Ok(s) => s,
        Err(e) => return err(e),
    };
    let ids = if all {
        match zealous_engine::parallel::select_runnable(&svc, max) {
            Ok((runnable, not_runnable)) => {
                if runnable.is_empty() {
                    println!("(no runnable queued tasks)");
                    return 0;
                }
                if !not_runnable.is_empty() {
                    println!("not runnable:");
                    for (id, reason) in &not_runnable {
                        println!("  {id}  {reason}");
                    }
                }
                runnable
            }
            Err(e) => return err(e),
        }
    } else {
        let mut parsed = Vec::new();
        for raw in &task_ids {
            match zealous_core::TaskId::parse(raw) {
                Ok(id) => parsed.push(id),
                Err(e) => return err(format!("无效 task id {raw}: {e}")),
            }
        }
        parsed
    };
    if !all {
        if let Err((i, id, reason)) =
            zealous_engine::parallel::validate_explicit_with_max(&svc, &ids, max)
        {
            return err(format!(
                "parallel 启动失败(未执行任何任务): 第 {} 个任务 {id}: {reason}",
                i + 1
            ));
        }
    }
    let config = match ProviderConfig::resolve() {
        Ok(c) => c,
        Err(e) => return err(format!("provider 配置无效: {e}")),
    };
    println!(
        "parallel: {} 个任务, isolated, max=1 batch/{}, auto_apply={auto_apply}, integrate={integrate}, on_conflict={on_conflict}",
        ids.len(),
        ids.len()
    );
    let lease = match zealous_engine::lease::try_acquire(&repo, "parallel", "parallel") {
        Ok(l) => l,
        Err(e) => return err(e.to_string()),
    };
    let model = config.model.clone();
    let provider: Arc<dyn Provider> = Arc::new(OpenAiCompatProvider::new(config));
    let providers = ids.iter().map(|_| provider.clone()).collect::<Vec<_>>();
    let auto = if yes {
        Some(ApprovalDecision::Approve)
    } else if no {
        Some(ApprovalDecision::Reject)
    } else {
        None
    };
    let abort = Arc::new(AtomicBool::new(false));
    if let Err(e) = ctrlc::set_handler({
        let abort = abort.clone();
        move || abort.store(true, Ordering::Relaxed)
    }) {
        return err(format!("install ctrl-c handler: {e}"));
    }
    let stdin_sink: Arc<dyn ApprovalSink> = Arc::new(StdinApproval::new(auto, abort.clone()));
    let sink = Arc::new(ParallelStdinApproval { inner: stdin_sink });
    let progress: Option<zealous_engine::progress::SharedSink> =
        Some(Arc::new(CliProgress { quiet }));
    let confirm = move |tid: &zealous_core::TaskId| -> bool {
        print!(
            "任务 {} 已完成,将其 fast-forward 应用回主分支? [y/N] ",
            tid.as_str()
        );
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return false;
        }
        let t = line.trim().to_ascii_lowercase();
        t == "y" || t == "yes"
    };
    let mut hooks = zealous_engine::parallel::ParallelHooks {
        on_task_started: Box::new(|tid, i, total| {
            println!(
                "=== parallel [{i}/{total}] task {} 开始(isolated) ===",
                tid.as_str()
            );
        }),
        on_task_finished: Box::new(|tid, status| {
            println!("=== parallel task {} 结束: {status:?} ===", tid.as_str());
        }),
        on_apply_started: Box::new(|tid| {
            println!("=== parallel task {} apply 开始 ===", tid.as_str());
        }),
        on_apply_finished: Box::new(|tid, ok, detail| {
            println!(
                "=== parallel task {} apply {}: {detail} ===",
                tid.as_str(),
                if ok { "成功" } else { "失败" }
            );
        }),
        on_merge_started: Box::new(|tid| {
            println!("=== parallel task {} merge 开始 ===", tid.as_str());
        }),
        on_merge_finished: Box::new(|tid, ok, detail| {
            println!(
                "=== parallel task {} merge {}: {detail} ===",
                tid.as_str(),
                if ok { "成功" } else { "失败" }
            );
        }),
        on_merge_conflict: Box::new(|tid, files| {
            println!(
                "=== parallel task {} merge 冲突已保留 workspace ===",
                tid.as_str()
            );
            println!("conflicted files:");
            for file in files {
                println!("  {file}");
            }
        }),
        confirm_apply: Box::new(confirm),
    };
    let summary = zealous_engine::parallel::run_parallel(
        &repo,
        &svc,
        providers,
        model,
        24,
        &ids,
        auto_apply,
        integration,
        on_conflict_strategy,
        sink,
        progress,
        abort,
        &lease,
        &mut hooks,
    );
    println!(
        "parallel 完成: integrated {} (ff {}) / completed {}, merged {}, conflict workspaces {}, failed {}, interrupted {}, base {}",
        summary.applied.len(),
        summary.applied.len().saturating_sub(summary.merged.len()),
        summary.completed.len(),
        summary.merged.len(),
        summary.merge_workspaces.len(),
        summary.failed.len(),
        summary.interrupted.len(),
        summary.base_commit
    );
    if !summary.stopped_reason.is_empty() {
        println!("parallel 停止: {}", summary.stopped_reason);
        return 1;
    }
    0
}

fn cmd_parallel_integrate(task_ids: Vec<String>, integrate: String, on_conflict: String) -> i32 {
    if task_ids.is_empty() {
        return err("parallel integrate 失败: 至少提供一个 task id");
    }
    if task_ids.len() > zealous_engine::parallel::MAX_PARALLEL_TASKS {
        return err(format!(
            "parallel integrate 失败: Alpha 最多支持 {} 个任务",
            zealous_engine::parallel::MAX_PARALLEL_TASKS
        ));
    }
    let integration = match zealous_engine::parallel::IntegrationStrategy::parse(&integrate) {
        Ok(strategy) => strategy,
        Err(e) => return err(format!("parallel integrate 失败: {e}")),
    };
    let on_conflict_strategy =
        match zealous_engine::parallel::OnConflictStrategy::parse(&on_conflict) {
            Ok(strategy) => strategy,
            Err(e) => return err(format!("parallel integrate 失败: {e}")),
        };
    let repo = match repo_root() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let svc = match svc() {
        Ok(s) => s,
        Err(e) => return err(e),
    };
    let mut ids = Vec::new();
    for raw in &task_ids {
        match zealous_core::TaskId::parse(raw) {
            Ok(id) => ids.push(id),
            Err(e) => return err(format!("无效 task id {raw}: {e}")),
        }
    }
    let lease = match zealous_engine::lease::try_acquire(&repo, "parallel", "parallel") {
        Ok(l) => l,
        Err(e) => return err(e.to_string()),
    };
    println!(
        "parallel integrate: {} 个任务, integrate={integrate}, on_conflict={on_conflict}",
        ids.len()
    );
    let mut hooks = zealous_engine::parallel::ParallelHooks {
        on_task_started: Box::new(|_, _, _| {}),
        on_task_finished: Box::new(|tid, status| {
            println!("=== integrate task {} 状态: {status:?} ===", tid.as_str());
        }),
        on_apply_started: Box::new(|tid| {
            println!("=== integrate task {} apply 开始 ===", tid.as_str());
        }),
        on_apply_finished: Box::new(|tid, ok, detail| {
            println!(
                "=== integrate task {} apply {}: {detail} ===",
                tid.as_str(),
                if ok { "成功" } else { "失败" }
            );
        }),
        on_merge_started: Box::new(|tid| {
            println!("=== integrate task {} merge 开始 ===", tid.as_str());
        }),
        on_merge_finished: Box::new(|tid, ok, detail| {
            println!(
                "=== integrate task {} merge {}: {detail} ===",
                tid.as_str(),
                if ok { "成功" } else { "失败" }
            );
        }),
        on_merge_conflict: Box::new(|tid, files| {
            println!(
                "=== integrate task {} merge 冲突保留 workspace ===",
                tid.as_str()
            );
            println!("conflicted files: {}", files.join(", "));
        }),
        confirm_apply: Box::new(|_| true),
    };
    let summary = zealous_engine::parallel::integrate(
        &repo,
        &svc,
        &ids,
        integration,
        on_conflict_strategy,
        &lease,
        &mut hooks,
    );
    println!(
        "parallel integrate 完成: integrated {} / completed {}, merged {}, conflict workspaces {}",
        summary.applied.len(),
        summary.completed.len(),
        summary.merged.len(),
        summary.merge_workspaces.len()
    );
    if !summary.stopped_reason.is_empty() {
        println!("parallel integrate 停止: {}", summary.stopped_reason);
        return 1;
    }
    0
}

fn cmd_parallel_status() -> i32 {
    let repo = match repo_root() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let svc = match svc() {
        Ok(s) => s,
        Err(e) => return err(e),
    };
    if zealous_engine::lease::is_held(&repo) {
        let info = zealous_engine::lease::peek_info(&repo);
        println!("active:");
        match info {
            Some(info) => {
                println!(
                    "  pid {} task {} mode {}",
                    info.pid, info.task_id, info.mode
                );
                if info.mode == "parallel" {
                    println!("  task ids: {}", info.task_ids.join(", "));
                    println!("  active ids: {}", info.active_task_ids.join(", "));
                }
            }
            None => println!("  (metadata 不可读)"),
        }
    } else {
        println!("active: (none)");
    }
    match zealous_engine::parallel::select_runnable(
        &svc,
        zealous_engine::parallel::MAX_PARALLEL_TASKS,
    ) {
        Ok((runnable, not_runnable)) => {
            println!("runnable parallel (--all order):");
            if runnable.is_empty() {
                println!("  (none)");
            }
            for (i, id) in runnable.iter().enumerate() {
                println!("  {}. {}", i + 1, id.as_str());
            }
            println!("not runnable:");
            if not_runnable.is_empty() {
                println!("  (none)");
            }
            for (id, reason) in &not_runnable {
                println!("  {id}  {reason}");
            }
        }
        Err(e) => return err(e),
    }
    0
}

fn cmd_queue_status() -> i32 {
    let repo = match repo_root() {
        Ok(r) => r,
        Err(e) => return err(e),
    };
    let svc = match svc() {
        Ok(s) => s,
        Err(e) => return err(e),
    };
    // active:lease 持有中?
    if zealous_engine::lease::is_held(&repo) {
        let info = zealous_engine::lease::peek_info(&repo);
        println!("active:");
        match info {
            Some(info) => {
                println!(
                    "  pid {} task {} mode {}",
                    info.pid, info.task_id, info.mode
                );
                if let Ok(tid) = zealous_core::TaskId::parse(&info.task_id) {
                    if let Ok((_l, proj, _)) = svc.open_task(&tid) {
                        println!("  status {:?}", proj.status);
                        if let Some(ws) = &proj.execution_workspace {
                            println!("  workspace branch {}", ws.branch);
                        }
                    }
                }
            }
            None => println!("  (metadata 不可读)"),
        }
    } else {
        println!("active: (none)");
    }
    match zealous_engine::queue::select_runnable(&svc) {
        Ok((runnable, not_runnable)) => {
            println!("runnable queue (--all order):");
            if runnable.is_empty() {
                println!("  (none)");
            }
            for (i, id) in runnable.iter().enumerate() {
                println!("  {}. {}", i + 1, id.as_str());
            }
            println!("not runnable:");
            if not_runnable.is_empty() {
                println!("  (none)");
            }
            for (id, reason) in &not_runnable {
                println!("  {}  {reason}", id.as_str());
            }
        }
        Err(e) => return err(e),
    }
    0
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

fn cmd_merge(task_id: &str, yes: bool) -> i32 {
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
        println!("已应用/合并过(幂等): {applied}");
        return 0;
    }
    if proj.status != TaskStatus::Completed {
        return err("任务尚未 Completed,不能 merge");
    }
    let tip = match git_out(&repo, &["rev-parse", &format!("refs/heads/{}", ws.branch)]) {
        Ok(t) => t.trim().to_string(),
        Err(e) => return err(e),
    };
    let head = match git_out(&repo, &["rev-parse", "HEAD"]) {
        Ok(t) => t.trim().to_string(),
        Err(e) => return err(e),
    };
    println!("base:       {} @ {}", ws.base_branch, ws.base_commit);
    println!("main HEAD:  {head}");
    println!("task tip:   {} @ {}", ws.branch, tip);
    println!("将三方合并的 commits:");
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
    println!("Git 无法自动合并时将立即 merge --abort,不保留冲突 index。");
    if !yes {
        print!("确认三方合并? [y/N] ");
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
    match zealous_engine::workspace::merge(&repo, &ledger) {
        Ok(out) => {
            println!(
                "merged: {} @ {} (task tip {})",
                out.branch, out.merge_commit, out.task_tip
            );
            0
        }
        Err(e) => err(format!("merge: {e}")),
    }
}

fn open_task_for_merge(
    task_id: &str,
) -> Result<(PathBuf, std::sync::Arc<zealous_engine::ledger::Ledger>), String> {
    let repo = repo_root().map_err(|e| e.to_string())?;
    let tid = zealous_core::TaskId::parse(task_id).map_err(|e| e.to_string())?;
    let service = svc().map_err(|e| e.to_string())?;
    let (ledger, _, _) = service.open_task(&tid).map_err(|e| e.to_string())?;
    Ok((repo, std::sync::Arc::new(ledger)))
}

fn cmd_merge_workspace(task_id: &str) -> i32 {
    let tid = match zealous_core::TaskId::parse(task_id) {
        Ok(t) => t,
        Err(e) => return err(e.to_string()),
    };
    let (repo, ledger) = match open_task_for_merge(task_id) {
        Ok(x) => x,
        Err(e) => return err(e),
    };
    match zealous_engine::merge_workspace::start(&repo, &tid, &ledger) {
        Ok(zealous_engine::merge_workspace::StartOutcome::Integrated {
            branch,
            merge_commit,
            task_tip,
            ..
        }) => {
            println!("merged: {branch} @ {merge_commit} (task tip {task_tip})");
            0
        }
        Ok(zealous_engine::merge_workspace::StartOutcome::Conflicted {
            branch,
            path,
            main_head,
            task_branch,
            task_tip,
            files,
        }) => {
            println!("status:        conflicted");
            println!("workspace:    {}", path.display());
            println!("merge branch: {branch}");
            println!("main head:    {main_head}");
            println!("task branch:  {task_branch}");
            println!("task tip:     {task_tip}");
            println!("conflicted files:");
            if files.is_empty() {
                println!("  (none reported)");
            }
            for file in files {
                println!("  {file}");
            }
            println!("请在上述 workspace 中手动解决冲突并 git add;然后执行:");
            println!("  zealous task merge-continue {task_id}");
            println!("或中止:");
            println!("  zealous task merge-abort {task_id}");
            0
        }
        Err(e) => err(format!("merge workspace: {e}")),
    }
}

fn cmd_merge_status(task_id: &str) -> i32 {
    let (repo, ledger) = match open_task_for_merge(task_id) {
        Ok(x) => x,
        Err(e) => return err(e),
    };
    match zealous_engine::merge_workspace::status(&repo, &ledger) {
        Ok(status) => {
            println!("status:        {}", status.status);
            println!("workspace:    {}", status.path);
            println!("merge branch: {}", status.branch);
            println!("main head:    {}", status.main_head);
            println!("task branch:  {}", status.task_branch);
            println!("task tip:     {}", status.task_tip);
            if let Some(commit) = status.merge_commit {
                println!("merge commit: {commit}");
            }
            println!("unresolved files:");
            if status.unresolved_files.is_empty() {
                println!("  (none)");
            }
            for file in status.unresolved_files {
                println!("  {file}");
            }
            0
        }
        Err(e) => err(format!("merge status: {e}")),
    }
}

fn cmd_merge_continue(task_id: &str, yes: bool) -> i32 {
    let (repo, ledger) = match open_task_for_merge(task_id) {
        Ok(x) => x,
        Err(e) => return err(e),
    };
    if !yes {
        match zealous_engine::merge_workspace::status(&repo, &ledger) {
            Ok(status) => {
                println!("merge branch: {}", status.branch);
                println!("main head:    {}", status.main_head);
                println!("task tip:     {}", status.task_tip);
                println!("unresolved files:");
                for file in &status.unresolved_files {
                    println!("  {file}");
                }
            }
            Err(e) => return err(format!("merge status: {e}")),
        }
        print!("确认提交已 stage 的手动解决结果? [y/N] ");
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return err("读取确认失败");
        }
        let answer = line.trim().to_ascii_lowercase();
        if answer != "y" && answer != "yes" {
            println!("已取消");
            return 0;
        }
    }
    match zealous_engine::merge_workspace::continue_merge(&repo, &ledger) {
        Ok(commit) => {
            println!("merged: {commit}");
            0
        }
        Err(e) => err(format!("merge continue: {e}")),
    }
}

fn print_resolution_block(label: &str, content: &str) {
    println!("--- {label} ---");
    println!("{content}");
    println!("--- end {label} ---");
}

fn cmd_resolve(task_id: &str, yes: bool, no: bool) -> i32 {
    if yes && no {
        return err("--yes 与 --no 互斥");
    }
    let (repo, ledger) = match open_task_for_merge(task_id) {
        Ok(x) => x,
        Err(e) => return err(e),
    };
    let (_proj_before, merge_ws, contexts) = {
        let svc = match svc() {
            Ok(s) => s,
            Err(e) => return err(e),
        };
        let tid = match zealous_core::TaskId::parse(task_id) {
            Ok(t) => t,
            Err(e) => return err(e.to_string()),
        };
        let (_, proj, _) = match svc.open_task(&tid) {
            Ok(x) => x,
            Err(e) => return err(e.to_string()),
        };
        let merge_ws = match &proj.merge_workspace {
            Some(view) if view.status == "conflicted" => view.clone(),
            _ => return err("merge workspace 不处于 conflicted 状态"),
        };
        let contexts = match zealous_engine::merge_resolution::collect_contexts(
            std::path::Path::new(&merge_ws.path),
        ) {
            Ok(c) => c,
            Err(e) => return err(format!("collect conflicts: {e}")),
        };
        (proj, merge_ws, contexts)
    };
    let _ = repo;
    let config = match ProviderConfig::resolve() {
        Ok(c) => c,
        Err(e) => return err(format!("provider 配置无效: {e}")),
    };
    let provider = OpenAiCompatProvider::new(config.clone());
    let proposal = match zealous_engine::merge_resolution::propose(
        std::path::Path::new(&merge_ws.path),
        &ledger,
        &provider,
        &provider_model(&config),
    ) {
        Ok(p) => p,
        Err(e) => return err(format!("resolve: {e}")),
    };

    println!("resolution_id: {}", proposal.resolution_id.as_str());
    println!("summary:       {}", proposal.summary);
    println!("proposed files: {}", proposal.resolved_files().join(", "));
    println!("manual files:   {}", proposal.manual_files().join(", "));
    println!("写入目标仅为 merge workspace;不会自动 commit,不会修改主工作区。");
    for file in &proposal.files {
        match file {
            zealous_engine::merge_resolution::ProposalFile::Resolved {
                path,
                content,
                rationale,
            } => {
                let context = contexts
                    .iter()
                    .find(|c| c.path == *path)
                    .expect("validated");
                println!("\n========== {} ==========", path);
                println!("mode: resolved");
                println!("rationale: {rationale}");
                print_resolution_block("Base", &context.base);
                print_resolution_block("Ours", &context.ours);
                print_resolution_block("Theirs", &context.theirs);
                print_resolution_block("Proposed", content);
            }
            zealous_engine::merge_resolution::ProposalFile::Manual { path, reason } => {
                println!("\n========== {} ==========", path);
                println!("mode: manual");
                println!("reason: {reason}");
            }
        }
    }

    let approved = if no {
        false
    } else if yes {
        true
    } else {
        print!("\n批准写入以上 resolved files? [y/N] ");
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            false
        } else {
            let answer = line.trim().to_ascii_lowercase();
            answer == "y" || answer == "yes"
        }
    };

    if !approved {
        match zealous_engine::merge_resolution::reject(
            &ledger,
            &proposal.resolution_id,
            Some("user rejected proposal".into()),
        ) {
            Ok(()) => {
                println!("proposal rejected;未写入任何文件");
                0
            }
            Err(e) => err(format!("reject resolution: {e}")),
        }
    } else {
        match zealous_engine::merge_resolution::apply_proposal(
            std::path::Path::new(&merge_ws.path),
            &ledger,
            &proposal,
        ) {
            Ok((staged, manual)) => {
                println!("resolved + staged: {}", staged.join(", "));
                if !manual.is_empty() {
                    println!("manual pending: {}", manual.join(", "));
                }
                println!("\nNext:");
                println!("  zealous task merge-status {task_id}");
                println!("  zealous task merge-continue {task_id}");
                0
            }
            Err(e) => err(format!("apply resolution: {e}")),
        }
    }
}

fn provider_model(config: &ProviderConfig) -> String {
    config.model.clone()
}

fn cmd_merge_abort(task_id: &str, force: bool, yes: bool) -> i32 {
    let tid = match zealous_core::TaskId::parse(task_id) {
        Ok(t) => t,
        Err(e) => return err(e.to_string()),
    };
    let (repo, ledger) = match open_task_for_merge(task_id) {
        Ok(x) => x,
        Err(e) => return err(e),
    };
    if force && !yes {
        println!("警告:--force 会丢弃 merge workspace 中尚未提交的用户修改。");
        print!("确认强制中止? [y/N] ");
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return err("读取确认失败");
        }
        let answer = line.trim().to_ascii_lowercase();
        if answer != "y" && answer != "yes" {
            println!("已取消");
            return 0;
        }
    }
    let _ = tid;
    match zealous_engine::merge_workspace::abort(&repo, &ledger, force) {
        Ok(()) => {
            println!("merge workspace aborted");
            0
        }
        Err(e) => err(format!("merge abort: {e}")),
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
    if let Some(strategy) = &proj.applied_strategy {
        let text = match strategy {
            zealous_core::AppliedStrategy::FastForward => "fast_forward",
            zealous_core::AppliedStrategy::ThreeWayMerge => "three_way_merge",
        };
        println!("applied_strategy: {text}");
    }
    if let Some(commit) = &proj.merge_commit {
        println!("merge_commit:    {commit}");
    }
    if let Some(merge_ws) = &proj.merge_workspace {
        println!("merge_workspace: {}", merge_ws.path);
        println!("merge_ws_branch: {}", merge_ws.branch);
        println!("merge_ws_status: {}", merge_ws.status);
    }
    if !proj.merge_conflicts.is_empty() {
        println!("merge_conflicts:");
        for file in &proj.merge_conflicts {
            println!("  {file}");
        }
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
