//! 编排器:Coordinator/Worker 的进程内 Agent 循环与 V1 固定编排策略。
//!
//! Agent 循环 = "模型输出一个 JSON 动作 → 执行 → 结果回填上下文" 的多轮循环:
//! - `{"action":"tool","tool":"read_file","args":{...}}`: 经 ToolExecutor(风险闸门)
//! - `{"action":"message","text":"..."}`: 本 Agent 的最终答复,Run 结束
//! - `{"action":"plan","plan":"...","workers":["..."]}`: 仅 Coordinator 首轮,拆解任务
//!
//! Worker 约束(spec 30/10):T0 工具权;最终答复作为提案写入任务隔离目录,
//! 由 Coordinator 串行 apply 到主工作区(写主工作区必须走 T1 审批)。
//! 中断(spec 30):`abort` 置位后,循环在轮次边界停止,Task → Interrupted。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use zealous_core::{
    AgentRef, AgentRole, AgentRunId, AgentRunStatus, EventPayload, TaskStatus, MAX_WORKERS,
};

use crate::executor::ToolExecutor;
use crate::ledger::Ledger;
use crate::provider::Provider;

#[derive(Debug)]
pub enum OrchestratorError {
    Ledger(crate::ledger::LedgerError),
    /// 账本损坏等不可恢复状态(任务状态保持原样)
    Corrupt(String),
}

impl std::fmt::Display for OrchestratorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrchestratorError::Ledger(e) => write!(f, "orchestrator ledger: {e}"),
            OrchestratorError::Corrupt(s) => write!(f, "orchestrator: {s}"),
        }
    }
}

impl From<crate::ledger::LedgerError> for OrchestratorError {
    fn from(e: crate::ledger::LedgerError) -> Self {
        OrchestratorError::Ledger(e)
    }
}

/// 模型动作解析结果
#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    Plan { plan: String, workers: Vec<String> },
    Tool { tool: String, args_json: String },
    Message { text: String },
}

/// 非法模型输出的最大修复轮数(spec 70):首轮 + 2 次修复 = 最多 3 次调用
pub const MAX_ACTION_REPAIRS: u32 = 2;

/// 从原始输出提取 JSON 文本:接受裸 JSON(整体恰为一个 object)或
/// 单个 markdown fenced JSON block;夹带说明文字/多个对象一律拒绝(spec 70)。
fn extract_json_block(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        let rest = rest.trim_start_matches(['\r', '\n']);
        let end = rest.rfind("```").ok_or("fenced block 未闭合")?;
        let body = rest[..end].trim();
        let after = rest[end + 3..].trim();
        if !after.is_empty() {
            return Err("fenced block 之后存在多余内容(疑似多个输出)".into());
        }
        if body.contains("```") {
            return Err("存在多个 fenced block".into());
        }
        if body.is_empty() {
            return Err("fenced block 为空".into());
        }
        return Ok(body.to_string());
    }
    if !trimmed.starts_with('{') {
        return Err("输出不是 JSON 对象:裸 JSON 必须整体恰为一个 object,禁止夹杂说明文字".into());
    }
    if !trimmed.ends_with('}') {
        return Err("输出末尾不是 '}':可能存在多个对象或说明文字".into());
    }
    Ok(trimmed.to_string())
}

fn require_keys(
    obj: &serde_json::Map<String, serde_json::Value>,
    allowed: &[&str],
) -> Result<(), String> {
    for k in obj.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(format!("未知字段: {k}(允许的字段: {allowed:?})"));
        }
    }
    Ok(())
}

/// 严格解析(spec 70):必填字段齐全、类型合法、workers ≤ 3、未知字段拒绝
fn parse_action_strict(raw: &str) -> Result<Action, String> {
    let body = extract_json_block(raw)?;
    let v: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("json 解析失败: {e}"))?;
    let obj = v.as_object().ok_or("顶层必须是 JSON object")?;
    let action = obj
        .get("action")
        .and_then(|a| a.as_str())
        .ok_or("缺少 action 字段(必须是字符串)")?;
    match action {
        "plan" => {
            require_keys(obj, &["action", "plan", "workers"])?;
            let plan = obj
                .get("plan")
                .and_then(|p| p.as_str())
                .ok_or("plan.plan 必须是字符串")?
                .to_string();
            let workers = match obj.get("workers") {
                None | Some(serde_json::Value::Null) => Vec::new(),
                Some(w) => {
                    let arr = w.as_array().ok_or("plan.workers 必须是字符串数组")?;
                    if arr.len() > MAX_WORKERS as usize {
                        return Err(format!(
                            "plan.workers 数量 {} 超过上限 {MAX_WORKERS}",
                            arr.len()
                        ));
                    }
                    arr.iter()
                        .map(|x| {
                            x.as_str()
                                .map(String::from)
                                .ok_or_else(|| "plan.workers 元素必须是字符串".into())
                        })
                        .collect::<Result<Vec<String>, String>>()?
                }
            };
            Ok(Action::Plan { plan, workers })
        }
        "tool" => {
            require_keys(obj, &["action", "tool", "args"])?;
            let tool = obj
                .get("tool")
                .and_then(|t| t.as_str())
                .ok_or("tool.tool 必须是字符串")?
                .to_string();
            let args = obj.get("args").ok_or("缺少 tool.args")?;
            if !args.is_object() {
                return Err("tool.args 必须是 object".into());
            }
            Ok(Action::Tool {
                tool,
                args_json: args.to_string(),
            })
        }
        "message" => {
            require_keys(obj, &["action", "text"])?;
            let text = obj
                .get("text")
                .and_then(|t| t.as_str())
                .ok_or("message.text 必须是字符串")?
                .to_string();
            Ok(Action::Message { text })
        }
        other => Err(format!("未知 action: {other}(允许: plan / tool / message)")),
    }
}

