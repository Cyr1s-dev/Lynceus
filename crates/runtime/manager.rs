//! `AuditManager` —— 编排一次审计并拥有审计图的全部写入
//! （`server/core/engine/manager.py::AuditManager`）。
//!
//! 单一写入路径：Solver/Observer 的一切产出都**在此校验并持久化**，
//! solver 绝不直接触碰仓储。7269 行 Python 原文按领域内聚拆分为多个
//! 模块（`events` / `mission_lifecycle` / `mission_runtime` /
//! `branch_runtime` / `closure` / `status`），本文件只承载结构体定义、
//! 构造布线与并发原语。

// Several collaborators are wired now and consumed by later milestones;
// keeping them on the manager preserves the migration's dependency graph.
#![allow(clippy::redundant_closure_for_method_calls)]
#![allow(dead_code)]
//!
//! 并发模型与 Python 的映射：
//! - `asyncio.Lock`（run 变更串行化）→ [`tokio::sync::Mutex`]；
//! - 每 Mission 一把 `asyncio.Lock`（`_mission_start_locks`）→ 外层
//!   tokio Mutex 守护 `mission_id → Arc<Mutex<()>>` 映射，临界区只锁
//!   内层克隆；
//! - `_mission_runtime_tokens` 的 `object()` 身份比较 → [`std::sync::Arc`]
//!   指针相等（`Arc::ptr_eq`）；
//! - `set_provider_runtime` / `set_tool_availability_resolver` 是同步
//!   `&self` 方法且只做值替换 → [`std::sync::RwLock`]（不进异步运行时）。
//!
//! 未移植的 Python 协作组件不预留字段或占位接口；真实需求接入时再按
//! Rust 模块边界实现，避免把 Python 的开放扩展面原样搬入核心管理器。

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::RwLock;

use agents::branch_generator::BranchGenerator;
use agents::capability_router::CapabilityRouter;
use agents::context::ContextCompressor;
use agents::coverage::CoverageChecker;
use agents::critique::CritiqueAgent;
use agents::escalation::EscalationGuard;
use agents::exit_gate::MetacogExitGate;
use agents::llm::{ProviderModelDiscoveryRuntime, ProviderRuntime};
use agents::metacognition::MetacognitionAgent;
use agents::observer::Observer;
use agents::reflector::Reflector;
use agents::solver::SolverRegistry;
use agents::strategy_board::StrategyBoardMaintainerService;
use agents::termination::TerminationEvaluator;
use agents::trajectory::TrajectorySummarizer;
use agents::worker::WorkerRuntimeSelector;
use engines::broker::mcp::McpServerState;
use models::ids::MissionId;
use models::ids::RunId;
use models::retrieval::ArtifactRecord;
use models::tool_invocation::ToolInvocation;
use storage::Repository;
use storage::SwarmOperationJournal;

use tokio::sync::MutexGuard;

use crate::notifications::NotificationHub;
use crate::task_backend::TaskBackend;
use crate::task_backend::TaskHandle;

/// 工具可用性解析器（Python `Callable[[], set[str]]`）：返回当前环境
/// 实际就绪的 CLI 工具名集合，用于阻止"不可能完成"的派遣。
pub type ToolAvailabilityResolver = Box<dyn Fn() -> HashSet<String> + Send + Sync>;

/// 一次 Mission runtime 提交的代际令牌（Python `object()` 身份语义）。
///
/// 指针相等（[`Arc::ptr_eq`]）即同一代 runtime：旧的提交在锁竞争下
/// 落盘状态时，以令牌比对防止覆盖新一代 runtime 的写入。
pub type RuntimeToken = Arc<()>;

