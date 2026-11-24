# 172 · 聊天体验增强(工具结果进转录 + / 命令系统)

状态: 已交付 · 2026-11-25(§1 工具卡片;§2 命令系统;§3 REPL 同步)
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

## 2. / 命令系统 —— 已交付(2026-11-25)

命令表、解析、补全候选与命令的文本产出都在 `zealous-cli/src/commands.rs`,
TUI 聊天屏与 REPL 共用;两个前端只负责拦截 `/` 开头的输入并显示文本。

| 命令 | 功能 |
|------|------|
| /help | 列出全部命令及说明 |
| /new | 新会话(任务与账本不动;`/model` 的覆盖保留) |
| /task | 当前会话任务 id |
| /status | 任务状态 |
| /context | 轮数、对话史条数、累计 tokens、最近一次 prompt tokens、模型调用次数 |
| /usage | token 用量(账本 UsageRecorded 汇总) |
| /model | 查看当前模型;`/model <name>` 切换后续运行的模型(仅本次会话生效,不改配置文件) |
| /resume | 列出历史会话(最近活动在前,带序号);`/resume <序号\|任务id>` 切换并从账本恢复转录、工具卡片、对话史与摘要 |
| /compact | 调用模型把对话史压缩成摘要,摘要落账(`ChatCompacted`)并在之后注入 prompt;重启后回放 |
| /clear | 清空转录显示(会话、对话史、摘要与账本保留) |
| /exit | 退出聊天(TUI 回任务列表 / REPL 退出;别名 /quit) |

规则:

- 以 `/` 开头但不在命令表里的输入 → 提示"未知命令",不会发给模型;
  不接受参数的命令带了参数 → 提示,同样不执行。
- 输入 `/` 时在输入框上方弹出补全菜单(按前缀过滤),`↑/↓` 在候选间移动,
  `Tab` 补全为高亮候选;带参数的命令补出尾随空格(`/model `)。
- 查询类命令(/task /status /context /usage)直接读账本,不依赖 TUI 内存状态。

### /compact 的数据流

1. 后台线程调用 `task_service::summarize_history`(非流式单轮;会把上一次的摘要
   并入输入,多次压缩不丢更早的上下文)。
2. 摘要落账:`EventPayload::ChatCompacted { summary, turns }`(投影不变,任何状态后都可追加)。
3. TUI 收到结果后同步账本:`transcript::replay` 遇到 ChatCompacted → 清空此前的对话史、
   记下摘要、在转录里加一条"✂ 已压缩 N 轮对话为摘要"。重启恢复走同一路径。
4. 之后的 prompt = 用户消息 + `[此前对话摘要(/compact)]` + 最近 4 对原文(摘要之后的轮次)。
5. 摘要为空 / 模型失败 / 落账失败 → 不改对话史,提示失败原因。

## 3. REPL 同步 —— 已交付

`zealous chat` 用同一份 `commands.rs`:`/help /new /task /status /context /usage /model
/resume /compact /clear /exit` 全部可用;启动时恢复最近会话的对话史与摘要,
`/resume` 切换后回放最近 6 条转录。`/clear` 真正清屏(此前是空操作)。
REPL 是行缓冲 stdin,没有 Tab 补全(终端里由 readline 类库负责,不在本次范围)。

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

§2/§3 命令系统(2026-11-25):

- TUI:逐键驱动(真实按键路径)验证 `/help` `/task` `/status` `/context` `/usage` `/model` `/model <name>`
  `/resume` `/resume <序号|id前缀>` `/exit`、未知命令、多余参数、Tab 补全;
  `/context` `/usage` 的数字来自账本(mock 每次应答 prompt 11 / completion 7,一轮两次调用)。
- TUI `/compact`:真实调用 mock 模型 → 账本出现 ChatCompacted → 对话史清空、摘要到位、
  转录出现"✂ 已压缩 1 轮对话为摘要" → 后续 prompt 含摘要 → 全新状态恢复后摘要与记录仍在;
  再次压缩提示"没有新的对话可压缩"。
- REPL:真实二进制 `zealous chat` + 流式 mock 跑一轮对话,逐条验证全部命令,
  含 `/compact`、`/new` 后 `/resume 1` 切回并回放摘要与转录、越界序号提示。
- 真实 TUI(release 构建 + 真实模型 glm-5.3-flash / glm-5.3):
  - 输入 `/` 弹出补全菜单(一屏 8 行);输入 `c` 过滤为 `/context /compact /clear`,
    `↓` `↓` 后 `Tab` 补成 `/clear`,菜单关闭。
  - `/context`:在真实会话上显示"6 轮 · 对话史 4 条 · 累计 13068 tokens(prompt 11323 /
    completion 1745)· 最近一次 prompt: 1233 tokens · 模型调用 10 次"。
  - `/model`:"当前模型: glm-5.3-flash(来自 provider 配置)";
    `/model glm-5.3`:"后续运行使用模型: glm-5.3(仅本会话生效,不改配置文件)"。
  - `/resume`:列出 6 个历史会话(标题、轮数、短 id,当前会话有标记);
    `/resume 5` 切到"你好啊"会话(标题栏与转录都换成该会话,提示"已切换到会话 01M788YHNE(3 轮)");
    `/resume 1` 切回。
  - `/compact`:标题栏显示"压缩中",数秒后转录出现模型生成的摘要;
    `/context` 变为"对话史 0 条"。退出重启后,摘要与"✂ 已压缩"记录从账本回放。
  - 测试中在集成测试里另外抓取了模型请求体:`/model foo` 后发送消息,请求体的
    `model` 字段是 `foo`,且流式请求体带 `tools`。

测试:3 个列表渲染测试因默认屏改为 Chat 而失败,已让它们显式切到 List 屏;
新增 default_screen_is_chat 锁定默认屏契约。

§2/§3 本轮测试:`cargo test --workspace --no-fail-fast` 256 通过 / 0 失败 / 1 忽略
(较 §1 新增 19 个:命令表/解析/补全/会话选择 7、引擎会话列表与压缩 4、TUI 状态 3、补全菜单渲染 1、
账本回放 1、TUI 命令与压缩集成 2、REPL 端到端 1)。

§1 本轮测试:新增 18 个(账本回放 8、卡片状态 2、渲染/换行滚动 4、恢复与按键 3、
流式请求体 1),删除 1 个被卡片取代的进度行测试;`cargo test --workspace --no-fail-fast`
237 通过 / 0 失败 / 1 忽略。已知既有偶发:`workspace_isolation::serve_isolated_run_get_workspace_apply`
在并行负载下偶发响应超时(基线代码 8 次失败 1 次,与本次改动无关)。