/// action schema 提示(修复轮附给模型)
const ACTION_SCHEMA: &str = r#"恰好输出一个 JSON 对象,三选一:
{"action":"plan","plan":"<步骤>","workers":["<子任务>", ...]}
{"action":"tool","tool":"<工具名>","args":{...}}
{"action":"message","text":"<最终答复>"}
禁止:说明文字、markdown 代码围栏外的内容、多个对象、未知字段。"#;

/// 修复轮 prompt:指出解析错误并附 schema(spec 70);原输出已保留在上下文
fn repair_prompt(error: &str, raw: &str) -> String {
    format!(
        "你上一轮的输出不是合法 action JSON。\n解析错误:{error}\n原始输出(截断):{}\n\n{}",
        truncate(raw, 300),
        ACTION_SCHEMA
    )
}

/// turn_action 的固定环境参数(账本 + 当前 Run 标识 + 事件主体)
struct TurnEnv<'a> {
    ledger: &'a Ledger,
    run_id: &'a AgentRunId,
    agent: &'a AgentRef,
}

pub struct Orchestrator {
    pub provider: Arc<dyn Provider>,
    pub executor: Arc<ToolExecutor>,
    pub abort: Arc<AtomicBool>,
    /// 单 Agent 的最大轮次(防模型死循环)
    pub max_turns: u64,
    /// 提供给 Provider 的模型名
    pub model: String,
}

/// 单 Agent 循环的上下文(内存中的对话转写;账本保存的是持久事实)。
/// 多轮上下文原生传递给 Provider(spec 70),不再拼接进单个字符串。
struct LoopCtx {
    transcript: Vec<crate::provider::ChatMessage>,
}

impl Orchestrator {
    fn aborted(&self) -> bool {
        self.abort.load(Ordering::Relaxed)
    }

    fn append(
        &self,
        ledger: &Ledger,
        agent: AgentRef,
        payload: EventPayload,
    ) -> Result<u64, crate::ledger::LedgerError> {
        ledger.append(agent, payload)
    }

    /// 单轮模型调用:返回原始输出(已进转写);解析与修复由调用方处理
    fn turn_raw(
        &self,
        ctx: &mut LoopCtx,
        system: &str,
        user: &str,
    ) -> Result<String, crate::provider::ProviderError> {
        ctx.transcript
            .push(crate::provider::ChatMessage::user(user));
        let resp = self.provider.complete(
            &self.model,
            &crate::provider::ChatRequest {
                system: Some(system.to_string()),
                messages: ctx.transcript.clone(),
            },
        )?;
        ctx.transcript
            .push(crate::provider::ChatMessage::assistant(resp.text.clone()));
        Ok(resp.text)
    }

    /// 单轮模型调用 + 严格解析(spec 70)。
    /// TurnStarted 在此落账;TurnCompleted 一律由调用方统一落账(恰好一次,
    /// 包括修复轮路径)。解析失败:ErrorOccurred(model_action) 落账后返回
    /// `NeedsRepair`;重试与超限由调用方裁决。
    fn turn_action(
        &self,
        env: &TurnEnv<'_>,
        ctx: &mut LoopCtx,
        system: &str,
        prompt: &str,
        turn_no: u64,
    ) -> Result<Action, AgentLoopOutcome> {
        self.append(
            env.ledger,
            env.agent.clone(),
            EventPayload::TurnStarted {
                run_id: env.run_id.clone(),
                turn: turn_no,
            },
        )
        .map_err(AgentLoopOutcome::Ledger)?;
        let raw = match self.turn_raw(ctx, system, prompt) {
            Ok(r) => r,
            Err(e) => return Err(AgentLoopOutcome::ProviderFailed(e)),
        };
        parse_action_strict(&raw).map_err(|e| {
            if let Err(le) = self.append(
                env.ledger,
                env.agent.clone(),
                EventPayload::ErrorOccurred {
                    scope: "model_action".into(),
                    message: format!("action 解析失败: {e}"),
                },
            ) {
                return AgentLoopOutcome::Ledger(le);
            }
            AgentLoopOutcome::NeedsRepair {
                next_prompt: repair_prompt(&e, &raw),
            }
        })
    }

