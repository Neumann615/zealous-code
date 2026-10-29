# spec 160:模型辅助冲突解决 Alpha

来源:2026-10-30 阶段规划。spec 159 已把冲突现场保留在独立 merge
workspace,但仍需用户完全手工解决。本 spec 允许模型在严格白名单内提出
冲突解决方案;用户审批前不写入任何文件。

## 目标

- 只读取 conflicted merge workspace 的 unresolved files;
- 收集 base/ours/theirs/working 四方上下文;
- 模型为每个文件给出完整解决方案或声明 manual;
- 严格校验输出契约;
- 用户审批后只写入 merge workspace 并 `git add`;
- 不自动 commit,继续沿用 `merge-continue`;
- 主工作区保持干净;
- 不自动选择 ours/theirs。

## 非目标

不自动 commit / 不自动 continue / 不写主工作区 / 不运行 shell /
不读取 unresolved files 之外的仓库文件 / 不处理超过规模上限的冲突 /
不做多模型投票。

## CLI

新增:

```text
zealous task resolve <task-id> [--yes] [--no]
```

流程:

1. 生成模型 proposal;
2. 展示每个文件的 base/ours/theirs/proposed/diff/rationale;
3. 用户批准或拒绝;
4. 批准后写入 resolved files 并 stage;
5. 输出 staged 与 manual pending 文件;
6. 提示 `merge-status` / `merge-continue`。

`--yes` 自动批准;`--no` 自动拒绝;两者互斥。

## 模型输入

只来自:

```text
git diff --name-only --diff-filter=U
```

每个文件收集:

- base:`git show :1:<path>`
- ours:`git show :2:<path>`
- theirs:`git show :3:<path>`
- working:workspace 当前内容

缺失 stage 如实标记 `missing`。

限制:

- unresolved files ≤ 8;
- 单文件 ≤ 256 KiB;
- 总上下文 ≤ 768 KiB。

## 输出契约

模型必须返回恰好一个 JSON object:

```json
{
  "action": "resolve",
  "summary": "string",
  "files": [
    {
      "path": "string",
      "mode": "resolved",
      "content": "complete file",
      "rationale": "string"
    },
    {
      "path": "string",
      "mode": "manual",
      "reason": "string"
    }
  ]
}
```

要求:

- files 精确覆盖 unresolved list,不能多、不能少;
- path 精确匹配;
- resolved 必须有 content + rationale;
- manual 必须有 reason;
- content 必须是完整内容,不是 patch;
- content 不得包含 conflict markers;
- resolved content 不得与 working 完全相同;
- 严格拒绝未知字段与非法 JSON;
- 最多 2 次修复轮。

## 事件与投影

新增事件:

- `MergeResolutionStarted { resolution_id, files }`
- `MergeResolutionProposed { resolution_id, summary, resolved_files, manual_files }`
- `MergeResolutionDecided { resolution_id, decision, reason }`
- `MergeResolutionApplied { resolution_id, staged_files }`

投影新增 `merge_resolution`:

- Started
- Proposed
- Approved
- Rejected
- Applied
- ManualPending
- Failed

同一时间只允许一个 pending proposal;拒绝后可重新发起。

## 写入语义

批准时:

1. 重新读取 unresolved list;
2. 与 proposal 文件集合比对,变化即拒绝;
3. 只写 mode=resolved 文件;
4. UTF-8 / path / conflict marker 再校验;
5. `git add`;
6. 不 commit;
7. manual 文件保持 unresolved。

## TUI / app-server

TUI 在 merge status 视图提供 R / Enter / A / D / S 操作。

app-server 新增:

- `propose_merge_resolution`
- `approve_merge_resolution`
- `reject_merge_resolution`

通知:

- `merge_resolution_started`
- `merge_resolution_proposed`
- `merge_resolution_approved`
- `merge_resolution_applied`
- `merge_resolution_rejected`
- `merge_resolution_failed`

## 验收

单元 / 集成覆盖上下文限制、契约解析、审批写入、partial resolution、
拒绝重试、CLI / TUI / app-server。真实 Dogfooding 覆盖语义合并、partial
manual、拒绝重试与安全边界。
