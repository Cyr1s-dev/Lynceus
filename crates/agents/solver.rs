//! Solver 契约 —— `server/core/agents/base_solver.py` 的移植。
//!
//! 关键不变量：**Solver 绝不直接触碰 `Repository` 或 `AuditGraph`。** 它收到
//! 只读上下文加白名单工具注册表，做完分析后返回描述其提议的
//! Facts/Evidence/Findings 与其产出的 `ToolInvocations` 的 `SolverResult`。
//! `AuditManager` 是把这些提交进核心状态的唯一写者。
//!
//! Python 侧 `SolverContext.tool_context()`（从 run config 派生
//! `ToolContext`）依赖尚未移植的 `core/tools` 子系统，留待工具层里程碑；
//! 当前所有 solver 均不经过该方法。

use std::collections::HashSet;
use std::sync::Arc;

use models::agent::ContextPack;
use models::common::StrMap;
use models::domain::AuditDomain;
use models::evidence::Evidence;
use models::fact::Fact;
use models::finding::Finding;
use models::hint::Hint;
use models::ids::BranchId;
use models::ids::MissionId;
use models::ids::ProjectId;
use models::ids::RunId;
use models::ids::TaskId;
use models::intent::Intent;
use models::project::Project;
use models::tool_invocation::ToolInvocation;
use serde_json::Map;
use serde_json::Value;

use crate::llm::ProviderRuntime;

/// 交给 solver 的一次任务的只读输入（Python `SolverContext`）。
///
/// 包含待处理的 intent、project target 描述符、相关 facts/hints、run
/// config 与步数预算。solver 可自由读取，但不得假定能持久化任何东西。
#[derive(Clone)]
pub struct SolverContext {
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Mission。
    pub mission_id: Option<MissionId>,
    /// 所属 Branch。
    pub branch_id: Option<BranchId>,
    /// 所属 Run。
    pub run_id: RunId,
    /// 关联 Task。
    pub task_id: TaskId,
    /// 待处理 Intent。
    pub intent: Option<Intent>,
    /// Project target 描述符。
    pub target: StrMap,
    /// 相关 Fact。
    pub facts: Vec<Fact>,
    /// 相关 Evidence。
    pub evidence: Vec<Evidence>,
    /// 相关 Finding。
    pub findings: Vec<Finding>,
    /// 相关 `ToolInvocation`。
    pub tool_invocations: Vec<ToolInvocation>,
    /// 相关 Hint。
    pub hints: Vec<Hint>,
    /// `AuditRun.config` 原文（键序 = 插入序）。
    pub config: Map<String, Value>,
    /// Provider id（纯工具扫描不需要 LLM，可为 `None`）。
    pub provider_id: Option<String>,
    /// Provider 运行时（可选）。
    pub provider_runtime: Option<Arc<dyn ProviderRuntime>>,
    /// 外部 Worker Runtime 选择器（可选；组合根注入。实际执行只经此
    /// 边界派发到外部 runtime）。
    pub worker_runtime: Option<Arc<dyn crate::worker::WorkerRuntimeSelector>>,
    /// Agent 预设来源（WP4：按 key 取启用预设；组合根注入仓储适配器）。
    pub agent_preset_source: Option<Arc<dyn models::agent_preset::AgentPresetSource>>,
    /// Coordinator 为当前 task 发放的 MCP grant；secret 仅驻留在内存，
    /// Solver 的 Debug 表示不展开该值。
    pub worker_mcp: Option<crate::worker::WorkerMcpBinding>,
    /// 上下文包（可选）。
    pub context_pack: Option<ContextPack>,
    /// 步数预算。
    pub budget_steps: i64,
}