    /// 单 Agent 的工具循环:循环执行 tool 动作,直到 message/abort/超轮次。
    /// 返回最终 message 文本(作为本 Agent 的产出)。修复计数由本循环持有。
    fn agent_loop(
        &self,
        ledger: &Ledger,
        run_id: &AgentRunId,
        agent: AgentRef,
        system: &str,
        first_prompt: &str,
    ) -> Result<String, AgentLoopOutcome> {
        let mut ctx = LoopCtx {
            transcript: Vec::new(),
        };
        let mut prompt = first_prompt.to_string();
        let mut repairs = 0u32;
        for turn_no in 1..=self.max_turns {
            if self.aborted() {
                return Err(AgentLoopOutcome::Aborted);
            }
            let action = match self.turn_action(
                &TurnEnv {
                    ledger,
                    run_id,
                    agent: &agent,
                },
                &mut ctx,
                system,
                &prompt,
                turn_no,
            ) {
                Ok(a) => {
                    repairs = 0;
                    a
                }
                Err(AgentLoopOutcome::NeedsRepair { next_prompt }) => {
                    repairs += 1;
                    if repairs > MAX_ACTION_REPAIRS {
                        return Err(AgentLoopOutcome::InvalidModelOutput(format!(
                            "model_action 修复 {MAX_ACTION_REPAIRS} 轮后仍非法"
                        )));
                    }
                    self.append(
                        ledger,
                        agent.clone(),
                        EventPayload::TurnCompleted {
                            run_id: run_id.clone(),
                            turn: turn_no,
                        },
                    )
                    .map_err(AgentLoopOutcome::Ledger)?;
                    prompt = next_prompt;
                    continue;
                }
                Err(e) => return Err(e),
            };
            match action {
                Action::Message { text } => {
                    self.append(
                        ledger,
                        agent.clone(),
                        EventPayload::MessageCompleted {
                            run_id: run_id.clone(),
                            turn: turn_no,
                            text: text.clone(),
                        },
                    )
                    .map_err(AgentLoopOutcome::Ledger)?;
                    self.append(
                        ledger,
                        agent.clone(),
                        EventPayload::TurnCompleted {
                            run_id: run_id.clone(),
                            turn: turn_no,
                        },
                    )
                    .map_err(AgentLoopOutcome::Ledger)?;
                    return Ok(text);
                }
                Action::Tool { tool, args_json } => {
                    let outcome = self
                        .executor
                        .run_tool(ledger, run_id, agent.clone(), &args_json, &tool)
                        .map_err(executor_err)?;
                    prompt = format!(
                        "tool `{tool}` result (ok={}):\n{}\n\n继续:返回下一个 JSON 动作。",
                        outcome.ok,
                        truncate(&outcome.text, 2000)
                    );
                }
                Action::Plan { .. } => {
                    // Worker 上下文中不允许 plan 动作:按非法动作处理
                    prompt =
                        "plan action is not allowed here; return a tool or message action.".into();
                }
            }
            // Tool/Plan 路径的收尾(Message 已提前 return;修复路径在分支内)
            self.append(
                ledger,
                agent.clone(),
                EventPayload::TurnCompleted {
                    run_id: run_id.clone(),
                    turn: turn_no,
                },
            )
            .map_err(AgentLoopOutcome::Ledger)?;
        }
        Err(AgentLoopOutcome::TooManyTurns(self.max_turns))
    }

