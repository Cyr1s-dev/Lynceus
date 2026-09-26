//! Engine 错误类型 —— `server/core/engine/errors.py` 的移植。
//!
//! 控制面映射契约（API 与 MCP 共同遵守，Python 侧注释原文照搬语义）：
//! - `ProjectNotFound` 等 KeyError 族 → HTTP 404；
//! - `SolverConfig` / `ProviderConfig` / `ModuleConfig` / `BudgetConfig` /
//!   `ReferenceValidation` / `Value`（Python 侧均为 ValueError 族）→ HTTP 422。
//!
//! Python 侧的子类关系（`BudgetConfigError` extends `SolverConfigError`
//! 等）以 [`EngineError::is_solver_config_error`] 表达：API 层用单个判断
//! 即可同时捕获两类 run 配置问题，等价于 `except SolverConfigError`。

#![allow(clippy::doc_markdown)]

use agents::branch_generator::BranchGeneratorError;
use storage::StorageError;

/// `AuditManager` 编排层的领域错误。
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// 引用的 Project 不存在（Python `ProjectNotFoundError`，映射 404）。
    #[error("unknown project: {0}")]
    ProjectNotFound(String),
    /// 引用的 AuditRun 不存在（Python `RunNotFoundError`，映射 404）。
    #[error("unknown audit run: {0}")]
    RunNotFound(String),
    /// 引用的 Mission 不存在（Python `MissionNotFoundError`，映射 404）。
    #[error("unknown mission: {0}")]
    MissionNotFound(String),
    /// 引用的 Branch 不存在（Python `BranchNotFoundError`，映射 404）。
    #[error("unknown branch: {0}")]
    BranchNotFound(String),
    /// 引用的 DecisionGate 不存在（Python `DecisionGateNotFoundError`，映射 404）。
    #[error("unknown decision gate: {0}")]
    DecisionGateNotFound(String),
    /// 引用的 Finding 不存在（映射 404）。
    #[error("unknown finding: {0}")]
    FindingNotFound(String),
    /// Provider 不存在（映射 404）。
    #[error("provider not found: {0}")]
    ProviderNotFound(String),
    /// 指纹规则包不存在（映射 404）。
    #[error("fingerprint rule pack not found: {0}")]
    FingerprintRulePackNotFound(String),
    /// 执行任务不存在（映射 404）。
    #[error("execution not found: {0}")]
    ExecutionNotFound(String),
    /// Tool Catalog 条目或安装任务不存在（映射 404）。
    #[error("{0}")]
    ToolCatalogNotFound(String),
    /// 执行 ID / 幂等键冲突（映射 409）。
    #[error("{0}")]
    ExecutionConflict(String),
    /// 公开执行入口拒绝了特权后端（映射 403）。
    #[error("{0}")]
    ExecutionBackendForbidden(String),
    /// Provider runtime 未配置（映射 503）。
    #[error("provider runtime is not configured")]
    ProviderRuntimeUnavailable,
    /// 外部 Worker Runtime 子系统未装配或探测未完成（映射 503）。
    #[error("worker runtime subsystem is not available: {0}")]
    WorkerRuntimeUnavailable(String),
    /// Tool Catalog 的内嵌清单或本地配置损坏（映射 500）。
    #[error("{0}")]
    ToolCatalogFailure(String),

    /// run 的 solver 配置非法（Python `SolverConfigError`，映射 422）。
    #[error("{0}")]
    SolverConfigError(String),
    /// run 引用了不可用的 provider（Python `ProviderConfigError`，是
    /// `SolverConfigError` 子类，映射 422）。
    #[error("{0}")]
    ProviderConfigError(String),
    /// run 引用了不可用的模块（Python `ModuleConfigError`，是
    /// `SolverConfigError` 子类，映射 422）。
    #[error("{0}")]
    ModuleConfigError(String),
    /// run 的步数预算配置非法（Python `BudgetConfigError`，是
    /// `SolverConfigError` 子类，映射 422）。
    #[error("{0}")]
    BudgetConfigError(String),
    /// 证据 / Finding 引用了不存在的节点（Python `ReferenceValidationError`，
    /// 映射 422；solver 结果路径上则记任务失败，不污染图）。
    #[error("{0}")]
    ReferenceValidationError(String),
    /// 泛化非法参数（Python 裸 `ValueError`：空白标题、非法枚举 wire 值、
    /// 不支持的批量动作等，映射 422；不属于 `SolverConfigError` 族）。
    #[error("{0}")]
    Value(String),
    /// FastAPI/Pydantic 风格的结构化请求校验错误（`detail` 数组）。
    ///
    /// 该变体只承载已经过 API 边界校验的稳定 wire 内容；领域层不应借此
    /// 绕过自己的强类型校验。
    #[error("request validation failed")]
    RequestValidation(serde_json::Value),

    /// DecisionGate 无法从当前状态迁移（Python `DecisionGateStateError`，
    /// 映射 409/422）。
    #[error("{0}")]
    DecisionGateStateError(String),
    /// DecisionAnswer 畸形或引用了非法选项（Python `InvalidDecisionAnswerError`）。
    #[error("{0}")]
    InvalidDecisionAnswerError(String),

    /// 存储层错误（仓储读写失败）。
    #[error(transparent)]
    Storage(#[from] StorageError),
}

impl From<BranchGeneratorError> for EngineError {
    /// 分支生成器只抛 `ValueError` 族（工具名分支标题），映射到通用
    /// `Value` 变体。
    fn from(value: BranchGeneratorError) -> Self {
        EngineError::Value(value.to_string())
    }
}

impl From<agents::context::ContextError> for EngineError {
    /// ContextPack 构建失败：存储层错误透传，其余（journal 读取、序列化）
    /// 以 Python 泛异常路径的消息形态映射 `Value`。
    fn from(value: agents::context::ContextError) -> Self {
        match value {
            agents::context::ContextError::Storage(error) => EngineError::Storage(error),
            other => EngineError::Value(other.to_string()),
        }
    }
}

impl EngineError {
    /// 是否属于 Python `SolverConfigError` 子类族（含
    /// `ProviderConfig` / `ModuleConfig` / `BudgetConfig`）。
    ///
    /// 等价于 Python 侧 `isinstance(exc, SolverConfigError)`，API 层据此
    /// 单点映射 422。
    #[must_use]
    pub const fn is_solver_config_error(&self) -> bool {
        matches!(
            self,
            EngineError::SolverConfigError(_)
                | EngineError::ProviderConfigError(_)
                | EngineError::ModuleConfigError(_)
                | EngineError::BudgetConfigError(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solver_config_family_covers_provider_module_budget() {
        assert!(EngineError::SolverConfigError("x".into()).is_solver_config_error());
        assert!(EngineError::ProviderConfigError("x".into()).is_solver_config_error());
        assert!(EngineError::ModuleConfigError("x".into()).is_solver_config_error());
        assert!(EngineError::BudgetConfigError("x".into()).is_solver_config_error());
        assert!(!EngineError::ProjectNotFound("x".into()).is_solver_config_error());
        assert!(!EngineError::ReferenceValidationError("x".into()).is_solver_config_error());
    }

    #[test]
    fn messages_mirror_python_str_forms() {
        assert_eq!(
            EngineError::ProjectNotFound("p1".into()).to_string(),
            "unknown project: p1"
        );
        assert_eq!(
            EngineError::RunNotFound("r1".into()).to_string(),
            "unknown audit run: r1"
        );
    }
}
