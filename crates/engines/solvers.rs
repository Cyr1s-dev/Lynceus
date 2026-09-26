//! 领域 solver 注册表（M6 的安全默认实现）。
//!
//! 每个领域都有独立的 solver 名称和审计域声明，因此能力路由不会再因
//! “注册表为空”把所有分支误判为不可执行。默认 solver 是 fail-closed
//! 的适配层：没有配置具体工具时只返回结构化说明，不伪造 Finding；具体
//! 工具适配器可按同一 `BaseSolver` 契约替换，不需要改编排器。

use std::collections::HashSet;

use agents::solver::{BaseSolver, SolverContext, SolverError, SolverRegistry, SolverResult};
use async_trait::async_trait;
use models::domain::AuditDomain;
use models::project::Project;
use serde_json::{Map, Value};

/// 领域 solver：默认是 fail-closed 适配层，没有真实工具时绝不伪造 Finding。
#[derive(Debug, Clone, Copy)]
pub struct DomainSolver {
    name: &'static str,
    domains: &'static [AuditDomain],
}

impl DomainSolver {
    /// 创建一个领域 solver。
    #[must_use]
    pub const fn new(name: &'static str, domains: &'static [AuditDomain]) -> Self {
        Self { name, domains }
    }
}

#[async_trait]
impl BaseSolver for DomainSolver {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &'static str {
        "Fail-closed domain adapter; configure a concrete tool engine to execute scans."
    }

    fn audit_domains(&self) -> HashSet<AuditDomain> {
        self.domains.iter().copied().collect()
    }

    /// 配置形状校验：
    /// - `engine` 若声明，必须是非空字符串；工具归属与本地可用性由运行时
    ///   catalog 决定，不在 profile 维护第二份 allowlist；
    /// - `content_discovery` 追加域结构校验。
    fn validate_config(
        &self,
        config: &Map<String, Value>,
        project: Option<&Project>,
    ) -> Result<(), agents::solver::SolverConfigError> {
        if let Some(value) = config.get("engine") {
            let engine = value
                .as_str()
                .map(str::trim)
                .filter(|engine| !engine.is_empty());
            let Some(engine) = engine else {
                return Err(agents::solver::SolverConfigError(
                    "solver engine must be a non-empty string".to_string(),
                ));
            };
            // The trusted catalog is the sole source of executable tool
            // membership. A syntactically valid but unavailable engine fails
            // closed in the Harness before any process is started.
            let _ = engine;
        }
        if self.name == "content_discovery" {
            return crate::domains::content_discovery::validate_config(config, project);
        }
        Ok(())
    }

    async fn solve(&self, context: SolverContext) -> Result<SolverResult, SolverError> {
        // 执行路径（红线，唯一）：实际执行一律经外部 Worker Runtime 派发。
        // 未装配 selector / 无可用 runtime / 未绑定 Connection → 显式
        // unavailable / configuration required，**绝不回退内部执行**，
        // 绝不伪造产出。Lynceus 不再自带任何执行器。
        let Some(selector) = context.worker_runtime.as_ref() else {
            return Err(SolverError::Other(
                "external worker runtime not configured: the composition root did not \
                 attach a worker registry; Lynceus does not execute tasks itself"
                    .to_string(),
            ));
        };
        crate::worker::dispatch::run_domain(std::sync::Arc::clone(selector), self.name, &context)
            .await
    }
}

/// Python `ToolRegistry` 注册的 19 个真实工具适配器名单
/// （`engines.runtime.apply_engines` 全量注册；`/health.tools` 即此名单，
/// 不做本地可用性过滤——可用性是 manager 工具闸的独立判定）。
  pub const REGISTERED_TOOL_ADAPTERS: [&str; 19] = [
    "afrog",
    "crlf",
    "dalfox",
    "ehole",
    "feroxbuster",
    "ffuf",
    "gobuster",
    "httpx",
    "jsluice",
    "katana",
    "naabu",
    "native_fingerprint",
    "nuclei",
    "page_hints",
    "semgrep",
    "subfinder",
    "traffic_artifact_import",
    "wappalyzergo",
    "web_exploit_campaign",
];

/// 构造与 Python production runtime 相同的 14 个可路由 solver 注册表。
///
/// `web_iast`、`fuzzing`、`supply_chain`、`cloud_native` 仍是能力规划项，
/// 不能仅因存在 fail-closed 占位实现就被健康检查宣称为已注册能力。
#[must_use]
pub fn default_solver_registry() -> SolverRegistry {
    let mut registry = SolverRegistry::new();
    for profile in crate::harness::profile::profiles() {
        registry.register(std::sync::Arc::new(DomainSolver::new(
            profile.solver_name,
            profile.audit_domains,
        )));
    }
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use agents::solver::SolverContext;
    use models::ids::{ProjectId, RunId, TaskId};

    fn domain_solver(name: &str) -> DomainSolver {
        let profile = crate::harness::profile::profile_for(name)
            .unwrap_or_else(|| panic!("missing test profile: {name}"));
        DomainSolver::new(profile.solver_name, profile.audit_domains)
    }

    #[test]
    fn default_registry_matches_python_runtime_solvers() {
        let names = default_solver_registry().names();
        assert_eq!(names.len(), 14);
        assert!(names.contains(&"web_sast".to_string()));
        assert!(names.contains(&"binary_analysis".to_string()));
        assert!(names.contains(&"web_validation".to_string()));
        assert!(names.contains(&"web_exploit".to_string()));
        assert!(!names.contains(&"cloud_native".to_string()));
    }

    #[test]
    fn content_discovery_rejects_invalid_config_before_execution() {
        let config = serde_json::json!({
            "content_discovery": {
                "target": "https://example.test",
                "wordlist": "D:/lists/common.txt",
                "engines": ["gobuster"],
                "threads": 500
            }
        })
        .as_object()
        .cloned()
        .expect("test configuration is an object");
        let error = domain_solver("content_discovery")
            .validate_config(&config, None)
            .expect_err("unbounded concurrency must be rejected");
        assert!(error.to_string().contains("threads"));
        assert!(error.to_string().contains("<= 100"));
    }

    #[test]
    fn engine_declaration_validates_shape_not_duplicate_membership() {
        for engine in ["nuclei", "future_yaml_tool"] {
            let config = serde_json::json!({"engine": engine})
                .as_object()
                .cloned()
                .expect("config object");
            domain_solver("web_sast")
                .validate_config(&config, None)
                .unwrap_or_else(|error| panic!("non-empty engine is valid: {error}"));
        }

        // engine 空串与非字符串都拒绝（fail-fast）。
        for bad in [serde_json::json!(""), serde_json::json!(3)] {
            let config = serde_json::json!({"engine": bad})
                .as_object()
                .cloned()
                .expect("config object");
            let error = domain_solver("web_sast")
                .validate_config(&config, None)
                .expect_err("engine must be a non-empty string");
            assert!(error.to_string().contains("non-empty string"));
        }
    }

    // ------------------------------------------------------------------
    // 外部 Worker 路径（内部执行器已删除；solve 只经 worker 边界派发）
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn solve_without_worker_runtime_fails_explicitly() {
        // 组合根未注入 worker registry → 显式失败，绝不回退内部执行。
        let context = SolverContext::new(
            ProjectId::new("proj_no_worker".to_string()),
            RunId::new("run_no_worker".to_string()),
            TaskId::new("task_no_worker".to_string()),
        );
        let error = domain_solver("web_sast")
            .solve(context)
            .await
            .expect_err("must fail closed without a worker runtime");
        assert!(
            error
                .to_string()
                .contains("external worker runtime not configured"),
            "{error}"
        );
    }
}