    /// 运行/恢复任务到终态(或 Interrupted)。返回最终 Task 状态。
    /// `ledger` 必须以 Arc 共享:Worker 线程克隆同一实例保证 seq 全局连续。
    pub fn run_task(
        &self,
        ledger: &Arc<Ledger>,
        task_prompt: &str,
    ) -> Result<TaskStatus, OrchestratorError> {
        let proj = ledger.fold_now().map_err(OrchestratorError::Ledger)?;
        let mut from = proj.status;

        // 崩溃规范化(spec 60 §2):投影为 Running = 上次进程异常终止。
        // TaskInterrupted 的 fold 语义自动把 dangling pending approval 收为
        // Rejected(Rejected 是 ToolCall 终态,失败结果由投影承载给新 Run)。
        if from == TaskStatus::Running {
            self.append(
                ledger,
                AgentRef::User,
                EventPayload::TaskInterrupted {
                    reason: "process_restart".into(),
                },
            )?;
            self.append(
                ledger,
                AgentRef::User,
                EventPayload::TaskStatusChanged {
                    from: TaskStatus::Running,
                    to: TaskStatus::Interrupted,
                    reason: Some("process_restart".into()),
                },
            )?;
            from = TaskStatus::Interrupted;
        }

        // resume 语义(spec 10):非 Queued 进入时记 TaskResumed;终态/中断都可 resume
        // resume 必须携带投影摘要(spec 70):模型据此续跑而非重新开始
        let resume_brief = if from != TaskStatus::Queued {
            Some(resume_brief(&proj))
        } else {
            None
        };
        if from != TaskStatus::Queued {
            let run_id = AgentRunId::generate();
            self.append(
                ledger,
                AgentRef::User,
                EventPayload::TaskResumed {
                    run_id: run_id.clone(),
                },
            )?;
        }
        self.append(
            ledger,
            AgentRef::User,
            EventPayload::TaskStatusChanged {
                from,
                to: TaskStatus::Running,
                reason: None,
            },
        )?;

        let coord_run = AgentRunId::generate();
        self.append(
            ledger,
            AgentRef::Coordinator,
            EventPayload::AgentRunStarted {
                run_id: coord_run.clone(),
                role: AgentRole::Coordinator,
            },
        )?;

        if self.aborted() {
            return self.interrupt(ledger, &coord_run, "aborted before start");
        }

        // ---- 阶段 1:规划(spec 10:1 Coordinator + ≤3 Worker)----
        let plan_prompt = if let Some(brief) = &resume_brief {
            format!(
                "{brief}\n\nTASK: {task_prompt}\n\n\
                 返回一个 JSON 动作。规划动作格式:\n\
                 {{\"action\":\"plan\",\"plan\":\"<步骤>\",\"workers\":[\"<worker 子任务>\", ...]}}\n\
                 workers 最多 3 个,只安排只读分析任务。注意:这是续跑,基于上方投影增量推进。"
            )
        } else {
            format!(
                "TASK: {task_prompt}\n\n\
                 返回一个 JSON 动作。规划动作格式:\n\
                 {{\"action\":\"plan\",\"plan\":\"<步骤>\",\"workers\":[\"<worker 子任务>\", ...]}}\n\
                 workers 最多 3 个,只安排只读分析任务。"
            )
        };
        // 工具契约由注册表生成(spec 60 §5),与实现天然一致
        let coord_system = format!(
            "你是 Coordinator:拆解任务、调度只读 Worker、串行应用结果、验证并汇报。\n\
             每轮只返回一个 JSON 动作对象。可用动作:\n\
             {{\"action\":\"plan\",\"plan\":\"<步骤>\",\"workers\":[\"<子任务>\", ...]}}(仅首轮)\n\
             {{\"action\":\"tool\",\"tool\":\"<工具名>\",\"args\":{{...}}}}\n\
             {{\"action\":\"message\",\"text\":\"<最终答复>\"}}\n\
             {}",
            crate::tools::tool_manifest(None)
        );
        let mut ctx = LoopCtx {
            transcript: Vec::new(),
        };
        // 规划轮同样受修复轮约束(spec 70):首轮 + ≤2 次修复;
        // turn 编号从此处起算,apply 阶段接续(spec 20:run 内 turn 单调)
        let action = {
            let mut prompt = plan_prompt;
            let mut parsed: Option<Action> = None;
            let mut turn_no: u64 = 0;
            while turn_no <= u64::from(MAX_ACTION_REPAIRS) {
                turn_no += 1;
                if self.aborted() {
                    return self.interrupt(ledger, &coord_run, "aborted during planning");
                }
                let env = TurnEnv {
                    ledger,
                    run_id: &coord_run,
                    agent: &AgentRef::Coordinator,
                };
                match self.turn_action(&env, &mut ctx, &coord_system, &prompt, turn_no) {
                    Ok(a) => {
                        // 成功路径的 TurnCompleted 由本循环补齐(break 前恰好一次)
                        self.append(
                            ledger,
                            AgentRef::Coordinator,
                            EventPayload::TurnCompleted {
                                run_id: coord_run.clone(),
                                turn: turn_no,
                            },
                        )?;
                        parsed = Some(a);
                        break;
                    }
                    Err(AgentLoopOutcome::NeedsRepair { next_prompt }) => {
                        self.append(
                            ledger,
                            AgentRef::Coordinator,
                            EventPayload::TurnCompleted {
                                run_id: coord_run.clone(),
                                turn: turn_no,
                            },
                        )?;
                        prompt = next_prompt;
                    }
                    Err(AgentLoopOutcome::ProviderFailed(e)) => {
                        return self.provider_failed(ledger, &coord_run, e);
                    }
                    Err(e) => {
                        return Err(OrchestratorError::Ledger(
                            crate::ledger::LedgerError::Corrupt {
                                line: 0,
                                detail: format!("规划轮中断: {e:?}"),
                            },
                        ));
                    }
                }
            }
            match parsed {
                Some(a) => (a, turn_no),
                None => {
                    return self.fail_task(
                        ledger,
                        &coord_run,
                        format!("model_action 规划轮修复 {MAX_ACTION_REPAIRS} 次后仍非法"),
                    );
                }
            }
        };
        let (action, planning_turns) = action;
        let (plan, worker_tasks) = match action {
            Action::Plan { plan, workers } => (plan, workers),
            Action::Message { text } => {
                // 模型直接给结论而无拆解:V1 允许(单 Agent 任务),作为方案落地
                (text, Vec::new())
            }
            Action::Tool { tool, args_json } => {
                // 规划轮不允许直接动工具:记录并失败(可解释)
                return self.fail_task(
                    ledger,
                    &coord_run,
                    format!("planning turn returned tool action: {tool} {args_json}"),
                );
            }
        };
        self.append(
            ledger,
            AgentRef::Coordinator,
            EventPayload::TaskPlanUpdated { plan: plan.clone() },
        )?;

        if self.aborted() {
            return self.interrupt(ledger, &coord_run, "aborted after planning");
        }

        // ---- 阶段 2:Worker 只读分析(进程内并发,产出写任务隔离目录)----
        // 单写入者约束(spec 20):Worker 与 Coordinator 共享同一 Arc<Ledger>,
        // 账本写入经内部 Mutex 串行,seq 全局连续
        let task_dir = ledger
            .path()
            .parent()
            .map(PathBuf::from)
            .ok_or_else(|| OrchestratorError::Corrupt("ledger has no parent dir".into()))?;
        let shared_ledger: Arc<Ledger> = ledger.clone();
        let mut worker_handles = Vec::new();
        for (i, subtask) in worker_tasks.iter().enumerate() {
            let worker_no = (i + 1) as u8;
            let provider = self.provider.clone();
            let executor = self.executor.clone();
            let abort = self.abort.clone();
            let model = self.model.clone();
            let worker_ledger = shared_ledger.clone();
            let ledger_snapshot_task_dir = task_dir.clone();
            let subtask = subtask.clone();
            let plan = plan.clone();
            worker_handles.push(std::thread::spawn(move || {
                run_worker(
                    provider,
                    executor,
                    abort,
                    model,
                    worker_ledger,
                    ledger_snapshot_task_dir,
                    worker_no,
                    subtask,
                    plan,
                )
            }));
        }
        let mut proposals = Vec::new();
        for (i, h) in worker_handles.into_iter().enumerate() {
            let worker_no = i + 1;
            match h.join() {
                Ok(Ok(text)) => {
                    proposals.push(format!("[worker-{} proposal]\n{}", worker_no, text))
                }
                Ok(Err(e)) => proposals.push(format!("[worker-{worker_no} failed] {e}")),
                Err(_) => proposals.push(format!("[worker-{worker_no} panicked]")),
            }
        }

        // ---- 阶段 3:Coordinator 串行 apply + 验证 + 汇报 ----
        let apply_prompt = if proposals.is_empty() {
            format!(
                "TASK: {task_prompt}\nPLAN: {plan}\n\n没有 Worker 提案。请直接完成:\
                 需要修改文件用 {{\"action\":\"tool\",\"tool\":\"write_file\",\"args\":{{\"path\":\"...\",\"content\":\"...\"}}}},\
                 验证用 {{\"action\":\"tool\",\"tool\":\"shell\",\"args\":{{\"command\":\"...\"}}}},\
                 完成后 {{\"action\":\"message\",\"text\":\"...\"}}。"
            )
        } else {
            format!(
                "TASK: {task_prompt}\nPLAN: {plan}\nWorker 提案如下(按顺序串行应用,逐轮一个动作):\n{}\n\n\
                 逐个用 write_file 应用到主工作区,再执行一次 shell 验证,最后用 message 汇报。",
                proposals.join("\n\n")
            )
        };
        // 续用规划轮的转写,带上 plan 上下文
        ctx.transcript.push(crate::provider::ChatMessage::assistant(
            serde_json::json!({"action":"plan","plan":plan}).to_string(),
        ));
        let run = self.agent_loop_continue(
            ledger,
            &coord_run,
            &mut ctx,
            &coord_system,
            &apply_prompt,
            planning_turns + 1,
        );
        self.finish_task(ledger, &coord_run, run)
    }

