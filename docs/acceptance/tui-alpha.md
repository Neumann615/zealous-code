# TUI Alpha 手工验收记录(spec 90 §5)

规则:每个场景必须记录"通过 / 失败 / 未执行";未执行的场景一律视为未通过,
不得以自动化测试通过或提交信息代替。

- 环境:Windows 11(第一验证平台)
- 终端:Windows Terminal
- 二进制:target/debug/zealous.exe(main @ 见 git log)
- 模型:智谱 glm-5.3-flash(OpenAI-compatible)
- 配置:`~/.zealous/config.json`(全局);密钥不回显

| # | 场景 | 结果 | 记录 |
| - | ---- | ---- | ---- |
| 1 | Windows Terminal 正常渲染(alternate screen、中文、布局) | 未执行 | 待人工 |
| 2 | 创建带 brief 的任务(表单 + 越界路径报错) | 未执行 | 待人工 |
| 3 | 使用真实模型运行任务(进度行、工具行出现) | 未执行 | 待人工 |
| 4 | 审批大型 edit_file 内容并完整滚动查看(位置指示/已完整查看标记) | 未执行 | 待人工 |
| 5 | x 中断任务(Interrupted、挂起审批收尾) | 未执行 | 待人工 |
| 6 | R 恢复任务(续跑、投影接续) | 未执行 | 待人工 |
| 7 | q 退出后终端无 raw mode / alternate screen 残留(输入有回显) | 未执行 | 待人工 |
| 8 | 损坏账本显示错误且 TUI 不崩溃(向 events.jsonl 写入残缺行后打开) | 未执行 | 待人工 |

## 已由自动化覆盖(不计入手工验收)

- reducer/审批通道/渲染缓冲(TestBackend)与 mock 控制器集成测试:
  见 `cargo test --all` 输出。
- 真实模型 CLI 链路:real_model_smoke(ignored,显式执行)。

## 执行记录

(验收后在此追加:日期、执行人、每项结果与现象;失败项附复现步骤。)
