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
