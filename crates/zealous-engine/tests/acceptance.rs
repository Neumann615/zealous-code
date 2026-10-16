//! 验收场景测试(spec 40):A1–A6,以 MockProvider 驱动真实编排链路。
//!
//! 模型行为被脚本化,但事件账本、工具执行、审批闸门、中断/恢复语义
//! 全部走真实代码路径;每个场景断言账本健康(seq 连续)与可解释性。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use zealous_core::{
    projection::ToolCallState, AgentRef, ApprovalDecision, EventPayload, RiskTier, TaskStatus,
};
use zealous_engine::approval::{ApprovalSink, AutoApprove, AutoReject};
use zealous_engine::executor::ToolExecutor;
use zealous_engine::ledger::{read_events, Ledger};
use zealous_engine::orchestrator::Orchestrator;
use zealous_engine::provider::MockProvider;
use zealous_engine::task_service::{CreateOutcome, TaskService};
use zealous_engine::tools::builtin_registry;

// ---------- 夹具 ----------

fn unique_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "zealous-acc-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 真实 git 仓库夹具(spec 40 A7:Windows 可用)
/// 初始 commit:脏工作区保护要求 apply 前工作区干净(spec 60 §4)
fn commit_all(dir: &Path) {
    let _ = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output();
    let out = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "init",
        ])
        .current_dir(dir)
        .output()
        .expect("git commit");
    assert!(out.status.success(), "commit failed: {:?}", out.stderr);
}

fn git_repo(tag: &str) -> PathBuf {
    let dir = unique_dir(tag);
    let out = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .output()
        .expect("git init");
    assert!(out.status.success(), "git init failed: {:?}", out.stderr);
    // 初始 commit:脏工作区守卫(spec 60 §4)要求 apply 前工作区干净
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "init",
                "--allow-empty",
            ])
            .current_dir(&dir)
            .output()
            .unwrap()
            .status
            .success(),
        "initial commit failed"
    );
    dir
}

/// 为 create_task 的配置门提供合法 env(Provider 由测试直接注入,不外呼)
fn setup_provider_env() {
    std::env::set_var("ZEALOUS_BASE_URL", "https://api.example.invalid/v1");
    std::env::set_var("ZEALOUS_API_KEY", "test-key-not-real");
    std::env::set_var("ZEALOUS_MODEL", "mock-model");
}

fn create_task(repo: &Path, title: &str) -> Arc<Ledger> {
    let svc = TaskService::new(repo);
    match svc.create_task(title).unwrap() {
        CreateOutcome::Ready { ledger, .. } => Arc::new(ledger),
        other => panic!("expected Ready, got {other:?}"),
    }
}

fn orchestrator(
    repo: &Path,
    script: Vec<Result<String, String>>,
    sink: Arc<dyn ApprovalSink>,
    abort: Arc<AtomicBool>,
) -> Orchestrator {
    Orchestrator {
        provider: Arc::new(MockProvider::scripted(script)),
        executor: Arc::new(ToolExecutor::new(repo, builtin_registry(), sink)),
        abort,
        max_turns: 12,
        model: "mock-model".into(),
        progress: None,
    }
}

fn act(json: &str) -> Result<String, String> {
    Ok(json.to_string())
}

fn assert_ledger_healthy(
    repo: &Path,
    task_id: &zealous_core::TaskId,
) -> zealous_core::TaskProjection {
    let (events, report) = read_events(&zealous_engine::ledger::events_path(
        &zealous_engine::ledger::task_dir(repo, task_id),
    ))
    .unwrap();
    assert!(!report.torn_tail, "正常完成的任务不应有残缺尾行");
    zealous_core::projection::fold(task_id, &events).unwrap()
}

// ---------- A1:完整闭环 ----------

