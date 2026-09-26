//! 轻量 agent 状态模型 —— `server/core/models/agent.py` 收口链所需部分的移植。
//!
//! 包含 [`TerminationAssessment`]（终止评估）、终止判定器消费的
//! [`Observation`] / [`WorkerLease`] 与策略板提示词消费的 [`ContextPack`]，
//! 以及编排引擎路径上的 [`WorkerProfile`]（租约并发控制）与
//! [`ContextCompressionReport`]（压缩审计）。其余 agent 模型
//! （`BlackboardRunPolicy` 等）在编排引擎阶段按需移植。

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::common::Timestamp;
use crate::common::new_id;
use crate::common::utcnow;
use crate::domain::AuditDomain;
use crate::ids::BranchId;
use crate::ids::ContextPackId;
use crate::ids::IntentId;
use crate::ids::MissionId;
use crate::ids::ObservationId;
use crate::ids::ProjectId;
use crate::ids::ReflectorReportId;
use crate::ids::RunId;
use crate::ids::TaskId;
use crate::ids::TerminationAssessmentId;
use crate::ids::WorkerLeaseId;

/// Ralph-Loop 式终止评估结果（`TerminationStatus`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminationStatus {
    /// 继续执行。
    Continue,
    /// 暂停。
    Pause,
    /// 完成。
    Complete,
    /// 需要人工决策。
    NeedsHumanDecision,
}

/// 结构化观察类别（`ObservationType`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationType {
    /// 进度。
    Progress,
    /// 工具结果。
    ToolResult,
    /// 阻塞。
    Blockage,
    /// 证据缺口。
    EvidenceGap,
    /// 失败边界。
    FailureBoundary,
    /// 矛盾。
    Contradiction,
    /// 工具失败。
    ToolFailure,
    /// 假设。
    Hypothesis,
    /// 假设更新。
    HypothesisUpdate,
    /// 用户备注。
    UserNote,
    /// 决策。
    Decision,
    /// 终态。
    TerminalState,
}

impl ObservationType {
    /// wire 值（Python `.value` 镜像，用于文本拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ObservationType::Progress => "progress",
            ObservationType::ToolResult => "tool_result",
            ObservationType::Blockage => "blockage",
            ObservationType::EvidenceGap => "evidence_gap",
            ObservationType::FailureBoundary => "failure_boundary",
            ObservationType::Contradiction => "contradiction",
            ObservationType::ToolFailure => "tool_failure",
            ObservationType::Hypothesis => "hypothesis",
            ObservationType::HypothesisUpdate => "hypothesis_update",
            ObservationType::UserNote => "user_note",
            ObservationType::Decision => "decision",
            ObservationType::TerminalState => "terminal_state",
        }
    }
}

/// 黑板 worker 租约生命周期（`WorkerLeaseStatus`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerLeaseStatus {
    /// 活跃。
    Active,
    /// Worker 执行失败后终结。
    Failed,
    /// 已释放。
    Released,
    /// 已过期。
    Expired,
    /// 已取消。
    Cancelled,
    /// 已完成。
    Completed,
}

fn default_termination_id() -> TerminationAssessmentId {
    TerminationAssessmentId::new(new_id("term"))
}

fn default_termination_status() -> TerminationStatus {
    TerminationStatus::Continue
}