/// 审计编排器（Python `AuditManager`）。
///
/// 拥有 Mission/Run/Branch/Task 的全部状态迁移与事件记录；被spawn 的
/// runtime 任务持有 `Arc<AuditManager>`，因此本结构体的所有字段在
/// `&self` 上可用。
pub struct AuditManager {
    repo: Arc<dyn Repository>,
    solvers: SolverRegistry,
    tasks: Arc<dyn TaskBackend>,
    /// NOTIFY 平面的进程内扇出器（Python Runtime.notifications）。
    pub(crate) hub: Arc<NotificationHub>,
    pub(crate) observer: Observer,
    provider_runtime: RwLock<Option<Arc<dyn ProviderRuntime>>>,
    provider_discovery_runtime: RwLock<Option<Arc<dyn ProviderModelDiscoveryRuntime>>>,
    /// 外部 Worker Runtime 选择器（组合根注入；实际执行只经此边界）。
    worker_runtime: RwLock<Option<Arc<dyn WorkerRuntimeSelector>>>,
    /// 公共 MCP broker（Coordinator 发放 WorkerGrant 的唯一来源）。
    worker_mcp: RwLock<Option<Arc<McpServerState>>>,
    operation_journal: Arc<SwarmOperationJournal>,
    pub(crate) context_compressor: ContextCompressor,
    pub(crate) reflector: Reflector,
    pub(crate) termination_evaluator: TerminationEvaluator,
    pub(crate) strategy_board: StrategyBoardMaintainerService,
    pub(crate) branch_generator: BranchGenerator,
    pub(crate) capability_router: CapabilityRouter,
    pub(crate) critique_agent: CritiqueAgent,
    pub(crate) coverage_checker: CoverageChecker,
    pub(crate) metacognition_agent: MetacognitionAgent,
    pub(crate) metacog_exit_gate: MetacogExitGate,
    pub(crate) escalation_guard: EscalationGuard,
    pub(crate) trajectory_summarizer: TrajectorySummarizer,
    run_mutation_lock: tokio::sync::Mutex<()>,
    mission_start_locks: tokio::sync::Mutex<HashMap<MissionId, Arc<tokio::sync::Mutex<()>>>>,
    mission_runtime_handles: tokio::sync::Mutex<HashMap<RunId, TaskHandle>>,
    mission_runtime_tokens: tokio::sync::Mutex<HashMap<RunId, RuntimeToken>>,
    /// 每个 run 的全局 worker 并发池（跨 branch 共享）：整个 mission 同时在跑的
    /// 外部 worker 尝试数上限。`run_branch_runtime` 入口创建、退出时由
    /// `WorkerPoolGuard` 摘除；`dispatch_intent` 每派一个 attempt 先取一个许可，
    /// 取不到就在池外 FIFO 排队——待领队列为空时根本没人来取，也就不会产生
    /// 空转的 LLM 调用。
    ///
    /// 用 `std::sync::Mutex` 而非 `tokio::sync::Mutex`：临界区只有一次
    /// HashMap 插/删且绝不跨 await；更关键的是 `WorkerPoolGuard::drop` 要在
    /// async 上下文里同步摘除，tokio 的 `blocking_lock` 在那里会直接 panic。
    pub(crate) worker_pools: std::sync::Mutex<HashMap<RunId, Arc<tokio::sync::Semaphore>>>,
    tool_availability_resolver: RwLock<Option<ToolAvailabilityResolver>>,
}

impl AuditManager {
    /// 构造编排器。
    ///
    /// Python 侧 `observer` / `reporter` / `decision_policy` /
    /// `execution_backend` / `worker_providers` 均为可注入可选组件：其中
    /// Observer 的 Rust 移植是确定性单元结构体（无需注入），其余未移植
    /// 的组件不在构造器中预留空位。
    #[must_use]
    pub fn new(
        repository: Arc<dyn Repository>,
        solvers: SolverRegistry,
        task_backend: Arc<dyn TaskBackend>,
    ) -> Self {
        Self::with_options(
            repository,
            solvers,
            task_backend,
            None,
            Arc::new(SwarmOperationJournal::default()),
        )
    }

