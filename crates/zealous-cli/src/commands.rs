//! spec 173:聊天 `/` 命令系统 —— TUI 聊天屏与 REPL(`zealous chat`)共用。
//! 命令表、解析、补全候选以及命令的文本产出都在这里;
//! 两个前端只负责:拦截以 `/` 开头的输入 → 解析 → 显示文本 → 执行少数有副作用的动作。

use zealous_core::{EventPayload, TaskEvent, TaskId};
use zealous_engine::task_service::ChatSession;

/// 一条聊天命令
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Help,
    New,
    Task,
    Status,
    Usage,
    Clear,
    Exit,
    Context,
    Model,
    Resume,
    Compact,
}

/// 命令表条目(补全菜单与 /help 的数据源)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSpec {
    pub name: &'static str,
    /// 参数提示;空串表示不接受参数
    pub args: &'static str,
    pub about: &'static str,
}

impl CommandSpec {
    pub fn command(&self) -> Command {
        match self.name {
            "/help" => Command::Help,
            "/new" => Command::New,
            "/task" => Command::Task,
            "/status" => Command::Status,
            "/usage" => Command::Usage,
            "/clear" => Command::Clear,
            "/exit" => Command::Exit,
            "/context" => Command::Context,
            "/model" => Command::Model,
            "/resume" => Command::Resume,
            "/compact" => Command::Compact,
            other => unreachable!("命令表出现未登记的命令: {other}"),
        }
    }
}

/// 命令表(即 /help 的显示顺序)
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec { name: "/help", args: "", about: "显示可用命令" },
    CommandSpec { name: "/new", args: "", about: "新会话(任务与账本不动)" },
    CommandSpec { name: "/task", args: "", about: "当前会话的任务 id" },
    CommandSpec { name: "/status", args: "", about: "任务状态" },
    CommandSpec { name: "/context", args: "", about: "轮数、对话史条数、累计 tokens" },
    CommandSpec { name: "/usage", args: "", about: "token 用量" },
    CommandSpec {
        name: "/model",
        args: "[name]",
        about: "查看模型;/model <name> 切换后续运行的模型",
    },
    CommandSpec {
        name: "/resume",
        args: "[序号|任务id]",
        about: "列出历史会话;带参数时切换并恢复转录",
    },
    CommandSpec {
        name: "/compact",
        args: "",
        about: "把对话史压缩成摘要(落账,重启后可回放)",
    },
    CommandSpec { name: "/clear", args: "", about: "清空转录显示(会话与账本保留)" },
    CommandSpec { name: "/exit", args: "", about: "退出聊天(TUI 回任务列表 / REPL 退出)" },
];

/// 解析一行输入。返回 `None` 表示不是命令(应作为普通消息发送);
/// `Some(Err(msg))` 表示是命令但无法执行(未知命令 / 参数不合法)。
pub fn parse(line: &str) -> Option<Result<(Command, String), String>> {
    let t = line.trim();
    if !t.starts_with('/') {
        return None;
    }
    let (head, rest) = match t.find(char::is_whitespace) {
        Some(i) => (&t[..i], t[i..].trim()),
        None => (t, ""),
    };
    let (cmd, takes_arg) = match head {
        "/help" => (Command::Help, false),
        "/new" => (Command::New, false),
        "/task" => (Command::Task, false),
        "/status" => (Command::Status, false),
        "/usage" => (Command::Usage, false),
        "/clear" => (Command::Clear, false),
        "/exit" | "/quit" => (Command::Exit, false),
        "/context" => (Command::Context, false),
        "/model" => (Command::Model, true),
        "/resume" => (Command::Resume, true),
        "/compact" => (Command::Compact, false),
        _ => {
            return Some(Err(format!(
                "未知命令: {head}(输入 /help 查看可用命令)"
            )))
        }
    };
    if !rest.is_empty() && !takes_arg {
        return Some(Err(format!("{head} 不接受参数(输入 /help 查看用法)")));
    }
    Some(Ok((cmd, rest.to_string())))
}

