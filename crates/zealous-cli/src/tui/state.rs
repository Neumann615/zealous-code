//! TUI 纯 UI 状态与 reducer(spec 90):只保存 UI 选择、输入草稿、滚动
//! 与渲染所需数据;任务/审批/工具状态全部来自投影或 engine 事件。

use zealous_core::{TaskEvent, TaskId, TaskProjection, TaskStatus};
use zealous_engine::task_service::TaskSummary;

/// 审批请求的 UI 形态(来自 channel sink)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequestUi {
    pub task_id: String,
    pub call_id: String,
    /// spec 167:工具名(会话始终允许判定用)
    pub tool: String,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FormField {
    #[default]
    Title,
    Brief,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// 任务列表 + 右侧摘要
    List,
    /// 选中任务详情(含事件流)
    Detail,
    /// 新建任务表单
    Form,
    /// 队列状态视图(spec 156)
    Queue,
    /// 并行状态视图(spec 157)
    Parallel,
    /// merge workspace 状态视图(spec 159)
    MergeStatus,
    /// 对话屏(spec 163):聊天式工作台
    Chat,
}

/// 新建任务表单草稿
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewTaskForm {
    pub title: String,
    pub brief: String,
    pub field: FormField,
}

/// spec 163:聊天屏角色
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Assistant,
    System,
    /// spec 173:工具调用卡片(事实来自账本工具事件)
    Tool,
}

/// spec 163:聊天转录条目(UI 派生数据;事实源仍是事件账本)
#[derive(Debug, Clone)]
pub struct ChatEntry {
    pub role: ChatRole,
    pub text: String,
    /// spec 173:role == Tool 时非空
    pub tool: Option<ChatToolCard>,
}

impl ChatEntry {
    pub fn system(text: impl Into<String>) -> Self {
        Self { role: ChatRole::System, text: text.into(), tool: None }
    }
}

/// spec 173:工具卡片状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatToolState {
    Running,
    WaitingApproval,
    Ok,
    Failed,
    Rejected,
}

/// spec 173:工具调用卡片(由账本 ToolCallRequested / ApprovalRequested /
/// ApprovalDecided / ToolCallCompleted 归并而成)
#[derive(Debug, Clone)]
pub struct ChatToolCard {
    pub call_id: String,
    pub tool: String,
    pub risk: zealous_core::RiskTier,
    /// 关键参数摘要(展示用)
    pub args_summary: String,
    pub state: ChatToolState,
    pub output: String,
}

pub const PROGRESS_RING_CAP: usize = 200;

/// spec 155:确认弹窗类别
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    IsolatedRun,
    Apply,
    Merge,
    MergeWorkspace,
    MergeResolutionApprove,
    MergeResolutionReject,
    MergeContinue,
    MergeAbort,
    WorkspaceRemove,
    /// spec 156:串行运行全部可运行 queued 任务(auto apply)
    QueueRun,
    /// spec 157/161:并行运行最多三个 runnable queued tasks(auto integrate)
    ParallelRun,
    /// spec 161:恢复集成已完成 isolated tasks
    ParallelIntegrate,
}