    fn agent_loop_continue(
        &self,
        ledger: &Ledger,
        run_id: &AgentRunId,
        ctx: &mut LoopCtx,
        system: &str,
        prompt: &str,
        start_turn: u64,
    ) -> Result<String, AgentLoopOutcome> {
        // 与 agent_loop 相同的循环体;拆出独立实现以复用已开始的转写。
        // turn 编号接续规划轮(run 内单调,spec 20)
        let mut prompt = prompt.to_string();
        let mut repairs = 0u32;
        for turn_no in start_turn..start_turn + self.max_turns {
            if self.aborted() {
                return Err(AgentLoopOutcome::Aborted);
            }
            let env = TurnEnv {
                ledger,
                run_id,
                agent: &AgentRef::Coordinator,
            };
            let action = match self.turn_action(&env, &mut *ctx, system, &prompt, turn_no) {
                Ok(a) => {
                    repairs = 0;
                    a
                }
                Err(AgentLoopOutcome::NeedsRepair { next_prompt }) => {
                    repairs += 1;
                    if repairs > MAX_ACTION_REPAIRS {
                        return Err(AgentLoopOutcome::InvalidModelOutput(format!(
                            "model_action 修复 {MAX_ACTION_REPAIRS} 轮后仍非法"
                        )));
                    }
                    self.append(
                        ledger,
                        AgentRef::Coordinator,
                        EventPayload::TurnCompleted {
                            run_id: run_id.clone(),
                            turn: turn_no,
                        },
                    )
                    .map_err(AgentLoopOutcome::Ledger)?;
                    prompt = next_prompt;
                    continue;
                }
                Err(e) => return Err(e),
            };
            match action {
                Action::Message { text } => {
                    self.append(
                        ledger,
                        AgentRef::Coordinator,
                        EventPayload::MessageCompleted {
                            run_id: run_id.clone(),
                            turn: turn_no,
                            text: text.clone(),
                        },
                    )
                    .map_err(AgentLoopOutcome::Ledger)?;
                    self.append(
                        ledger,
                        AgentRef::Coordinator,
                        EventPayload::TurnCompleted {
                            run_id: run_id.clone(),
                            turn: turn_no,
                        },
                    )
                    .map_err(AgentLoopOutcome::Ledger)?;
                    return Ok(text);
                }
                Action::Tool { tool, args_json } => {
                    let outcome = self
                        .executor
                        .run_tool(ledger, run_id, AgentRef::Coordinator, &args_json, &tool)
                        .map_err(executor_err)?;
                    prompt = format!(
                        "tool `{tool}` result (ok={}):\n{}\n\n继续:返回下一个 JSON 动作。",
                        outcome.ok,
                        truncate(&outcome.text, 2000)
                    );
                }
                Action::Plan { .. } => {
                    prompt = "plan is not allowed after planning; use tool or message.".into();
                }
            }
            // Tool/Plan 路径的收尾(Message 已提前 return;修复路径在分支内)
            self.append(
                ledger,
                AgentRef::Coordinator,
                EventPayload::TurnCompleted {
                    run_id: run_id.clone(),
                    turn: turn_no,
                },
            )
            .map_err(AgentLoopOutcome::Ledger)?;
        }
        Err(AgentLoopOutcome::TooManyTurns(self.max_turns))
    }