/// TerminationAssessment：外置的确定性 stop/pause/continue 评估
/// （`TerminationAssessment`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminationAssessment {
    /// 终止评估标识符。
    #[serde(default = "default_termination_id")]
    pub id: TerminationAssessmentId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Run。
    pub run_id: RunId,
    /// 评估结果。
    #[serde(default = "default_termination_status")]
    pub status: TerminationStatus,
    /// 理由。
    #[serde(default)]
    pub reasons: Vec<String>,
    /// 覆盖摘要。
    #[serde(default)]
    pub coverage_summary: String,
    /// 未解决 Intent ID 列表。
    #[serde(default)]
    pub unresolved_intent_ids: Vec<String>,
    /// 未解决 Branch ID 列表。
    #[serde(default)]
    pub unresolved_branch_ids: Vec<String>,
    /// 证据缺口数。
    #[serde(default)]
    pub evidence_gap_count: i64,
    /// 高价值待解问题。
    #[serde(default)]
    pub high_value_open_questions: Vec<String>,
    /// 目标是否已满足（未知为 `None`）。
    #[serde(default)]
    pub goal_satisfied: Option<bool>,
    /// 目标需求列表。
    #[serde(default)]
    pub goal_requirements: Vec<String>,
    /// 已满足的目标需求。
    #[serde(default)]
    pub satisfied_goal_requirements: Vec<String>,
    /// 未满足的目标需求。
    #[serde(default)]
    pub unmet_goal_requirements: Vec<String>,
    /// 置信度（Python 侧约束 `[0.0, 1.0]`）。
    #[serde(default)]
    pub confidence: f64,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
}

impl TerminationAssessment {
    /// 以 Python 默认值构造（`TerminationAssessment(project_id=...,
    /// run_id=...)`）。
    #[must_use]
    pub fn new(project_id: ProjectId, run_id: RunId) -> Self {
        Self {
            id: default_termination_id(),
            project_id,
            run_id,
            status: default_termination_status(),
            reasons: Vec::new(),
            coverage_summary: String::new(),
            unresolved_intent_ids: Vec::new(),
            unresolved_branch_ids: Vec::new(),
            evidence_gap_count: 0,
            high_value_open_questions: Vec::new(),
            goal_satisfied: None,
            goal_requirements: Vec::new(),
            satisfied_goal_requirements: Vec::new(),
            unmet_goal_requirements: Vec::new(),
            confidence: 0.0,
            created_at: utcnow(),
        }
    }
}

fn default_observation_id() -> ObservationId {
    ObservationId::new(new_id("obs"))
}

fn default_observation_type() -> ObservationType {
    ObservationType::Progress
}

fn default_observation_source() -> String {
    "worker".to_string()
}

/// Observation：来自 worker、工具、模型调用或评审者的结构化观察
/// （`Observation`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    /// 观察标识符。
    #[serde(default = "default_observation_id")]
    pub id: ObservationId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Mission。
    #[serde(default)]
    pub mission_id: Option<MissionId>,
    /// 所属 Branch。
    #[serde(default)]
    pub branch_id: Option<BranchId>,
    /// 所属 Run。
    pub run_id: RunId,
    /// 关联 Task。
    #[serde(default)]
    pub task_id: Option<TaskId>,
    /// 关联 Intent。
    #[serde(default)]
    pub intent_id: Option<IntentId>,
    /// 关联 Worker。
    #[serde(default)]
    pub worker_id: Option<String>,
    /// 观察类别。
    #[serde(default = "default_observation_type")]
    pub observation_type: ObservationType,
    /// 向后兼容的来源标签。
    #[serde(default = "default_observation_source")]
    pub source: String,
    /// 行为者。
    #[serde(default)]
    pub actor: Option<String>,
    /// 摘要。
    pub summary: String,
    /// 结构化数据（键序 = 插入序）。
    #[serde(default)]
    pub data: Map<String, Value>,
    /// 关联 Fact ID 列表。
    #[serde(default)]
    pub related_fact_ids: Vec<String>,
    /// 关联 Evidence ID 列表。
    #[serde(default)]
    pub related_evidence_ids: Vec<String>,
    /// 关联 Finding ID 列表。
    #[serde(default)]
    pub related_finding_ids: Vec<String>,
    /// 关联 `ToolInvocation` ID 列表。
    #[serde(default)]
    pub related_tool_invocation_ids: Vec<String>,
    /// 关联 Task ID 列表。
    #[serde(default)]
    pub related_task_ids: Vec<String>,
    /// 关联 Intent ID 列表。
    #[serde(default)]
    pub related_intent_ids: Vec<String>,
    /// 关联 `DecisionGate` ID 列表。
    #[serde(default)]
    pub related_decision_gate_ids: Vec<String>,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
}

