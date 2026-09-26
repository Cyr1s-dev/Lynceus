//! 外部 Worker Runtime 边界（Lynceus 不再自带执行层）。
//!
//! 实际执行交给用户本机安装的外部 Worker Runtime：Claude Code / Codex /
//! Pi / DeepSeek Harness 四个显式适配器，经 [`process`] 进程驱动统一
//! 受控执行（无 shell 解释、有界输出、超时强杀、取消传播）。
//!
//! 职责边界（红线）：
//! - **认证只来自 Lynceus Connection**（复用 Provider 仓储 + SecretStore）；
//!   未绑定有效 Connection 的 runtime 是 NotReady，绝不复用本机 CLI 登录；
//! - adapter 只做 headless 接口映射（版本感知），不实现工具循环/Harness；
//! - 外部输出 = 受控 observation（密封 transcript + 有界事件），不直接
//!   产生 Finding；
//! - 本模块**不持有调度状态机**——Mission/Branch/AgentTask 的生命周期、
//!   取消与超时仍归编排层。
//!
//! 传统 Tool Catalog / profile 体系在迁移验证完成前保留
//! （**retirement candidate**——勿为新架构扩展它们）。

pub mod adapters;
pub mod command_policy;
pub mod dispatch;
pub mod gateway;
pub mod process;
pub mod registry;

pub use adapters::ClaudeCodeWorker;
pub use adapters::CodexWorker;

pub use adapters::DeepSeekHarnessWorker;
pub use adapters::PiWorker;
pub use command_policy::CommandPolicy;
pub use command_policy::CONFIG_COMMAND_POLICY;
pub use dispatch::CONFIG_WORKER_RUNTIME;
pub use dispatch::CONFIG_WORKER_TIMEOUT_SECONDS;
pub use gateway::GatewayManager;
pub use registry::WorkerRegistry;

use std::sync::Arc;
use std::sync::OnceLock;
use storage::Repository;

/// 组合根装配：以仓储句柄构造生产 worker 注册表（selector + resolver）。
#[must_use]
pub fn default_worker_registry(repository: Arc<dyn Repository>) -> Arc<WorkerRegistry> {
    Arc::new(WorkerRegistry::new(repository))
}

static GLOBAL_REGISTRY: OnceLock<Arc<WorkerRegistry>> = OnceLock::new();

/// 组合根装配并登记进程级 worker 注册表（幂等：重复 attach 返回首个
/// 实例）。生产入口是 [`crate::worker`]；`/worker-runtimes` 观测端点经
/// [`global_registry`] 读取。
pub fn attach_global_registry(repository: Arc<dyn Repository>) -> Arc<WorkerRegistry> {
    let registry = default_worker_registry(repository);
    let _ = GLOBAL_REGISTRY.set(Arc::clone(&registry));
    GLOBAL_REGISTRY
        .get()
        .cloned()
        .unwrap_or_else(|| Arc::clone(&registry))
}

/// 当前进程登记的注册表（未 attach 时为 None）。
#[must_use]
pub fn global_registry() -> Option<Arc<WorkerRegistry>> {
    GLOBAL_REGISTRY.get().cloned()
}
