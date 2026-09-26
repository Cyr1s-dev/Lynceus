//! Repository 协议 —— `server/core/storage/repository.py` 的阶段 1 移植。
//!
//! 覆盖核心对象链的 7 个实体（`Mission` / `Branch` / `AuditRun` /
//! `AgentTask` / `ToolInvocation` / `Evidence` / `Finding`）；其余 39 个实体的方法在对应模型
//! 移植后补齐（`commit_solver_result` 依赖 Intent / Fact / AuditEvent，同批）。
//!
//! 与 Python 协议的两处**有意**差异（记录于 `docs/REFACTOR_PROGRESS.md`）：
//!
//! 1. 方法为同步而非 async：rusqlite 是同步驱动，SQLite 本身微秒级；
//!    异步边界属于引擎层（tokio `spawn_blocking`），在库层包 async 只会
//!    制造"async 但阻塞"的假象。Python 侧 async 只是让 SQL/网络后端能
//!    满足同一接口的调用点兼容手段。
//! 2. 实参按引用传入、返回克隆：Python 按值传入按值返回，Rust 侧等价
//!    语义下避免无谓的移动。
//!
//! 并发契约与 Python 一致：实现必须在多线程并发调用下安全（本 crate 的
//! [`crate::SqliteRepository`](crate::sqlite::SqliteRepository) 用互斥锁
//! 串行化，对应 Python 的 `threading.RLock`）。

use serde::Serialize;
use serde::de::DeserializeOwned;

use models::{
    AgentNarrativeEvent, AgentTask, ArtifactRecord, AuditEvent, AuditRun, BlackboardEntry,
    BlackboardEntryKind, Branch, ContextCompressionReport, ContextPack, CoverageAssessment,
    CritiqueReport, DecisionAnswer, DecisionGate, EscalationGuardVerdict, Evidence, ExecutionJob,
    ExecutionStatus, ExitGateDecision, Fact, Finding, Hint, IntelEntity, IntelEntityKind,
    IntelEntityRecord, IntelEntityStatus, IntelIngestBatch, IntelIngestOutcome, IntelRawRecord,
    IntelRelationRecord, Intent, KnowledgeCard, KnowledgeCorpusStatus, KnowledgeRetrievalQuery,
    KnowledgeRetrievalResult, MetacognitionAssessment, Mission, MissionAsset,
    MissionAssetSensitivity, MissionAssetType, ModelCapability, ModelInvocation, ModuleConfig,
    Observation, Project, ProjectId, ProviderConfig, ProviderRouteBinding, ReflectorReport,
    RetrievalInvocation, RunId, RuntimeSetting, StrategyBoardSnapshot,
    TerminationAssessment, ToolInvocation, TrajectorySummary, UserDirective,
    WorkerInvocation, WorkerLease, WorkerProfile, WorkerRun, WorkerRuntimeProfile,
    WorkerRuntimeType,
};
use models::FindingRetest;

use crate::error::StorageError;

/// 实体的物理持久化布局。
///
/// Python 侧用 `_TABLES` / `_RUN_SCOPED` 集合 + `_insert` 的列拼接表达
/// 同一信息；Rust 侧把它变成每个实体的关联常量与访问器，使布局错误
/// （表名拼错、scope 列误配）在编译期不可能。
pub trait Storable: Serialize + DeserializeOwned + Sized {
    /// 表名（静态常量，插值进 SQL 的是编译期已知标识符，无注入面）。
    const TABLE: &'static str;
    /// 表是否带 `run_id` scope 列（Python `_RUN_SCOPED` 成员）。
    const RUN_SCOPED: bool;
    /// 实体 ID（`id` 列 + `id` 字段）。
    fn entity_id(&self) -> &str;
    /// `project_id` scope 列取值。
    fn scope_project_id(&self) -> Option<&str>;
    /// `run_id` scope 列取值（仅 [`Self::RUN_SCOPED`] 表使用）。
    fn scope_run_id(&self) -> Option<&str>;
    /// `created_at` 列取值：`created_at`（缺省回退 `started_at`）的
    /// `isoformat()`，即 Python `_created_at` 辅助函数。
    fn created_at_column(&self) -> Option<String>;
}

macro_rules! impl_storable {
    (
        $type:ty, $table:literal, run_scoped = $run_scoped:literal,
        $value:ident => {
            id: $id:expr,
            project: $project:expr,
            run: $run:expr,
            created_at: $created_at:expr $(,)?
        }
    ) => {
        impl Storable for $type {
            const TABLE: &'static str = $table;
            const RUN_SCOPED: bool = $run_scoped;

            fn entity_id(&self) -> &str {
                let $value = self;
                let _ = $value;
                $id
            }

            fn scope_project_id(&self) -> Option<&str> {
                let $value = self;
                let _ = $value;
                $project
            }

            fn scope_run_id(&self) -> Option<&str> {
                let $value = self;
                let _ = $value;
                $run
            }

            fn created_at_column(&self) -> Option<String> {
                let $value = self;
                let _ = $value;
                $created_at
            }
        }
    };
}

macro_rules! storable_project {
    ($type:ty, $table:literal) => {
        impl_storable!($type, $table, run_scoped = false, value => {
            id: value.id.as_str(),
            project: Some(value.project_id.as_str()),
            run: None,
            created_at: Some(value.created_at.isoformat()),
        });
    };
}

macro_rules! storable_run {
    ($type:ty, $table:literal) => {
        impl_storable!($type, $table, run_scoped = true, value => {
            id: value.id.as_str(),
            project: Some(value.project_id.as_str()),
            run: Some(value.run_id.as_str()),
            created_at: Some(value.created_at.isoformat()),
        });
    };
}

macro_rules! storable_global {
    ($type:ty, $table:literal) => {
        impl_storable!($type, $table, run_scoped = false, value => {
            id: value.id.as_str(),
            project: None,
            run: None,
            created_at: Some(value.created_at.isoformat()),
        });
    };
}

