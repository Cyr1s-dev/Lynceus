//! 【RETIREMENT CANDIDATE】旧内部 Harness 的残余元数据。
//!
//! Lynceus 的内部 Harness 编排循环（AgentToolHarness）、CLI/native 适配器、
//! interaction plane、IDA MCP 已随外部 Worker Runtime 边界的落地整体删除。
//! 注意：工具的安全 argv 执行通道 [`crate::tool_gateway`] **保留**，仍是
//! lynceus-mcp `tool_execute` 的唯一执行路径（见该模块文档）。本目录只保留
//! [`profile::DomainProfile`]——14 个审计域的 solver 名 / audit_domains /
//! 风险级元数据，`solvers::default_solver_registry` 与
//! `tool_retrieval` 仍引用它。
//!
//! 迁移验证（真实外部 Runtime 端到端验收）已完成：DomainProfile 属于
//! **retirement candidates**——不要为它新增字段/能力，新架构的能力
//! 元数据归 `crate::worker` 与 Connection 体系；后续以
//! "外部 Worker capability 声明"取代本表后整体移除。

pub mod profile;

pub use profile::{DomainProfile, RemoteToolSourceKind, ToolRisk, profile_for, profiles};
