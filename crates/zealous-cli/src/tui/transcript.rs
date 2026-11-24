//! spec 173:聊天转录与事件账本的同步(工具卡片回放)。
//! 纯函数:输入 `TuiState` 与账本事件切片,只改转录派生数据;
//! 启动恢复与运行中增量同步共用同一实现,事实源始终是账本。

use super::state::{ChatEntry, ChatRole, ChatToolCard, ChatToolState, TuiState};
use zealous_core::{ApprovalDecision, EventPayload, TaskEvent};

/// 关键参数的展示优先级(命中即作为卡片主参数)
const PREFERRED_KEYS: [&str; 6] = ["path", "command", "cmd", "query", "object", "file"];
/// 主参数摘要的最大字符数
const ARGS_MAX_CHARS: usize = 100;

/// 工具卡片的关键参数摘要。
///
/// 优先取 `path` / `command` 等主参数;没有则列出全部字段,
/// 长字符串字段(如 `content`)只显示长度。参数不是 JSON 对象时原样截断。
pub(crate) fn args_summary(args_json: &str) -> String {
    let trimmed = args_json.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return clip(&single_line(trimmed), ARGS_MAX_CHARS);
    };
    let Some(obj) = value.as_object() else {
        return clip(&single_line(trimmed), ARGS_MAX_CHARS);
    };

    if let Some(k) = PREFERRED_KEYS.iter().copied().find(|k| obj.contains_key(*k)) {
        let v = &obj[k];
        let text = v.as_str().map(single_line).unwrap_or_else(|| v.to_string());
        return clip(&text, ARGS_MAX_CHARS);
    }

    let mut parts: Vec<String> = Vec::new();
    for (k, v) in obj {
        match v.as_str() {
            Some(s) if s.chars().count() <= 40 && !s.contains('\n') => parts.push(format!("{k}={s}")),
            Some(s) => parts.push(format!("{k}({}字)", s.chars().count())),
            None if v.is_boolean() || v.is_number() => parts.push(format!("{k}={v}")),
            None => parts.push(format!("{k}=…")),
        }
    }
    clip(&parts.join(" "), ARGS_MAX_CHARS)
}

fn single_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

