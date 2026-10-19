# 扩展 Dogfooding 记录(spec 140)

- 执行日期:2026-10-20
- 模型:智谱 glm-5.3-flash(OpenAI-compatible,~/.zealous/config.json);C7 用本地 mock endpoint
- 二进制:target/release/zealous.exe(与 RC 0.1.0 同源;运行时代码自 66cad6f 起未变)
- 仓库根:`%TEMP%\zealous-dogfooding-140\`(夹具 scripts/dogfooding-140-fixture.ps1 生成)
- 驱动:serve 协议(`dist/dogfood140-drive.py`,未入库);审批策略 per task
- 证据目录:`dist/dogfood140-results/<ID>.json`(事件链、账本审计、git 审计)

## Round A · 日常编码能力

### A1 多文件小功能(a1-multifile)

- 目标:calc 包新增 rectangle_perimeter,导出、测试、README 用法,保持旧测试通过。
- 结果:**Completed**;turns=13,tools=9,approvals=4(edit_file×3 + shell×1),修复轮次=2
  (均为 action JSON 夹带说明文字,repair 环内自纠,ErrorOccurred scope=model_action)。
- 写入 commit:`6937ff9 edit calc\area.py`、`df90c37 edit test_area.py`、`3aad4ca edit README.md`
  (3 文件,每笔均带 rollback commit)。
- 验收:perimeter 实现正确(2*(w+h));`python -m unittest test_area` 5 tests OK;
  README 含用例;测试文件仅新增用例。事件 62 条 seq 连续。
- 非预期修改:仅 `__pycache__/`(跑测试副产物,非内容修改);无路径越界、无未审批写入、无泄漏。
- 操作者结论:多文件保持边界清晰,审批摘要可审查,过程可信。

### A2 Bug 定位与修复(a2-bugfix)

- 目标:失败测试 test_mathlib → 定位根因 → 修复 → 重跑 → 汇报原因。
- 结果:**Completed**;turns=6,tools=4,approvals=2,修复轮次=0。
- 写入 commit:`b769248 zealous: edit mathlib.py`(仅 1 文件)。
- 验收:`add()`/`multiply()` 正确(multiply 为 `a * b`);3 tests OK;
  **汇报含根因("乘法误写成减法 return a - b")与改动边界**(add/测试未动,
  自动提交作为回退点)。事件 32 条 seq 连续。
- 非预期修改:仅 `__pycache__/`;无越界、无未审批写入、无泄漏。
- 操作者结论:定位-修复-验证-汇报四步完整,根因解释准确。

### A3 重构(a3-refactor)

- 目标:report.py 去重(两个 summarize 同实现),行为不变,先读后改再验证。
- 结果:**Completed**;turns=8,tools=5,approvals=2,修复轮次=1(model_action 修复环)。
- 写入 commit:`f93424a zealous: edit report.py`(仅 1 文件;`git diff --stat` 6+/9-)。
- 验收:抽出 `_summarize`,公共函数名/输出格式不变;`python -m unittest test_report`
  3 tests OK;**test_report.py 未修改**。事件 39 条 seq 连续。
- 非预期修改:仅 `__pycache__/`;无越界、无未审批写入、无泄漏。
- 操作者结论:重构克制且边界符合要求(不动测试、不改 API)。
