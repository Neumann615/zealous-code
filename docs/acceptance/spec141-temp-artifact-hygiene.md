# spec 141 验收记录:工具副产物治理(临时文件卫生)

日期:2026-10-20
实现提交:`be2f206`(feat(engine): spec 141 工具副产物治理)
spec:`docs/spec/141-temp-artifact-hygiene.md`

## 1. 质量门(全 PASS)

| 门 | 命令 | 结果 |
| -- | ---- | ---- |
| fmt | cargo fmt --check | PASS |
| test | cargo test --all | PASS(130 passed + 1 ignored;较上阶段 +17) |
| clippy | cargo clippy --all-targets -- -D warnings | PASS |
| real_model_smoke | cargo test --test cli_e2e real_model_smoke -- --ignored | PASS(19.5s) |

新增测试:worktree 分类 8 项 + spec141 单测 8 项 + 集成 1 项。

## 2. 单测(spec 141 验收清单)

| 项 | 用例 | 结果 |
| -- | ---- | ---- |
| 1 | 纯 `__pycache__/`(根 + 嵌套)不阻塞 edit_file | PASS |
| 2 | 纯 `*.pyc` 不阻塞 edit_file | PASS |
| 3 | nested `__pycache__/` 正确分类(含 git_status 分段) | PASS |
| 4 | tracked `.pyc` 修改 → UserChange 并阻塞写入 | PASS |
| 5 | 用户改动 + 副产物并存 → 拒绝写入,错误分段展示 | PASS |
| 6 | `.zealous/` 不阻塞写入 | PASS |
| 7 | git_status 输出三类分段 + 无用户改动时明确标记 | PASS |
| 8 | apply 后副产物未被 stage/commit,commit 只含目标文件 | PASS |

实现中确定的细则(已回写 spec):

- 分类输入用 `git status --porcelain -uall`:默认 porcelain 把 untracked
  目录折叠成 `?? pkg/`,内层 `pkg/__pycache__/` 无从识别;逐文件列出后
  分类才确定(首跑单测暴露的真问题,非测试瑕疵)。
- 目录名以 `.pyc` 结尾(如 `weird.pyc/`)不是副产物文件,保持 UserChange。
- tracked 的 `.pyc` / `__pycache__` 修改一律 UserChange,不降级。

## 3. 集成测试

`crates/zealous-engine/tests/spec141_hygiene.rs`:干净 Git 仓库 → 制造
`__pycache__/test.pyc` → `ToolExecutor` + `AutoApprove` 走完整审批链路调用
edit_file → 断言全部成立:

- 目标文件修改成功(审批通过后写入);
- 自动生成 rollback commit(`zealous: edit src…`);
- `__pycache__/test.pyc` 仍为 untracked;
- commit 只包含目标文件(`git log --name-only` 不含副产物)。

PASS。

## 4. 真实模型 Dogfooding 回归(先测 → 后写 → 再测)

- 二进制:`target/release/zealous.exe`(be2f206 构建);
- 模型:glm-5.3-flash;
- 夹具:`%TEMP%\zealous-dogfooding-140\sp141-order`(wordstats 包,
  BRIEF 严格规定先测后写顺序);
- 驱动:`dist/dogfood140-drive.py` + `dist/dogfood140-specs/SP141.json`;
- 结果:`dist/dogfood140-results/SP141.json` → **RESULT: PASS**。

### 4.1 任务顺序(账本 seq 证据)

| seq | 动作 | 结果 |
| --- | ---- | ---- |
| 49 | shell `python -m unittest test_wordstats -v`(先测) | ok,测试全过 |
| 58–67 | read_file ×3(读源码) | ok |
| 70/76/82 | edit_file ×3(core.py / `__init__.py` / 测试) | **全部 ok,未被副产物阻塞** |
| 88 | shell 同命令重测(再测) | ok,含 count_vowels 新用例全过 |
| 94 | git_status | ok,三分类分段输出 |

首次成功测试(49)先于写入(70+),`__pycache__/` 在写入前已产生
——"纯副产物不阻塞写入"在真实链路成立。

### 4.2 写入与 rollback commit

- 3 个 auto-commit:`zealous: edit wordstats\core.py` /
  `zealous: edit wordstats\__init__.py` / `zealous: edit test_wordstats.py`,
  每个 commit 恰含 1 个目标文件;
- 写入后工作区仅剩 untracked 副产物与 `.zealous/`,无其他改动。

### 4.3 副产物分类(spec 141 三分类)

写入后 `git status --porcelain -uall`:

```
?? .zealous/tasks/task_01M5AQVRE4KRAXW0QFSKHW07XA/events.jsonl
?? __pycache__/test_wordstats.cpython-312.pyc
?? wordstats/__pycache__/__init__.cpython-312.pyc
?? wordstats/__pycache__/core.cpython-312.pyc
```

按 spec 141 规则:3 条 `__pycache__/` → **RuntimeArtifact**(untracked 且
命中 `__pycache__` 段);`.zealous/…` → ZealousRuntimeData;**UserChange = 0**
——审计不再把 `__pycache__` 记为"非预期用户改动"(140 P2 #1 的目标)。

模型侧证据(账本 seq 95,git_status 工具输出原文):

```
== user changes ==
(无)
== runtime artifacts ==
?? __pycache__/test_wordstats.cpython-312.pyc
?? wordstats/__pycache__/__init__.cpython-312.pyc
?? wordstats/__pycache__/core.cpython-312.pyc
== zealous runtime data ==
?? .zealous/tasks/task_01M5AQVRE4KRAXW0QFSKHW07XA/events.jsonl
== clean ==
没有用户改动
```

### 4.4 审批与审计

- 审批:10 次 ApprovalRequested → Approve(含 3 次 edit_file T1),
  无卡死、无越权;
- 账本:101 events,seq 连续,无 parse error,无 torn tail;
- status_chain:Running → Completed;
- 密钥扫描:leaks 空(无 key 值 / Bearer / Authorization 泄露)。

### 4.5 观察(不评分,不进 P2 backlog)

1. BRIEF 指定的 `python -I -m unittest` 因隔离模式不含 cwd 而导入失败
   (夹具简报措辞问题,非产品缺陷);模型自行改为 `python -m unittest`
   在**写入前**跑通,"先测后写"顺序未破坏。
2. 模型 action 格式错误 3 次(2 次 action JSON 解析失败、1 次未知字段
   `tool_result`,scope=model_action):产品按协议拒绝,模型在会话内自愈,
   任务照常完成。属 glm-flash 协议噪声,非产品缺陷;若后续复发影响任务
   完成率,再按"同问题 ×3"规则单独开 spec。

## 5. 结论

- spec 141 验收项全部 PASS:单测 8/8、集成 1/1、dogfooding 闭环 1/1;
- 140 P2 #1(`__pycache__` ×3 进"非预期修改"审计)已闭环;
- 副产物全程未被删除 / gitignore / commit,T1 权限未扩大,apply 事务未改。
