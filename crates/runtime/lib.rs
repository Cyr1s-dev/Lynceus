//! Lynceus 编排引擎 —— `server/core/engine/manager.py` 的移植。
//!
//! 模块按领域内聚拆分（Python 单文件 7269 行 → 多模块）：
//! - [`manager`]：`AuditManager` 结构体、构造布线、并发原语
//!   （mission 启动锁 / runtime 令牌 / run 变更锁）；
//! - [`events`]：过程日志（`AuditEvent` 记录 + journal 追加）；
//! - [`task_backend`]：执行调度抽象（内存后端 + 可替换 trait）。
//!
//! 收口链（COV → META → MGATE → EGUARD）与 branch driver 循环在
//! `run_branch_runtime` / `reassess_mission_completion` 路径中接入
//! （见 `agents` 的对应组件）。

// 测试代码大量使用 unwrap/expect 直陈前置条件（与 agents 同一惯例）。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod assets;
pub mod attractiveness;
pub mod branch_runtime;
pub mod closure;
pub mod decision_runtime;
pub mod errors;
pub mod events;
pub mod execution_control;
pub mod finding_retests;
pub mod graph_writes;
pub mod manager;
pub mod mission_lifecycle;
pub mod mission_runtime;
pub mod module_writes;
pub mod narratives;
pub mod notifications;
pub mod provider_writes;
pub mod solver_bootstrap;
pub mod task_backend;

pub use errors::EngineError;
pub use branch_runtime::MissionInterruptOutcome;
pub use execution_control::{
    ExecutionBackend, ExecutionBackendError, ExecutionControlError, ExecutionControlPlane,
    ExecutionWaitOutcome, SafeLocalExecutionBackend, SubmitExecutionOptions,
};
pub use manager::AuditManager;
pub use manager::RuntimeToken;
pub use manager::ToolAvailabilityResolver;
pub use models::{MissionNotification, NotificationKind};
pub use notifications::{
    NotificationHub, Subscription, notification_for_decision_gate, notification_for_event,
    notification_for_finding, notification_for_mission_status,
};
pub use task_backend::InMemoryTaskBackend;
pub use task_backend::TaskBackend;
pub use task_backend::TaskFn;
pub use task_backend::TaskHandle;
pub use task_backend::TaskJoinError;