impl Observation {
    /// 以 Python 默认值构造（`Observation(project_id=..., run_id=...,
    /// summary=...)`）。
    #[must_use]
    pub fn new(project_id: ProjectId, run_id: RunId, summary: String) -> Self {
        Self {
            id: default_observation_id(),
            project_id,
            mission_id: None,
            branch_id: None,
            run_id,
            task_id: None,
            intent_id: None,
            worker_id: None,
            observation_type: default_observation_type(),
            source: default_observation_source(),
            actor: None,
            summary,
            data: Map::new(),
            related_fact_ids: Vec::new(),
            related_evidence_ids: Vec::new(),
            related_finding_ids: Vec::new(),
            related_tool_invocation_ids: Vec::new(),
            related_task_ids: Vec::new(),
            related_intent_ids: Vec::new(),
            related_decision_gate_ids: Vec::new(),
            created_at: utcnow(),
        }
    }
}

fn default_lease_status() -> WorkerLeaseStatus {
    WorkerLeaseStatus::Active
}

/// WorkerLease：面向未来分布式探索的黑板式 worker 租约（`WorkerLease`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerLease {
    /// 租约标识符。
    #[serde(default = "default_lease_id")]
    pub id: WorkerLeaseId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Mission；新的 scope-bound lease 必须具备。
    #[serde(default)]
    pub mission_id: Option<MissionId>,
    /// 所属 Run。
    pub run_id: RunId,
    /// 关联 Intent。
    pub intent_id: IntentId,
    /// Worker 标识符。
    pub worker_id: String,
    /// 持有 lease 的 WorkerRun；状态迁移以此作为 owner。
    #[serde(default)]
    pub worker_run_id: Option<String>,
    /// 关联 Task。
    #[serde(default)]
    pub task_id: Option<TaskId>,
    /// 租约状态。
    #[serde(default = "default_lease_status")]
    pub status: WorkerLeaseStatus,
    /// 成功 claim 的时间。
    #[serde(default = "crate::common::utcnow")]
    pub acquired_at: Timestamp,
    /// 过期时间。
    pub lease_expires_at: Timestamp,
    /// 心跳时间。
    #[serde(default = "crate::common::utcnow")]
    pub heartbeat_at: Timestamp,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
    /// 最后更新时间。
    #[serde(default = "crate::common::utcnow")]
    pub updated_at: Timestamp,
    /// CAS 版本；每次 owner 状态迁移严格递增。
    #[serde(default)]
    pub revision: i64,
    /// 附加元数据（键序 = 插入序）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
    /// 向后兼容别名：租约开始时间。
    #[serde(default)]
    pub leased_at: Option<Timestamp>,
    /// 向后兼容别名：过期时间。
    #[serde(default)]
    pub expires_at: Option<Timestamp>,
    /// 向后兼容别名：取消时间。
    #[serde(default)]
    pub cancelled_at: Option<Timestamp>,
}

fn default_lease_id() -> WorkerLeaseId {
    WorkerLeaseId::new(new_id("lease"))
}

fn default_context_pack_id() -> ContextPackId {
    ContextPackId::new(new_id("ctx"))
}

fn default_token_budget() -> i64 {
    2048
}

fn default_compression_strategy() -> String {
    "deterministic_v1".to_string()
}