/// 输入以 `/` 开头且还没输入参数时,给出补全候选(完整替换文本)。
/// 带参数的命令候选以空格结尾,Tab 之后即可直接输入参数。
pub fn complete(input: &str) -> Vec<String> {
    let t = input.trim_start();
    if !t.starts_with('/') || t.contains(char::is_whitespace) {
        return Vec::new();
    }
    let mut out: Vec<String> = COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(t))
        .map(|c| {
            if c.args.is_empty() {
                c.name.to_string()
            } else {
                format!("{} ", c.name)
            }
        })
        .collect();
    // 已经原样输入的命令不再提示
    out.retain(|c| c != t);
    out
}

fn pad(s: &str, width: usize) -> String {
    let w = s.chars().count();
    if w >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - w))
    }
}

/// /help 的文本:按显示宽度对齐两列(命令 + 参数 / 说明)
pub fn help_text() -> String {
    let width = COMMANDS
        .iter()
        .map(|c| c.name.chars().count() + c.args.chars().count() + 1)
        .max()
        .unwrap_or(0);
    let mut out = String::from("命令:");
    for c in COMMANDS {
        let left = if c.args.is_empty() {
            c.name.to_string()
        } else {
            format!("{} {}", c.name, c.args)
        };
        out.push_str(&format!("\n  {}  {}", pad(&left, width), c.about));
    }
    out
}

/// 会话账本的 token 用量汇总 (prompt, completion, total)
pub fn token_totals(events: &[TaskEvent]) -> (u64, u64, u64) {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::UsageRecorded {
                prompt_tokens,
                completion_tokens,
                total_tokens,
                ..
            } => Some((*prompt_tokens, *completion_tokens, *total_tokens)),
            _ => None,
        })
        .fold((0, 0, 0), |(a, b, c), (x, y, z)| (a + x, b + y, c + z))
}

/// 用户轮数(ChatUserTurn 条数)
pub fn turn_count(events: &[TaskEvent]) -> u64 {
    events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::ChatUserTurn { .. }))
        .count() as u64
}

/// /usage 的文本
pub fn usage_text(events: &[TaskEvent]) -> String {
    let (p, c, tot) = token_totals(events);
    format!("用量: prompt={p} completion={c} total={tot} tokens")
}

/// /context 的文本:轮数、对话史条数、累计 tokens、最近一次 prompt tokens
pub fn context_text(events: &[TaskEvent], history_len: usize) -> String {
    let (p, c, tot) = token_totals(events);
    let last_prompt = events
        .iter()
        .rev()
        .find_map(|e| match &e.payload {
            EventPayload::UsageRecorded {
                prompt_tokens, ..
            } => Some(*prompt_tokens),
            _ => None,
        })
        .unwrap_or(0);
    let model_calls = events
        .iter()
        .filter(|e| matches!(&e.payload, EventPayload::UsageRecorded { .. }))
        .count();
    format!(
        "上下文: {} 轮 · 对话史 {history_len} 条 · 累计 {tot} tokens(prompt {p} / completion {c})\n\
         最近一次 prompt: {last_prompt} tokens · 模型调用 {model_calls} 次",
        turn_count(events)
    )
}

/// 任务 id 的短显示(与 TUI 标题栏一致:去掉 task_ 前缀,取前 10 位)
pub fn short_task_id(id: &str) -> String {
    id.strip_prefix("task_")
        .unwrap_or(id)
        .chars()
        .take(10)
        .collect()
}

/// /resume 的会话列表文本。序号即 /resume <序号> 的编号(最近活动在前)。
pub fn resume_text(sessions: &[ChatSession], current: Option<&TaskId>) -> String {
    if sessions.is_empty() {
        return "没有历史会话".to_string();
    }
    let mut out = String::from("历史会话(最近活动在前):");
    for (i, s) in sessions.iter().enumerate() {
        let mark = if current == Some(&s.task_id) {
            " · 当前"
        } else {
            ""
        };
        let title = s.title.replace('\n', " ");
        out.push_str(&format!(
            "\n  {}. {} · {} 轮 · {}{}",
            i + 1,
            title,
            s.turns,
            short_task_id(s.task_id.as_str()),
            mark
        ));
    }
    out.push_str("\n/resume <序号|任务id> 切换并恢复该会话的转录");
    out
}

