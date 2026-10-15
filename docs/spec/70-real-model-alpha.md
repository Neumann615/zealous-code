# 70 · 真实模型 Alpha

状态: 已定案 · 2026-10-15
目标: 让真实模型(OpenAI-compatible endpoint)在 CLI 上稳定、可审计、可恢复地工作。
仍不做: TUI、桌面端、app-server、插件系统、OS sandbox。

## 1. 模型动作契约与修复轮

- 模型每轮必须输出**恰好一个** JSON action 对象,三选一:
  - `{"action":"plan","plan":"<步骤>","workers":["<子任务>", ...]}`(workers 可省略,≤3)
  - `{"action":"tool","tool":"<工具名>","args":{...}}`(args 必须是 object)
  - `{"action":"message","text":"<最终答复>"}`
- 接受形态:裸 JSON(整体恰为一个 object)或单个 markdown fenced JSON block。
- 拒绝形态:夹杂说明文字、多个对象、未知字段、缺必填字段、workers 超限、未知 action。
- 修复轮:解析失败 → `ErrorOccurred(scope=model_action)` 落账 → 下一轮 user message
  明确指出解析错误并附 action schema(assistant 原输出保留在上下文);
  每个非法输出最多 2 次修复,耗尽后当前 AgentRun 以可解释原因 Failed。
- Turn 语义:一次模型调用 = 一个 turn;TurnStarted/TurnCompleted 恰好成对,
  run 内 turn 编号单调递增(规划轮与 apply 轮接续,不跳号)。
- Worker 规划策略:默认 0 Worker;仅存在独立可并行的只读分析子任务时才创建;
  描述必须非空且互不重复(违反即解析失败进修复轮)。

## 2. 原生多消息上下文

- `ChatRequest = { system, messages: Vec<ChatMessage(user|assistant)> }`;
  多轮上下文原生传递,不再拼接进单个 user 字符串。
- OpenAI 映射:system 在前,历史 user/assistant 按序透传。
- Provider trait 保持可 Mock;常规单测不访问外网。

## 3. Resume 携带事件投影上下文

恢复(非 Queued 进入)时,新 Run 的初始 prompt 必须包含投影摘要:
原标题、恢复前状态与中断次数、已有计划、各 Run 最后产出、全部工具调用摘要
(tool/risk/state)、上次错误;并明确三条语义:这是续跑不是重开、
已完成的读操作不必重复、被拒或失败的工具需要调整方案。

## 4. apply 事务(主工作区写入)

`write_file` 审批通过后按事务流程执行,成功才落 `ToolCallCompleted(ok=true)`:

```
检查工作区干净(用户改动 → 拒绝)
→ 写同目录临时文件 → 原子替换目标文件
→ git add <file> → git commit(作者 zealous,作为回退点)
→ 任一 git 步骤失败:恢复目标文件原内容(或删除新建文件)+ git reset 清 index
   → ToolCallCompleted(ok=false),错误说明已回滚
   → 回滚也失败:错误信息明确要求人工处理
```

**崩溃窗口如实声明**:临时文件替换与 git commit 之间存在非原子窗口
(进程在此间崩溃 → 文件已替换但未提交,工作区呈脏状态)。
V1 不做虚假的完整事务保证;该窗口的恢复依赖既有语义:下次 resume 时
异常 Running 规范化(spec 60 §2)+ 脏工作区保护(spec 60 §4)会让后续
写入被拒绝,由用户决定 commit 或回退。测试覆盖进程内失败恢复,不覆盖
进程崩溃窗口。

## 5. 真实端点 Smoke Test

- CLI 集成测试新增 `#[ignore] real_model_smoke`:默认不进入常规测试
  (`cargo test --all` 不得访问外网);显式执行:
  `cargo test --test cli_e2e real_model_smoke -- --ignored --nocapture`
- 配置:读取仓库根 `.env`,但不打印任何配置值、不把 .env 复制进临时仓库、
  只把必要环境变量注入子进程。
- 断言:任务 Completed;账本含 read_file 与 write_file;目标文件存在;
  存在 `zealous: apply` 回退 commit;事件 seq 连续;
  API key 与 Authorization 不落账、不出现在任何测试输出。

## 验收

- 全部既有测试(Mock 驱动)继续通过,新增:解析严格性、修复轮恢复/耗尽、
  多消息顺序、resume 上下文断言、apply 回滚。
- 显式执行 real_model_smoke 通过一次。
- 任何失败输出只含脱敏诊断,不含凭据。