/// ContextPack：工件感知的提示词上下文 + 精确的操作日志窗口
/// （`ContextPack`）。
///
/// `summary` 是面向用户/导航的投影。模型消费下方结构化操作记录；
/// 摘要绝不替代那些记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPack {
    /// 上下文包标识符。
    #[serde(default = "default_context_pack_id")]
    pub id: ContextPackId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Run。
    pub run_id: RunId,
    /// 关联 Task。
    #[serde(default)]
    pub task_id: Option<TaskId>,
    /// 用途。
    pub purpose: String,
    /// Fact 内容列表。
    #[serde(default)]
    pub facts: Vec<String>,
    /// Fact ID 列表。
    #[serde(default)]
    pub fact_ids: Vec<String>,
    /// Intent 内容列表。
    #[serde(default)]
    pub intents: Vec<String>,
    /// Intent ID 列表。
    #[serde(default)]
    pub intent_ids: Vec<String>,
    /// Hint 内容列表。
    #[serde(default)]
    pub hints: Vec<String>,
    /// Hint ID 列表。
    #[serde(default)]
    pub hint_ids: Vec<String>,
    /// Evidence ID 列表。
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    /// Finding ID 列表。
    #[serde(default)]
    pub finding_ids: Vec<String>,
    /// `ToolInvocation` ID 列表。
    #[serde(default)]
    pub tool_invocation_ids: Vec<String>,
    /// 操作日志记录（键序 = 插入序）。
    #[serde(default)]
    pub operation_log_records: Vec<Map<String, Value>>,
    /// 摘要。
    pub summary: String,
    /// token 预算（≥1）。
    #[serde(default = "default_token_budget")]
    pub token_budget: i64,
    /// 压缩策略。
    #[serde(default = "default_compression_strategy")]
    pub compression_strategy: String,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
    /// 附加元数据（键序 = 插入序）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

impl ContextPack {
    /// 以 Python 默认值构造（`ContextPack(project_id=..., run_id=...,
    /// purpose=..., summary=...)`）。
    #[must_use]
    pub fn new(project_id: ProjectId, run_id: RunId, purpose: String, summary: String) -> Self {
        Self {
            id: default_context_pack_id(),
            project_id,
            run_id,
            task_id: None,
            purpose,
            facts: Vec::new(),
            fact_ids: Vec::new(),
            intents: Vec::new(),
            intent_ids: Vec::new(),
            hints: Vec::new(),
            hint_ids: Vec::new(),
            evidence_ids: Vec::new(),
            finding_ids: Vec::new(),
            tool_invocation_ids: Vec::new(),
            operation_log_records: Vec::new(),
            summary,
            token_budget: default_token_budget(),
            compression_strategy: default_compression_strategy(),
            created_at: utcnow(),
            metadata: Map::new(),
        }
    }
}

impl WorkerLease {
    /// 以 Python 默认值构造（`WorkerLease(project_id=..., run_id=...,
    /// intent_id=..., worker_id=..., lease_expires_at=...)`）。
    #[must_use]
    pub fn new(
        project_id: ProjectId,
        run_id: RunId,
        intent_id: IntentId,
        worker_id: String,
        lease_expires_at: Timestamp,
    ) -> Self {
        Self {
            id: default_lease_id(),
            project_id,
            mission_id: None,
            run_id,
            intent_id,
            worker_id,
            worker_run_id: None,
            task_id: None,
            status: default_lease_status(),
            acquired_at: utcnow(),
            lease_expires_at,
            heartbeat_at: utcnow(),
            created_at: utcnow(),
            updated_at: utcnow(),
            revision: 0,
            metadata: Map::new(),
            leased_at: None,
            expires_at: None,
            cancelled_at: None,
        }
    }
}

/// 黑板 worker 角色（`WorkerType`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerType {
    /// 通用。
    General,
    /// 求解器。
    Solver,
    /// 顾问。
    Advisor,
    /// 反思器。
    Reflector,
    /// 外部 agent。
    ExternalAgent,
}

fn default_profile_id() -> crate::ids::WorkerProfileId {
    crate::ids::WorkerProfileId::new(new_id("worker"))
}

fn default_worker_type() -> WorkerType {
    WorkerType::General
}

fn default_profile_enabled() -> bool {
    true
}

fn default_max_concurrent_leases() -> i64 {
    1
}