/// /resume <参数>:按序号(1 起)或任务 id(完整或唯一前缀)选会话
pub fn pick_session<'a>(
    sessions: &'a [ChatSession],
    arg: &str,
) -> Result<&'a ChatSession, String> {
    let arg = arg.trim();
    if let Ok(n) = arg.parse::<usize>() {
        return sessions
            .get(n.checked_sub(1).ok_or("序号从 1 开始")?)
            .ok_or_else(|| format!("没有第 {n} 个会话(共 {} 个)", sessions.len()));
    }
    if arg.len() < 6 {
        return Err(format!("没有匹配的会话: {arg}(任务 id 至少输入 6 位)"));
    }
    let needle = arg.trim_start_matches("task_");
    let mut hits = sessions
        .iter()
        .filter(|s| s.task_id.as_str().trim_start_matches("task_").starts_with(needle));
    let first = hits
        .next()
        .ok_or_else(|| format!("没有匹配的会话: {arg}"))?;
    match hits.next() {
        None => Ok(first),
        Some(_) => Err(format!("{arg} 匹配多个会话,请输入更长的任务 id")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, ts: &str) -> ChatSession {
        ChatSession {
            task_id: TaskId::parse(id).unwrap(),
            title: format!("标题 {ts}"),
            turns: 2,
            last_ts: ts.to_string(),
        }
    }

    #[test]
    fn table_covers_every_command_and_maps_back() {
        use Command::*;
        let mut seen: Vec<Command> = Vec::new();
        for c in COMMANDS {
            assert_eq!(parse(c.name).unwrap().unwrap().0, c.command(), "{}", c.name);
            seen.push(c.command());
        }
        for cmd in [
            Help, New, Task, Status, Usage, Clear, Exit, Context, Model, Resume, Compact,
        ] {
            assert!(seen.contains(&cmd), "{cmd:?} 不在命令表里");
        }
        // 表内名称唯一
        let mut names: Vec<_> = COMMANDS.iter().map(|c| c.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), COMMANDS.len());
    }

    #[test]
    fn parse_handles_args_aliases_and_rejects_bad_input() {
        assert_eq!(parse("你好 /help"), None, "非 / 开头是普通消息");
        assert_eq!(parse("  /model glm-5.3 ").unwrap().unwrap(), (Command::Model, "glm-5.3".into()));
        assert_eq!(parse("/resume 2").unwrap().unwrap(), (Command::Resume, "2".into()));
        assert_eq!(parse("/model").unwrap().unwrap(), (Command::Model, String::new()));
        assert_eq!(parse("/quit").unwrap().unwrap().0, Command::Exit);
        let err = parse("/context").unwrap().unwrap();
        assert_eq!(err.0, Command::Context);
        assert_eq!(
            parse("/nope").unwrap(),
            Err("未知命令: /nope(输入 /help 查看可用命令)".to_string())
        );
        assert_eq!(
            parse("/compact now").unwrap(),
            Err("/compact 不接受参数(输入 /help 查看用法)".to_string())
        );
    }

    #[test]
    fn complete_filters_by_prefix_and_skips_typed_exact() {
        assert_eq!(
            complete("/"),
            vec![
                "/help", "/new", "/task", "/status", "/context", "/usage", "/model ", "/resume ",
                "/compact", "/clear", "/exit"
            ]
        );
        assert_eq!(complete("  /"), complete("/"), "行首空白不影响");
        assert_eq!(complete("/c"), vec!["/context", "/compact", "/clear"]);
        assert_eq!(complete("/model"), vec!["/model "], "带参数的命令补出尾随空格");
        assert!(complete("/help").is_empty(), "已完整输入不再提示");
        assert!(complete("/model glm").is_empty(), "输入参数后不再提示");
        assert!(complete("读一下 /help").is_empty());
        assert!(complete("").is_empty());
    }

    #[test]
    fn help_text_lists_every_command_with_description() {
        let text = help_text();
        for c in COMMANDS {
            assert!(text.contains(c.about), "缺少说明: {}", c.name);
            assert!(text.contains(c.name), "缺少命令: {}", c.name);
        }
        assert!(text.starts_with("命令:\n  /help"), "{text}");
    }

    #[test]
    fn pick_session_by_index_full_id_and_unique_prefix() {
        let a = TaskId::parse("task_01M78D3CDH83EEDMRGAWS2AZTH").unwrap();
        let b = TaskId::parse("task_01M78D3CDH83EEDMRGAWS2AZTJ").unwrap();
        let c = TaskId::parse("task_01M87ADREJ5Z3V8035M8Y1BVTC").unwrap();
        let sessions = vec![
            session(a.as_str(), "3"),
            session(b.as_str(), "2"),
            session(c.as_str(), "1"),
        ];
        assert_eq!(pick_session(&sessions, "1").unwrap().task_id, a);
        assert_eq!(pick_session(&sessions, "3").unwrap().task_id, c);
        assert_eq!(pick_session(&sessions, "0").unwrap_err(), "序号从 1 开始");
        assert_eq!(pick_session(&sessions, "4").unwrap_err(), "没有第 4 个会话(共 3 个)");
        assert_eq!(pick_session(&sessions, a.as_str()).unwrap().task_id, a);
        assert_eq!(pick_session(&sessions, "01M87ADREJ").unwrap().task_id, c);
        assert_eq!(
            pick_session(&sessions, "01M78D3CDH").unwrap_err(),
            "01M78D3CDH 匹配多个会话,请输入更长的任务 id"
        );
        assert_eq!(pick_session(&sessions, "01M87ZZZZZ").unwrap_err(), "没有匹配的会话: 01M87ZZZZZ");
        assert!(pick_session(&sessions, "01M").unwrap_err().contains("至少输入 6 位"));
    }

    #[test]
    fn resume_text_numbers_sessions_and_marks_current() {
        let a = TaskId::parse("task_01M78D3CDH83EEDMRGAWS2AZTH").unwrap();
        let sessions = vec![session(a.as_str(), "2"), session("task_01M87ADREJ5Z3V8035M8Y1BVTC", "1")];
        let text = resume_text(&sessions, Some(&a));
        assert!(text.contains("1. 标题 2 · 2 轮 · 01M78D3CDH · 当前"), "{text}");
        assert!(text.contains("2. 标题 1 · 2 轮 · 01M87ADREJ"), "{text}");
        assert_eq!(resume_text(&[], None), "没有历史会话");
    }

    fn ev(payload: EventPayload) -> TaskEvent {
        TaskEvent {
            seq: 0,
            ts: String::new(),
            v: 1,
            task_id: TaskId::generate(),
            agent: zealous_core::AgentRef::User,
            payload,
        }
    }

    #[test]
    fn usage_and_context_text_are_derived_from_ledger() {
        let events = vec![
            ev(EventPayload::ChatUserTurn { text: "q1".into() }),
            ev(EventPayload::UsageRecorded {
                run_id: zealous_core::AgentRunId::generate(),
                turn: 1,
                prompt_tokens: 100,
                completion_tokens: 20,
                total_tokens: 120,
            }),
            ev(EventPayload::ChatUserTurn { text: "q2".into() }),
            ev(EventPayload::UsageRecorded {
                run_id: zealous_core::AgentRunId::generate(),
                turn: 2,
                prompt_tokens: 150,
                completion_tokens: 30,
                total_tokens: 180,
            }),
        ];
        assert_eq!(usage_text(&events), "用量: prompt=250 completion=50 total=300 tokens");
        let ctx = context_text(&events, 2);
        assert!(ctx.contains("2 轮"), "{ctx}");
        assert!(ctx.contains("对话史 2 条"), "{ctx}");
        assert!(ctx.contains("累计 300 tokens"), "{ctx}");
        assert!(ctx.contains("最近一次 prompt: 150 tokens"), "{ctx}");
        assert!(ctx.contains("模型调用 2 次"), "{ctx}");
        assert_eq!(turn_count(&events), 2);
    }
}