    fn finish_task(
        &self,
        ledger: &Ledger,
        coord_run: &AgentRunId,
        result: Result<String, AgentLoopOutcome>,
    ) -> Result<TaskStatus, OrchestratorError> {
        match result {
            // turn_action 返回 NeedsRepair 时由循环消化;到达 finish 视为不可达
            Err(AgentLoopOutcome::NeedsRepair { next_prompt }) => self.fail_task(
                ledger,
                coord_run,
                format!("model_action 未消化修复轮: {}", truncate(&next_prompt, 200)),
            ),
            Ok(_) => {
                self.append(
                    ledger,
                    AgentRef::Coordinator,
                    EventPayload::AgentRunEnded {
                        run_id: coord_run.clone(),
                        status: AgentRunStatus::Completed,
                        reason: None,
                    },
                )?;
                self.append(
                    ledger,
                    AgentRef::User,
                    EventPayload::TaskStatusChanged {
                        from: TaskStatus::Running,
                        to: TaskStatus::Completed,
                        reason: None,
                    },
                )?;
                Ok(TaskStatus::Completed)
            }
            Err(AgentLoopOutcome::Aborted) => self.interrupt(ledger, coord_run, "aborted"),
            Err(AgentLoopOutcome::ProviderFailed(e)) => {
                self.fail_task(ledger, coord_run, format!("provider: {e}"))
            }
            Err(AgentLoopOutcome::TooManyTurns(max)) => self.fail_task(
                ledger,
                coord_run,
                format!("agent loop exceeded max turns ({max})"),
            ),
            Err(AgentLoopOutcome::InvalidModelOutput(reason)) => {
                self.fail_task(ledger, coord_run, reason)
            }
            Err(AgentLoopOutcome::Ledger(e)) => Err(OrchestratorError::Ledger(e)),
        }
    }

    fn provider_failed(
        &self,
        ledger: &Ledger,
        coord_run: &AgentRunId,
        e: crate::provider::ProviderError,
    ) -> Result<TaskStatus, OrchestratorError> {
        self.fail_task(ledger, coord_run, format!("provider: {e}"))
    }

    /// 失败落账:Run Failed + Task Failed + ErrorOccurred(原因可解释,A6)
    fn fail_task(
        &self,
        ledger: &Ledger,
        coord_run: &AgentRunId,
        reason: String,
    ) -> Result<TaskStatus, OrchestratorError> {
        self.append(
            ledger,
            AgentRef::Coordinator,
            EventPayload::AgentRunEnded {
                run_id: coord_run.clone(),
                status: AgentRunStatus::Failed,
                reason: Some(reason.clone()),
            },
        )?;
        self.append(
            ledger,
            AgentRef::Coordinator,
            EventPayload::ErrorOccurred {
                scope: "orchestrator".into(),
                message: reason.clone(),
            },
        )?;
        self.append(
            ledger,
            AgentRef::User,
            EventPayload::TaskStatusChanged {
                from: TaskStatus::Running,
                to: TaskStatus::Failed,
                reason: Some(reason),
            },
        )?;
        Ok(TaskStatus::Failed)
    }

    /// 中断落账(spec 30):挂起审批由 ToolExecutor 的 sink 语义处理,
    /// 这里记录 TaskInterrupted + 状态 Interrupted。
    fn interrupt(
        &self,
        ledger: &Ledger,
        coord_run: &AgentRunId,
        reason: &str,
    ) -> Result<TaskStatus, OrchestratorError> {
        self.append(
            ledger,
            AgentRef::User,
            EventPayload::TaskInterrupted {
                reason: reason.to_string(),
            },
        )?;
        self.append(
            ledger,
            AgentRef::Coordinator,
            EventPayload::AgentRunEnded {
                run_id: coord_run.clone(),
                status: AgentRunStatus::Killed,
                reason: Some(reason.to_string()),
            },
        )?;
        self.append(
            ledger,
            AgentRef::User,
            EventPayload::TaskStatusChanged {
                from: TaskStatus::Running,
                to: TaskStatus::Interrupted,
                reason: Some(reason.to_string()),
            },
        )?;
        Ok(TaskStatus::Interrupted)
    }
}

#[derive(Debug)]
enum AgentLoopOutcome {
    Aborted,
    /// 解析失败,需要修复轮;next_prompt 已附错误与 schema
    NeedsRepair {
        next_prompt: String,
    },
    /// 修复轮耗尽仍非法:当前 Run 以可解释原因失败(spec 70)
    InvalidModelOutput(String),
    ProviderFailed(crate::provider::ProviderError),
    TooManyTurns(u64),
    Ledger(crate::ledger::LedgerError),
}

