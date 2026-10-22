# spec 154:工具与上下文增强(read 分片 / 正则检索 / git 历史视图)

来源:2026-10-22 阶段规划。Dogfooding 151/152/153 之后,真实任务的
主要摩擦转向上下文获取:大文件只能整读、检索只有字面量、模型看不到
分支与历史。本 spec 在**不扩大写权限、不改变 T0/T1/T2 风险模型**的
前提下增强只读工具。

## 目标

- 降低大文件读取成本(read_file 分片);
- 提升代码检索精度(search_text 正则 + 作用域);
- 让模型能查看历史与分支上下文(git_show / git_branch);
- 工具 manifest 由注册表自动生成,新能力自动可见。

## read_file 分片读取

默认行为不变:`{"path": "src/lib.rs"}` 返回完整内容(兼容)。

新增可选参数:

    {"path": "src/lib.rs", "offset": 101, "limit": 120}

- `offset`:1-based 起始行,默认 1;必须 > 0,否则 BadArgs。
- `limit`:最大返回行数,1–500;0 拒绝;>500 拒绝(固定拒绝语义,
  不做 clamp)。
- 使用 offset 或 limit 即进入分片模式,输出带行号:

      path: src/lib.rs
      lines: 101-220 / 1800

        101 fn example() {
        102     todo!()

  (总行数为文件实际行数;offset 超出文件末尾返回明确提示空范围。)
- 分片模式单次最大读取 4 MiB,超限拒绝(不静默截断)。
- write_file / edit_file 契约不变;非 UTF-8 文件仍失败,不猜测解码。

## search_text 作用域与正则

字面量搜索兼容:`{"query": "WidgetFactoryZQ"}`。

新增参数:

    {"query": "fn\\s+\\w+", "regex": true, "path": "src/core", "max_results": 100}

- `regex=true` 使用 Rust `regex` 引擎;表达式最长 1024 字符;
  无效表达式返回可解释 BadArgs。
- `path` 可选:仓库内相对路径,文件或目录均可;越界直接拒绝。
- `max_results`:默认 100,范围 1–200。
- 每文件最多 20 个匹配,避免单文件刷屏。
- 输出行格式不变:`path:line: 匹配行内容`。
- 只读;替换仍必须走 edit_file(T1 审批)。

## git_show(T0)

参数:`{"object": "HEAD:README.md"}` 或 `{"object": "HEAD"}`。

- 底层执行 `git show <object>`;输出上限 64 KiB,超限截断并显式标记
  `[truncated: output exceeds 64 KiB]`。
- 无网络、无配置写入、无仓库变更;object 由 git 校验,无效返回
  git 错误文本。
- 用于查看 commit diff、历史文件内容、HEAD:path。

## git_branch(T0)

参数:`{"all": true, "limit": 100}`。

- 默认列本地分支;`all=true` 列 local + remote(`git branch --all`)。
- `limit` 默认 100,最大 500。
- 输出:current 标记(`*`)+ 分支名 + 最后一次提交短说明;
  总输出上限 64 KiB(截断标记同 git_show)。
- 只读;不创建、删除或切换分支。

## Manifest

- manifest 由工具注册表自动生成(args_template/description),
  新能力自动包含,不手写重复说明;
- Worker 仍只见 T0;Coordinator 见 T0/T1/T2。

## 非目标

- 不做 worktree 隔离、多任务队列、桌面壳;
- 不处理 Cargo.lock 副产物(计数 1);
- 不扩大任何写权限。
