# TUI Alpha 手工验收记录(spec 90 §5)

规则:每个场景必须记录"通过 / 失败 / 未执行";未执行视为未通过。
全部 8 项通过前,禁止打包 RC(spec 120 质量门会检查本文件)。

- 环境:Windows 11,Windows Terminal
- 二进制:target/debug/zealous.exe
- 模型:智谱 glm-5.3-flash
- 配置:~/.zealous/config.json

| # | 场景 | 结果 | 记录 |
| - | ---- | ---- | ---- |
| 1 | Windows Terminal 正常渲染(alternate screen、中文、布局) | 未执行 | |
| 2 | 创建带 brief 的任务 + 越界路径报错 | 未执行 | |
| 3 | 真实模型运行任务(进度行可见) | 未执行 | |
| 4 | 审批大型 edit_file 并完整滚动查看 | 未执行 | |
| 5 | x 中断任务并确认挂起审批收尾 | 未执行 | |
| 6 | R 恢复任务并确认投影接续 | 未执行 | |
| 7 | q 退出后终端无 raw mode / alternate screen 残留 | 未执行 | |
| 8 | 损坏账本显示错误且 TUI 不崩溃 | 未执行 | |

## 执行记录

(验收后在此追加:日期、执行人、每项结果与现象。)
