//! TUI 纯 UI 状态与 reducer(spec 90):只保存 UI 选择、输入草稿、滚动
//! 与渲染所需数据;任务/审批/工具状态全部来自投影或 engine 事件。

use zealous_core::{TaskEvent, TaskId, TaskProjection, TaskStatus};
use zealous_engine::task_service::TaskSummary;

/// 审批请求的 UI 形态(来自 channel sink)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequestUi {
    pub task_id: String,
    pub call_id: String,
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
}

/// 新建任务表单草稿
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewTaskForm {
    pub title: String,
    pub brief: String,
    pub field: FormField,
}

pub const PROGRESS_RING_CAP: usize = 200;

/// spec 155:确认弹窗类别
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    IsolatedRun,
    Apply,
    Merge,
    WorkspaceRemove,
    /// spec 156:串行运行全部可运行 queued 任务(auto apply)
    QueueRun,
    /// spec 157:并行运行最多两个 runnable queued tasks(auto apply)
    ParallelRun,
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
            screen: Screen::List,
            confirm: None,
            workspace_visible: false,
            queue_ids: Vec::new(),
            queue_status_text: None,
            parallel_ids: Vec::new(),
            parallel_status_text: None,
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
    }

    #[test]
    fn merge_confirmation_state() {
        let mut s = TuiState::new();
        s.open_confirm(ConfirmKind::Merge);
        assert_eq!(s.confirm, Some(ConfirmKind::Merge));
        assert_eq!(s.take_confirm(), Some(ConfirmKind::Merge));
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
            task_id: "task_1".into(),
            call_id: "1".into(),
            summary: "a".into(),
        });
        s.set_approval_total(5);
        s.approval_scroll_down(4);
        s.approval_arrived(ApprovalRequestUi {
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