impl std::fmt::Debug for SolverContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // ProviderRuntime 是运行时句柄而非领域数据，Debug 只呈现其存在性。
        let provider_runtime = if self.provider_runtime.is_some() {
            "Some(<ProviderRuntime>)"
        } else {
            "None"
        };
        let worker_runtime = if self.worker_runtime.is_some() {
            "Some(<WorkerRuntimeSelector>)"
        } else {
            "None"
        };
        let agent_preset_source = if self.agent_preset_source.is_some() {
            "Some(<AgentPresetSource>)"
        } else {
            "None"
        };
        let worker_mcp = if self.worker_mcp.is_some() {
            "Some(<WorkerMcpBinding>)"
        } else {
            "None"
        };
        f.debug_struct("SolverContext")
            .field("project_id", &self.project_id)
            .field("mission_id", &self.mission_id)
            .field("branch_id", &self.branch_id)
            .field("run_id", &self.run_id)
            .field("task_id", &self.task_id)
            .field("intent", &self.intent)
            .field("target", &self.target)
            .field("facts", &self.facts)
            .field("evidence", &self.evidence)
            .field("findings", &self.findings)
            .field("tool_invocations", &self.tool_invocations)
            .field("hints", &self.hints)
            .field("config", &self.config)
            .field("provider_id", &self.provider_id)
            .field("provider_runtime", &provider_runtime)
            .field("worker_runtime", &worker_runtime)
            .field("agent_preset_source", &agent_preset_source)
            .field("worker_mcp", &worker_mcp)
            .field("context_pack", &self.context_pack)
            .field("budget_steps", &self.budget_steps)
            .finish()
    }
}

impl SolverContext {
    /// 以必填标识构造，其余字段取 Python 默认值。
    #[must_use]
    pub fn new(project_id: ProjectId, run_id: RunId, task_id: TaskId) -> Self {
        Self {
            project_id,
            mission_id: None,
            branch_id: None,
            run_id,
            task_id,
            intent: None,
            target: StrMap::new(),
            facts: Vec::new(),
            evidence: Vec::new(),
            findings: Vec::new(),
            tool_invocations: Vec::new(),
            hints: Vec::new(),
            config: Map::new(),
            provider_id: None,
            provider_runtime: None,
            worker_runtime: None,
            agent_preset_source: None,
            worker_mcp: None,
            context_pack: None,
            budget_steps: 8,
        }
    }
}

/// solver 任务的结构化输出（Python `SolverResult`）——solver 返回的唯一东西。
///
/// Manager 校验并持久化这些内容。注意：提议对象携带真实 id（由 solver 生
/// 成），因此 evidence/finding 可以在被提交前交叉引用彼此及其来源 fact。
#[derive(Debug, Clone, Default)]
pub struct SolverResult {
    /// 提议的新 Fact。
    pub new_facts: Vec<Fact>,
    /// 提议的新 Evidence。
    pub new_evidence: Vec<Evidence>,
    /// 提议的新 Finding。
    pub new_findings: Vec<Finding>,
    /// 产出的 ToolInvocation（审计日志）。
    pub tool_invocations: Vec<ToolInvocation>,
    /// 产出的外部 Worker 会话记录（独立审计概念，非 `ToolInvocation`）。
    pub worker_runs: Vec<models::worker::WorkerRun>,
    /// 产出的外部 Worker 调用审计。
    pub worker_invocations: Vec<models::worker::WorkerInvocation>,
    /// solver 建议的后续 Intent（manager 决定是否保留）。
    pub proposed_intents: Vec<Intent>,
    /// 备注。
    pub notes: Option<String>,
}

/// solver 底层工具失败时抛出（Python `SolverExecutionError`）。
///
/// 携带失败前产出的 `ToolInvocation` 记录，manager 仍可持久化审计日志——
/// 即使任务失败，"我们跑过 semgrep 且它报错了"的审计记录不会丢。
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct SolverExecutionError {
    /// 错误消息。
    pub message: String,
    /// 失败前产出的工具调用记录。
    pub tool_invocations: Vec<ToolInvocation>,
    /// 失败前产出的外部 Worker 会话记录。
    pub worker_runs: Vec<models::worker::WorkerRun>,
    /// 失败前产出的外部 Worker 调用审计。
    pub worker_invocations: Vec<models::worker::WorkerInvocation>,
}