    /// 全参数构造（Python 关键字参数 `provider_runtime` /
    /// `operation_journal` 的镜像）。
    #[must_use]
    pub fn with_options(
        repository: Arc<dyn Repository>,
        solvers: SolverRegistry,
        task_backend: Arc<dyn TaskBackend>,
        provider_runtime: Option<Arc<dyn ProviderRuntime>>,
        operation_journal: Arc<SwarmOperationJournal>,
    ) -> Self {
        let context_compressor = ContextCompressor::with_operation_journal(
            Arc::clone(&repository),
            Arc::clone(&operation_journal),
        );
        Self {
            repo: repository,
            solvers,
            tasks: task_backend,
            observer: Observer,
            provider_runtime: RwLock::new(provider_runtime),
            provider_discovery_runtime: RwLock::new(None),
            worker_runtime: RwLock::new(None),
            worker_mcp: RwLock::new(None),
            operation_journal,
            context_compressor,
            reflector: Reflector,
            termination_evaluator: TerminationEvaluator,
            strategy_board: StrategyBoardMaintainerService::new(
                agents::strategy_board::StrategyBoardKnowledgeRetriever::new(),
            ),
            branch_generator: BranchGenerator,
            capability_router: CapabilityRouter,
            critique_agent: CritiqueAgent,
            coverage_checker: CoverageChecker,
            metacognition_agent: MetacognitionAgent::new(),
            metacog_exit_gate: MetacogExitGate,
            escalation_guard: EscalationGuard::new(),
            trajectory_summarizer: TrajectorySummarizer::new(),
            run_mutation_lock: tokio::sync::Mutex::new(()),
            mission_start_locks: tokio::sync::Mutex::new(HashMap::new()),
            mission_runtime_handles: tokio::sync::Mutex::new(HashMap::new()),
            mission_runtime_tokens: tokio::sync::Mutex::new(HashMap::new()),
            worker_pools: std::sync::Mutex::new(HashMap::new()),
            tool_availability_resolver: RwLock::new(None),
            hub: Arc::new(NotificationHub::new()),
        }
    }

    /// 挂接 provider 运行时（未来 LLM-aware solver 与 META 的 LLM 路径用）。
    ///
    /// Python 侧同时重建 `_agent_planner`；该组件未移植（见模块文档），
    /// 此处仅替换运行时引用。
    pub fn set_provider_runtime(&self, provider_runtime: Option<Arc<dyn ProviderRuntime>>) {
        // 中毒恢复：临界区只有值替换，panic 也只可能留下"旧值或新值"，
        // `into_inner` 直接取回数据继续服务。
        let mut guard = self
            .provider_runtime
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = provider_runtime;
    }

    /// 挂接外部 Worker Runtime 选择器（组合根注入）。
    ///
    /// 注入后，DomainSolver 的实际执行经外部 runtime 派发；未注入时保持
    /// 迁移过渡期的内部基线路径（删除阶段移除）。
    pub fn set_worker_runtime(&self, selector: Option<Arc<dyn WorkerRuntimeSelector>>) {
        let mut guard = self
            .worker_runtime
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = selector;
    }