/// ExecutorError → 循环级账本错误(审计链断裂按 Corrupt 语义向上冒泡)
fn executor_err(e: crate::executor::ExecutorError) -> AgentLoopOutcome {
    use crate::executor::ExecutorError;
    match e {
        ExecutorError::LedgerIo(le) => AgentLoopOutcome::Ledger(le),
        ExecutorError::Ledger(fe, _what) => {
            AgentLoopOutcome::Ledger(crate::ledger::LedgerError::Fold(fe))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_worker(
    provider: Arc<dyn Provider>,
    executor: Arc<ToolExecutor>,
    abort: Arc<AtomicBool>,
    model: String,
    ledger: Arc<Ledger>,
    task_dir: PathBuf,
    worker_no: u8,
    subtask: String,
    plan: String,
) -> Result<String, String> {
    let orchestrator = Orchestrator {
        provider,
        executor,
        abort,
        max_turns: 10,
        model,
    };
    let agent = AgentRef::Worker(worker_no);
    let run_id = AgentRunId::generate();
    orchestrator
        .append(
            &ledger,
            agent.clone(),
            EventPayload::AgentRunStarted {
                run_id: run_id.clone(),
                role: AgentRole::Worker(worker_no),
            },
        )
        .map_err(|e| format!("{e}"))?;

    if orchestrator.aborted() {
        orchestrator
            .append(
                &ledger,
                agent.clone(),
                EventPayload::AgentRunEnded {
                    run_id: run_id.clone(),
                    status: AgentRunStatus::Killed,
                    reason: Some("aborted".into()),
                },
            )
            .map_err(|e| format!("{e}"))?;
        return Err("aborted".into());
    }

    let system = format!(
        "你是 worker-{worker_no},只做只读分析,禁止修改任何文件。\n\
         完成分析后返回 {{\"action\":\"message\",\"text\":\"<提案>\"}}。\n\
         {}",
        crate::tools::tool_manifest(Some(zealous_core::RiskTier::T0))
    );
    let prompt = format!("OVERALL PLAN: {plan}\nYOUR SUBTASK: {subtask}");
    match orchestrator.agent_loop(&ledger, &run_id, agent.clone(), &system, &prompt) {
        Ok(proposal) => {
            // 提案写任务隔离目录:框架行为,不经工具层(spec 30)
            let dir = task_dir.join("workers").join(format!("worker-{worker_no}"));
            std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir: {e}"))?;
            std::fs::write(dir.join("proposal.md"), &proposal)
                .map_err(|e| format!("write: {e}"))?;
            orchestrator
                .append(
                    &ledger,
                    agent,
                    EventPayload::AgentRunEnded {
                        run_id,
                        status: AgentRunStatus::Completed,
                        reason: None,
                    },
                )
                .map_err(|e| format!("{e}"))?;
            Ok(proposal)
        }
        Err(AgentLoopOutcome::Aborted) => {
            let _ = orchestrator.append(
                &ledger,
                agent,
                EventPayload::AgentRunEnded {
                    run_id,
                    status: AgentRunStatus::Killed,
                    reason: Some("aborted".into()),
                },
            );
            Err("aborted".into())
        }
        Err(e) => {
            let _ = orchestrator.append(
                &ledger,
                agent,
                EventPayload::AgentRunEnded {
                    run_id,
                    status: AgentRunStatus::Failed,
                    reason: Some(format!("{e:?}")),
                },
            );
            Err(format!("{e:?}"))
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// 恢复上下文摘要(spec 70):从投影构造,包含原任务、中断次数、已有计划、
/// 各 Run 最后产出、工具调用摘要与上次错误。三条硬性提示:续跑非重开、
/// 已完成的读操作不必重复、被拒/失败的工具需调整方案。
fn resume_brief(proj: &zealous_core::TaskProjection) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "【这是续跑(RESUME),不是重新开始】任务原标题:{}",
        proj.title
    );
    let _ = writeln!(
        s,
        "恢复前状态:{:?};历史中断次数:{};已完成读操作不必重复",
        proj.status, proj.interrupted_count
    );
    if let Some(plan) = &proj.plan {
        let _ = writeln!(s, "已有计划:{}", truncate(plan, 400));
    }
    for r in &proj.runs {
        if let Some(m) = &r.last_message {
            let _ = writeln!(s, "[run {} 最后产出] {}", r.run_id, truncate(m, 300));
        }
    }
    for c in &proj.tool_calls {
        let _ = writeln!(
            s,
            "[工具摘要] {} 风险={:?} 状态={:?}",
            c.tool, c.risk, c.state
        );
    }
    if let Some((scope, msg)) = &proj.last_error {
        let _ = writeln!(
            s,
            "上次错误 [{scope}]:{}(被拒绝或失败的工具需要调整方案)",
            truncate(msg, 300)
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::AutoApprove;
    use crate::executor::ToolExecutor;
    use crate::ledger::{read_events, Ledger};
    use crate::tools::builtin_registry;
    use std::path::Path;
    use zealous_core::{projection, EventPayload, RiskTier, TaskId};

    pub(crate) fn fixture_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zealous-orch-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(crate) fn msg(text: &str) -> Result<String, String> {
        Ok(format!("{{\"action\":\"message\",\"text\":\"{text}\"}}"))
    }

    /// 模拟一次"进程被 kill"的账本:任务 Running + 挂起审批,无收尾事件
    pub(crate) fn crashed_running_ledger(dir: &Path) -> Arc<Ledger> {
        let task_id = TaskId::generate();
        let ledger = Arc::new(Ledger::create(dir, &task_id).unwrap());
        ledger
            .append(
                AgentRef::User,
                EventPayload::TaskCreated {
                    title: "t".into(),
                    repo_root: dir.display().to_string(),
                },
            )
            .unwrap();
        ledger
            .append(
                AgentRef::User,
                EventPayload::TaskStatusChanged {
                    from: TaskStatus::Queued,
                    to: TaskStatus::Running,
                    reason: None,
                },
            )
            .unwrap();
        let run = AgentRunId::generate();
        ledger
            .append(
                AgentRef::Coordinator,
                EventPayload::AgentRunStarted {
                    run_id: run.clone(),
                    role: AgentRole::Coordinator,
                },
            )
            .unwrap();
        let call = zealous_core::ToolCallId::generate();
        ledger
            .append(
                AgentRef::Coordinator,
                EventPayload::ToolCallRequested {
                    call_id: call.clone(),
                    run_id: run,
                    tool: "shell".into(),
                    args_json: "{}".into(),
                    risk: RiskTier::T2,
                },
            )
            .unwrap();
        ledger
            .append(
                AgentRef::Coordinator,
                EventPayload::ApprovalRequested {
                    call_id: call,
                    summary: "echo".into(),
                },
            )
            .unwrap();
        ledger
    }

    #[test]
    fn resume_normalizes_crashed_running_and_rejects_dangling_approval() {
        let dir = fixture_dir("crash");
        let ledger = crashed_running_ledger(&dir);
        // 前置:确实挂着一个审批
        let proj = ledger.fold_now().unwrap();
        assert_eq!(proj.status, TaskStatus::Running);
        assert_eq!(proj.pending_approvals.len(), 1);

        let orch = Orchestrator {
            provider: Arc::new(crate::provider::MockProvider::scripted(vec![
                msg("straight answer"),
                msg("recovered after restart"),
            ])),
            executor: Arc::new(ToolExecutor::new(
                &dir,
                builtin_registry(),
                Arc::new(AutoApprove),
            )),
            abort: Arc::new(AtomicBool::new(false)),
            max_turns: 8,
            model: "mock".into(),
        };
        let status = orch.run_task(&ledger, "t").unwrap();
        assert_eq!(status, TaskStatus::Completed);

        // 从磁盘全量重读:seq 连续、可折叠(审计链合法)
        let (events, report) =
            read_events(&crate::ledger::events_path(ledger.path().parent().unwrap())).unwrap();
        assert!(!report.torn_tail);
        let proj = projection::fold(ledger.task_id(), &events).unwrap();
        assert_eq!(proj.status, TaskStatus::Completed);
        // dangling approval 被规范化为 Rejected(spec 60 §2)
        assert_eq!(
            proj.tool_calls[0].state,
            projection::ToolCallState::Rejected
        );
        assert!(proj.pending_approvals.is_empty());
        // 至少:原 Run + 规范化中断 + 新 Coordinator Run
        assert!(proj.runs.len() >= 2);
        assert!(proj.interrupted_count >= 1);
        assert!(events.iter().any(|e| matches!(
            &e.payload,
            EventPayload::TaskInterrupted { reason } if reason == "process_restart"
        )));
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn bare_json_parses() {
        let a = parse_action_strict(r#"{"action":"message","text":"hi"}"#).unwrap();
        assert_eq!(a, Action::Message { text: "hi".into() });
    }

    #[test]
    fn fenced_json_parses() {
        let a =
            parse_action_strict("```json\n{\"action\":\"message\",\"text\":\"hi\"}\n```").unwrap();
        assert_eq!(a, Action::Message { text: "hi".into() });
    }

    #[test]
    fn prose_plus_json_rejected() {
        assert!(
            parse_action_strict("好的,这是我的动作:{\"action\":\"message\",\"text\":\"hi\"}")
                .is_err()
        );
    }

    #[test]
    fn multiple_objects_rejected() {
        assert!(parse_action_strict(
            r#"{"action":"message","text":"a"} {"action":"message","text":"b"}"#
        )
        .is_err());
    }

    #[test]
    fn unknown_field_rejected() {
        assert!(
            parse_action_strict(r#"{"action":"message","text":"hi","confidence":0.9}"#).is_err()
        );
    }

    #[test]
    fn missing_required_rejected() {
        assert!(parse_action_strict(r#"{"action":"message"}"#).is_err());
        assert!(parse_action_strict(r#"{"action":"tool","args":{}}"#).is_err());
    }

    #[test]
    fn tool_args_must_be_object() {
        assert!(
            parse_action_strict(r#"{"action":"tool","tool":"read_file","args":"a.txt"}"#).is_err()
        );
    }

    #[test]
    fn workers_over_limit_rejected() {
        assert!(
            parse_action_strict(r#"{"action":"plan","plan":"p","workers":["a","b","c","d"]}"#)
                .is_err()
        );
    }

    #[test]
    fn unknown_action_rejected() {
        assert!(parse_action_strict(r#"{"action":"deploy","target":"prod"}"#).is_err());
    }
}

#[cfg(test)]
mod resume_ctx_tests {
    use super::tests::*;
    use super::*;
    use crate::approval::AutoApprove;
    use crate::tools::builtin_registry;

    #[test]
    fn resume_prompt_carries_projection_context() {
        let dir = fixture_dir("resume-ctx");
        let ledger = crashed_running_ledger(&dir);
        // 增补历史要素:计划 + 错误(投影摘要的断言素材)
        ledger
            .append(
                AgentRef::Coordinator,
                EventPayload::TaskPlanUpdated {
                    plan: "先分析再修改".into(),
                },
            )
            .unwrap();
        ledger
            .append(
                AgentRef::Coordinator,
                EventPayload::ErrorOccurred {
                    scope: "orchestrator".into(),
                    message: "验证命令失败: exit 1".into(),
                },
            )
            .unwrap();

        let mock = Arc::new(crate::provider::MockProvider::scripted(vec![msg(
            "续跑完成",
        )]));
        let orch = Orchestrator {
            provider: mock.clone(),
            executor: Arc::new(ToolExecutor::new(
                &dir,
                builtin_registry(),
                Arc::new(AutoApprove),
            )),
            abort: Arc::new(AtomicBool::new(false)),
            max_turns: 8,
            model: "mock".into(),
        };
        let status = orch.run_task(&ledger, "t").unwrap();
        assert_eq!(status, TaskStatus::Completed);

        let prompts = mock.prompts.lock().unwrap();
        assert!(!prompts.is_empty());
        let first = &prompts[0];
        let joined = format!(
            "{}\n{}",
            first.system.clone().unwrap_or_default(),
            first
                .messages
                .iter()
                .map(|m| m.content.clone())
                .collect::<Vec<_>>()
                .join("\n")
        );
        // spec 70:resume prompt 必须携带计划、工具摘要、上次错误与续跑语义
        assert!(joined.contains("续跑"), "缺少续跑语义: {joined}");
        assert!(joined.contains("先分析再修改"), "缺少历史计划: {joined}");
        assert!(
            joined.contains("工具摘要") && joined.contains("shell"),
            "缺少工具摘要: {joined}"
        );
        assert!(joined.contains("验证命令失败"), "缺少上次错误: {joined}");
        assert!(
            joined.contains("不必重复"),
            "缺少读操作免重复提示: {joined}"
        );
    }
}