/// TUI 全部 UI 状态;任务事实以投影/账本为准,这里只有派生显示数据
#[derive(Debug)]
pub struct TuiState {
    pub screen: Screen,
    /// spec 155:待确认操作(I/P/W 触发,Enter 确认 / Esc 取消)
    pub confirm: Option<ConfirmKind>,
    /// spec 155:详情页是否显示 workspace 区块(W 切换)
    pub workspace_visible: bool,
    /// spec 156:待运行的队列任务(确认弹窗展示 + Enter 执行)
    pub queue_ids: Vec<TaskId>,
    /// spec 156:queue status 文本(Q 视图)
    pub queue_status_text: Option<String>,
    /// spec 157:待并行运行任务(确认弹窗展示)
    pub parallel_ids: Vec<TaskId>,
    /// spec 157:parallel status 文本(Shift+Q 视图)
    pub parallel_status_text: Option<String>,
    /// spec 159:merge workspace 状态文本
    pub merge_status_text: Option<String>,
    /// spec 160:待审批模型解决方案(UI-only;事实状态仍在事件投影)
    pub merge_resolution_proposal: Option<zealous_engine::merge_resolution::Proposal>,
    pub tasks: Vec<TaskSummary>,
    pub selected: usize,
    pub detail: Option<TaskProjection>,
    pub detail_events: Vec<TaskEvent>,
    pub events_expanded: bool,
    pub progress_lines: Vec<String>,
    pub important_lines: Vec<String>,
    pub pending_approval: Option<ApprovalRequestUi>,
    /// 审批弹窗滚动位置与总行数(总行数由渲染层按换行宽度写入)
    pub approval_scroll: usize,
    pub approval_total_lines: usize,
    /// 事件流滚动位置与总行数
    pub events_scroll: usize,
    pub events_total_lines: usize,
    pub last_status: Option<TaskStatus>,
    pub error: Option<String>,
    pub form: NewTaskForm,
    /// spec 163:聊天输入框
    pub chat_input: String,
    /// spec 163:聊天转录条目
    pub chat_entries: Vec<ChatEntry>,
    /// spec 163:转录区自底部起的滚动偏移(0 = 贴底跟随)
    pub chat_scroll: usize,
    /// spec 163:会话任务(首条消息创建,后续 resume)
    pub chat_task: Option<TaskId>,
    /// spec 163:压缩对话史 (user, assistant);/compact 之后只含摘要之后的轮次
    pub chat_history: Vec<(String, String)>,
    /// spec 173:最近一次 /compact 生成的对话摘要(账本事实;注入后续 prompt)
    pub chat_summary: Option<String>,
    /// spec 173:/model <name> 设置的会话级模型覆盖(只影响后续运行,不写配置)
    pub chat_model_override: Option<String>,
    /// spec 173:/compact 进行中(等待模型返回摘要)
    pub chat_compacting: bool,
    /// spec 173:/ 补全菜单里高亮的候选下标(渲染时按候选数取模)
    pub chat_menu_selected: usize,
    /// spec 163:当前 run 是否在流式输出中
    pub chat_streaming: bool,
    /// spec 173:call_id → chat_entries 下标(账本事件归并用)
    pub chat_tool_index: std::collections::HashMap<String, usize>,
    /// spec 173:会话账本已应用到转录的事件数(增量同步游标)
    pub chat_event_cursor: usize,
    /// spec 173:有信号表明账本可能已有新的工具事件,需尽快同步
    pub chat_sync_pending: bool,
    /// spec 173:上次账本同步时刻(流式中节流)
    pub chat_last_sync: Option<std::time::Instant>,
    /// spec 173:工具卡片展开完整输出(Ctrl+O 切换)
    pub chat_expand_tools: bool,
    /// spec 167:会话级始终允许的工具(TUI 存活期)
    pub tool_allowlist: std::collections::HashSet<String>,
    /// engine 线程是否在运行(R 已发出且未收到 Finished/Failure)
    pub running: bool,
    pub abort_requested: bool,
    pub quit: bool,
}

impl Default for TuiState {
    fn default() -> Self {
        Self::new()
    }
}

impl TuiState {
    pub fn new() -> Self {
        Self {
            screen: Screen::Chat,
            confirm: None,
            workspace_visible: false,
            queue_ids: Vec::new(),
            queue_status_text: None,
            parallel_ids: Vec::new(),
            parallel_status_text: None,
            merge_status_text: None,
            merge_resolution_proposal: None,
            tasks: Vec::new(),
            selected: 0,
            detail: None,
            detail_events: Vec::new(),
            events_expanded: false,
            progress_lines: Vec::new(),
            important_lines: Vec::new(),
            pending_approval: None,
            approval_scroll: 0,
            approval_total_lines: 0,
            events_scroll: 0,
            events_total_lines: 0,
            last_status: None,
            error: None,
            form: NewTaskForm::default(),
            chat_input: String::new(),
            chat_entries: Vec::new(),
            chat_scroll: 0,
            chat_task: None,
            chat_history: Vec::new(),
            chat_summary: None,
            chat_model_override: None,
            chat_compacting: false,
            chat_menu_selected: 0,
            chat_streaming: false,
            chat_tool_index: std::collections::HashMap::new(),
            chat_event_cursor: 0,
            chat_sync_pending: false,
            chat_last_sync: None,
            chat_expand_tools: false,
            tool_allowlist: std::collections::HashSet::new(),
            running: false,
            abort_requested: false,
            quit: false,
        }
    }

    // ---- 任务列表 ----

    pub fn refresh_tasks(&mut self, tasks: Vec<TaskSummary>) {
        self.tasks = tasks;
        if self.selected >= self.tasks.len() {
            self.selected = self.tasks.len().saturating_sub(1);
        }
    }