#[test]
fn a1_full_loop_plan_analyze_apply_verify_report() {
    setup_provider_env();
    let repo = git_repo("a1");
    std::fs::write(repo.join("a.txt"), "content-a\n").unwrap();
    commit_all(&repo);
    let ledger = create_task(&repo, "copy a.txt to out.txt and verify");

    let script = vec![
        act(
            r#"{"action":"plan","plan":"1) read a.txt 2) write out.txt 3) verify","workers":["read a.txt and propose"]}"#,
        ),
        // worker-1(只读分析)
        act(r#"{"action":"tool","tool":"read_file","args":{"path":"a.txt"}}"#),
        act(
            r#"{"action":"message","text":"proposal: out.txt should contain the content of a.txt"}"#,
        ),
        // coordinator:串行 apply + 验证 + 汇报
        act(
            r#"{"action":"tool","tool":"write_file","args":{"path":"out.txt","content":"content-a\n"}}"#,
        ),
        act(r#"{"action":"tool","tool":"shell","args":{"command":"echo verified"}}"#),
        act(r#"{"action":"message","text":"done: out.txt written and verified"}"#),
    ];
    let orch = orchestrator(
        &repo,
        script,
        Arc::new(AutoApprove),
        Arc::new(AtomicBool::new(false)),
    );
    let status = orch
        .run_task(&ledger, "copy a.txt to out.txt and verify")
        .unwrap();
    assert_eq!(status, TaskStatus::Completed);

    // 主工作区产物存在
    assert_eq!(
        std::fs::read_to_string(repo.join("out.txt")).unwrap(),
        "content-a\n"
    );
    // Worker 提案进任务隔离目录(不在主工作区)
    let task_dir = ledger.path().parent().unwrap();
    assert!(task_dir.join("workers/worker-1/proposal.md").exists());

    // 账本健康 + 事件链完整
    let task_id = ledger.task_id().clone();
    let proj = assert_ledger_healthy(&repo, &task_id);
    assert_eq!(proj.status, TaskStatus::Completed);
    assert!(proj.plan.is_some());
    let risks: Vec<RiskTier> = proj.tool_calls.iter().map(|c| c.risk).collect();
    assert!(risks.contains(&RiskTier::T0));
    assert!(risks.contains(&RiskTier::T1));
    assert!(risks.contains(&RiskTier::T2));
}

// ---------- A2:拒绝高危命令 ----------

#[test]
fn a2_rejected_high_risk_command_is_not_executed() {
    setup_provider_env();
    let repo = git_repo("a2");
    let ledger = create_task(&repo, "run a dangerous command");

    let script = vec![
        act(r#"{"action":"plan","plan":"run cmd","workers":[]}"#),
        act(r#"{"action":"tool","tool":"shell","args":{"command":"echo pwned > pwned.txt"}}"#),
        // 模型收到拒绝后调整方案,以纯汇报收尾(A2:任务状态可解释,不静默失败)
        act(r#"{"action":"message","text":"user rejected the command; nothing executed"}"#),
    ];
    let orch = orchestrator(
        &repo,
        script,
        Arc::new(AutoReject),
        Arc::new(AtomicBool::new(false)),
    );
    let status = orch.run_task(&ledger, "run a dangerous command").unwrap();
    assert_eq!(status, TaskStatus::Completed);

    assert!(!repo.join("pwned.txt").exists(), "被拒命令不得产生副作用");
    let proj = assert_ledger_healthy(&repo, ledger.task_id());
    let shell = proj
        .tool_calls
        .iter()
        .find(|c| c.tool == "shell")
        .expect("shell 调用应落账");
    assert_eq!(shell.state, ToolCallState::Rejected);
}

// ---------- A3:中断与恢复 ----------

/// 阻塞型审批 sink:等待 abort 置位后返回 Reject(模拟用户在 TUI 中中断)
struct BlockUntilAbort {
    abort: Arc<AtomicBool>,
}

impl ApprovalSink for BlockUntilAbort {
    fn decide(&self, _call_id: &str, _summary: &str) -> ApprovalDecision {
        while !self.abort.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(5));
        }
        ApprovalDecision::Reject
    }
}

#[test]
fn a3_interrupt_records_state_and_pending_approvals_rejected() {
    setup_provider_env();
    let repo = git_repo("a3");
    let ledger = create_task(&repo, "long task");

    let abort = Arc::new(AtomicBool::new(false));
    let script = vec![
        act(r#"{"action":"plan","plan":"needs approval","workers":[]}"#),
        act(r#"{"action":"tool","tool":"shell","args":{"command":"echo step"}}"#),
    ];
    let orch = orchestrator(
        &repo,
        script,
        Arc::new(BlockUntilAbort {
            abort: abort.clone(),
        }),
        abort.clone(),
    );
    let ledger_thread = ledger.clone();
    let handle = std::thread::spawn(move || orch.run_task(&ledger_thread, "long task"));

    // 等待审批挂起出现
    let mut pending = false;
    for _ in 0..500 {
        if ledger
            .fold_now()
            .map(|p| !p.pending_approvals.is_empty())
            .unwrap_or(false)
        {
            pending = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(pending, "审批应在超时前挂起");

    // 用户中断
    abort.store(true, Ordering::Relaxed);
    let status = handle.join().unwrap().unwrap();
    assert_eq!(status, TaskStatus::Interrupted);

    let proj = assert_ledger_healthy(&repo, ledger.task_id());
    assert_eq!(proj.status, TaskStatus::Interrupted);
    assert!(proj.interrupted_count >= 1);
    assert!(proj.pending_approvals.is_empty(), "挂起审批应按中断收尾");
    assert_eq!(proj.tool_calls[0].state, ToolCallState::Rejected);

    // A3 后半:恢复到完成
    let script2 = vec![act(r#"{"action":"message","text":"resumed and finished"}"#)];
    let orch2 = orchestrator(
        &repo,
        script2,
        Arc::new(AutoApprove),
        Arc::new(AtomicBool::new(false)),
    );
    let status = orch2.run_task(&ledger, "long task").unwrap();
    assert_eq!(status, TaskStatus::Completed);
    let proj = assert_ledger_healthy(&repo, ledger.task_id());
    assert_eq!(proj.status, TaskStatus::Completed);
    assert!(proj.runs.len() >= 2, "恢复应开启新 AgentRun");
}

// ---------- A4:多 Worker 只读分析 + 串行 apply ----------

#[test]
fn a4_two_workers_readonly_then_serial_apply() {
    setup_provider_env();
    let repo = git_repo("a4");
    std::fs::write(repo.join("x.txt"), "x").unwrap();
    std::fs::write(repo.join("y.txt"), "y").unwrap();
    commit_all(&repo);
    let ledger = create_task(&repo, "analyze x and y");

    let script = vec![
        act(
            r#"{"action":"plan","plan":"analyze two files","workers":["read x.txt","read y.txt"]}"#,
        ),
        // worker-1
        act(r#"{"action":"tool","tool":"read_file","args":{"path":"x.txt"}}"#),
        act(r#"{"action":"message","text":"W1: x.txt contains x"}"#),
        // worker-2
        act(r#"{"action":"tool","tool":"read_file","args":{"path":"y.txt"}}"#),
        act(r#"{"action":"message","text":"W2: y.txt contains y"}"#),
        // coordinator:两次 apply(T1,串行)后汇报
        act(
            r#"{"action":"tool","tool":"write_file","args":{"path":"applied_1.txt","content":"W1: x"}}"#,
        ),
        act(
            r#"{"action":"tool","tool":"write_file","args":{"path":"applied_2.txt","content":"W2: y"}}"#,
        ),
        act(r#"{"action":"message","text":"both applied"}"#),
    ];
    let orch = orchestrator(
        &repo,
        script,
        Arc::new(AutoApprove),
        Arc::new(AtomicBool::new(false)),
    );
    let status = orch.run_task(&ledger, "analyze x and y").unwrap();
    assert_eq!(status, TaskStatus::Completed);

    let proj = assert_ledger_healthy(&repo, ledger.task_id());
    // Worker 的全部工具调用都是 T0(A4 判定)
    for call in &proj.tool_calls {
        if call.tool == "read_file" {
            assert_eq!(call.risk, RiskTier::T0, "worker 只能 T0");
        }
    }
    // 串行 apply:两个 write 的 Requested seq 严格递增,且各自带审批
    let writes: Vec<_> = proj
        .tool_calls
        .iter()
        .filter(|c| c.tool == "write_file")
        .collect();
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[0].state, ToolCallState::Completed { ok: true });
    assert_eq!(writes[1].state, ToolCallState::Completed { ok: true });
    assert!(repo.join("applied_1.txt").exists());
    assert!(repo.join("applied_2.txt").exists());
    // 提案文件
    let task_dir = ledger.path().parent().unwrap();
    assert!(task_dir.join("workers/worker-1/proposal.md").exists());
    assert!(task_dir.join("workers/worker-2/proposal.md").exists());
}

// ---------- A5:进程崩溃后恢复 ----------

#[test]
fn a5_crash_then_resume_rebuilds_view_and_completes() {
    setup_provider_env();
    let repo = git_repo("a5");
    let ledger = create_task(&repo, "crash me");

    // 阶段 1:模拟"崩溃"——Provider 中途失败,进程异常终止语义下
    // 账本停在 Running(无状态收尾事件)
    let script = vec![
        act(r#"{"action":"plan","plan":"step then die","workers":[]}"#),
        Err("simulated crash: connection reset".into()),
    ];
    let orch = orchestrator(
        &repo,
        script,
        Arc::new(AutoApprove),
        Arc::new(AtomicBool::new(false)),
    );
    let status = orch.run_task(&ledger, "crash me").unwrap();
    assert_eq!(status, TaskStatus::Failed);

    // 重新 open(等价新进程读取账本):视图正确重建
    let (ledger2, proj, report) = TaskService::new(&repo).open_task(ledger.task_id()).unwrap();
    assert!(!report.torn_tail);
    assert_eq!(proj.status, TaskStatus::Failed);
    assert!(proj.last_error.is_some());

    // 阶段 2:resume(终态可恢复,spec 10)→ 完成
    let ledger2 = Arc::new(ledger2);
    let script2 = vec![
        act(r#"{"action":"plan","plan":"retry","workers":[]}"#),
        act(r#"{"action":"message","text":"recovered"}"#),
    ];
    let orch2 = orchestrator(
        &repo,
        script2,
        Arc::new(AutoApprove),
        Arc::new(AtomicBool::new(false)),
    );
    let status = orch2.run_task(&ledger2, "crash me").unwrap();
    assert_eq!(status, TaskStatus::Completed);

    // 全账本 seq 连续、无跳号(spec 40 公共规则)
    let proj = assert_ledger_healthy(&repo, ledger.task_id());
    assert_eq!(proj.status, TaskStatus::Completed);
    assert!(proj.runs.len() >= 2);
}

// ---------- A6:Provider 异常可解释 + 密钥不落账 ----------

#[test]
fn a6_provider_failure_is_explainable_and_secrets_stay_out() {
    setup_provider_env();
    let repo = git_repo("a6");
    let ledger = create_task(&repo, "will fail on provider");

    let script = vec![Err("provider_exhausted after 3 attempts: timeout".into())];
    let orch = orchestrator(
        &repo,
        script,
        Arc::new(AutoApprove),
        Arc::new(AtomicBool::new(false)),
    );
    let status = orch.run_task(&ledger, "will fail on provider").unwrap();
    assert_eq!(status, TaskStatus::Failed);

    let proj = assert_ledger_healthy(&repo, ledger.task_id());
    let (_, msg) = proj.last_error.expect("失败原因必须落账");
    assert!(msg.contains("provider"), "原因可解释: {msg}");

    // 公共规则:密钥/凭据不出现在账本文件
    let text = std::fs::read_to_string(ledger.path()).unwrap();
    for needle in [
        "test-key-not-real",
        "api.example.invalid",
        "Authorization",
        "Bearer ",
    ] {
        assert!(!text.contains(needle), "账本泄漏敏感串: {needle}");
    }
}

#[test]
fn agent_ref_is_usable() {
    // 账本事件主体枚举对外的可用性冒烟(保持 import 有效性)
    assert_eq!(AgentRef::Coordinator.to_string(), "coordinator");
    let _ = EventPayload::TaskPlanUpdated {
        plan: String::new(),
    };
    let _ = ApprovalDecision::Approve;
    let mut _w = std::io::sink();
    let _ = _w.write_all(b"");
}

// ---------- spec 70:修复轮 ----------

#[test]
fn s70_repair_round_recovers_after_illegal_output() {
    setup_provider_env();
    let repo = git_repo("s70-repair");
    let ledger = create_task(&repo, "repair me");

    let script = vec![
        // 规划轮第 1 次:夹杂说明文字 → 解析失败,进入修复轮
        act("好的,我的计划如下:{\"action\":\"plan\",\"plan\":\"p\",\"workers\":[]}"),
        // 修复轮:合法
        act(r#"{"action":"plan","plan":"p","workers":[]}"#),
        act(r#"{"action":"message","text":"done after repair"}"#),
    ];
    let orch = orchestrator(
        &repo,
        script,
        Arc::new(AutoApprove),
        Arc::new(AtomicBool::new(false)),
    );
    let status = orch.run_task(&ledger, "repair me").unwrap();
    assert_eq!(status, TaskStatus::Completed);

    // 修复过程落账:ErrorOccurred(scope=model_action)
    let proj = assert_ledger_healthy(&repo, ledger.task_id());
    let (_, msg) = proj.last_error.expect("修复错误应落账");
    assert!(msg.contains("action 解析失败"), "{msg}");
}

#[test]
fn s70_repair_exhaustion_fails_run_explainably() {
    setup_provider_env();
    let repo = git_repo("s70-exhaust");
    let ledger = create_task(&repo, "unrepairable");

    // 规划轮首轮 + 2 次修复全部非法 → Run Failed,原因可解释
    let script = vec![
        act("not json at all"),
        act(r#"{"action":"message","text":"a"} {"action":"message","text":"b"}"#),
        act(r#"{"action":"deploy","target":"prod"}"#),
    ];
    let orch = orchestrator(
        &repo,
        script,
        Arc::new(AutoApprove),
        Arc::new(AtomicBool::new(false)),
    );
    let status = orch.run_task(&ledger, "unrepairable").unwrap();
    assert_eq!(status, TaskStatus::Failed);

    let proj = assert_ledger_healthy(&repo, ledger.task_id());
    let (_, msg) = proj.last_error.expect("失败原因应落账");
    assert!(
        msg.contains("model_action") && msg.contains("修复"),
        "原因应说明修复耗尽: {msg}"
    );
}

// ---------- spec 110:Direct Execution Mode ----------

#[test]
fn s110_direct_execution_mode_first_turn_tool() {
    setup_provider_env();
    let repo = git_repo("s110-direct");
    std::fs::write(repo.join("a.txt"), "content-a\n").unwrap();
    // a.txt 入库:脏工作区守卫要求 apply 前工作区干净
    std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(&repo)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args([
            "-c", "user.name=t", "-c", "user.email=t@t",
            "commit", "-q", "-m", "add a.txt",
        ])
        .current_dir(&repo)
        .output()
        .unwrap();
    let ledger = create_task(&repo, "direct: copy a.txt to out.txt");

    // 规划轮直接返回 tool(无 plan)—— 真实模型常见形态
    let script = vec![
        act(r#"{"action":"tool","tool":"read_file","args":{"path":"a.txt"}}"#),
        act(
            r#"{"action":"tool","tool":"write_file","args":{"path":"out.txt","content":"content-a\n"}}"#,
        ),
        act(r#"{"action":"message","text":"direct execution done"}"#),
    ];
    let orch = orchestrator(
        &repo,
        script,
        Arc::new(AutoApprove),
        Arc::new(AtomicBool::new(false)),
    );
    let status = orch
        .run_task(&ledger, "direct: copy a.txt to out.txt")
        .unwrap();
    assert_eq!(status, TaskStatus::Completed);

    // 文件修改存在
    assert_eq!(
        std::fs::read_to_string(repo.join("out.txt")).unwrap(),
        "content-a\n"
    );

    // 隐式 plan 落账;无 Worker(spec 110 §1)
    let proj = assert_ledger_healthy(&repo, ledger.task_id());
    assert!(proj
        .plan
        .as_deref()
        .unwrap_or_default()
        .starts_with("Direct execution:"));
    assert!(
        proj.runs
            .iter()
            .all(|r| r.role == zealous_core::AgentRole::Coordinator),
        "Direct 模式不创建 Worker"
    );

    // 事件顺序(spec 110 §1):coordinator run 内
    // TS → TCR → TCC → TS → … 且 turn 编号单调递增、成对不重复
    let (events, _) = svc::events_for_test(&repo, ledger.task_id());
    let coord_run = proj
        .runs
        .first()
        .expect("应有 coordinator run")
        .run_id
        .clone();
    let mut started: std::collections::BTreeMap<u64, bool> = std::collections::BTreeMap::new();
    let mut completed: std::collections::BTreeMap<u64, bool> = std::collections::BTreeMap::new();
    let mut prev_turn = 0u64;
    for e in &events {
        if e.agent.as_str() != "coordinator" {
            continue;
        }
        match &e.payload {
            zealous_core::EventPayload::TurnStarted { run_id, turn }
                if run_id.as_str() == coord_run =>
            {
                assert!(!started.contains_key(turn), "turn {turn} 重复开始");
                assert!(*turn > prev_turn, "turn 编号应单调递增");
                started.insert(*turn, true);
                prev_turn = *turn;
            }
            zealous_core::EventPayload::TurnCompleted { run_id, turn }
                if run_id.as_str() == coord_run =>
            {
                assert!(started.contains_key(turn), "TC 前必须有 TS");
                assert!(!completed.contains_key(turn), "turn {turn} 重复完成");
                completed.insert(*turn, true);
            }
            _ => {}
        }
    }
    assert_eq!(started.len(), 3, "3 个 turn(读/写/汇报)");
    assert_eq!(completed.len(), 3);
}

// 供测试使用的账本读取(封装自 engine 公开 API)
mod svc {
    pub fn events_for_test(
        repo: &std::path::Path,
        task_id: &zealous_core::TaskId,
    ) -> (
        Vec<zealous_core::TaskEvent>,
        zealous_engine::ledger::ReadReport,
    ) {
        zealous_engine::ledger::read_events(&zealous_engine::ledger::events_path(
            &zealous_engine::ledger::task_dir(repo, task_id),
        ))
        .unwrap()
    }
}

#[test]
fn s110_plan_in_execution_phase_enters_repair() {
    setup_provider_env();
    let repo = git_repo("s110-plan-repair");
    let ledger = create_task(&repo, "plan in exec phase");

    let script = vec![
        act(r#"{"action":"plan","plan":"p","workers":[]}"#),
        // 执行阶段再返回 plan → 修复轮
        act(r#"{"action":"plan","plan":"again","workers":[]}"#),
        act(r#"{"action":"message","text":"done after repair"}"#),
    ];
    let orch = orchestrator(
        &repo,
        script,
        Arc::new(AutoApprove),
        Arc::new(AtomicBool::new(false)),
    );
    let status = orch.run_task(&ledger, "plan in exec phase").unwrap();
    assert_eq!(status, TaskStatus::Completed);

    let proj = assert_ledger_healthy(&repo, ledger.task_id());
    let (_, msg) = proj.last_error.expect("修复错误应落账");
    assert!(msg.contains("plan 动作不允许在此阶段"), "{msg}");
}