/// WorkerProfile：黑板 worker 的能力画像（`WorkerProfile`）。
///
/// `acquire_worker_lease` 据此做启用检查与并发租约上限控制。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerProfile {
    /// worker 画像标识符。
    #[serde(default = "default_profile_id")]
    pub id: crate::ids::WorkerProfileId,
    /// 名称。
    pub name: String,
    /// 角色。
    #[serde(default = "default_worker_type")]
    pub worker_type: WorkerType,
    /// 声明覆盖的审计域。
    #[serde(default)]
    pub supported_audit_domains: Vec<AuditDomain>,
    /// 声明支持的能力标签。
    #[serde(default)]
    pub supported_capabilities: Vec<String>,
    /// Provider id。
    #[serde(default)]
    pub provider_id: Option<String>,
    /// 是否启用。
    #[serde(default = "default_profile_enabled")]
    pub enabled: bool,
    /// 并发租约上限（Python 侧约束 ≥1）。
    #[serde(default = "default_max_concurrent_leases")]
    pub max_concurrent_leases: i64,
    /// 附加元数据（键序 = 插入序）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
    /// 更新时间。
    #[serde(default = "crate::common::utcnow")]
    pub updated_at: Timestamp,
}

impl WorkerProfile {
    /// 以 Python 默认值构造（`WorkerProfile(name=...)`）。
    #[must_use]
    pub fn new(name: String) -> Self {
        Self {
            id: default_profile_id(),
            name,
            worker_type: default_worker_type(),
            supported_audit_domains: Vec::new(),
            supported_capabilities: Vec::new(),
            provider_id: None,
            enabled: default_profile_enabled(),
            max_concurrent_leases: default_max_concurrent_leases(),
            metadata: Map::new(),
            created_at: utcnow(),
            updated_at: utcnow(),
        }
    }
}

fn default_report_id() -> crate::ids::ContextCompressionReportId {
    crate::ids::ContextCompressionReportId::new(new_id("ctxr"))
}

/// ContextCompressionReport：一次 `ContextPack` 压缩的审计报告。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCompressionReport {
    /// 报告标识符。
    #[serde(default = "default_report_id")]
    pub id: crate::ids::ContextCompressionReportId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Run。
    pub run_id: RunId,
    /// 源计数（键序 = 插入序）。
    #[serde(default)]
    pub source_counts: Map<String, Value>,
    /// 纳入计数（键序 = 插入序）。
    #[serde(default)]
    pub included_counts: Map<String, Value>,
    /// 丢弃计数（键序 = 插入序）。
    #[serde(default)]
    pub dropped_counts: Map<String, Value>,
    /// 压缩策略。
    #[serde(default = "default_compression_strategy")]
    pub strategy: String,
    /// 告警。
    #[serde(default)]
    pub warnings: Vec<String>,
    /// 摘要。
    pub summary: String,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
}

impl ContextCompressionReport {
    /// 以 Python 默认值构造（`ContextCompressionReport(project_id=...,
    /// run_id=..., summary=...)`）。
    #[must_use]
    pub fn new(project_id: ProjectId, run_id: RunId, summary: String) -> Self {
        Self {
            id: default_report_id(),
            project_id,
            run_id,
            source_counts: Map::new(),
            included_counts: Map::new(),
            dropped_counts: Map::new(),
            strategy: default_compression_strategy(),
            warnings: Vec::new(),
            summary,
            created_at: utcnow(),
        }
    }
}

/// Reflector MVP 发出的确定性失败类别（`ReflectorFailureType`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReflectorFailureType {
    /// 工具不可用。
    ToolUnavailable,
    /// 配置非法。
    InvalidConfig,
    /// 超时。
    Timeout,
    /// 上下文不足。
    InsufficientContext,
    /// 目标不受支持。
    UnsupportedTarget,
    /// 求解器错误。
    SolverError,
    /// 未知。
    Unknown,
}

impl ReflectorFailureType {
    /// wire 值（Python `.value` 镜像）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ReflectorFailureType::ToolUnavailable => "tool_unavailable",
            ReflectorFailureType::InvalidConfig => "invalid_config",
            ReflectorFailureType::Timeout => "timeout",
            ReflectorFailureType::InsufficientContext => "insufficient_context",
            ReflectorFailureType::UnsupportedTarget => "unsupported_target",
            ReflectorFailureType::SolverError => "solver_error",
            ReflectorFailureType::Unknown => "unknown",
        }
    }
}

