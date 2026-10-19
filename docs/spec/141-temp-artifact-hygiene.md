# spec 141:工具副产物治理(临时文件卫生)

来源:dogfooding 140 问题分级 —— P2 #1 `__pycache__/` 测试副产物在 A1/A2/A3
三次进入"非预期修改"审计,触发"相同 P2 ×3 → 单独开 spec"规则(2026-10-20)。
本 spec 将目标落成确定性规则并实现。

## 问题

- 任务运行 `python -m unittest` 等测试命令(T2)后,`__pycache__/`、`*.pyc`
  等解释器副产物出现在工作区;zealous 的 auto-commit 只提交目标文件,副产物
  留在 untracked。
- 影响:
  1. dogfooding/审计的"非预期修改"噪音(3/9 任务出现);
  2. 与脏工作区守卫叠加时有阻碍风险:守卫把 untracked 视为脏(`??`),若
     任务先跑测试再写文件,写入会被拒绝,模型被迫走绕路(参见 B5 行为观察)。

## 确定性规则

### 识别范围(只处理 Python 运行副产物)

只识别以下两类路径,不扩展到 `target/`、`node_modules/`、`.pytest_cache/`
等任何其他目录:

1. 任意层级的 `__pycache__/`(路径任一组成部分等于 `__pycache__`);
2. 任意层级的 `*.pyc` 文件(文件名以 `.pyc` 结尾)。

### 状态语义(输入为 `git status --porcelain -uall`)

必须用 `-uall`:`git status --porcelain` 默认把 untracked 目录折叠成
`?? pkg/`,内层 `pkg/__pycache__/` 无从识别;逐文件列出后分类才确定。

工作区状态三分类:

| 分类 | 判定 |
| ---- | ---- |
| `ZealousRuntimeData` | untracked(`??`)且路径在 `.zealous/` 下 |
| `RuntimeArtifact` | untracked(`??`)且命中上述两类副产物路径 |
| `UserChange` | 其余全部:新增/修改/删除/重命名/staged 等 |

硬性细则:

- **只有 untracked 状态的副产物路径才降级为 `RuntimeArtifact`;
  已被 Git 跟踪的 `.pyc` / `__pycache__` 发生修改 → `UserChange`,不降级**
  (tracked 内容是用户仓库事实,谁改的必须能追)。
- `.zealous/` 仍是 zealous 自身运行数据,不视为用户改动。
- Git quoted path(含空格/非 ASCII 的路径会带引号与 C 风格转义)必须
  解析(unquote)后再判断,不能只做字符串前缀匹配。

### 副产物处理(不变式)

- 不自动删除;
- 不自动加入 `.gitignore`;
- 不自动 commit;
- 不扩大 T1 写权限(仍需审批);
- apply 事务不变:`edit_file` / `write_file` 仍只 stage 与 commit 目标
  文件,不 stage 副产物、不执行 `git add -A`;写入成功后副产物保持 untracked。

### 写入守卫行为(`assert_clean_worktree`)

```
if UserChange 非空:
    拒绝写入
else:
    允许写入
```

- 仅存在 `.zealous/` 与纯运行副产物时,允许 `edit_file` / `write_file`
  继续走原有审批与 apply 流程;
- 若同时存在真实用户改动,仍拒绝写入;
- 错误信息必须区分"用户改动"与"运行副产物",格式:

```
Git 工作区存在用户改动,拒绝写入主工作区。
用户改动:
  M src/lib.rs
运行副产物(不自动处理,不阻塞本次写入):
  ?? __pycache__/
请先 commit/stash 用户改动。
```

### `git_status` 工具输出(降噪但不隐藏事实)

分段输出,原始 porcelain 行保留在对应分段内:

```
== user changes ==
M src/lib.rs

== runtime artifacts ==
?? __pycache__/
?? out.pyc

== zealous runtime data ==
?? .zealous/

== clean ==
没有用户改动
```

- 三类分段始终输出(空段写 `(无)`);
- 无用户改动(不论有无副产物)时,明确输出 `没有用户改动`;
- 不新增独立配置面。

## 验收

- 单测:纯 `__pycache__/` 不阻塞 edit_file;纯 `*.pyc` 不阻塞 edit_file;
  nested `__pycache__/` 正确分类;tracked `.pyc` 修改视为 UserChange 并阻塞
  写入;用户改动 + 副产物并存时拒绝写入且错误分段展示;`.zealous/` 不阻塞
  写入;git_status 输出三类分段;apply 后副产物未被 stage/commit。
- 集成测试:干净仓库 → 制造 `__pycache__/test.pyc` → edit_file + 审批通过
  → 目标文件修改成功、rollback commit 生成、副产物仍 untracked、commit 只含
  目标文件。
- dogfooding:真实模型"先测 → 再改 → 再测"顺序任务闭环;写入不被纯副产物
  阻塞;审计将 `__pycache__` 归类为运行副产物。

## 非目标

- 不做全局 `.gitignore` 注入(仓库策略归用户)。
- 不缓存/清理测试产物(那是工具自身的事)。
- 不处理 Python 以外的生态副产物。

## 优先级

P2 → 本阶段实现(spec 141,2026-10-20)。