impl SolverExecutionError {
    /// 构造无工具记录的执行错误。
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            tool_invocations: Vec::new(),
            worker_runs: Vec::new(),
            worker_invocations: Vec::new(),
        }
    }

    /// 附加工具调用记录。
    #[must_use]
    pub fn with_tool_invocations(mut self, tool_invocations: Vec<ToolInvocation>) -> Self {
        self.tool_invocations = tool_invocations;
        self
    }

    /// 附加外部 Worker 会话记录。
    #[must_use]
    pub fn with_worker_runs(mut self, worker_runs: Vec<models::worker::WorkerRun>) -> Self {
        self.worker_runs = worker_runs;
        self
    }

    /// 附加外部 Worker 调用审计。
    #[must_use]
    pub fn with_worker_invocations(
        mut self,
        worker_invocations: Vec<models::worker::WorkerInvocation>,
    ) -> Self {
        self.worker_invocations = worker_invocations;
        self
    }
}

/// `solve` 的错误通道（Python 异常族的可判别对应）。
#[derive(Debug, thiserror::Error)]
pub enum SolverError {
    /// Python `SolverExecutionError`（含工具调用记录）。
    #[error(transparent)]
    Execution(#[from] SolverExecutionError),
    /// 其它任意异常（Python 泛 `Exception` 路径；消息已含异常类名）。
    #[error("{0}")]
    Other(String),
}

impl SolverError {
    /// Python `f"{type(exc).__name__}: {exc}"` 的镜像。
    #[must_use]
    pub fn task_error_label(&self) -> String {
        match self {
            SolverError::Execution(error) => format!("SolverExecutionError: {error}"),
            SolverError::Other(message) => message.clone(),
        }
    }
}

/// run config 切片校验失败（Python `SolverConfigError`）。
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct SolverConfigError(pub String);

/// solver 抽象契约（Python `BaseSolver`）。
///
/// 子类声明唯一 `name`（用于路由/注册）并收到白名单化的工具访问，只能
/// 调用沙箱工具——绝不任意 shell。
#[async_trait::async_trait]
pub trait BaseSolver: Send + Sync {
    /// 唯一 solver 名（路由/注册键）。
    fn name(&self) -> &str;

    /// 描述。
    fn description(&self) -> &'static str {
        ""
    }

    /// 声明覆盖的审计域。
    fn audit_domains(&self) -> HashSet<AuditDomain> {
        HashSet::new()
    }

    /// 校验与本 solver 相关的 run config 切片，非法时返回错误。
    ///
    /// manager 在创建任何 task **之前**调用，让配置问题以
    /// `SolverConfigError`（映射 HTTP 422 并标记 run FAILED）暴露，而不是
    /// 变成失败的 task。默认为空操作；接受 config 的 solver 覆写它以预校验
    /// 用户输入。`project` 供 solver 考虑 target 回退。
    ///
    /// # Errors
    ///
    /// 配置切片非法。
    fn validate_config(
        &self,
        _config: &Map<String, Value>,
        _project: Option<&Project>,
    ) -> Result<(), SolverConfigError> {
        Ok(())
    }

    /// 分析 intent/target 并返回结构化结果。
    ///
    /// # Errors
    ///
    /// 工具失败（[`SolverExecutionError`，含部分工具记录）或任意其它异常。
    async fn solve(&self, context: SolverContext) -> Result<SolverResult, SolverError>;
}

/// name → `BaseSolver` 查表，manager 用它路由任务（Python `SolverRegistry`）。
#[derive(Default)]
pub struct SolverRegistry {
    solvers: Vec<Arc<dyn BaseSolver>>,
}

