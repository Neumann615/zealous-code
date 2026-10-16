# 100 · app-server 协议 Alpha

状态: 已定案 · 2026-10-16
目标: `zealous serve` 通过 stdio JSON-lines 协议暴露 engine 能力,
为桌面/Web 壳铺路(spec 50 演进路线第 2 条)。
仍不做: 多客户端、远程连接、桌面壳、插件系统。

## 1. 协议原则

- app-server 是薄协议层,不拥有任务状态;事实仍来自事件账本与投影。
- engine 是唯一写入者;客户端不允许直接写账本。
- 协议载荷复用 core 的事件与投影类型,不创造专用概念。
- V1 单客户端;所有请求带 `id`,响应保留对应 `id`。
- 协议版本不匹配显式失败(`version_mismatch`),不做隐式兼容。

## 2. 传输

- stdin 读一行 JSON 请求,stdout 写一行 JSON 响应/通知,均以 `\n` 结束。
- stderr 只输出脱敏诊断(禁止 API key/Authorization)。
- broken pipe、JSON 解析错误、未知命令 → 结构化错误响应或干净退出,
  不产生敏感输出。
- 不引入 async runtime:同步 IO + 线程 + channel。

## 3. 请求(id + cmd + params)

| cmd | params | 语义 |
| --- | ------ | ---- |
| initialize | `{version}` | 协商版本;返回 capability;未初始化前其余命令报 `not_initialized` |
| list_tasks | `{}` | 任务摘要列表 |
| get_task | `{task_id}` | 投影(TaskId 先经 parse 校验) |
| subscribe_events | `{task_id, after_seq}` | 先重放账本事件(task_updated),再推送后续新事件 |
| create_task | `{title, brief?}` | brief 为**文本**(非文件);复用 TaskService |
| run_task | `{task_id}` | 启动 engine worker;V1 同时只允许一个运行中任务 |
| interrupt_task | `{task_id}` | 设置 abort,不直接改状态 |
| decide_approval | `{task_id, call_id, decision}` | task 须匹配运行任务、call_id 须匹配当前 pending(未知/重复/过期 → `no_pending_approval`);决策后清除绑定 |
| shutdown | `{}` | 等待 worker 完成后退出 |

## 4. 响应与通知

- 响应:`{"id":N,"ok":true,"result":{…}}` 或
  `{"id":N,"ok":false,"error":{"code":"…","message":"…"}}`。
- 通知(无 id):`{"event":"…","params":{…}}`,事件:
  `task_updated` / `progress` / `approval_requested` / `run_finished` /
  `error_occurred`。
- 错误码:`parse_error` `unknown_command` `missing_field` `version_mismatch`
  `not_initialized` `invalid_params` `already_running` `no_pending_approval`
  `task_not_found` `internal`。

## 5. 生命周期

- 启动即进入事件循环;stdin EOF 时等待当前 worker 完成再退出,不丢弃线程。
- shutdown:等待 worker(join)→ 回 ok → 退出。
- 崩溃/错误路径不输出敏感信息。

## 测试

- 协议单测:roundtrip、未知命令、缺字段、未知/重复审批、版本不匹配。
- 生命周期:未 initialize 报错;shutdown 等待 worker;EOF 行为。
- 订阅:after_seq 重放正确;新事件顺序连续;损坏账本返回错误。
- 集成:真实 `zealous serve` 子进程 + mock endpoint,
  走 initialize → create_task → run_task → approval_requested →
  decide_approval → run_finished → get_task(Completed);断言文件修改、
  seq 连续、审批进账本。
