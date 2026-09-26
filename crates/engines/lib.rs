//! Lynceus 领域引擎——外部执行边界与平台元数据。
//!
//! Lynceus **不自带执行层**：任务的实际执行经 [`worker`] 边界派发给用户
//! 本机安装的外部 Worker Runtime（Claude Code / Codex / Pi /
//! DeepSeek Harness）。本 crate 的职责：
//!
//! - [`worker`]：外部 Worker Runtime 边界（probe/start/resume/events/
//!   cancel/capabilities）+ 四个显式适配器 + 进程驱动 + 编排派发层；
//! - [`model_providers`]：多 Provider LLM 网关（Profiler / Metacognition
//!   / StrategyBoard / Intake 等平台侧 LLM 消费方）；
//! - [`tool_catalog`] / [`tool_retrieval`] / [`tool_settings`] /
//!   [`tool_gateway`]：传统工具目录与 PATH 探测（**RETIREMENT
//!   CANDIDATES**——内部 Harness 已删除，保留仅为健康探针与渐进迁移；
//!   勿为新架构扩展）；
//! - [`harness::profile`]：14 个审计域的 solver 注册表元数据（retirement
//!   candidate）；
//! - [`domains`]：领域复用逻辑（content_discovery 配置校验、工件目录
//!   约定）；
//! - [`solvers`]：`DomainSolver` 注册表——fail-closed 适配层，solve 一律
//!   经 [`worker::dispatch`] 派发，无可用 runtime 时显式 unavailable；
//! - [`upload_intake`]：上传分类助手（api 消费）。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod broker;
pub mod skills;
pub mod domains;
pub mod harness;
pub mod model_providers;
pub mod solvers;
pub mod tool_catalog;
pub mod tool_gateway;
pub mod tool_retrieval;
pub mod tool_settings;
pub mod traffic;
pub mod upload_intake;
pub mod worker;

pub use solvers::{DomainSolver, default_solver_registry};
pub use tool_gateway::{ToolGateway, ToolRequest, ToolResult};