    /// 当前注入的 worker 选择器快照。
    #[must_use]
    pub fn worker_runtime(&self) -> Option<Arc<dyn WorkerRuntimeSelector>> {
        self.worker_runtime
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 挂接公共 MCP broker；WorkerGrant 只能由此组合根提供。
    pub fn set_worker_mcp(&self, mcp: Option<Arc<McpServerState>>) {
        let mut guard = self
            .worker_mcp
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = mcp;
    }

    /// 当前 WorkerGrant 发放器快照。
    #[must_use]
    pub fn worker_mcp(&self) -> Option<Arc<McpServerState>> {
        self.worker_mcp
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 挂接模型发现运行时（可选；通常与完整 provider 网关共享实现）。
    pub fn set_provider_discovery_runtime(
        &self,
        runtime: Option<Arc<dyn ProviderModelDiscoveryRuntime>>,
    ) {
        let mut guard = self
            .provider_discovery_runtime
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = runtime;
    }

    /// 读取模型发现运行时（未配置时返回 `None`）。
    #[must_use]
    pub fn provider_discovery_runtime(&self) -> Option<Arc<dyn ProviderModelDiscoveryRuntime>> {
        self.provider_discovery_runtime
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 读取当前 provider 运行时（若无返回 `None`）。
    #[must_use]
    pub fn provider_runtime(&self) -> Option<Arc<dyn ProviderRuntime>> {
        self.provider_runtime
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 挂接工具可用性解析器（Python `set_tool_availability_resolver`）。
    pub fn set_tool_availability_resolver(&self, resolver: Option<ToolAvailabilityResolver>) {
        let mut guard = self
            .tool_availability_resolver
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = resolver;
    }

    /// 当前环境就绪的 CLI 工具名集合（Python `_available_tool_names` 的
    /// 无 resolver 分支返回 `None`）。
    #[must_use]
    pub fn available_tool_names(&self) -> Option<HashSet<String>> {
        let guard = self
            .tool_availability_resolver
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let resolver = guard.as_ref()?;
        Some(
            resolver()
                .into_iter()
                .map(|name| name.trim().to_lowercase())
                .filter(|name| !name.is_empty())
                .collect(),
        )
    }

    /// 仓储句柄。
    #[must_use]
    pub fn repository(&self) -> &Arc<dyn Repository> {
        &self.repo
    }

    /// MCP 公共工具的唯一审计写入口：先追加 ToolInvocation，再追加其
    /// sealed ArtifactRecord 元数据。MCP 层不直接写仓储，也不创建 Evidence。
    pub fn persist_mcp_audit(
        &self,
        invocation: ToolInvocation,
        artifacts: Vec<ArtifactRecord>,
    ) -> Result<(), crate::errors::EngineError> {
        self.repo.add_tool_invocation(&invocation)?;
        for artifact in artifacts {
            self.repo.add_artifact_record(&artifact)?;
        }
        Ok(())
    }

    /// solver 注册表。
    #[must_use]
    pub fn solvers(&self) -> &SolverRegistry {
        &self.solvers
    }

    /// 任务后端。
    #[must_use]
    pub fn task_backend(&self) -> &Arc<dyn TaskBackend> {
        &self.tasks
    }

    /// 操作日志构件。
    #[must_use]
    pub fn operation_journal(&self) -> &Arc<SwarmOperationJournal> {
        &self.operation_journal
    }

    /// 取（或惰性创建）某个 Mission 的启动锁（Python
    /// `_mission_start_locks.setdefault(mission_id, asyncio.Lock())`）。
    ///
    /// 返回内层锁的 `Arc` 克隆后再 await——外层映射锁只在查表瞬间持有，
    /// 不跨越临界区，与 Python 语义一致。
    #[must_use]
    pub async fn mission_start_lock(&self, mission_id: &MissionId) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.mission_start_locks.lock().await;
        locks
            .entry(mission_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// 注册一代 runtime 令牌（Python `self._mission_runtime_tokens[run_id]
    /// = runtime_token`）。
    pub async fn register_runtime_token(&self, run_id: &RunId, token: RuntimeToken) {
        let mut tokens = self.mission_runtime_tokens.lock().await;
        tokens.insert(run_id.clone(), token);
    }

    /// 令牌是否仍是该 run 当前的代（Python `self._mission_runtime_tokens
    /// .get(run_id) is runtime_token`）。
    #[must_use]
    pub async fn is_current_runtime_token(&self, run_id: &RunId, token: &RuntimeToken) -> bool {
        let tokens = self.mission_runtime_tokens.lock().await;
        tokens
            .get(run_id)
            .is_some_and(|current| Arc::ptr_eq(current, token))
    }

    /// 若令牌仍是当前代则登记 runtime 句柄（Python `_submit_mission_runtime`
    /// 提交后的条件写入）。
    pub async fn register_runtime_handle_if_current(
        &self,
        run_id: &RunId,
        token: &RuntimeToken,
        handle: TaskHandle,
    ) {
        if !self.is_current_runtime_token(run_id, token).await {
            return;
        }
        let mut handles = self.mission_runtime_handles.lock().await;
        handles.insert(run_id.clone(), handle);
    }

    /// 若令牌仍是当前代则清理令牌与句柄（Python `_run_mission_runtime_safely`
    /// 的 `finally` 分支）。
    pub async fn clear_runtime_if_current(&self, run_id: &RunId, token: &RuntimeToken) {
        if !self.is_current_runtime_token(run_id, token).await {
            return;
        }
        let mut tokens = self.mission_runtime_tokens.lock().await;
        tokens.remove(run_id);
        drop(tokens);
        let mut handles = self.mission_runtime_handles.lock().await;
        handles.remove(run_id);
    }

    /// run 当前是否存在已登记的 runtime 句柄（Python `run.id not in
    /// self._mission_runtime_handles`）。
    #[must_use]
    pub async fn has_runtime_handle(&self, run_id: &RunId) -> bool {
        let handles = self.mission_runtime_handles.lock().await;
        handles.contains_key(run_id)
    }

    /// 摘除并返回 run 的 runtime 句柄（Python `self._mission_runtime_handles
    /// .pop(run.id, None)`）。
    #[must_use]
    pub async fn take_runtime_handle(&self, run_id: &RunId) -> Option<TaskHandle> {
        let mut handles = self.mission_runtime_handles.lock().await;
        handles.remove(run_id)
    }

    /// 摘除 run 的 runtime 令牌（Python `self._mission_runtime_tokens.pop(
    /// run.id, None)`，`pause_mission` 用）。
    pub async fn remove_runtime_token(&self, run_id: &RunId) {
        let mut tokens = self.mission_runtime_tokens.lock().await;
        tokens.remove(run_id);
    }

    /// run 变更串行化锁的守卫（Python `async with self._run_mutation_lock`）。
    pub(crate) async fn run_mutation_guard(&self) -> MutexGuard<'_, ()> {
        self.run_mutation_lock.lock().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager() -> AuditManager {
        // 内存测试遵循 agents crate 惯例：tempfile 上的 SqliteRepository。
        let dir = tempfile::tempdir().expect("临时目录必须可创建");
        let repo =
            storage::SqliteRepository::open(dir.path().join("mgr.sqlite3")).expect("库必须可打开");
        // 目录句柄泄漏到测试进程生命周期，与 agents crate 测试同做法。
        std::mem::forget(dir);
        AuditManager::new(
            Arc::new(repo),
            SolverRegistry::new(),
            Arc::new(crate::task_backend::InMemoryTaskBackend::default()),
        )
    }

    #[tokio::test]
    async fn mission_start_lock_is_stable_per_mission() {
        let manager = manager();
        let mission = MissionId::new("mission_1".to_string());
        let first = manager.mission_start_lock(&mission).await;
        let second = manager.mission_start_lock(&mission).await;
        // 同一 Mission 复用同一把锁。
        assert!(Arc::ptr_eq(&first, &second));

        let other = MissionId::new("mission_2".to_string());
        let third = manager.mission_start_lock(&other).await;
        assert!(!Arc::ptr_eq(&first, &third));
    }

    #[tokio::test]
    async fn runtime_token_identity_semantics() {
        let manager = manager();
        let run = RunId::new("run_1".to_string());

        let token = RuntimeToken::new(());
        manager
            .register_runtime_token(&run, Arc::clone(&token))
            .await;
        assert!(manager.is_current_runtime_token(&run, &token).await);

        // 新一代令牌取代旧代。
        let new_token = RuntimeToken::new(());
        manager
            .register_runtime_token(&run, Arc::clone(&new_token))
            .await;
        assert!(!manager.is_current_runtime_token(&run, &token).await);
        assert!(manager.is_current_runtime_token(&run, &new_token).await);

        manager.clear_runtime_if_current(&run, &new_token).await;
        assert!(!manager.is_current_runtime_token(&run, &new_token).await);
    }

    #[test]
    fn tool_availability_resolver_round_trip() {
        let manager = manager();
        assert!(manager.available_tool_names().is_none());

        manager.set_tool_availability_resolver(Some(Box::new(|| {
            ["nuclei".to_string(), " Semgrep ".to_string()]
                .into_iter()
                .collect()
        })));
        let names = manager.available_tool_names().expect("resolver 已挂接");
        assert!(names.contains("nuclei"));
        assert!(names.contains("semgrep"), "工具名做 strip+lowercase 归一化");

        manager.set_tool_availability_resolver(None);
        assert!(manager.available_tool_names().is_none());
    }
}