fn default_reflector_id() -> ReflectorReportId {
    ReflectorReportId::new(new_id("reflector"))
}

fn default_reflector_failure_type() -> ReflectorFailureType {
    ReflectorFailureType::Unknown
}

/// 任务失败后的复盘报告，用于知识沉淀（`ReflectorReport`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReflectorReport {
    /// 报告标识符。
    #[serde(default = "default_reflector_id")]
    pub id: ReflectorReportId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Run。
    pub run_id: RunId,
    /// 关联 Task。
    #[serde(default)]
    pub task_id: Option<TaskId>,
    /// 失败类别。
    #[serde(default = "default_reflector_failure_type")]
    pub failure_type: ReflectorFailureType,
    /// 根因摘要。
    #[serde(default)]
    pub root_cause_summary: String,
    /// 失败模式列表。
    #[serde(default)]
    pub failure_modes: Vec<String>,
    /// 经验教训列表。
    #[serde(default)]
    pub lessons: Vec<String>,
    /// 建议的 playbook 更新。
    #[serde(default)]
    pub suggested_playbook_updates: Vec<String>,
    /// 关联 Observation ID 列表。
    #[serde(default)]
    pub related_observation_ids: Vec<String>,
    /// 向后兼容：结果标签（Reflector 恒写 `"failed"`）。
    #[serde(default)]
    pub outcome: String,
    /// 向后兼容：推荐 playbook 列表。
    #[serde(default)]
    pub recommended_playbooks: Vec<String>,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
}