/// 把账本里尚未应用的事件(`chat_event_cursor` 之后)归并进转录。
///
/// * 工具事件 → 新建/更新工具卡片(按 `call_id` 归并)
/// * `TaskInterrupted` → 仍在运行/待审批的卡片标记为失败
/// * `include_text` → 同时回放用户输入与助手最终答复(启动恢复用;
///   运行中的文本由流式增量负责,不重复入转录)
///
/// 返回是否有新事件被应用;游标推进到切片末尾。
pub(crate) fn replay(state: &mut TuiState, events: &[TaskEvent], include_text: bool) -> bool {
    let start = state.chat_event_cursor.min(events.len());
    let mut applied = false;

    for ev in &events[start..] {
        // 只接受会话任务自己的账本(切换会话会重置游标,这里兜底)
        if let Some(tid) = state.chat_task.as_ref() {
            if tid.as_str() != ev.task_id.as_str() {
                continue;
            }
        }
        applied = true;
        match &ev.payload {
            EventPayload::ChatUserTurn { text } if include_text => {
                state.chat_entries.push(ChatEntry {
                    role: ChatRole::User,
                    text: text.clone(),
                    tool: None,
                });
            }
            EventPayload::MessageCompleted { text, .. } if include_text && !text.is_empty() => {
                state.chat_entries.push(ChatEntry {
                    role: ChatRole::Assistant,
                    text: text.clone(),
                    tool: None,
                });
            }
            EventPayload::ToolCallRequested { call_id, tool, args_json, risk, .. } => {
                // 流式占位的空助手条目直接让位给卡片,避免出现空气泡
                if matches!(state.chat_entries.last(), Some(l) if l.role == ChatRole::Assistant && l.text.is_empty())
                {
                    state.chat_entries.pop();
                }
                if !state.chat_tool_index.contains_key(call_id.as_str()) {
                    state.chat_push_tool(ChatToolCard {
                        call_id: call_id.as_str().to_string(),
                        tool: tool.clone(),
                        risk: *risk,
                        args_summary: args_summary(args_json),
                        state: ChatToolState::Running,
                        output: String::new(),
                    });
                }
            }
            EventPayload::ApprovalRequested { call_id, .. } => {
                if let Some(card) = state.chat_tool_mut(call_id.as_str()) {
                    card.state = ChatToolState::WaitingApproval;
                }
            }
            EventPayload::ApprovalDecided { call_id, decision } => {
                if let Some(card) = state.chat_tool_mut(call_id.as_str()) {
                    match decision {
                        ApprovalDecision::Reject => {
                            card.state = ChatToolState::Rejected;
                            if card.output.is_empty() {
                                card.output = "用户拒绝审批".into();
                            }
                        }
                        ApprovalDecision::Approve => {
                            if card.state == ChatToolState::WaitingApproval {
                                card.state = ChatToolState::Running;
                            }
                        }
                    }
                }
            }
            EventPayload::ToolCallCompleted { call_id, ok, output_summary } => {
                if let Some(card) = state.chat_tool_mut(call_id.as_str()) {
                    // 被拒是终态:随后的 ok=false 完成事件不覆盖它
                    if card.state != ChatToolState::Rejected {
                        card.state = if *ok { ChatToolState::Ok } else { ChatToolState::Failed };
                    }
                    card.output = output_summary.clone();
                }
            }
            // spec 173:/compact 的摘要取代此前的对话史;转录里留一条可回放的记录
            EventPayload::ChatCompacted { summary, turns } => {
                state.chat_history.clear();
                state.chat_summary = Some(summary.clone());
                let mut text = format!("✂ 已压缩 {turns} 轮对话为摘要:");
                for l in summary.lines() {
                    text.push_str("\n      ");
                    text.push_str(l);
                }
                state.chat_entries.push(ChatEntry::system(text));
                state.chat_scroll = 0;
            }
            EventPayload::TaskInterrupted { reason } => {
                let note = format!("已中断: {reason}");
                finalize_unfinished(state, &note);
            }
            _ => {}
        }
    }

    state.chat_event_cursor = events.len();
    applied
}