impl SolverRegistry {
    /// 空注册表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册（或按名覆盖）一个 solver。
    pub fn register(&mut self, solver: Arc<dyn BaseSolver>) {
        let name = solver.name().to_string();
        if let Some(existing) = self
            .solvers
            .iter_mut()
            .find(|item| item.name() == name.as_str())
        {
            *existing = solver;
            return;
        }
        self.solvers.push(solver);
    }

    /// 按名取 solver；Python 在缺失时抛 `KeyError`，此处以 `None` 表达。
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn BaseSolver>> {
        self.solvers
            .iter()
            .find(|solver| solver.name() == name)
            .cloned()
    }

    /// 是否已注册该名。
    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.solvers.iter().any(|solver| solver.name() == name)
    }

    /// 全部已注册名（排序——Python `sorted(self._solvers)`）。
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.solvers.iter().map(|s| s.name().to_string()).collect();
        names.sort();
        names
    }

    /// 声明域与给定集合有交集的已注册 solver 名（注册序）。
    #[must_use]
    pub fn matching_audit_domains(&self, domains: &HashSet<AuditDomain>) -> Vec<String> {
        self.solvers
            .iter()
            .filter(|solver| {
                let declared = solver.audit_domains();
                !declared.is_empty() && declared.intersection(domains).next().is_some()
            })
            .map(|solver| solver.name().to_string())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoSolver {
        name: &'static str,
        domains: HashSet<AuditDomain>,
    }

    #[async_trait::async_trait]
    impl BaseSolver for EchoSolver {
        fn name(&self) -> &str {
            self.name
        }

        fn audit_domains(&self) -> HashSet<AuditDomain> {
            self.domains.clone()
        }

        async fn solve(&self, _context: SolverContext) -> Result<SolverResult, SolverError> {
            Ok(SolverResult::default())
        }
    }

    fn solver(name: &'static str, domains: &[AuditDomain]) -> Arc<dyn BaseSolver> {
        Arc::new(EchoSolver {
            name,
            domains: domains.iter().copied().collect(),
        })
    }

    #[test]
    fn registry_register_get_has_and_names() {
        let mut registry = SolverRegistry::new();
        registry.register(solver(
            "web_sast",
            &[AuditDomain::WebSast, AuditDomain::CodeDeepSast],
        ));
        registry.register(solver("web_recon", &[AuditDomain::WebRecon]));

        assert!(registry.has("web_sast"));
        assert!(registry.get("web_sast").is_some());
        assert!(!registry.has("nope"));
        assert!(registry.get("").is_none(), "Python KeyError 以 None 表达");
        // Python names() 排序返回。
        assert_eq!(registry.names(), ["web_recon", "web_sast"]);
    }

    #[test]
    fn registry_register_replaces_same_name() {
        let mut registry = SolverRegistry::new();
        registry.register(solver("web_sast", &[AuditDomain::WebSast]));
        registry.register(solver("web_sast", &[AuditDomain::WebRecon]));
        assert_eq!(registry.names().len(), 1);
        let domains = [AuditDomain::WebRecon].into();
        assert_eq!(registry.matching_audit_domains(&domains), ["web_sast"]);
    }

    #[test]
    fn registry_matching_audit_domains_filters_intersection() {
        let mut registry = SolverRegistry::new();
        registry.register(solver("web_sast", &[AuditDomain::WebSast]));
        registry.register(solver("web_recon", &[AuditDomain::WebRecon]));
        registry.register(solver("empty", &[]));

        let query: HashSet<AuditDomain> = [AuditDomain::WebSast, AuditDomain::BinaryStatic].into();
        assert_eq!(registry.matching_audit_domains(&query), ["web_sast"]);
    }

    #[test]
    fn solver_error_labels_mirror_python_repr() {
        let execution = SolverExecutionError::new("boom");
        assert_eq!(
            SolverError::Execution(execution).task_error_label(),
            "SolverExecutionError: boom"
        );
        assert_eq!(
            SolverError::Other("ValueError: bad".to_string()).task_error_label(),
            "ValueError: bad"
        );
    }
}
