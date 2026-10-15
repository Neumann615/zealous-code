//! 编排与执行内核:事件账本写入、Coordinator/Worker 编排、工具注册表与
//! 风险分级、审批流、OpenAI-compatible Provider(`Provider` trait 隔离)。
//! 对应 spec: `docs/spec/20-event-ledger.md`、`docs/spec/30-permissions.md`、
//! `docs/spec/50-architecture.md`。
//!
//! 本 crate 是未来桌面 app 复用的内核主体;UI 一律为薄客户端。

// 骨架阶段:各模块随对应 spec 的首次实现逐个落地,
// 依赖方向保持 cli → engine → core,禁止反向。