    pub fn select_next(&mut self) {
        if self.selected + 1 < self.tasks.len() {
            self.selected += 1;
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn selected_task(&self) -> Option<&TaskSummary> {
        self.tasks.get(self.selected)
    }

    // ---- 详情 ----

    pub fn set_detail(&mut self, proj: TaskProjection, events: Vec<TaskEvent>) {
        self.detail = Some(proj);
        self.detail_events = events;
    }

    pub fn toggle_events(&mut self) {
        self.events_expanded = !self.events_expanded;
    }

    // ---- 进度 ----

    pub fn push_progress(&mut self, line: String) {
        self.progress_lines.push(line);
        if self.progress_lines.len() > PROGRESS_RING_CAP {
            let overflow = self.progress_lines.len() - PROGRESS_RING_CAP;
            self.progress_lines.drain(..overflow);
        }
    }

    pub fn push_important(&mut self, line: &str) {
        self.important_lines.push(line.to_string());
        self.push_progress(format!("[!] {line}"));
    }

    // ---- 审批 ----

    pub fn approval_arrived(&mut self, req: ApprovalRequestUi) {
        self.pending_approval = Some(req);
        self.approval_scroll = 0;
        self.approval_total_lines = 0;
    }

    /// 取走当前待决审批(a/d 决策后由调用方发送决策)
    pub fn take_pending_approval(&mut self) -> Option<ApprovalRequestUi> {
        self.pending_approval.take()
    }

    // ---- 滚动(spec 90 收口:审批内容必须能完整查看)----

    pub fn set_approval_total(&mut self, total: usize) {
        self.approval_total_lines = total;
        if self.approval_scroll > total.saturating_sub(1) {
            self.approval_scroll = total.saturating_sub(1);
        }
    }

    pub fn approval_scroll_down(&mut self, page: usize) {
        let max = self.approval_total_lines.saturating_sub(1);
        self.approval_scroll = (self.approval_scroll + page).min(max);
    }

    pub fn approval_scroll_up(&mut self, page: usize) {
        self.approval_scroll = self.approval_scroll.saturating_sub(page);
    }

    /// 是否已滚动到末尾(配合可见行数判断;不到末尾不得提示"已完整查看")
    pub fn approval_at_end(&self, visible: usize) -> bool {
        self.approval_scroll + visible >= self.approval_total_lines
    }

    pub fn set_events_total(&mut self, total: usize) {
        self.events_total_lines = total;
        if self.events_scroll > total.saturating_sub(1) {
            self.events_scroll = total.saturating_sub(1);
        }
    }

    pub fn events_scroll_down(&mut self, page: usize) {
        let max = self.events_total_lines.saturating_sub(1);
        self.events_scroll = (self.events_scroll + page).min(max);
    }

    pub fn events_scroll_up(&mut self, page: usize) {
        self.events_scroll = self.events_scroll.saturating_sub(page);
    }

    // ---- 表单 ----

    /// spec 155:确认弹窗与 workspace 区块
    pub fn open_confirm(&mut self, kind: ConfirmKind) {
        self.confirm = Some(kind);
    }

    pub fn take_confirm(&mut self) -> Option<ConfirmKind> {
        self.confirm.take()
    }

    pub fn toggle_workspace(&mut self) {
        self.workspace_visible = !self.workspace_visible;
    }

    pub fn open_form(&mut self) {
        self.screen = Screen::Form;
        self.form = NewTaskForm {
            field: FormField::Title,
            ..Default::default()
        };
    }

    pub fn form_close(&mut self) {
        self.screen = Screen::List;
    }

    pub fn form_toggle_field(&mut self) {
        self.form.field = match self.form.field {
            FormField::Title => FormField::Brief,
            FormField::Brief => FormField::Title,
        };
    }

    pub fn form_char(&mut self, c: char) {
        match self.form.field {
            FormField::Title => self.form.title.push(c),
            FormField::Brief => self.form.brief.push(c),
        }
    }

    pub fn form_backspace(&mut self) {
        match self.form.field {
            FormField::Title => {
                self.form.title.pop();
            }
            FormField::Brief => {
                self.form.brief.pop();
            }
        }
    }

    /// 表单校验:提交前置条件;返回错误信息
    pub fn form_validate(&self) -> Result<(), String> {
        if self.form.title.trim().is_empty() {
            return Err("title 必填".into());
        }
        Ok(())
    }

    // ---- 中断/退出/错误 ----

    /// x:请求中断当前任务,不退出
    pub fn request_abort(&mut self) {
        if self.running {
            self.abort_requested = true;
        }
    }

    /// q / Ctrl-C:请求中断并退出(engine 运行中等落账后退出)
    pub fn request_quit(&mut self) {
        self.quit = true;
        self.request_abort();
    }

    // ---- 聊天屏(spec 163)----

    pub fn chat_input_char(&mut self, c: char) {
        self.chat_input.push(c);
        self.chat_menu_selected = 0;
    }

    pub fn chat_input_backspace(&mut self) {
        self.chat_input.pop();
        self.chat_menu_selected = 0;
    }

    /// spec 173:/ 补全菜单里移动高亮(down = 向下)。
    /// 返回 false 表示菜单没有多个候选(调用方按普通滚动处理)。
    pub fn chat_menu_move(&mut self, down: bool) -> bool {
        let n = self.chat_completions().len();
        if n <= 1 {
            return false;
        }
        self.chat_menu_selected = match (down, self.chat_menu_selected) {
            (true, i) => (i + 1) % n,
            (false, 0) => n - 1,
            (false, i) => i - 1,
        };
        true
    }

    /// 发送用户消息:转录入条目 + 助手流式条目占位;返回要发送的文本
    pub fn chat_begin_turn(&mut self, msg: &str) {
        self.chat_entries.push(ChatEntry { role: ChatRole::User, text: msg.to_string(), tool: None });
        self.chat_entries.push(ChatEntry { role: ChatRole::Assistant, text: String::new(), tool: None });
        self.chat_scroll = 0;
    }

    /// spec 173:转录末尾追加工具卡片并登记 call_id
    pub fn chat_push_tool(&mut self, card: ChatToolCard) {
        let call_id = card.call_id.clone();
        let idx = self.chat_entries.len();
        self.chat_entries.push(ChatEntry {
            role: ChatRole::Tool,
            text: String::new(),
            tool: Some(card),
        });
        self.chat_tool_index.insert(call_id, idx);
        self.chat_scroll = 0;
    }

    /// spec 173:按 call_id 取卡片(不存在返回 None)
    pub fn chat_tool_mut(&mut self, call_id: &str) -> Option<&mut ChatToolCard> {
        let idx = *self.chat_tool_index.get(call_id)?;
        self.chat_entries[idx].tool.as_mut()
    }

    /// spec 173:清空转录派生数据(条目、索引、游标、滚动)
    pub fn chat_clear_transcript(&mut self) {
        self.chat_entries.clear();
        self.chat_tool_index.clear();
        self.chat_scroll = 0;
    }

    /// 流式 delta 追加到最后一个助手条目
    /// 流式 delta 追加:尾部是助手条目则累积,否则新建
    /// (spec 166:新建前收起思考条目)
    pub fn chat_append_delta(&mut self, piece: &str) {
        match self.chat_entries.last_mut() {
            Some(last) if last.role == ChatRole::Assistant => {
                last.text.push_str(piece);
            }
            _ => {
                self.chat_collapse_reasoning();
                self.chat_entries.push(ChatEntry {
                    role: ChatRole::Assistant,
                    text: piece.to_string(), tool: None,
                });
            }
        }
        self.chat_scroll = 0;
    }

    /// spec 166:思考流增量(暗色 System 条目累积;正文到达时收起)
    pub fn chat_append_reasoning(&mut self, piece: &str) {
        match self.chat_entries.last_mut() {
            Some(last) if last.role == ChatRole::System && last.text.starts_with("💭 ") => {
                last.text.push_str(piece);
            }
            _ => {
                self.chat_entries.push(ChatEntry {
                    role: ChatRole::System,
                    text: format!("💭 {piece}"), tool: None,
                });
            }
        }
        self.chat_scroll = 0;
    }

    fn chat_collapse_reasoning(&mut self) {
        if let Some(last) = self.chat_entries.last_mut() {
            if last.role == ChatRole::System && last.text.starts_with("💭 ") {
                last.text = "💭 …(思考已收起)".to_string();
            }
        }
    }

    /// run 结束:用账本最终答复替换流式原文,并写入压缩对话史
    pub fn chat_finish_reply(&mut self, reply: Option<String>) {
        let user = self
            .chat_entries
            .iter()
            .rev()
            .find(|e| e.role == ChatRole::User)
            .map(|e| e.text.clone())
            .unwrap_or_default();
        if let Some(text) = reply.filter(|s| !s.is_empty()) {
            // spec 173:尾部可能已是工具卡片(无流式增量到达),此时补一条助手条目
            match self.chat_entries.last_mut() {
                Some(last) if last.role == ChatRole::Assistant => last.text = text.clone(),
                _ => self.chat_entries.push(ChatEntry {
                    role: ChatRole::Assistant,
                    text: text.clone(),
                    tool: None,
                }),
            }
            self.chat_history.push((user, text));
        } else if let Some(last) = self.chat_entries.last_mut() {
            if last.role == ChatRole::Assistant && last.text.is_empty() {
                last.text = "(无最终答复)".into();
            }
        }
    }

    pub fn chat_scroll_up(&mut self, page: usize) {
        self.chat_scroll = self.chat_scroll.saturating_add(page);
    }

    pub fn chat_scroll_down(&mut self, page: usize) {
        self.chat_scroll = self.chat_scroll.saturating_sub(page);
    }

    /// spec 162 §5 / 173:对话上下文注入 prompt。
    /// = 用户消息 + [/compact 摘要] + 最近 4 对原文(单条 600 字)
    pub fn chat_history_prompt(&self, msg: &str) -> String {
        let summary = self
            .chat_summary
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| chat_truncate_str(s, CHAT_SUMMARY_MAX_CHARS));
        // 没有任何上下文时原样发送,不改写用户消息
        if summary.is_none() && self.chat_history.is_empty() {
            return msg.to_string();
        }
        let mut out = format!("用户消息: {msg}");
        if let Some(summary) = summary {
            out.push_str("\n\n[此前对话摘要(/compact)]:\n");
            out.push_str(&summary);
        }
        if !self.chat_history.is_empty() {
            out.push_str("\n\n[历史对话(压缩参考)]:\n");
            out.push_str(&chat_history_block(&self.chat_history));
        }
        out
    }

    /// spec 173:当前 / 补全候选(输入以 / 开头且未输入参数时)
    pub fn chat_completions(&self) -> Vec<String> {
        crate::commands::complete(&self.chat_input)
    }

    /// spec 173:补全菜单里实际高亮的候选
    pub fn chat_menu_choice(&self) -> Option<usize> {
        let n = self.chat_completions().len();
        (n > 0).then(|| self.chat_menu_selected % n)
    }

    /// spec 173:用候选替换输入(Tab 补全)
    pub fn chat_apply_completion(&mut self, candidate: &str) {
        self.chat_input = candidate.to_string();
        self.chat_menu_selected = 0;
        self.chat_scroll = 0;
    }

    /// /new 语义:清空会话(任务与账本不动)
    pub fn chat_reset(&mut self) {
        self.chat_input.clear();
        self.chat_clear_transcript();
        self.chat_tool_index.clear();
        self.chat_event_cursor = 0;
        self.chat_sync_pending = false;
        self.chat_last_sync = None;
        self.chat_task = None;
        self.chat_history.clear();
        self.chat_summary = None;
        self.chat_compacting = false;
        self.chat_menu_selected = 0;
        // chat_model_override 跨会话保留:/model 切换的是"后续运行"的模型
        self.chat_streaming = false;
    }

    pub fn task_finished(&mut self, status: TaskStatus) {
        self.last_status = Some(status);
        self.running = false;
        self.abort_requested = false;
    }

    pub fn set_error(&mut self, msg: String) {
        self.error = Some(msg);
    }

    pub fn dismiss_error(&mut self) {
        self.error = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zealous_core::TaskId;

    fn task(_id: &str, status: TaskStatus, title: &str) -> TaskSummary {
        TaskSummary {
            task_id: TaskId::generate(),
            title: title.into(),
            status,
        }
    }

    #[test]
    fn selection_moves_and_clamps() {
        let mut s = TuiState::new();
        s.refresh_tasks(vec![
            task("a", TaskStatus::Queued, "A"),
            task("b", TaskStatus::Running, "B"),
        ]);
        assert_eq!(s.selected, 0);
        s.select_prev();
        assert_eq!(s.selected, 0, "顶端再上移应保持");
        s.select_next();
        assert_eq!(s.selected, 1);
        s.select_next();
        assert_eq!(s.selected, 1, "底端再下移应保持");
        // 刷新后 selection 收敛
        s.refresh_tasks(vec![task("a", TaskStatus::Queued, "A")]);
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn parallel_run_confirmation_and_status_screen() {
        let mut s = TuiState::new();
        s.parallel_ids = vec![TaskId::generate()];
        s.open_confirm(ConfirmKind::ParallelRun);
        assert_eq!(s.confirm, Some(ConfirmKind::ParallelRun));
        assert_eq!(s.take_confirm(), Some(ConfirmKind::ParallelRun));
        s.parallel_status_text = Some("active: parallel".into());
        s.screen = Screen::Parallel;
        assert_eq!(s.screen, Screen::Parallel);
        s.open_confirm(ConfirmKind::ParallelIntegrate);
        assert_eq!(s.take_confirm(), Some(ConfirmKind::ParallelIntegrate));
    }

    #[test]
    fn merge_confirmation_state() {
        let mut s = TuiState::new();
        s.open_confirm(ConfirmKind::Merge);
        assert_eq!(s.confirm, Some(ConfirmKind::Merge));
        assert_eq!(s.take_confirm(), Some(ConfirmKind::Merge));
    }

    #[test]
    fn merge_resolution_confirmation_state() {
        let mut s = TuiState::new();
        s.open_confirm(ConfirmKind::MergeResolutionApprove);
        assert_eq!(s.take_confirm(), Some(ConfirmKind::MergeResolutionApprove));
        s.open_confirm(ConfirmKind::MergeResolutionReject);
        assert_eq!(s.take_confirm(), Some(ConfirmKind::MergeResolutionReject));
    }

    #[test]
    fn merge_workspace_confirmation_and_status_screen() {
        let mut s = TuiState::new();
        s.open_confirm(ConfirmKind::MergeWorkspace);
        assert_eq!(s.confirm, Some(ConfirmKind::MergeWorkspace));
        assert_eq!(s.take_confirm(), Some(ConfirmKind::MergeWorkspace));
        s.merge_status_text = Some("status: conflicted".into());
        s.screen = Screen::MergeStatus;
        assert_eq!(s.screen, Screen::MergeStatus);
    }

    #[test]
    fn form_input_and_validation() {
        let mut s = TuiState::new();
        s.open_form();
        assert_eq!(s.screen, Screen::Form);
        assert_eq!(s.form.field, FormField::Title);
        assert!(s.form_validate().is_err(), "空 title 应校验失败");
        for c in "修复登录".chars() {
            s.form_char(c);
        }
        assert_eq!(s.form.title, "修复登录");
        s.form_toggle_field();
        assert_eq!(s.form.field, FormField::Brief);
        s.form_char('B');
        s.form_backspace();
        s.form_char('I');
        assert_eq!(s.form.brief, "I", "backspace 应删除最后一个字符");
        assert!(s.form_validate().is_ok());
        s.form_close();
        assert_eq!(s.screen, Screen::List);
    }

    #[test]
    fn approval_modal_arrive_and_take() {
        let mut s = TuiState::new();
        assert!(s.pending_approval.is_none());
        s.approval_arrived(ApprovalRequestUi {
            tool: "shell".into(),
            task_id: "task_1".into(),
            call_id: "call_1".into(),
            summary: "write_file ...".into(),
        });
        assert!(s.pending_approval.is_some());
        let taken = s.take_pending_approval().unwrap();
        assert_eq!(taken.call_id, "call_1");
        assert!(s.pending_approval.is_none(), "决策后弹窗消失");
    }

    #[test]
    fn abort_and_quit_states() {
        let mut s = TuiState::new();
        s.running = true;
        s.request_abort();
        assert!(s.abort_requested);
        assert!(!s.quit, "x 不退出");
        s.request_quit();
        assert!(s.quit);
        s.task_finished(TaskStatus::Interrupted);
        assert!(!s.running);
        assert!(!s.abort_requested, "结束清除中断标志");
        assert!(s.quit, "退出意图保持,循环在 !running 后退出");
        assert_eq!(s.last_status, Some(TaskStatus::Interrupted));
    }

    #[test]
    fn events_expanded_toggles() {
        let mut s = TuiState::new();
        assert!(!s.events_expanded);
        s.toggle_events();
        assert!(s.events_expanded);
        s.toggle_events();
        assert!(!s.events_expanded);
    }

    #[test]
    fn progress_ring_is_capped() {
        let mut s = TuiState::new();
        for i in 0..(PROGRESS_RING_CAP + 50) {
            s.push_progress(format!("line {i}"));
        }
        assert_eq!(s.progress_lines.len(), PROGRESS_RING_CAP);
        assert_eq!(s.progress_lines[0], "line 50");
    }

    #[test]
    fn error_set_and_dismiss() {
        let mut s = TuiState::new();
        s.set_error("boom".into());
        assert_eq!(s.error.as_deref(), Some("boom"));
        s.dismiss_error();
        assert!(s.error.is_none());
    }
}

#[cfg(test)]
mod scroll_tests {
    use super::*;

    #[test]
    fn approval_scroll_clamps_and_reports_end() {
        let mut s = TuiState::new();
        s.approval_arrived(ApprovalRequestUi {
            tool: "shell".into(),
            task_id: "task_c".into(),
            call_id: "c".into(),
            summary: "x".into(),
        });
        s.set_approval_total(10);
        assert!(s.approval_at_end(10), "一屏能放下时默认即到末尾");
        s.approval_scroll_down(3);
        assert_eq!(s.approval_scroll, 3);
        assert!(!s.approval_at_end(5), "3+5 < 10,不应到末尾");
        s.approval_scroll_down(5);
        assert!(s.approval_at_end(5), "8+5 >= 10,已到末尾");
        s.approval_scroll_up(10);
        assert_eq!(s.approval_scroll, 0);
        s.approval_scroll_down(999);
        assert_eq!(s.approval_scroll, 9, "向下滚动应钳制在末行");
        assert!(s.approval_at_end(2));
    }

    #[test]
    fn approval_arrived_resets_scroll() {
        let mut s = TuiState::new();
        s.approval_arrived(ApprovalRequestUi {
            tool: "shell".into(),
            task_id: "task_1".into(),
            call_id: "1".into(),
            summary: "a".into(),
        });
        s.set_approval_total(5);
        s.approval_scroll_down(4);
        s.approval_arrived(ApprovalRequestUi {
            tool: "shell".into(),
            task_id: "task_2".into(),
            call_id: "2".into(),
            summary: "b".into(),
        });
        assert_eq!(s.approval_scroll, 0, "新审批应从头查看");
    }

    #[test]
    fn events_scroll_clamps() {
        let mut s = TuiState::new();
        s.set_events_total(20);
        s.events_scroll_down(25);
        assert_eq!(s.events_scroll, 19);
        s.events_scroll_up(5);
        assert_eq!(s.events_scroll, 14);
    }
}

/// spec 173:注入 prompt 的摘要上限(字符)
const CHAT_SUMMARY_MAX_CHARS: usize = 3000;

/// spec 162 §5:最近几对原文,单条截断
fn chat_history_block(history: &[(String, String)]) -> String {
    const MAX_PAIRS: usize = 4;
    const MAX_CHARS: usize = 600;
    let start = history.len().saturating_sub(MAX_PAIRS);
    let mut hist = String::new();
    for (u, a) in &history[start..] {
        hist.push_str(&format!(
            "user: {}\nassistant: {}\n",
            chat_truncate_str(u, MAX_CHARS),
            chat_truncate_str(a, MAX_CHARS)
        ));
    }
    hist
}

fn chat_truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod chat_state_tests {
    use super::*;

    #[test]
    fn default_screen_is_chat() {
        assert_eq!(TuiState::new().screen, Screen::Chat);
    }

    #[test]
    fn chat_history_prompt_empty_degrades_to_message() {
        let s = TuiState::new();
        assert_eq!(s.chat_history_prompt("你好"), "你好");
    }

    #[test]
    fn chat_delta_append_and_finish_replaces_stream_text() {
        let mut s = TuiState::new();
        s.chat_begin_turn("1+1=?");
        s.chat_append_delta("{\"action\":\"mess");
        s.chat_append_delta("age\"...}");
        assert_eq!(s.chat_entries.len(), 2);
        assert_eq!(s.chat_entries[0].role, ChatRole::User);
        s.chat_finish_reply(Some("1+1等于2".into()));
        // 流式原文被账本最终答复替换(spec 163 §1)
        assert_eq!(s.chat_entries.last().unwrap().text, "1+1等于2");
        assert_eq!(s.chat_history.len(), 1);
        assert_eq!(s.chat_history[0], ("1+1=?".to_string(), "1+1等于2".to_string()));
    }

    #[test]
    fn chat_history_prompt_includes_recent_pairs() {
        let mut s = TuiState::new();
        s.chat_history.push(("q1".into(), "a1".into()));
        let p = s.chat_history_prompt("q2");
        assert!(p.contains("user: q1"));
        assert!(p.contains("assistant: a1"));
        assert!(p.contains("用户消息: q2"));
    }

    #[test]
    fn summary_is_injected_before_recent_pairs() {
        let mut s = TuiState::new();
        assert_eq!(s.chat_history_prompt("你好"), "你好", "没有任何上下文时原样发送");
        s.chat_history.push(("q1".into(), "a1".into()));
        s.chat_summary = Some("之前做过的事".into());
        let p = s.chat_history_prompt("q2");
        let summary_at = p.find("[此前对话摘要(/compact)]:\n之前做过的事").expect("摘要块");
        let history_at = p.find("[历史对话(压缩参考)]:").expect("历史块");
        assert!(summary_at < history_at, "摘要应排在最近对话之前: {p}");
        assert!(p.contains("user: q1") && p.contains("assistant: a1"));
        assert!(p.ends_with("assistant: a1\n"), "{p}");
    }

    #[test]
    fn completion_menu_wraps_selection_and_tab_fills_input() {
        let mut s = TuiState::new();
        s.chat_input = "/c".into();
        let cands = s.chat_completions();
        assert_eq!(cands, vec!["/context", "/compact", "/clear"]);
        s.chat_menu_selected = 5; // 5 % 3 = 2
        assert_eq!(s.chat_menu_choice(), Some(2));
        s.chat_apply_completion(&cands[2]);
        assert_eq!(s.chat_input, "/clear");
        assert_eq!(s.chat_menu_selected, 0);
        assert!(s.chat_completions().is_empty(), "已完整输入不再有候选");
        assert_eq!(s.chat_menu_choice(), None);
    }

    #[test]
    fn model_override_survives_new_session() {
        let mut s = TuiState::new();
        s.chat_model_override = Some("glm-5.3".into());
        s.chat_summary = Some("x".into());
        s.chat_reset();
        assert_eq!(s.chat_model_override.as_deref(), Some("glm-5.3"));
        assert_eq!(s.chat_summary, None);
    }

    #[test]
    fn chat_reset_clears_session_but_not_tasks() {
        let mut s = TuiState::new();
        s.chat_begin_turn("hi");
        s.chat_input.push_str("x");
        s.chat_reset();
        assert!(s.chat_entries.is_empty());
        assert!(s.chat_input.is_empty());
        assert!(s.chat_task.is_none());
    }
}

#[cfg(test)]
mod chat_ux_tests {
    use super::*;

    #[test]
    fn reasoning_accumulates_then_collapses_on_text() {
        let mut s = TuiState::new();
        s.chat_begin_turn("q");
        s.chat_append_reasoning("先想一想");
        s.chat_append_reasoning("…再验证");
        // 思考条目是暗色 System,以 💏 前缀累积
        let last = s.chat_entries.last().unwrap();
        assert_eq!(last.role, ChatRole::System);
        assert!(last.text.contains("先想一想"));
        // 正文到达 → 思考收起 + 新建助手条目
        s.chat_append_delta("答案");
        assert_eq!(s.chat_entries.last().unwrap().text, "答案");
        assert_eq!(s.chat_entries[s.chat_entries.len() - 2].text, "💭 …(思考已收起)");
    }

    #[test]
    fn tool_card_push_and_lookup_by_call_id() {
        let mut s = TuiState::new();
        s.chat_begin_turn("q");
        s.chat_push_tool(ChatToolCard {
            call_id: "c1".into(),
            tool: "read_file".into(),
            risk: zealous_core::RiskTier::T0,
            args_summary: "README.md".into(),
            state: ChatToolState::Running,
            output: String::new(),
        });
        let last = s.chat_entries.last().unwrap();
        assert_eq!(last.role, ChatRole::Tool);
        assert_eq!(last.tool.as_ref().unwrap().state, ChatToolState::Running);
        s.chat_tool_mut("c1").unwrap().state = ChatToolState::Ok;
        s.chat_tool_mut("c1").unwrap().output = "hello".into();
        let card = s.chat_entries.last().unwrap().tool.as_ref().unwrap();
        assert_eq!(card.state, ChatToolState::Ok);
        assert_eq!(card.output, "hello");
        assert!(s.chat_tool_mut("nope").is_none());
    }

    #[test]
    fn reset_and_clear_drop_tool_index_and_cursor() {
        let mut s = TuiState::new();
        s.chat_push_tool(ChatToolCard {
            call_id: "c1".into(),
            tool: "shell".into(),
            risk: zealous_core::RiskTier::T1,
            args_summary: "ls".into(),
            state: ChatToolState::Failed,
            output: String::new(),
        });
        s.chat_event_cursor = 7;
        s.chat_clear_transcript();
        assert!(s.chat_tool_index.is_empty());
        assert!(s.chat_tool_mut("c1").is_none());
        s.chat_reset();
        assert_eq!(s.chat_event_cursor, 0);
        assert!(!s.chat_sync_pending);
        assert!(s.chat_last_sync.is_none());
    }

    #[test]
    fn shift_enter_newline_goes_into_input() {
        let mut s = TuiState::new();
        s.chat_input_char('a');
        s.chat_input_char('\n');
        s.chat_input_char('b');
        assert_eq!(s.chat_input, "a\nb");
    }
}