/// 把仍在运行/待审批的卡片标记为失败(中断事件,或启动恢复时收尾上次
/// 进程遗留的调用:进程被杀时账本里不会有中断事件)。
pub(crate) fn finalize_unfinished(state: &mut TuiState, note: &str) {
    let pending: Vec<String> = state
        .chat_entries
        .iter()
        .filter_map(|e| e.tool.as_ref())
        .filter(|c| matches!(c.state, ChatToolState::Running | ChatToolState::WaitingApproval))
        .map(|c| c.call_id.clone())
        .collect();
    for id in pending {
        if let Some(card) = state.chat_tool_mut(&id) {
            card.state = ChatToolState::Failed;
            card.output = note.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zealous_core::{AgentRunId, RiskTier, ToolCallId};

    fn ev(payload: EventPayload) -> TaskEvent {
        TaskEvent {
            seq: 0,
            ts: String::new(),
            v: 1,
            task_id: zealous_core::TaskId::generate(),
            agent: zealous_core::AgentRef::User,
            payload,
        }
    }

    fn requested(call_id: &ToolCallId, tool: &str, args: &str, risk: RiskTier) -> TaskEvent {
        ev(EventPayload::ToolCallRequested {
            call_id: call_id.clone(),
            run_id: AgentRunId::generate(),
            tool: tool.into(),
            args_json: args.into(),
            risk,
        })
    }

    fn approval(call_id: &ToolCallId) -> TaskEvent {
        ev(EventPayload::ApprovalRequested { call_id: call_id.clone(), summary: "s".into() })
    }

    fn decided(call_id: &ToolCallId, d: ApprovalDecision) -> TaskEvent {
        ev(EventPayload::ApprovalDecided { call_id: call_id.clone(), decision: d })
    }

    fn completed(call_id: &ToolCallId, ok: bool, out: &str) -> TaskEvent {
        ev(EventPayload::ToolCallCompleted {
            call_id: call_id.clone(),
            ok,
            output_summary: out.into(),
        })
    }

    #[test]
    fn args_summary_prefers_key_args() {
        assert_eq!(args_summary(r#"{"path":"src/main.rs","limit":50}"#), "src/main.rs");
        assert_eq!(args_summary(r#"{"command":"cargo test --workspace"}"#), "cargo test --workspace");
        assert_eq!(args_summary(r#"  "#), "");
    }

    #[test]
    fn args_summary_falls_back_to_fields_and_bare_text() {
        // 短值内联,长值只显示长度
        assert_eq!(args_summary(r#"{"content":"abcdef"}"#), "content=abcdef");
        let long = "y".repeat(80);
        assert_eq!(args_summary(&format!(r#"{{"content":"{long}"}}"#)), "content(80字)");
        assert_eq!(args_summary(r#"{"all":true,"limit":5}"#), "all=true limit=5");
        assert_eq!(args_summary("not json"), "not json");
        let long = "x".repeat(300);
        let s = args_summary(&format!(r#"{{"path":"{long}"}}"#));
        assert_eq!(s.chars().count(), 100);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn replay_builds_and_updates_cards_in_ledger_order() {
        let mut s = TuiState::new();
        let (c1, c2) = (ToolCallId::generate(), ToolCallId::generate());
        let events = vec![
            ev(EventPayload::ChatUserTurn { text: "q".into() }),
            requested(&c1, "edit_file", r#"{"path":"README.md"}"#, RiskTier::T1),
            approval(&c1),
            decided(&c1, ApprovalDecision::Approve),
            completed(&c1, true, "ok: README.md"),
            requested(&c2, "shell", r#"{"command":"ls"}"#, RiskTier::T1),
        ];
        assert!(replay(&mut s, &events, false));
        // 运行中同步不回放文本:只有两张卡
        assert_eq!(s.chat_entries.len(), 2);
        assert_eq!(s.chat_entries[0].role, ChatRole::Tool);
        let card = s.chat_entries[0].tool.as_ref().unwrap();
        assert_eq!(card.tool, "edit_file");
        assert_eq!(card.risk, RiskTier::T1);
        assert_eq!(card.args_summary, "README.md");
        assert_eq!(card.state, ChatToolState::Ok);
        assert_eq!(card.output, "ok: README.md");
        assert_eq!(s.chat_entries[1].tool.as_ref().unwrap().state, ChatToolState::Running);
        // 游标推进后再次同步不重复
        assert!(!replay(&mut s, &events, false));
        assert_eq!(s.chat_entries.len(), 2);
        assert_eq!(s.chat_event_cursor, events.len());
    }

    #[test]
    fn rejected_is_terminal_and_failed_completes() {
        let mut s = TuiState::new();
        let (r, f) = (ToolCallId::generate(), ToolCallId::generate());
        replay(
            &mut s,
            &[
                requested(&r, "shell", r#"{"command":"rm -rf"}"#, RiskTier::T2),
                approval(&r),
                decided(&r, ApprovalDecision::Reject),
                completed(&r, false, "rejected"),
                requested(&f, "read_file", r#"{"path":"nope"}"#, RiskTier::T0),
                completed(&f, false, "not found"),
            ],
            false,
        );
        let cards: Vec<_> = s.chat_entries.iter().filter_map(|e| e.tool.as_ref()).collect();
        assert_eq!(cards[0].state, ChatToolState::Rejected);
        assert_eq!(cards[1].state, ChatToolState::Failed);
        assert_eq!(cards[1].output, "not found");
    }

    #[test]
    fn interrupted_marks_unfinished_cards_failed() {
        let mut s = TuiState::new();
        let (a, b) = (ToolCallId::generate(), ToolCallId::generate());
        replay(
            &mut s,
            &[
                requested(&a, "read_file", r#"{"path":"a"}"#, RiskTier::T0),
                completed(&a, true, "ok"),
                requested(&b, "shell", r#"{"command":"sleep 9"}"#, RiskTier::T1),
                approval(&b),
                ev(EventPayload::TaskInterrupted { reason: "user abort".into() }),
            ],
            false,
        );
        let cards: Vec<_> = s.chat_entries.iter().filter_map(|e| e.tool.as_ref()).collect();
        assert_eq!(cards[0].state, ChatToolState::Ok);
        assert_eq!(cards[1].state, ChatToolState::Failed);
        assert!(cards[1].output.contains("user abort"), "{}", cards[1].output);
    }

    #[test]
    fn compaction_replaces_history_with_summary_and_is_replayed() {
        let mut s = TuiState::new();
        s.chat_history.push(("q1".into(), "a1".into()));
        let c = ToolCallId::generate();
        let events = vec![
            requested(&c, "read_file", "{}", RiskTier::T0),
            completed(&c, true, "ok"),
            ev(EventPayload::ChatCompacted { summary: "摘要\n第二行".into(), turns: 1 }),
        ];
        // 运行中同步(不回放文本):对话史清空,摘要到位,转录多一条压缩记录
        replay(&mut s, &events, false);
        assert!(s.chat_history.is_empty());
        assert_eq!(s.chat_summary.as_deref(), Some("摘要\n第二行"));
        let last = s.chat_entries.last().unwrap();
        assert_eq!(last.role, ChatRole::System);
        assert_eq!(last.text, "✂ 已压缩 1 轮对话为摘要:\n      摘要\n      第二行");
        // 工具卡片不受影响
        assert_eq!(s.chat_entries.len(), 2);
        // 重启恢复(回放文本)得到同一条记录
        let mut fresh = TuiState::new();
        replay(&mut fresh, &events, true);
        assert_eq!(fresh.chat_entries.last().unwrap().text, last.text);
        assert_eq!(fresh.chat_summary.as_deref(), Some("摘要\n第二行"));
    }

    #[test]
    fn finalize_marks_only_unfinished_cards() {
        let mut s = TuiState::new();
        let (a, b) = (ToolCallId::generate(), ToolCallId::generate());
        replay(
            &mut s,
            &[
                requested(&a, "read_file", r#"{"path":"a"}"#, RiskTier::T0),
                completed(&a, true, "ok"),
                requested(&b, "shell", r#"{"command":"sleep 9"}"#, RiskTier::T1),
                approval(&b),
            ],
            false,
        );
        finalize_unfinished(&mut s, "未完成(会话已中断)");
        let cards: Vec<_> = s.chat_entries.iter().filter_map(|e| e.tool.as_ref()).collect();
        assert_eq!(cards[0].state, ChatToolState::Ok);
        assert_eq!(cards[0].output, "ok");
        assert_eq!(cards[1].state, ChatToolState::Failed);
        assert_eq!(cards[1].output, "未完成(会话已中断)");
    }

    #[test]
    fn restore_replays_text_around_cards_and_drops_empty_placeholder() {
        let mut s = TuiState::new();
        let c = ToolCallId::generate();
        s.chat_begin_turn("q"); // 流式占位:User + 空 Assistant
        replay(
            &mut s,
            &[
                requested(&c, "read_file", r#"{"path":"a"}"#, RiskTier::T0),
                completed(&c, true, "ok"),
                ev(EventPayload::MessageCompleted {
                    run_id: AgentRunId::generate(),
                    turn: 1,
                    text: "answer".into(),
                }),
            ],
            true,
        );
        // 占位的空助手条目让位给卡片;最终答复仍回放为助手条目
        let roles: Vec<_> = s.chat_entries.iter().map(|e| e.role).collect();
        assert_eq!(roles, vec![ChatRole::User, ChatRole::Tool, ChatRole::Assistant]);
        assert_eq!(s.chat_entries[2].text, "answer");
    }

    #[test]
    fn events_of_other_task_are_ignored() {
        let mut s = TuiState::new();
        s.chat_task = Some(zealous_core::TaskId::generate());
        let c = ToolCallId::generate();
        assert!(!replay(&mut s, &[requested(&c, "read_file", "{}", RiskTier::T0)], false));
        assert!(s.chat_entries.is_empty());
    }
}