storable_project!(Mission, "missions");
impl_storable!(Branch, "branches", run_scoped = true, value => {
    id: value.id.as_str(),
    project: Some(value.project_id.as_str()),
    run: value.run_id.as_ref().map(RunId::as_str),
    created_at: Some(value.created_at.isoformat()),
});
storable_project!(AuditRun, "audit_runs");
storable_run!(AgentTask, "agent_tasks");
impl_storable!(ToolInvocation, "tool_invocations", run_scoped = false, value => {
    id: value.id.as_str(),
    project: value.project_id.as_ref().map(ProjectId::as_str),
    run: None,
    created_at: Some(value.started_at.isoformat()),
});
impl_storable!(ExecutionJob, "execution_jobs", run_scoped = true, value => {
    id: value.id.as_str(),
    project: value.request.project_id.as_ref().map(ProjectId::as_str),
    run: value.request.run_id.as_ref().map(RunId::as_str),
    created_at: Some(value.submitted_at.isoformat()),
});
storable_project!(Evidence, "evidence");
storable_project!(Finding, "findings");
storable_project!(FindingRetest, "finding_retests");
storable_global!(ProviderConfig, "providers");
storable_global!(ProviderRouteBinding, "provider_routes");
impl_storable!(ModelInvocation, "model_invocations", run_scoped = false, value => {
    id: value.id.as_str(),
    project: value.project_id.as_ref().map(ProjectId::as_str),
    run: None,
    created_at: Some(value.started_at.isoformat()),
});
storable_global!(ModelCapability, "model_capabilities");
impl_storable!(Project, "projects", run_scoped = false, value => {
    id: value.id.as_str(),
    project: Some(value.id.as_str()),
    run: None,
    created_at: Some(value.created_at.isoformat()),
});
storable_project!(Fact, "facts");
storable_project!(Intent, "intents");
storable_global!(ModuleConfig, "modules");
impl_storable!(AuditEvent, "audit_events", run_scoped = true, value => {
    id: &value.id,
    project: Some(value.project_id.as_str()),
    run: value.run_id.as_ref().map(RunId::as_str),
    created_at: Some(value.created_at.isoformat()),
});
storable_run!(Observation, "observations");
storable_run!(CoverageAssessment, "coverage_assessments");
storable_run!(MetacognitionAssessment, "metacognition_assessments");
storable_run!(ExitGateDecision, "exit_gate_decisions");
storable_run!(EscalationGuardVerdict, "escalation_guard_verdicts");
impl_storable!(CritiqueReport, "critique_reports", run_scoped = true, value => {
    id: value.id.as_str(),
    project: Some(value.project_id.as_str()),
    run: value.run_id.as_ref().map(RunId::as_str),
    created_at: Some(value.created_at.isoformat()),
});
storable_run!(TrajectorySummary, "trajectory_summaries");
impl_storable!(AgentNarrativeEvent, "agent_narrative_events", run_scoped = true, value => {
    id: value.id.as_str(),
    project: Some(value.project_id.as_str()),
    run: value.audit_run_id.as_ref().map(RunId::as_str),
    created_at: Some(value.created_at.isoformat()),
});
impl_storable!(StrategyBoardSnapshot, "strategy_board_snapshots", run_scoped = true, value => {
    id: value.id.as_str(),
    project: Some(value.project_id.as_str()),
    run: value.run_id.as_ref().map(RunId::as_str),
    created_at: Some(value.created_at.isoformat()),
});
storable_project!(Hint, "hints");
storable_global!(KnowledgeCard, "knowledge_cards");
storable_run!(TerminationAssessment, "termination_assessments");
storable_run!(WorkerLease, "worker_leases");
storable_global!(WorkerProfile, "worker_profiles");
impl_storable!(DecisionGate, "decision_gates", run_scoped = true, value => {
    id: value.id.as_str(),
    project: Some(value.project_id.as_str()),
    run: Some(value.audit_run_id.as_str()),
    created_at: Some(value.created_at.isoformat()),
});
storable_run!(ContextPack, "context_packs");
storable_run!(ContextCompressionReport, "context_compression_reports");
impl_storable!(RetrievalInvocation, "retrieval_invocations", run_scoped = true, value => {
    id: value.id.as_str(),
    project: Some(value.project_id.as_str()),
    run: value.run_id.as_ref().map(RunId::as_str),
    created_at: Some(value.created_at.isoformat()),
});
impl_storable!(RuntimeSetting, "runtime_settings", run_scoped = true, value => {
    id: value.id.as_str(),
    project: value.project_id.as_ref().map(ProjectId::as_str),
    run: value.run_id.as_ref().map(RunId::as_str),
    created_at: Some(value.created_at.isoformat()),
});
impl_storable!(ArtifactRecord, "artifact_records", run_scoped = true, value => {
    id: value.id.as_str(),
    project: value.project_id.as_ref().map(ProjectId::as_str),
    run: value.run_id.as_ref().map(RunId::as_str),
    created_at: Some(value.created_at.isoformat()),
});
impl_storable!(UserDirective, "user_directives", run_scoped = true, value => {
    id: value.id.as_str(),
    project: Some(value.project_id.as_str()),
    run: value.run_id.as_ref().map(RunId::as_str),
    created_at: Some(value.created_at.isoformat()),
});
storable_run!(ReflectorReport, "reflector_reports");
/// 核心持久化协议（`server/core/storage/repository.py` 阶段 1 子集）。
///
/// 方法名、参数语义、返回语义与 Python 协议逐一对应；差异仅在于错误以
/// [`StorageError`] 返回而非抛出、入参按引用。返回的永远是领域模型，
/// 绝不泄漏 DB 行。
///
/// # Errors
///
/// 全部方法统一返回 [`StorageError`]：IO / SQL 失败、约束冲突
/// （重复 id）、payload 解码失败、非法参数（如负步数增量）。
///
/// `Send + Sync` 超 trait：LLM 网关与编排引擎持有
/// `Arc<dyn Repository>` 跨 await 点，trait object 必须线程安全
/// （`SqliteRepository` 内部 `Mutex<Connection>` 天然满足）。
#[allow(clippy::missing_errors_doc)]
pub trait Repository: Send + Sync {
    /// 新建 Mission（重复 id 报错，不覆盖）。
    fn create_mission(&self, mission: &Mission) -> Result<Mission, StorageError>;
    /// 按 id 取 Mission，不存在返回 `None`。
    fn get_mission(&self, mission_id: &str) -> Result<Option<Mission>, StorageError>;
    /// 列 Mission；`project_id` 为 `None` 时列全部（按 `seq` 升序）。
    fn list_missions(&self, project_id: Option<&str>) -> Result<Vec<Mission>, StorageError>;
    /// 更新 Mission（不存在则插入，upsert 语义）。
    fn update_mission(&self, mission: &Mission) -> Result<Mission, StorageError>;
    /// 删除 Mission 及其 `mission_assets` 行（单事务）。
    fn delete_mission(&self, mission_id: &str) -> Result<(), StorageError>;

