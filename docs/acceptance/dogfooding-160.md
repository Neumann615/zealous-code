# Dogfooding 160 验收记录(spec 160:模型辅助冲突解决 Alpha)

- 执行日期:2026-10-30
- 二进制:`target/debug/zealous.exe`
- 模型:智谱 `glm-5.3-flash`
- 临时仓库:`%TEMP%\zealous-dogfooding-160-*`
- 质量门:fmt / test --all / clippy 全部通过
- 单元 / 集成:resolution contract 4/4、projection lifecycle 1/1、
  CLI AI resolution E2E 1/1、serve propose/approve/continue E2E 1/1 通过

## DR1 独立语义冲突 —— PASS

有效仓库:`%TEMP%\zealous-dogfooding-160-dr1b-0ba5e3dd`

- base:`cc550eee581adb19033e329d8364bb3c7ccd7045`
- 任务:
  - `task_01M647N2SKPY0ZPB868FJEGTHF`:color red → blue
  - `task_01M647N2TBDZ1QHP073QYB3X96`:size small → large
- Resolution:
  - `resolution_01M647P1DPDVJQ07KVH85CFYPT`
  - 模型合并双方意图,保留 `enabled=true`;
  - proposed content:`color=blue size=large enabled=true`
- 结果:
  - 用户审批后写入 merge workspace 并 stage;
  - 未自动 commit;
  - `merge-continue` 生成 merge commit
    `1ae907b788f3e71babe13dee81e2e0d3fecea41d`;
  - merge workspace 清理;
  - 原 task worktree 保留;
  - 主工作区干净。

附加强验证:首轮 DR1 使用强耦合 type/default 冲突,模型合理返回
`manual`,系统不写入、不 stage,merge-continue 拒绝。该行为符合安全边界,
不是缺陷。

## DR2 多文件 partial resolution —— PASS

- 仓库:`%TEMP%\zealous-dogfooding-160-dr2-dee3fe85`
- base:`db080d1ab11881a76e8ff5335d35efca1673e787`
- 任务:
  - `task_01M647PZ0BWNE7EKFK1KWH17PE`
  - `task_01M647PZ14P90DM4BHS55SMTNX`
- Resolution:
  - `resolution_01M647RFM1AP28HZBD0AMM7539`
  - `config.txt`:mode=resolved,合并 color 与 size;
  - `opaque.txt`:mode=manual,因为 opaque token 无法判断权威值。
- 结果:
  - 批准后仅 `config.txt` 写入并 stage;
  - `opaque.txt` 仍 unresolved;
  - merge-continue 首次拒绝;
  - 用户写入 `token=user-final` 并 stage;
  - continue 成功,merge commit:
    `90c87ee96605942bf2a45d7fed78e61eae29f81e`;
  - 最终保留模型合并结果与用户手动结果;
  - 主工作区干净,原 task worktree 保留。

## DR3 Proposal 拒绝后重试 —— PASS

- 仓库:`%TEMP%\zealous-dogfooding-160-dr3-3fd7cc9e`
- 任务:
  - `task_01M647SFVK6XQDQBK6EFP0WA7X`
  - `task_01M647SFWACXC4NT0NWXZ8Q7YS`
- 第一次 proposal:
  - `resolution_01M647TB9P0JGNV6YHMK6X0BA9`
  - 用户执行 `--no`;
  - 未写入、未 stage;
  - 投影进入 rejected。
- 第二次 proposal:
  - `resolution_01M647TJHFB89KM0Q3DSNY9XD9`
  - 用户执行 `--yes`;
  - `config.txt` 写入并 stage;
  - merge commit:
    `0f2deb4ef0cc046f49527e8706f3628e521715d3`;
  - 最终:`color=blue size=large enabled=true`。

## DR4 安全边界 —— PASS

- 仓库:`%TEMP%\zealous-dogfooding-160-dr4-47d07dcd`
- 任务:
  - `task_01M647VP67GH8PREEEHV0S4Q2M`
  - `task_01M647VP70V0FWADZGNRSMWSRA`
- 冲突文件:`conflict.txt`(包含疑似敏感 token)
- 未冲突文件:`unrelated.txt`(包含 `must-not-be-sent` 标记)
- 本地转发代理只记录 request body,不记录 Authorization 头。
- 验证:
  - request body 包含 conflict 内容;
  - request body 不包含 `must-not-be-sent`;
  - request body 不包含 proxy dummy key;
  - 模型对敏感 scalar 返回 `manual`,未写入;
  - merge workspace 保留,主工作区干净;
  - ledger 无 `Authorization` / `Bearer`;
  - resolver 无 shell 工具能力,不读取 unresolved 之外的文件。

## LOCAL TRIAL release 回归 —— PASS

- RC zip SHA256:`52A6F9772573428D3954512D6EDC9841684BF07DE5C1F505063F7F54C753F082`
- 解压二进制:`%TEMP%\zealous-160-localtrial-c177c896\bin\zealous.exe`
- 仓库:`%TEMP%\zealous-160-localtrial-c177c896`
- base:`04aef0e6f157c95b979e76350ed5f6715a37c670`
- 任务:
  - `task_01M648EYWDYYEPVNTDNBVTM42E`:color red → blue
  - `task_01M648EYX01RX6VZ5JMZZSFQY1`:size small → large
- Resolution:
  - `resolution_01M648FY2MXQ8XM3S66RFQEYFX`
  - proposed:`color=blue size=large enabled=true`
- 结果:
  - release binary 生成 proposal;
  - 审批后写入 merge workspace 并 stage;
  - 未自动 commit;
  - merge commit:`4a3f5a2519482760cb0c069dd923ab4c13d9e6f7`;
  - merge workspace 清理,原 task worktree 保留;
  - 主工作区干净;
  - 最终内容保留双方意图。

## 账本审计

- 事件 seq 连续;
- pending approvals 为 0;
- 无 API key / Authorization / Bearer 落账;
- Started / Proposed / Decided / Applied 状态与最终工作区一致;
- partial manual 场景保持 unresolved,直到用户手动 stage。

## 结论

DR1 / DR2 / DR3 / DR4 全部 PASS。无 P0 / P1。
模型只能提出方案;用户审批前不写入,写入后不自动 commit,
所有最终集成仍由 `merge-continue` 完成。

## 阶段末本地 bundle(spec 160)

| File | Size | SHA256 |
| ---- | ---- | ------ |
| zealous-code-main-airesolution160.bundle | 479451 | 11414BCE4EB258953185A50AE09EE1E043EC22645FDA165FC49236C7CC7A29C8 |

- `git bundle verify` 通过,包含 main 至 RC record commit
  `7e54f9475102ac75354638011b7446f74fba7589`;
- 本提交为 bundle rotation commit,按 spec 150 三段溯源模型不进入 bundle;
- verify 通过后删除旧 `zealous-code-main-mergeworkspace159.bundle`;
- archive bundle 保留;OFFLINE RC lock 已刷新;仍不自动推送 remote。
