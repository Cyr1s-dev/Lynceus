//! `content_discovery` 域配置校验（fail-fast）。
//!
//! 【RETIREMENT CANDIDATE】内部 Harness 已删除；本模块只剩配置形状校验
//! （`validate_config`），供 `DomainSolver::validate_config` 消费。
//! 解析/归一化/执行计划等 adapter 支撑代码已随内部执行链一并移除。

mod config;

use agents::solver::SolverConfigError;
use serde_json::{Map, Value};

/// 校验 mission config 的 `content_discovery` 配置段（fail-fast）。
pub(crate) fn validate_config(
    config: &Map<String, Value>,
    project: Option<&models::Project>,
) -> Result<(), SolverConfigError> {
    config::validate(config, project)
        .map_err(|error| SolverConfigError(format!("ContentDiscoveryConfigError: {error}")))
}