    /// 新建 Branch。
    fn create_branch(&self, branch: &Branch) -> Result<Branch, StorageError>;
    /// 按 id 取 Branch。
    fn get_branch(&self, branch_id: &str) -> Result<Option<Branch>, StorageError>;
    /// 列 Branch：`project_id` / `run_id` 走 SQL 列过滤，`mission_id` 是
    /// payload 字段，按 Python `_list_filtered` 语义在查询后内存过滤。
    fn list_branches(
        &self,
        project_id: Option<&str>,
        mission_id: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<Vec<Branch>, StorageError>;
    /// 更新 Branch（upsert）。
    fn update_branch(&self, branch: &Branch) -> Result<Branch, StorageError>;

    /// 新建 Run。
    fn create_run(&self, run: &AuditRun) -> Result<AuditRun, StorageError>;
    /// 按 id 取 Run。
    fn get_run(&self, run_id: &str) -> Result<Option<AuditRun>, StorageError>;
    /// 列某 Project 的全部 Run。
    fn list_runs(&self, project_id: &str) -> Result<Vec<AuditRun>, StorageError>;
    /// 更新 Run（upsert）。
    fn update_run(&self, run: &AuditRun) -> Result<AuditRun, StorageError>;
    /// 原子地把 `delta` 加到 Run 的步数计数器上（负数报错；不存在返回 `None`）。
    fn increment_run_steps(
        &self,
        run_id: &str,
        delta: i64,
    ) -> Result<Option<AuditRun>, StorageError>;
    /// 在执行前原子预留 Run 步数；不足时返回 `false`，不会超预算。
    fn reserve_run_steps(&self, run_id: &str, amount: i64) -> Result<bool, StorageError>;
    /// 原子地追加 task id（已存在则不重复；不存在返回 `None`）。
    fn append_run_task(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<Option<AuditRun>, StorageError>;

    /// 新建 Task。
    fn create_task(&self, task: &AgentTask) -> Result<AgentTask, StorageError>;
    /// 按 id 取 Task。
    fn get_task(&self, task_id: &str) -> Result<Option<AgentTask>, StorageError>;
    /// 列某 Run 的全部 Task。
    fn list_tasks(&self, run_id: &str) -> Result<Vec<AgentTask>, StorageError>;
    /// 更新 Task（upsert）。
    fn update_task(&self, task: &AgentTask) -> Result<AgentTask, StorageError>;
    /// 单事务原子提交一次 solver 结果产出的全部图谱/审计记录
    /// （Python `commit_solver_result`）：`tool_invocations` / `facts` /
    /// `evidence` / `findings` / `proposed_intents` 依次插入 → `task` 与
    /// `intent` upsert → `events` 插入；任一步失败整体回滚，约束冲突映射
    /// [`StorageError::SolverCommitConflict`]。
    // The solver commit is intentionally one atomic boundary; grouping these
    // slices into a second command object would obscure the Python wire
    // contract and make call sites less explicit.
    #[allow(clippy::too_many_arguments)]
    fn commit_solver_result(
        &self,
        task: &AgentTask,
        intent: &Intent,
        tool_invocations: &[ToolInvocation],
        facts: &[Fact],
        evidence: &[Evidence],
        findings: &[Finding],
        proposed_intents: &[Intent],
        events: &[AuditEvent],
    ) -> Result<(), StorageError>;

    /// 新建持久执行任务（重复 ID 报错，不覆盖）。
    fn create_execution_job(&self, job: &ExecutionJob) -> Result<ExecutionJob, StorageError>;
    /// 更新既有执行任务；不存在时返回 [`StorageError::NotFound`]。
    fn update_execution_job(&self, job: &ExecutionJob) -> Result<ExecutionJob, StorageError>;
    /// 按稳定 ID 取执行任务。
    fn get_execution_job(&self, execution_id: &str) -> Result<Option<ExecutionJob>, StorageError>;
    /// 列执行任务，可按 Project / Run / 状态组合过滤。
    fn list_execution_jobs(
        &self,
        project_id: Option<&str>,
        run_id: Option<&str>,
        status: Option<ExecutionStatus>,
    ) -> Result<Vec<ExecutionJob>, StorageError>;

    /// 追加 `ToolInvocation` 审计记录。
    fn add_tool_invocation(&self, inv: &ToolInvocation) -> Result<ToolInvocation, StorageError>;
    /// 更新 ToolInvocation（upsert）。
    fn update_tool_invocation(&self, inv: &ToolInvocation) -> Result<ToolInvocation, StorageError>;
    /// 列 `ToolInvocation`；`project_id` 为 `None` 时列全部。
    fn list_tool_invocations(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<ToolInvocation>, StorageError>;

    /// 追加 Evidence。
    fn add_evidence(&self, evidence: &Evidence) -> Result<Evidence, StorageError>;
    /// 更新 Evidence（upsert）。
    fn update_evidence(&self, evidence: &Evidence) -> Result<Evidence, StorageError>;
    /// 列某 Project 的全部 Evidence。
    fn list_evidence(&self, project_id: &str) -> Result<Vec<Evidence>, StorageError>;

    /// 追加 Finding。
    fn add_finding(&self, finding: &Finding) -> Result<Finding, StorageError>;
    /// 按 id 取 Finding。
    fn get_finding(&self, finding_id: &str) -> Result<Option<Finding>, StorageError>;
    /// 列某 Project 的全部 Finding。
    fn list_findings(&self, project_id: &str) -> Result<Vec<Finding>, StorageError>;
    /// 更新 Finding（upsert）。
    fn update_finding(&self, finding: &Finding) -> Result<Finding, StorageError>;

    /// 追加复测记录。
    fn add_finding_retest(&self, retest: &FindingRetest) -> Result<FindingRetest, StorageError>;
    /// 按 id 取复测记录。
    fn get_finding_retest(&self, retest_id: &str) -> Result<Option<FindingRetest>, StorageError>;
    /// 列某 Project 的全部复测记录（按创建时间倒序）。
    fn list_finding_retests(&self, project_id: &str) -> Result<Vec<FindingRetest>, StorageError>;
    /// 更新复测记录（upsert）。
    fn update_finding_retest(
        &self,
        retest: &FindingRetest,
    ) -> Result<FindingRetest, StorageError>;

    /// 新建 Provider 配置（`is_default` 抢占：其余默认位被清空）。
    fn create_provider(&self, provider: &ProviderConfig) -> Result<ProviderConfig, StorageError>;
    /// 按 id 取 Provider 配置，不存在返回 `None`。
    fn get_provider(&self, provider_id: &str) -> Result<Option<ProviderConfig>, StorageError>;
    /// 列全部 Provider 配置。
    fn list_providers(&self) -> Result<Vec<ProviderConfig>, StorageError>;
    /// 更新 Provider 配置（upsert；`is_default` 抢占同 create）。
    fn update_provider(&self, provider: &ProviderConfig) -> Result<ProviderConfig, StorageError>;
    /// 删除 Provider 配置。
    fn delete_provider(&self, provider_id: &str) -> Result<(), StorageError>;

    /// 写入或更新一条 purpose 路由（upsert）。
    fn upsert_provider_route(
        &self,
        route: &ProviderRouteBinding,
    ) -> Result<ProviderRouteBinding, StorageError>;
    /// 按 id 取路由。
    fn get_provider_route(
        &self,
        route_id: &str,
    ) -> Result<Option<ProviderRouteBinding>, StorageError>;
    /// 列路由；`purpose` 过滤（strip + 小写归一）后按
    /// `(priority, weight)` 降序稳定排序。
    fn list_provider_routes(
        &self,
        purpose: Option<&str>,
    ) -> Result<Vec<ProviderRouteBinding>, StorageError>;
    /// 删除路由。
    fn delete_provider_route(&self, route_id: &str) -> Result<(), StorageError>;

    /// 追加模型调用审计记录。
    fn add_model_invocation(&self, inv: &ModelInvocation) -> Result<ModelInvocation, StorageError>;
    /// 列模型调用审计记录；`project_id` 为 `None` 时列全部。
    fn list_model_invocations(
        &self,
        project_id: Option<&str>,
    ) -> Result<Vec<ModelInvocation>, StorageError>;

    /// 写入或更新 Provider 模型能力元数据。
    fn upsert_model_capability(
        &self,
        capability: &ModelCapability,
    ) -> Result<ModelCapability, StorageError>;
    /// 列模型能力，可按 provider 过滤。
    fn list_model_capabilities(
        &self,
        provider_id: Option<&str>,
    ) -> Result<Vec<ModelCapability>, StorageError>;

    /// 新建 Project。
    fn create_project(&self, project: &Project) -> Result<Project, StorageError>;
    /// 按 id 取 Project，不存在返回 `None`。
    fn get_project(&self, project_id: &str) -> Result<Option<Project>, StorageError>;
    /// 列全部 Project。
    fn list_projects(&self) -> Result<Vec<Project>, StorageError>;
    /// 更新 Project（upsert）。
    fn update_project(&self, project: &Project) -> Result<Project, StorageError>;
    /// 删除 Project 并级联清空所有携带该 `project_id` 的表（单事务）。
    fn delete_project(&self, project_id: &str) -> Result<(), StorageError>;

    /// 追加 Fact（append-only：重复 id 报 [`StorageError::AppendOnlyConflict`]）。
    fn add_fact(&self, fact: &Fact) -> Result<Fact, StorageError>;
    /// 列某 Project 的全部 Fact。
    fn list_facts(&self, project_id: &str) -> Result<Vec<Fact>, StorageError>;

    /// 新建 Intent。
    fn add_intent(&self, intent: &Intent) -> Result<Intent, StorageError>;
    /// 按 id 取 Intent。
    fn get_intent(&self, intent_id: &str) -> Result<Option<Intent>, StorageError>;
    /// 列某 Project 的全部 Intent。
    fn list_intents(&self, project_id: &str) -> Result<Vec<Intent>, StorageError>;
    /// 更新 Intent（upsert）。
    fn update_intent(&self, intent: &Intent) -> Result<Intent, StorageError>;

    /// 追加审计事件（过程日志流）。
    fn add_event(&self, event: &AuditEvent) -> Result<AuditEvent, StorageError>;
    /// 列审计事件：`run_id` 走 SQL 过滤；`limit` 夹取到 `[1, 1000]`；
    /// `after_id` 以子查询定位 seq 增量拉取（id 不存在时子查询为 NULL，
    /// 结果为空——照搬 Python 行为）。
    fn list_events(
        &self,
        project_id: &str,
        run_id: Option<&str>,
        limit: i64,
        after_id: Option<&str>,
    ) -> Result<Vec<AuditEvent>, StorageError>;

    /// 追加 Mission 白板记录；相同 Mission + 幂等键的完全相同重试返回原记录。
    fn append_blackboard_entry(
        &self,
        entry: &BlackboardEntry,
    ) -> Result<BlackboardEntry, StorageError>;
    /// 按 Mission/Run/cursor 读取白板，返回稳定排序记录及下一页 sequence cursor。
    fn list_blackboard_entries(
        &self,
        project_id: &str,
        mission_id: &str,
        run_id: &str,
        after_sequence: Option<i64>,
        kind: Option<BlackboardEntryKind>,
        limit: usize,
        context_byte_budget: usize,
    ) -> Result<(Vec<BlackboardEntry>, Option<i64>), StorageError>;

    /// 追加 Observation。
    fn add_observation(&self, observation: &Observation) -> Result<Observation, StorageError>;
    /// 列 Observation；`run_id` 为 `None` 时列整个 Project。
    fn list_observations(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<Observation>, StorageError>;

    /// 追加 COV 覆盖评估。
    fn add_coverage_assessment(
        &self,
        assessment: &CoverageAssessment,
    ) -> Result<CoverageAssessment, StorageError>;
    /// 列 COV 覆盖评估。
    fn list_coverage_assessments(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<CoverageAssessment>, StorageError>;

    /// 追加 META 元认知评估。
    fn add_metacognition_assessment(
        &self,
        assessment: &MetacognitionAssessment,
    ) -> Result<MetacognitionAssessment, StorageError>;
    /// 列 META 元认知评估。
    fn list_metacognition_assessments(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<MetacognitionAssessment>, StorageError>;

    /// 追加 MGATE 出口判定。
    fn add_exit_gate_decision(
        &self,
        decision: &ExitGateDecision,
    ) -> Result<ExitGateDecision, StorageError>;
    /// 列 MGATE 出口判定。
    fn list_exit_gate_decisions(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<ExitGateDecision>, StorageError>;

    /// 追加 EGUARD 升级闸裁决。
    fn add_escalation_guard_verdict(
        &self,
        verdict: &EscalationGuardVerdict,
    ) -> Result<EscalationGuardVerdict, StorageError>;
    /// 列 EGUARD 升级闸裁决。
    fn list_escalation_guard_verdicts(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<EscalationGuardVerdict>, StorageError>;

    /// 追加 CRITIC 批判报告。
    fn add_critique_report(&self, report: &CritiqueReport) -> Result<CritiqueReport, StorageError>;
    /// 按 id 取 CRITIC 批判报告。
    fn get_critique_report(&self, report_id: &str) -> Result<Option<CritiqueReport>, StorageError>;
    /// 列批判报告；`run_id` 走 SQL，`branch_id` 是 payload 字段，
    /// 查询后内存过滤（Python `_list_filtered` 语义）。
    fn list_critique_reports(
        &self,
        project_id: &str,
        run_id: Option<&str>,
        branch_id: Option<&str>,
    ) -> Result<Vec<CritiqueReport>, StorageError>;

    /// 追加轨迹摘要段。
    fn add_trajectory_summary(
        &self,
        summary: &TrajectorySummary,
    ) -> Result<TrajectorySummary, StorageError>;
    /// 按 id 取轨迹摘要段。
    fn get_trajectory_summary(
        &self,
        summary_id: &str,
    ) -> Result<Option<TrajectorySummary>, StorageError>;
    /// 列轨迹摘要；`run_id` 走 SQL，`branch_id` 查询后内存过滤。
    fn list_trajectory_summaries(
        &self,
        project_id: &str,
        run_id: Option<&str>,
        branch_id: Option<&str>,
    ) -> Result<Vec<TrajectorySummary>, StorageError>;

    /// 追加人读 agent 叙事事件（append-only）。
    fn add_agent_narrative_event(
        &self,
        event: &AgentNarrativeEvent,
    ) -> Result<AgentNarrativeEvent, StorageError>;
    /// 列叙事事件；`run_id` 走 SQL，`mission_id` / `branch_id` 查询后内存
    /// 过滤，`limit` 在过滤后截断（seq 升序）。
    fn list_agent_narrative_events(
        &self,
        project_id: &str,
        run_id: Option<&str>,
        mission_id: Option<&str>,
        branch_id: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<AgentNarrativeEvent>, StorageError>;

    /// 追加策略板快照（append-only 流：更新即新快照）。
    fn add_strategy_board_snapshot(
        &self,
        snapshot: &StrategyBoardSnapshot,
    ) -> Result<StrategyBoardSnapshot, StorageError>;
    /// 按 id 取策略板快照。
    fn get_strategy_board_snapshot(
        &self,
        snapshot_id: &str,
    ) -> Result<Option<StrategyBoardSnapshot>, StorageError>;
    /// 列策略板快照。
    fn list_strategy_board_snapshots(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<StrategyBoardSnapshot>, StorageError>;

    /// 新建模块配置。
    fn create_module(&self, module: &ModuleConfig) -> Result<ModuleConfig, StorageError>;
    /// 按 id 取模块配置。
    fn get_module(&self, module_id: &str) -> Result<Option<ModuleConfig>, StorageError>;
    /// 列全部模块配置。
    fn list_modules(&self) -> Result<Vec<ModuleConfig>, StorageError>;
    /// 更新模块配置（upsert）。
    fn update_module(&self, module: &ModuleConfig) -> Result<ModuleConfig, StorageError>;
    /// 删除模块配置。
    fn delete_module(&self, module_id: &str) -> Result<(), StorageError>;
    /// 列全部启用（`enabled=true`）的模块配置。
    fn list_enabled_modules(&self) -> Result<Vec<ModuleConfig>, StorageError>;

    /// 追加 Hint（append-only：重复 id 报
    /// [`StorageError::AppendOnlyConflict`]）。
    fn add_hint(&self, hint: &Hint) -> Result<Hint, StorageError>;
    /// 列某 Project 的全部 Hint。
    fn list_hints(&self, project_id: &str) -> Result<Vec<Hint>, StorageError>;

    /// 写入或更新知识卡（upsert，project 级共享；FTS 索引随写维护）。
    fn add_knowledge_card(&self, card: &KnowledgeCard) -> Result<KnowledgeCard, StorageError>;
    /// 列全部知识卡。
    fn list_knowledge_cards(&self) -> Result<Vec<KnowledgeCard>, StorageError>;
    /// 知识检索：QueryNormalizer + 结构化过滤 + FTS5/BM25 主路，FTS
    /// 不可用时回退确定性线性扫描（`retrieval_reason` 注明回退）。
    fn search_knowledge_cards(
        &self,
        query: &KnowledgeRetrievalQuery,
    ) -> Result<Vec<KnowledgeRetrievalResult>, StorageError>;
    /// 全量重建知识 FTS 索引（index-sync）。返回索引行数。
    fn sync_knowledge_index(&self) -> Result<KnowledgeCorpusStatus, StorageError>;
    /// 知识语料/索引状态（empty/ready/stale）。
    fn knowledge_corpus_status(&self) -> Result<KnowledgeCorpusStatus, StorageError>;

    /// 新建决策门（重复 id 报错，不覆盖）。
    fn add_decision_gate(&self, gate: &DecisionGate) -> Result<DecisionGate, StorageError>;
    /// 按 id 取决策门。
    fn get_decision_gate(&self, gate_id: &str) -> Result<Option<DecisionGate>, StorageError>;
    /// 列决策门；`project_id` / `audit_run_id` 均可选，动态 WHERE。
    fn list_decision_gates(
        &self,
        project_id: Option<&str>,
        audit_run_id: Option<&str>,
    ) -> Result<Vec<DecisionGate>, StorageError>;
    /// 更新决策门（upsert）。
    fn update_decision_gate(&self, gate: &DecisionGate) -> Result<DecisionGate, StorageError>;
    /// 记录决策门的回答（置 `ANSWERED` + `answered_at`；缺失报
    /// [`StorageError::NotFound`]）。
    fn answer_decision_gate(
        &self,
        gate_id: &str,
        answer: &DecisionAnswer,
    ) -> Result<DecisionGate, StorageError>;

    /// 写入或更新 worker 画像（upsert）。
    fn add_worker_profile(&self, profile: &WorkerProfile) -> Result<WorkerProfile, StorageError>;
    /// 列全部 worker 画像。
    fn list_worker_profiles(&self) -> Result<Vec<WorkerProfile>, StorageError>;

    /// 新建 worker 租约（append-only）。
    fn add_worker_lease(&self, lease: &WorkerLease) -> Result<WorkerLease, StorageError>;
    /// 更新 worker 租约（upsert）。
    fn update_worker_lease(&self, lease: &WorkerLease) -> Result<WorkerLease, StorageError>;
    /// 列 worker 租约；`run_id` 为 `None` 时列整个 Project。
    fn list_worker_leases(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<WorkerLease>, StorageError>;
    /// 数据库事务内原子 claim 一个 Task；`None` 表示不可运行或已被有效 lease 持有。
    fn claim_worker_lease(
        &self,
        project_id: &str,
        mission_id: &str,
        run_id: &str,
        task_id: &str,
        worker_id: &str,
        worker_run_id: &str,
        lease_seconds: i64,
    ) -> Result<Option<WorkerLease>, StorageError>;
    /// owner + revision CAS 心跳。
    fn heartbeat_worker_lease(
        &self,
        lease_id: &str,
        worker_run_id: &str,
        revision: i64,
        lease_seconds: i64,
    ) -> Result<Option<WorkerLease>, StorageError>;
    /// owner + revision CAS 完成。
    fn complete_worker_lease(
        &self,
        lease_id: &str,
        worker_run_id: &str,
        revision: i64,
    ) -> Result<Option<WorkerLease>, StorageError>;
    /// owner + revision CAS 失败。
    fn fail_worker_lease(
        &self,
        lease_id: &str,
        worker_run_id: &str,
        revision: i64,
    ) -> Result<Option<WorkerLease>, StorageError>;
    /// owner + revision CAS 取消。
    fn cancel_worker_lease(
        &self,
        lease_id: &str,
        worker_run_id: &str,
        revision: i64,
    ) -> Result<Option<WorkerLease>, StorageError>;
    /// 惰性回收指定范围内已经过期的 Active lease。
    fn reclaim_expired_worker_leases(
        &self,
        project_id: &str,
        mission_id: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<usize, StorageError>;

    /// 追加终止评估（append-only 流）。
    fn add_termination_assessment(
        &self,
        assessment: &TerminationAssessment,
    ) -> Result<TerminationAssessment, StorageError>;
    /// 列终止评估；`run_id` 为 `None` 时列整个 Project。
    fn list_termination_assessments(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<TerminationAssessment>, StorageError>;

    /// 追加 ContextPack（append-only）。
    fn add_context_pack(&self, pack: &ContextPack) -> Result<ContextPack, StorageError>;
    /// 按 id 取 `ContextPack`。
    fn get_context_pack(&self, pack_id: &str) -> Result<Option<ContextPack>, StorageError>;
    /// 列 `ContextPack`；`run_id` 为 `None` 时列整个 Project。
    fn list_context_packs(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<ContextPack>, StorageError>;

    /// 追加压缩审计报告（append-only）。
    fn add_context_compression_report(
        &self,
        report: &ContextCompressionReport,
    ) -> Result<ContextCompressionReport, StorageError>;
    /// 列压缩审计报告；`run_id` 为 `None` 时列整个 Project。
    fn list_context_compression_reports(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<ContextCompressionReport>, StorageError>;

    /// 追加检索调用审计记录（append-only）。
    ///
    /// 由 solver bootstrap 的三条 telemetry 路径写入：knowledge bootstrap、
    /// task profile、tool retrieval。与已删除的 evidence-chunk 检索
    /// （`search_retrieval` / `retrieval_chunks`）无关。
    fn add_retrieval_invocation(
        &self,
        invocation: &RetrievalInvocation,
    ) -> Result<RetrievalInvocation, StorageError>;

    /// 写入或替换同一 key/scope 的运行时设置。
    fn upsert_runtime_setting(
        &self,
        setting: &RuntimeSetting,
    ) -> Result<RuntimeSetting, StorageError>;
    /// 按 run > project > global 优先级读取运行时设置。
    fn get_runtime_setting(
        &self,
        key: &str,
        project_id: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<Option<RuntimeSetting>, StorageError>;
    /// 列出当前 project/run 可见的运行时设置。
    fn list_runtime_settings(
        &self,
        project_id: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<Vec<RuntimeSetting>, StorageError>;
    /// 删除精确 key/scope 的运行时设置。
    fn delete_runtime_setting(
        &self,
        key: &str,
        project_id: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<(), StorageError>;

    /// 追加工件记录（append-only：重复 id 报
    /// [`StorageError::AppendOnlyConflict`]，Python `_insert` 语义）。
    fn add_artifact_record(
        &self,
        artifact: &ArtifactRecord,
    ) -> Result<ArtifactRecord, StorageError>;
    /// 按 id 取工件记录。
    fn get_artifact_record(
        &self,
        artifact_id: &str,
    ) -> Result<Option<ArtifactRecord>, StorageError>;
    /// 写入或更新工件记录（upsert，Python `_upsert` 语义）。
    fn update_artifact_record(
        &self,
        artifact: &ArtifactRecord,
    ) -> Result<ArtifactRecord, StorageError>;
    /// 列工件记录；`project_id=None` 列全表，给定 project 时 `run_id`
    /// 为 `None` 列整个 Project，否则 SQL 列过滤；`run_id` 给定而
    /// `project_id=None` 时查询后内存过滤（Python `_list_project_run`）。
    fn list_artifact_records(
        &self,
        project_id: Option<&str>,
        run_id: Option<&str>,
    ) -> Result<Vec<ArtifactRecord>, StorageError>;

    /// 新建用户指令（append-only）。
    fn add_user_directive(&self, directive: &UserDirective) -> Result<UserDirective, StorageError>;
    /// 按 id 取用户指令。
    fn get_user_directive(&self, directive_id: &str)
    -> Result<Option<UserDirective>, StorageError>;
    /// 列用户指令：`project_id` / `run_id` 走 SQL 列过滤，`mission_id` /
    /// `branch_id` 是 payload 字段，查询后内存过滤（Python `_list_filtered`）。
    fn list_user_directives(
        &self,
        project_id: Option<&str>,
        mission_id: Option<&str>,
        run_id: Option<&str>,
        branch_id: Option<&str>,
    ) -> Result<Vec<UserDirective>, StorageError>;
    /// 更新用户指令（upsert）。
    fn update_user_directive(
        &self,
        directive: &UserDirective,
    ) -> Result<UserDirective, StorageError>;

    /// 新建 Mission 资产（重复 id 报错，不覆盖）。
    fn create_mission_asset(&self, asset: &MissionAsset) -> Result<MissionAsset, StorageError>;
    /// 按 id 取 Mission 资产。
    fn get_mission_asset(&self, asset_id: &str) -> Result<Option<MissionAsset>, StorageError>;
    /// 列 Mission 资产：`mission_id` / `project_id` / `asset_type` 走 SQL 列
    /// 过滤，`sensitivity` 查询后内存过滤（Python 同）。
    fn list_mission_assets(
        &self,
        mission_id: Option<&str>,
        project_id: Option<&str>,
        sensitivity: Option<MissionAssetSensitivity>,
        asset_type: Option<MissionAssetType>,
    ) -> Result<Vec<MissionAsset>, StorageError>;
    /// 更新 Mission 资产（按 id upsert）。
    fn update_mission_asset(&self, asset: &MissionAsset) -> Result<MissionAsset, StorageError>;
    /// 去重键 upsert：`(mission_id, asset_type, normalized_value)` 已存在时
    /// 与既有记录保守合并（`merge_mission_assets`），否则插入。
    fn upsert_mission_asset(&self, asset: &MissionAsset) -> Result<MissionAsset, StorageError>;

    /// 追加失败复盘报告（append-only）。
    fn add_reflector_report(
        &self,
        report: &ReflectorReport,
    ) -> Result<ReflectorReport, StorageError>;
    /// 列失败复盘报告；`run_id` 为 `None` 时列整个 Project。
    fn list_reflector_reports(
        &self,
        project_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<ReflectorReport>, StorageError>;

    // ------------------------------------------------------------------
    // Intelligence Hub（外部情报：实体 / 关系 / 原始记录）
    // ------------------------------------------------------------------

    /// 原子写入一个 source result：Raw records 先落库，再写实体、关系
    /// 和 provenance associations；任一步失败则整个批次回滚。
    fn ingest_intel_batch(
        &self,
        batch: &IntelIngestBatch,
    ) -> Result<IntelIngestOutcome, StorageError>;
    /// 按去重键取情报实体。
    fn get_intel_entity_by_key(
        &self,
        kind: IntelEntityKind,
        normalized_value: &str,
    ) -> Result<Option<IntelEntity>, StorageError>;
    /// 按 id 取情报实体。
    fn get_intel_entity(&self, entity_id: &str) -> Result<Option<IntelEntity>, StorageError>;
    /// 显式变更情报实体晋升状态（promote/reject 的唯一通道；dedup
    /// 合并绝不触碰状态）。
    fn set_intel_entity_status(
        &self,
        entity_id: &str,
        status: IntelEntityStatus,
    ) -> Result<Option<IntelEntity>, StorageError>;
    /// 列情报实体：`kind` 走 SQL 列过滤，`query` 对
    /// `normalized_value` 做大小写不敏感的子串过滤（内存）。
    fn list_intel_entities(
        &self,
        kind: Option<IntelEntityKind>,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<IntelEntityRecord>, StorageError>;
    /// 列情报关系：`entity_id` 提供时返回以之为起点或终点的全部边。
    fn list_intel_relations(
        &self,
        entity_id: Option<&str>,
    ) -> Result<Vec<IntelRelationRecord>, StorageError>;
    /// 列情报原始记录（`source` 过滤，按写入序倒排取 `limit`）。
    fn list_intel_raw_records(
        &self,
        source: Option<&str>,
        limit: usize,
    ) -> Result<Vec<IntelRawRecord>, StorageError>;

    // ------------------------------------------------------------------
    // 外部 Worker Runtime（WorkerRun / WorkerInvocation / Profile）
    // ------------------------------------------------------------------

    /// upsert 一个外部 worker 会话（payload 整体替换；id 不变）。
        /// 聚合 worker 用量（可选按 project 过滤；部分成本缺失时聚合值为 None）。
    fn sum_worker_usage(
        &self,
        project_id: Option<&str>,
    ) -> Result<models::WorkerUsageSummary, StorageError>;
    /// 用量多维报表（可选 project 过滤 + 最近 `days` 天窗口；按日/模型/运行时切片）。
    /// `group_by` 提供时额外返回（日 × 维度键）聚合 `grouped_daily`。
    fn sum_worker_usage_breakdown(
        &self,
        project_id: Option<&str>,
        days: Option<i64>,
        group_by: Option<models::WorkerUsageDimension>,
    ) -> Result<models::WorkerUsageBreakdown, StorageError>;
    fn upsert_worker_run(&self, run: &WorkerRun) -> Result<(), StorageError>;
    /// 按 id 取一个外部 worker 会话。
    fn get_worker_run(&self, run_id: &str) -> Result<Option<WorkerRun>, StorageError>;
    /// 删除一个外部 worker 会话行；返回是否真的删掉了。
    ///
    /// swarm 收口用：竞速落败的 attempt 直接消失，只留获胜者（用户要求
    /// "最后只显示成功的那个"）。删的只是 `worker_runs` 这一行——没有外键
    /// 指向它；task / intent / event 仍由常规提交流程落库，审计不丢。
    fn delete_worker_run(&self, run_id: &str) -> Result<bool, StorageError>;
    /// 列外部 worker 会话：给定过滤条件按 seq 倒排取 `limit` 条；
    /// 全部为 None 时返回全表最近 `limit` 条。
    fn list_worker_runs(
        &self,
        project_id: Option<&str>,
        run_id: Option<&str>,
        task_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<WorkerRun>, StorageError>;
    /// upsert 一条外部 worker 调用审计记录。
    fn upsert_worker_invocation(&self, invocation: &WorkerInvocation) -> Result<(), StorageError>;
    /// 列某个 worker 会话的调用审计（按 seq 升序）。
    fn list_worker_invocations(
        &self,
        worker_run_id: Option<&str>,
        project_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<WorkerInvocation>, StorageError>;
    /// upsert 一个 Worker Runtime Profile。
    fn upsert_worker_runtime_profile(
        &self,
        profile: &WorkerRuntimeProfile,
    ) -> Result<(), StorageError>;
    /// 按 id 取 Worker Runtime Profile。
    fn get_worker_runtime_profile(
        &self,
        profile_id: &str,
    ) -> Result<Option<WorkerRuntimeProfile>, StorageError>;
    /// 列 Worker Runtime Profile（`runtime_type` 可选过滤，seq 倒排）。
    fn list_worker_runtime_profiles(
        &self,
        runtime_type: Option<WorkerRuntimeType>,
    ) -> Result<Vec<WorkerRuntimeProfile>, StorageError>;
    /// 删除 Worker Runtime Profile（返回是否删除了行）。
    fn delete_worker_runtime_profile(&self, profile_id: &str) -> Result<bool, StorageError>;

    // ------------------------------------------------------------------
    // Agent 预设（三处硬编码提示词的可编辑收编）
    // ------------------------------------------------------------------

    /// upsert 一个 Agent 预设（payload 整体替换；id = 稳定 key）。
    fn upsert_agent_preset(&self, preset: &models::AgentPreset) -> Result<(), StorageError>;
    /// 按 key 取 Agent 预设。
    fn get_agent_preset(&self, key: &str) -> Result<Option<models::AgentPreset>, StorageError>;
    /// 列 Agent 预设（`builtin` 可选过滤，seq 倒排）。
    fn list_agent_presets(
        &self,
        builtin: Option<bool>,
    ) -> Result<Vec<models::AgentPreset>, StorageError>;
    /// 删除 Agent 预设（内置预设由 API 层拒绝；返回是否删除了行）。
    fn delete_agent_preset(&self, key: &str) -> Result<bool, StorageError>;

    // ------------------------------------------------------------------
    // Skill 调用台账（WP6；无外键，统计比任务活得久）
    // ------------------------------------------------------------------

    /// 追加一条 skill 调用记录（存在与否都记）。
    fn record_skill_usage(
        &self,
        row: &models::skill::SkillUsageRow,
    ) -> Result<(), StorageError>;
    /// 列 skill 调用记录（`skill` 可选过滤，seq 倒排取 `limit`）。
    fn list_skill_usage(
        &self,
        skill: Option<&str>,
        limit: usize,
    ) -> Result<Vec<models::skill::SkillUsageRow>, StorageError>;
    /// 缺口清单：found=0 按（次数降序、最近时间）排序。
    fn skill_missing_report(&self) -> Result<Vec<models::skill::SkillMissingEntry>, StorageError>;

    // ------------------------------------------------------------------
    // 仪表盘聚合（GET /stats/dashboard）
    // ------------------------------------------------------------------

    /// 仪表盘首行五卡一次性聚合（活跃任务 / 确认发现 / 资产节点 /
    /// 工具调用 / Token 用量）。全量口径，不按 project 过滤。
    fn dashboard_stats(&self) -> Result<models::DashboardStats, StorageError>;
}
