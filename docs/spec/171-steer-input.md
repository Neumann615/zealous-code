# 171 · Steer 插话(运行中用户补上下文)

状态: Alpha 实施中 · 2026-11-12
来源: 对照 dsh-TUI 的 placement 语义(steer/followup/now),
补齐"运行中用户可补上下文"的核心交互。

## 1. 设计

- Orchestrator 新增 `steer_rx: Option<mpsc::Receiver<String>>`;
  agent_loop_tools 在每轮模型调用前检查是否有待处理的 steer 消息;
  有则将其作为 user 消息追加到转写末尾,模型下一轮可见;
- REPL:输入 broker 在 run 期间将用户输入发到 steer 通道(而非排队);
  运行结束后多余的输入仍走正常 REPL 流程;
- 不改变事件账本:steer 消息只进转写(内存),不落账本。

## 2. 验收

- 单测:运行中发送 steer 消息,模型下一轮可看到;
- REPL:运行中输入文本,本轮结束后下一轮模型可引用。

## 3. 状态记录(2026-11-25 核对)

- 引擎侧已实现:Orchestrator.steer_rx + agent_loop_tools 在轮次边界消费并注入转写。
- 生产入口未接线:cmd_run / cmd_chat / TUI 的 Orchestrator 均为 steer_rx: None,
  用户运行中的输入不会进入 steer 通道。此前向用户报告"steer 插话已交付"不准确。
- 另:类型为 Option<Receiver<String>>,而 REPL 的输入通道是 Arc<Mutex<Receiver>>,
  接线前需要先统一类型。
