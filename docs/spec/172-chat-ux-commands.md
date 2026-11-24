# 172 · 聊天体验增强(工具结果进转录 + / 命令系统)

状态: 部分交付 · 2026-11-25(§1 已交付;§2 的 /context /model /resume /compact 未实现)
来源: 用户反馈 —— 聊天屏转录区缺少工具执行结果,且缺少 / 命令系统。

## 1. 工具调用进转录 —— 已交付(2026-11-25)

每次工具调用在聊天屏生成一张卡片,数据只来自事件账本:

| 账本事件 | 卡片变化 |
|----------|----------|
| ToolCallRequested | 新建卡片:工具名 + 关键参数 + 运行中 |
| ApprovalRequested | → 等待审批 |
| ApprovalDecided | Approve → 运行中;Reject → 已拒绝(终态) |
| ToolCallCompleted | → 完成 / 失败 + 结果摘要 |
| TaskInterrupted | 仍在运行/待审批的卡片 → 失败(已中断: 原因) |

- 关键参数:优先 `path` / `command` / `query` 等主参数,否则列出各字段
  (长值只显示长度),上限 100 字符。
- 卡片收起时显示前 2 行结果,`Ctrl+O` 展开全部(最多 60 行)。
- 启动恢复会话时用同一回放逻辑从账本重建卡片(mod.restore_chat_session)。
- 运行中同步:工具参数开始流式返回、审批到达、工具进度行、run 结束时触发,
  流式输出期间另有 300ms 节流轮询(tui/mod.rs maybe_sync_chat)。
- 旧的 `⚙ 工具 x 完成` 进度行不再进转录(进度行仍可在任务详情的进度区看到)。
- 聊天屏长行自动换行,滚动高度按换行后的行数计算。

已知缺口:模型在工具调用旁边输出的中间文本没有写入账本,重启后这部分不回放
(最终答复 MessageCompleted 有账本,正常回放)。

## 2. / 命令系统

| 命令 | 功能 |
|------|------|
| /help | 显示可用命令 |
| /new | 新会话 |
| /exit | 退出 |
| /task | 当前任务 id |
| /status | 任务状态 |
| /usage | token 用量 |
| /clear | 清屏(转录) |

## 3. REPL 同步

REPL 已有 /new /task /status /exit;新增 /help /usage /clear。

## 4. 验收记录(2026-11-25)

实际交付(已在真实 TUI 里逐项驱动验证):

- 聊天屏 `/` 命令:`/help` `/new` `/task` `/status` `/usage` `/clear` `/exit`;
  未知命令(如 `/context`)返回 `未知命令: /context(输入 /help 查看可用命令)`,
  不再被静默吞掉,也不会发给模型。
- 启动即进聊天屏,并从账本恢复最近会话(mod.restore_chat_session;
  此前 2a1804f 的提交说明声称已做,实际插错了文件,未生效)。
- 聊天屏底栏改为聊天屏专属提示(之前显示的是任务列表的按键说明)。
- REPL:`--show-thinking` 与 `/help` `/usage` `/clear` `/new` `/task` `/status` `/exit`。

§1 工具卡片(2026-11-25,release 构建在真实 TUI 里用真实模型驱动验证):

- 发起"读取 Cargo.toml 并用 shell 创建文件":`read_file` 卡片实时出现,
  显示 `Cargo.toml` 与 `✓ 完成`,下方是结果前 2 行 + `…另有 12 行(Ctrl+O 展开)`;
  `shell` 卡片显示命令、`[T2]` 与 `? 等待审批`,同时弹出审批窗。
- 聊天屏按 `a`:`shell` 卡片变为 `✓ 完成`,最终答复正常流出。
- `Ctrl+O`:卡片展开显示全部结果行;再按一次收起。
- 运行中 `Ctrl-C`:1 秒内中断并退出,账本落 `TaskInterrupted(aborted)`。
- 退出后重启:卡片从账本回放,状态与结果摘要一致;`↑` 可向上翻看更早的卡片。
- 上次进程被杀、账本停在 `ApprovalRequested` 的 `shell` 卡片,重启后显示
  `✗ 失败 · 未完成(会话已中断)`,不再永远停在"等待审批"。
- 转录区不再出现 `⚙ 工具 x 完成` 行。

验证过程中发现并一并修复的三个现成 bug(此前工具卡片在真实 TUI 里根本出不来):

1. **流式请求漏发工具声明**(engine `provider.rs`):`once_streaming` 的请求体没有
   `tools`,而 TUI 走流式,模型收不到工具定义,只会回一句"我先读取…"就结束。
   现已与非流式共用 `tools_json`;新增测试抓取流式请求体断言。
2. **聊天屏无法审批**:`a/d/w` 只在详情屏处理,聊天屏(默认屏)里按键被打进输入框。
   现审批键与滚动在任意屏生效。
3. **Ctrl-C / q 的中断不传引擎**:只有详情屏 `x` 会置 abort 信号,聊天屏里运行中
   按 Ctrl-C 会一直停在"已请求中断,等待引擎落账"(有审批挂起时永久卡死)。
   现主循环每轮把中断请求同步到 abort 信号。

未实现:

- `/context` `/resume` `/compact` `/model`、`/` 补全菜单与 Tab 补全(第 2 步)。

测试:3 个列表渲染测试因默认屏改为 Chat 而失败,已让它们显式切到 List 屏;
新增 default_screen_is_chat 锁定默认屏契约。

§1 本轮测试:新增 18 个(账本回放 8、卡片状态 2、渲染/换行滚动 4、恢复与按键 3、
流式请求体 1),删除 1 个被卡片取代的进度行测试;`cargo test --workspace --no-fail-fast`
237 通过 / 0 失败 / 1 忽略。已知既有偶发:`workspace_isolation::serve_isolated_run_get_workspace_apply`
在并行负载下偶发响应超时(基线代码 8 次失败 1 次,与本次改动无关)。