impl ReflectorReport {
    /// 以 Python 默认值构造（`ReflectorReport(project_id=..., run_id=...)`）。
    #[must_use]
    pub fn new(project_id: ProjectId, run_id: RunId) -> Self {
        Self {
            id: default_reflector_id(),
            project_id,
            run_id,
            task_id: None,
            failure_type: default_reflector_failure_type(),
            root_cause_summary: String::new(),
            failure_modes: Vec::new(),
            lessons: Vec::new(),
            suggested_playbook_updates: Vec::new(),
            related_observation_ids: Vec::new(),
            outcome: String::new(),
            recommended_playbooks: Vec::new(),
            created_at: utcnow(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{assert_roundtrip, assert_wire_values};

    fn project_id() -> ProjectId {
        ProjectId::new("proj_test".to_string())
    }

    fn run_id() -> RunId {
        RunId::new("run_x".to_string())
    }

    #[test]
    fn agent_enums_match_python_wire_values() {
        assert_wire_values(&[
            (TerminationStatus::Continue, "continue"),
            (TerminationStatus::Pause, "pause"),
            (TerminationStatus::Complete, "complete"),
            (
                TerminationStatus::NeedsHumanDecision,
                "needs_human_decision",
            ),
        ]);
        assert_wire_values(&[
            (ObservationType::Progress, "progress"),
            (ObservationType::ToolResult, "tool_result"),
            (ObservationType::Blockage, "blockage"),
            (ObservationType::EvidenceGap, "evidence_gap"),
            (ObservationType::FailureBoundary, "failure_boundary"),
            (ObservationType::Contradiction, "contradiction"),
            (ObservationType::ToolFailure, "tool_failure"),
            (ObservationType::Hypothesis, "hypothesis"),
            (ObservationType::HypothesisUpdate, "hypothesis_update"),
            (ObservationType::UserNote, "user_note"),
            (ObservationType::Decision, "decision"),
            (ObservationType::TerminalState, "terminal_state"),
        ]);
        assert_wire_values(&[
            (WorkerLeaseStatus::Active, "active"),
            (WorkerLeaseStatus::Failed, "failed"),
            (WorkerLeaseStatus::Released, "released"),
            (WorkerLeaseStatus::Expired, "expired"),
            (WorkerLeaseStatus::Cancelled, "cancelled"),
            (WorkerLeaseStatus::Completed, "completed"),
        ]);
        assert_wire_values(&[
            (WorkerType::General, "general"),
            (WorkerType::Solver, "solver"),
            (WorkerType::Advisor, "advisor"),
            (WorkerType::Reflector, "reflector"),
            (WorkerType::ExternalAgent, "external_agent"),
        ]);
    }

    #[test]
    fn worker_profile_defaults_match_python() {
        let profile = WorkerProfile::new("scanner".to_string());
        assert!(profile.id.as_str().starts_with("worker_"));
        assert_eq!(profile.worker_type, WorkerType::General);
        assert!(profile.enabled);
        assert_eq!(profile.max_concurrent_leases, 1);
        assert!(profile.supported_audit_domains.is_empty());
        assert!(profile.provider_id.is_none());
        // 缺字段解析补 Python 默认值。
        let parsed: WorkerProfile = serde_json::from_str(r#"{"name":"n"}"#)
            .unwrap_or_else(|error| panic!("合法输入必须可解析: {error}"));
        assert!(parsed.enabled);
        assert_eq!(parsed.max_concurrent_leases, 1);
    }

    #[test]
    fn context_compression_report_defaults_match_python() {
        let report = ContextCompressionReport::new(project_id(), run_id(), "s".to_string());
        assert!(report.id.as_str().starts_with("ctxr_"));
        assert_eq!(report.strategy, "deterministic_v1");
        assert!(report.warnings.is_empty());
        assert!(report.source_counts.is_empty());
    }

    #[test]
    #[allow(clippy::float_cmp)] // 默认值是精确字面量，位级相等即语义相等
    fn termination_assessment_defaults_match_python() {
        let assessment = TerminationAssessment::new(project_id(), run_id());
        assert!(assessment.id.as_str().starts_with("term_"));
        assert_eq!(assessment.status, TerminationStatus::Continue);
        assert_eq!(assessment.goal_satisfied, None);
        assert_eq!(assessment.confidence, 0.0);
        assert_eq!(assessment.evidence_gap_count, 0);
    }

    #[test]
    fn observation_defaults_match_python() {
        let observation = Observation::new(project_id(), run_id(), "s".to_string());
        assert!(observation.id.as_str().starts_with("obs_"));
        assert_eq!(observation.observation_type, ObservationType::Progress);
        assert_eq!(observation.source, "worker");
    }

    #[test]
    fn worker_lease_defaults_match_python() {
        let expires = "2026-08-24T12:01:00Z"
            .parse()
            .unwrap_or_else(|error| panic!("固定时间必须可解析: {error}"));
        let lease = WorkerLease::new(
            project_id(),
            run_id(),
            IntentId::new("intent_1".to_string()),
            "worker".to_string(),
            expires,
        );
        assert!(lease.id.as_str().starts_with("lease_"));
        assert_eq!(lease.status, WorkerLeaseStatus::Active);
        assert_eq!(lease.leased_at, None);
        assert_roundtrip(&WorkerLeaseStatus::Expired);
    }

    #[test]
    fn termination_assessment_rejects_unknown_fields() {
        let result: Result<TerminationAssessment, _> =
            serde_json::from_str(r#"{"project_id":"p","run_id":"r","extra":1}"#);
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }

    #[test]
    fn context_pack_defaults_match_python() {
        let pack = ContextPack::new(
            project_id(),
            run_id(),
            "strategy_board_maintainer".to_string(),
            "s".to_string(),
        );
        assert!(pack.id.as_str().starts_with("ctx_"));
        assert_eq!(pack.task_id, None);
        assert!(pack.facts.is_empty());
        assert_eq!(pack.token_budget, 2048);
        assert_eq!(pack.compression_strategy, "deterministic_v1");
        assert!(pack.operation_log_records.is_empty());
        assert!(pack.metadata.is_empty());
    }

    #[test]
    fn context_pack_rejects_unknown_fields() {
        let result: Result<ContextPack, _> = serde_json::from_str(
            r#"{"project_id":"p","run_id":"r","purpose":"x","summary":"s","extra":1}"#,
        );
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }
}
