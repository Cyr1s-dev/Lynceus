//! Mission 层模型 —— `server/core/models/mission.py` 的移植。
//!
//! 枚举部分先行（wire 值冻结守护），实体结构体（`Mission` /
//! `MissionGoalContract` / `Branch`）字段顺序、默认值、serde wire 格式与
//! Python 侧逐字节一致——这是差分对拍 payload 一致性的前提。Python 侧的
//! `model_validator` 语义通过 `validated()`（构造路径）与 serde
//! `try_from`（解析路径）双入口镜像。

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::common::StrMap;
use crate::common::Timestamp;
use crate::common::new_id;
use crate::common::utcnow;
use crate::ids::BranchId;
use crate::ids::MissionId;
use crate::ids::ProjectId;
use crate::ids::RunId;
use crate::ids::UserDirectiveId;
use crate::lifecycle::EvidenceKind;


/// Mission 生命周期（`MissionStatus`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionStatus {
    /// 草稿，尚未启动。
    Draft,
    /// 运行中。
    Running,
    /// 已暂停（用户或系统指令）。
    Paused,
    /// 等待人工决策（decision gate / HITL）。
    WaitingForDecision,
    /// 已完成（须通过 MGATE 出口判定）。
    Completed,
    /// 已失败。
    Failed,
    /// 已取消。
    Cancelled,
}

impl MissionStatus {
    /// wire 值（Python `.value` 镜像，用于事件 status 字段与文本拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            MissionStatus::Draft => "draft",
            MissionStatus::Running => "running",
            MissionStatus::Paused => "paused",
            MissionStatus::WaitingForDecision => "waiting_for_decision",
            MissionStatus::Completed => "completed",
            MissionStatus::Failed => "failed",
            MissionStatus::Cancelled => "cancelled",
        }
    }
}

/// 三级审批模式（`ApprovalMode`）：从“每次工具调用前询问”到“完全放开”。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// 每个敏感操作前必须获得人工批准。
    AskForApproval,
    /// 预授权：系统按既定策略自行批准。
    ApproveForMe,
    /// 完全放开，无需审批。
    FullAccess,
}

impl ApprovalMode {
    /// wire 值（Python `.value` 镜像，用于 run config 写入与文本拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ApprovalMode::AskForApproval => "ask_for_approval",
            ApprovalMode::ApproveForMe => "approve_for_me",
            ApprovalMode::FullAccess => "full_access",
        }
    }

    /// 从 wire 值解析（Python `ApprovalMode(value)` 的可判别对应）。
    ///
    /// # Errors
    ///
    /// wire 值不在枚举内。
    pub fn parse(raw: &str) -> Result<Self, String> {
        Ok(match raw {
            "ask_for_approval" => ApprovalMode::AskForApproval,
            "approve_for_me" => ApprovalMode::ApproveForMe,
            "full_access" => ApprovalMode::FullAccess,
            other => return Err(other.to_string()),
        })
    }
}

/// Branch（探索分支）生命周期（`BranchStatus`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchStatus {
    /// 已提出，尚未激活。
    Proposed,
    /// 激活执行中。
    Active,
    /// 被阻塞（依赖未满足 / 预算耗尽）。
    Blocked,
    /// 已成功。
    Succeeded,
    /// 已失败。
    Failed,
    /// 已放弃（用户指令或策略）。
    Abandoned,
    /// 被后续分支取代。
    Superseded,
}

impl BranchStatus {
    /// wire 值（Python `.value` 镜像，用于文本拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            BranchStatus::Proposed => "proposed",
            BranchStatus::Active => "active",
            BranchStatus::Blocked => "blocked",
            BranchStatus::Succeeded => "succeeded",
            BranchStatus::Failed => "failed",
            BranchStatus::Abandoned => "abandoned",
            BranchStatus::Superseded => "superseded",
        }
    }
}

/// 用户指令类型（`UserDirectiveType`）：HITL 干预动作词表。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserDirectiveType {
    /// 追加需求。
    AddRequirement,
    /// 暂停。
    Pause,
    /// 恢复。
    Resume,
    /// 优先某个分支。
    PrioritizeBranch,
    /// 放弃某个分支。
    AbandonBranch,
    /// 重开某个分支。
    ReopenBranch,
    /// 回滚补丁。
    RollbackPatch,
    /// 收窄范围。
    NarrowScope,
    /// 排除范围。
    ExcludeScope,
    /// 向 Agent 提问。
    AskQuestion,
}

impl UserDirectiveType {
    /// wire 值（Python `.value` 镜像，用于事件标题与文本拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            UserDirectiveType::AddRequirement => "add_requirement",
            UserDirectiveType::Pause => "pause",
            UserDirectiveType::Resume => "resume",
            UserDirectiveType::PrioritizeBranch => "prioritize_branch",
            UserDirectiveType::AbandonBranch => "abandon_branch",
            UserDirectiveType::ReopenBranch => "reopen_branch",
            UserDirectiveType::RollbackPatch => "rollback_patch",
            UserDirectiveType::NarrowScope => "narrow_scope",
            UserDirectiveType::ExcludeScope => "exclude_scope",
            UserDirectiveType::AskQuestion => "ask_question",
        }
    }

    /// 从 wire 值解析（Python `UserDirectiveType(value)` 的可判别对应）。
    ///
    /// # Errors
    ///
    /// wire 值不在枚举内。
    pub fn parse(raw: &str) -> Result<Self, String> {
        Ok(match raw {
            "add_requirement" => UserDirectiveType::AddRequirement,
            "pause" => UserDirectiveType::Pause,
            "resume" => UserDirectiveType::Resume,
            "prioritize_branch" => UserDirectiveType::PrioritizeBranch,
            "abandon_branch" => UserDirectiveType::AbandonBranch,
            "reopen_branch" => UserDirectiveType::ReopenBranch,
            "rollback_patch" => UserDirectiveType::RollbackPatch,
            "narrow_scope" => UserDirectiveType::NarrowScope,
            "exclude_scope" => UserDirectiveType::ExcludeScope,
            "ask_question" => UserDirectiveType::AskQuestion,
            other => return Err(other.to_string()),
        })
    }
}

/// 用户指令状态（`UserDirectiveStatus`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserDirectiveStatus {
    /// 待处理。
    Pending,
    /// 已应用。
    Applied,
    /// 已拒绝。
    Rejected,
    /// 被后续指令取代。
    Superseded,
}

impl UserDirectiveStatus {
    /// wire 值（Python `.value` 镜像，用于事件 status 字段）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            UserDirectiveStatus::Pending => "pending",
            UserDirectiveStatus::Applied => "applied",
            UserDirectiveStatus::Rejected => "rejected",
            UserDirectiveStatus::Superseded => "superseded",
        }
    }
}

/// 目标契约是否已通过机器可校验的分类（`GoalContractStatus`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalContractStatus {
    /// 已确认的完成契约。
    Resolved,
    /// 需要人工评审或分类。
    NeedsReview,
}

/// 完成门槛的结果类别（`GoalOutcomeType`）。
///
/// 自然语言在 intake 时被分类为其中一个值；终止状态机只消费这个结构化
/// 契约，绝不从用户提示的措辞里重新发现意图。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalOutcomeType {
    /// 捕获 flag。
    FlagCapture,
    /// 已确认的 Finding。
    ConfirmedFinding,
    /// 已验证的 Evidence。
    VerifiedEvidence,
    /// 覆盖率达标。
    Coverage,
    /// 自定义（未分类）。
    Custom,
}

impl GoalOutcomeType {
    /// wire 值（Python `.value` 镜像，用于文本拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            GoalOutcomeType::FlagCapture => "flag_capture",
            GoalOutcomeType::ConfirmedFinding => "confirmed_finding",
            GoalOutcomeType::VerifiedEvidence => "verified_evidence",
            GoalOutcomeType::Coverage => "coverage",
            GoalOutcomeType::Custom => "custom",
        }
    }
}

/// 目标契约的产生来源（`GoalContractSource`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalContractSource {
    /// 操作员显式给出。
    Operator,
    /// intake 模型分类。
    IntakeModel,
    /// 确定性回退规则。
    DeterministicFallback,
    /// 历史数据（未分类）。
    Legacy,
}

/// 目标契约校验失败（镜像 pydantic 字段约束）。
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
pub enum GoalContractError {
    /// `minimum_count` 超出 `[1, 1000]`。
    #[error("minimum_count {value} is out of range [1, 1000]")]
    MinimumCountOutOfRange {
        /// 越界的取值。
        value: i64,
    },
    /// `confidence` 超出 `[0.0, 1.0]`。
    #[error("confidence {value} is out of range [0.0, 1.0]")]
    ConfidenceOutOfRange {
        /// 越界的取值。
        value: f64,
    },
}

/// Mission 成功条件的类型化、fail-closed 定义（`MissionGoalContract`）。
///
/// 不可绕过门（`_enforce_non_bypassable_gates`）在 [`Self::validated`] 中
/// 镜像：模型/操作员可以分类意图，但无法削弱证明门。构造与解析
/// （`model_validate_json` / serde）两条路径都会执行同一套规范化。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "GoalContractWire")]
pub struct MissionGoalContract {
    /// 契约 schema 版本。
    pub schema_version: String,
    /// 契约是否已分类确认。
    pub status: GoalContractStatus,
    /// 结果类别。
    pub outcome_type: GoalOutcomeType,
    /// 人类可读的成功条件描述。
    pub description: String,
    /// 最少满足数量。
    pub minimum_count: i64,
    /// 命中的 Finding 规则 ID。
    pub finding_rule_ids: Vec<String>,
    /// 接受的 Evidence 类型。
    pub evidence_kinds: Vec<EvidenceKind>,
    /// 是否要求已确认的 Finding。
    pub require_confirmed_findings: bool,
    /// 是否要求出处（Provenance）。
    pub require_provenance: bool,
    /// 满足后是否自动完成 Mission。
    pub auto_complete: bool,
    /// 契约来源。
    pub source: GoalContractSource,
    /// 分类置信度。
    pub confidence: f64,
    /// 分类理由。
    pub rationale: Option<String>,
}

impl Default for MissionGoalContract {
    fn default() -> Self {
        // 与 Python `MissionGoalContract()` 逐字段一致；custom 出口的
        // else 分支不改变任何默认值，字面量即规范化结果。
        Self {
            schema_version: "goal-contract.v1".to_string(),
            status: GoalContractStatus::NeedsReview,
            outcome_type: GoalOutcomeType::Custom,
            description: "Success condition requires classification or operator review".to_string(),
            minimum_count: 1,
            finding_rule_ids: Vec::new(),
            evidence_kinds: Vec::new(),
            require_confirmed_findings: true,
            require_provenance: true,
            auto_complete: false,
            source: GoalContractSource::Legacy,
            confidence: 0.0,
            rationale: None,
        }
    }
}

impl MissionGoalContract {
    /// 校验字段约束并应用不可绕过门，返回规范化后的契约。
    ///
    /// 消费 `self`：规范化可能覆盖字段（如 `flag_capture` 强制
    /// `finding_rule_ids`），返回值才是可持久化状态。
    ///
    /// # Errors
    /// - [`GoalContractError::MinimumCountOutOfRange`]；
    /// - [`GoalContractError::ConfidenceOutOfRange`]。
    pub fn validated(mut self) -> Result<Self, GoalContractError> {
        if !(1..=1000).contains(&self.minimum_count) {
            return Err(GoalContractError::MinimumCountOutOfRange {
                value: self.minimum_count,
            });
        }
        if !(0.0..=1.0).contains(&self.confidence) {
            return Err(GoalContractError::ConfidenceOutOfRange {
                value: self.confidence,
            });
        }
        let resolved = self.status == GoalContractStatus::Resolved;
        match self.outcome_type {
            GoalOutcomeType::FlagCapture => {
                self.finding_rule_ids = vec!["web_exploit.flag_capture".to_string()];
                self.require_confirmed_findings = true;
                self.require_provenance = true;
                self.auto_complete = resolved;
            }
            GoalOutcomeType::ConfirmedFinding => {
                self.require_confirmed_findings = true;
                self.require_provenance = true;
                self.auto_complete = resolved;
            }
            GoalOutcomeType::VerifiedEvidence => {
                self.require_provenance = true;
                self.auto_complete = resolved;
            }
            GoalOutcomeType::Coverage => {
                self.finding_rule_ids = Vec::new();
                self.evidence_kinds = Vec::new();
                self.require_confirmed_findings = false;
                self.auto_complete = resolved;
            }
            GoalOutcomeType::Custom => {
                self.status = GoalContractStatus::NeedsReview;
                self.auto_complete = false;
            }
        }
        Ok(self)
    }
}

/// [`MissionGoalContract`] 的解析镜像：缺省字段取 Python 默认值，
/// 未知字段拒绝（`extra="forbid"`），解析后走 `validated()`。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct GoalContractWire {
    schema_version: String,
    status: GoalContractStatus,
    outcome_type: GoalOutcomeType,
    description: String,
    minimum_count: i64,
    finding_rule_ids: Vec<String>,
    evidence_kinds: Vec<EvidenceKind>,
    require_confirmed_findings: bool,
    require_provenance: bool,
    auto_complete: bool,
    source: GoalContractSource,
    confidence: f64,
    rationale: Option<String>,
}

impl Default for GoalContractWire {
    fn default() -> Self {
        let base = MissionGoalContract::default();
        Self {
            schema_version: base.schema_version,
            status: base.status,
            outcome_type: base.outcome_type,
            description: base.description,
            minimum_count: base.minimum_count,
            finding_rule_ids: base.finding_rule_ids,
            evidence_kinds: base.evidence_kinds,
            require_confirmed_findings: base.require_confirmed_findings,
            require_provenance: base.require_provenance,
            auto_complete: base.auto_complete,
            source: base.source,
            confidence: base.confidence,
            rationale: base.rationale,
        }
    }
}

impl TryFrom<GoalContractWire> for MissionGoalContract {
    type Error = GoalContractError;

    fn try_from(wire: GoalContractWire) -> Result<Self, Self::Error> {
        Self {
            schema_version: wire.schema_version,
            status: wire.status,
            outcome_type: wire.outcome_type,
            description: wire.description,
            minimum_count: wire.minimum_count,
            finding_rule_ids: wire.finding_rule_ids,
            evidence_kinds: wire.evidence_kinds,
            require_confirmed_findings: wire.require_confirmed_findings,
            require_provenance: wire.require_provenance,
            auto_complete: wire.auto_complete,
            source: wire.source,
            confidence: wire.confidence,
            rationale: wire.rationale,
        }
        .validated()
    }
}

fn default_mission_id() -> MissionId {
    MissionId::new(new_id("mission"))
}

fn default_approval_mode() -> ApprovalMode {
    ApprovalMode::AskForApproval
}

fn default_mission_status() -> MissionStatus {
    MissionStatus::Draft
}

fn default_created_at() -> Timestamp {
    utcnow()
}

fn default_updated_at() -> Timestamp {
    utcnow()
}

fn default_created_by_user() -> String {
    "user".to_string()
}

fn default_created_by_branch_generator() -> String {
    "branch_generator".to_string()
}

/// Mission：被提升为一等编排对象的用户目标（`Mission`）。
///
/// 字段顺序、默认值与 wire 格式冻结自 Python 模型；`metadata` 键序保持
/// 插入序（`serde_json` `preserve_order`），与 Python dict 一致。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mission {
    /// Mission 标识符。
    #[serde(default = "default_mission_id")]
    pub id: MissionId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 用户的原始目标描述。
    pub user_goal: String,
    /// 展示标题。
    #[serde(default)]
    pub title: Option<String>,
    /// 目标物键值描述（如 `{"url": ...}`，值恒为字符串）。
    #[serde(default)]
    pub target: StrMap,
    /// 执行约束。
    #[serde(default)]
    pub constraints: Vec<String>,
    /// 成功标准（自然语言）。
    #[serde(default)]
    pub success_criteria: Vec<String>,
    /// 机器可校验的完成契约。
    #[serde(default)]
    pub goal_contract: MissionGoalContract,
    /// 标签（Python 侧上限 16 个）。
    #[serde(default)]
    pub tags: Vec<String>,
    /// 分类。
    #[serde(default)]
    pub category: Option<String>,
    /// 审批模式。
    #[serde(default = "default_approval_mode")]
    pub approval_mode: ApprovalMode,
    /// 是否归档。
    #[serde(default)]
    pub archived: bool,
    /// 生命周期状态。
    #[serde(default = "default_mission_status")]
    pub status: MissionStatus,
    /// 当前活跃 Run。
    #[serde(default)]
    pub active_run_id: Option<RunId>,
    /// 创建时间。
    #[serde(default = "default_created_at")]
    pub created_at: Timestamp,
    /// 最后更新时间。
    #[serde(default = "default_updated_at")]
    pub updated_at: Timestamp,
    /// 结束时间。
    #[serde(default)]
    pub finished_at: Option<Timestamp>,
    /// 创建者。
    #[serde(default = "default_created_by_user")]
    pub created_by: String,
    /// 附加元数据（键序 = 插入序）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

impl Mission {
    /// 以 Python 默认值构造（`Mission(project_id=..., user_goal=...)`）。
    #[must_use]
    pub fn new(project_id: ProjectId, user_goal: String) -> Self {
        Self {
            id: default_mission_id(),
            project_id,
            user_goal,
            title: None,
            target: StrMap::new(),
            constraints: Vec::new(),
            success_criteria: Vec::new(),
            goal_contract: MissionGoalContract::default(),
            tags: Vec::new(),
            category: None,
            approval_mode: default_approval_mode(),
            archived: false,
            status: default_mission_status(),
            active_run_id: None,
            created_at: utcnow(),
            updated_at: utcnow(),
            finished_at: None,
            created_by: default_created_by_user(),
            metadata: Map::new(),
        }
    }
}

fn default_branch_id() -> BranchId {
    BranchId::new(new_id("branch"))
}

fn default_branch_status() -> BranchStatus {
    BranchStatus::Proposed
}

fn default_branch_priority() -> i64 {
    50
}

fn default_branch_confidence() -> f64 {
    0.5
}

fn default_branch_budget_steps() -> i64 {
    4
}

/// Branch：Mission 内可证伪的假设或审计路线（`Branch`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Branch {
    /// Branch 标识符。
    #[serde(default = "default_branch_id")]
    pub id: BranchId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Mission。
    pub mission_id: MissionId,
    /// 关联 Run。
    #[serde(default)]
    pub run_id: Option<RunId>,
    /// 父 Branch（派生关系）。
    #[serde(default)]
    pub parent_branch_id: Option<BranchId>,
    /// 标题。
    pub title: String,
    /// 可证伪的假设。
    pub hypothesis: String,
    /// 立论理由。
    #[serde(default)]
    pub rationale: String,
    /// 生命周期状态。
    #[serde(default = "default_branch_status")]
    pub status: BranchStatus,
    /// 优先级（Python 侧约束 `[0, 100]`）。
    #[serde(default = "default_branch_priority")]
    pub priority: i64,
    /// 置信度（Python 侧约束 `[0.0, 1.0]`）。
    #[serde(default = "default_branch_confidence")]
    pub confidence: f64,
    /// 步数预算（Python 侧约束 `>= 1`）。
    #[serde(default = "default_branch_budget_steps")]
    pub budget_steps: i64,
    /// 已消耗步数（Python 侧约束 `>= 0`）。
    #[serde(default)]
    pub steps_used: i64,
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
    /// 指派的 Worker。
    #[serde(default)]
    pub assigned_worker_id: Option<String>,
    /// 创建者。
    #[serde(default = "default_created_by_branch_generator")]
    pub created_by: String,
    /// 创建时间。
    #[serde(default = "default_created_at")]
    pub created_at: Timestamp,
    /// 最后更新时间。
    #[serde(default = "default_updated_at")]
    pub updated_at: Timestamp,
    /// 附加元数据（键序 = 插入序）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

impl Branch {
    /// 以 Python 默认值构造（`Branch(project_id=..., mission_id=...,
    /// title=..., hypothesis=...)`）。
    #[must_use]
    pub fn new(
        project_id: ProjectId,
        mission_id: MissionId,
        title: String,
        hypothesis: String,
    ) -> Self {
        Self {
            id: default_branch_id(),
            project_id,
            mission_id,
            run_id: None,
            parent_branch_id: None,
            title,
            hypothesis,
            rationale: String::new(),
            status: default_branch_status(),
            priority: default_branch_priority(),
            confidence: default_branch_confidence(),
            budget_steps: default_branch_budget_steps(),
            steps_used: 0,
            related_fact_ids: Vec::new(),
            related_evidence_ids: Vec::new(),
            related_finding_ids: Vec::new(),
            related_tool_invocation_ids: Vec::new(),
            assigned_worker_id: None,
            created_by: default_created_by_branch_generator(),
            created_at: utcnow(),
            updated_at: utcnow(),
            metadata: Map::new(),
        }
    }
}

fn default_directive_id() -> UserDirectiveId {
    UserDirectiveId::new(new_id("directive"))
}

fn default_directive_status() -> UserDirectiveStatus {
    UserDirectiveStatus::Pending
}

fn default_directive_created_by() -> String {
    "user".to_string()
}

/// 用户在 Mission 中途下达的控制指令的只追加记录（`UserDirective`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserDirective {
    /// 指令标识符。
    #[serde(default = "default_directive_id")]
    pub id: UserDirectiveId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Mission。
    pub mission_id: MissionId,
    /// 关联 Run。
    #[serde(default)]
    pub run_id: Option<RunId>,
    /// 关联 Branch。
    #[serde(default)]
    pub branch_id: Option<BranchId>,
    /// 指令类型。
    pub directive_type: UserDirectiveType,
    /// 指令内容。
    pub content: String,
    /// 解析后的结构化意图（键序 = 插入序）。
    #[serde(default)]
    pub parsed_intent: Map<String, Value>,
    /// 生命周期状态。
    #[serde(default = "default_directive_status")]
    pub status: UserDirectiveStatus,
    /// 创建时间。
    #[serde(default = "default_created_at")]
    pub created_at: Timestamp,
    /// 应用时间。
    #[serde(default)]
    pub applied_at: Option<Timestamp>,
    /// 创建者。
    #[serde(default = "default_directive_created_by")]
    pub created_by: String,
    /// 附加元数据（键序 = 插入序）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

impl UserDirective {
    /// 以 Python 默认值构造（`UserDirective(project_id=..., mission_id=...,
    /// directive_type=..., content=...)`）。
    #[must_use]
    pub fn new(
        project_id: ProjectId,
        mission_id: MissionId,
        directive_type: UserDirectiveType,
        content: String,
    ) -> Self {
        Self {
            id: default_directive_id(),
            project_id,
            mission_id,
            run_id: None,
            branch_id: None,
            directive_type,
            content,
            parsed_intent: Map::new(),
            status: default_directive_status(),
            created_at: utcnow(),
            applied_at: None,
            created_by: default_directive_created_by(),
            metadata: Map::new(),
        }
    }
}

/// Router / runtime 观测的能力缺口严重度（`CapabilityGapSeverity`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityGapSeverity {
    /// 纯信息性。
    Info,
    /// 轻微。
    Low,
    /// 中等。
    Medium,
    /// 严重。
    High,
}

fn default_gap_severity() -> CapabilityGapSeverity {
    CapabilityGapSeverity::Info
}

/// Router 输出：描述一个 Branch 应如何被推进（`CapabilityDispatch`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityDispatch {
    /// 目标 Branch。
    pub branch_id: BranchId,
    /// 选定的求解器。
    #[serde(default)]
    pub solver: Option<String>,
    /// 审计域。
    #[serde(default)]
    pub audit_domain: Option<String>,
    /// 附加配置（键序 = 插入序）。
    #[serde(default)]
    pub config: Map<String, Value>,
    /// 能力名称。
    #[serde(default)]
    pub capability_name: Option<String>,
    /// 决策理由。
    pub rationale: String,
    /// 是否命中能力缺口。
    #[serde(default)]
    pub capability_gap: bool,
    /// 缺口摘要。
    #[serde(default)]
    pub gap_summary: Option<String>,
    /// 缺口严重度。
    #[serde(default = "default_gap_severity")]
    pub gap_severity: CapabilityGapSeverity,
}

/// Mission 启动/恢复操作的返回封装（`MissionStartResult`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MissionStartResult {
    /// 启动后的 Mission。
    pub mission: Mission,
    /// 本次 Run。
    pub run_id: RunId,
    /// 本次 Run 的 Branch 列表。
    #[serde(default)]
    pub branches: Vec<Branch>,
}

/// wire 值冻结守护：与 `server/core/models/mission.py` 逐值比对。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{assert_roundtrip, assert_wire_values};

    fn timestamp() -> Timestamp {
        "2026-08-24T12:00:00.123456Z"
            .parse()
            .unwrap_or_else(|error| panic!("固定时间必须可解析: {error}"))
    }

    #[test]
    fn mission_enums_match_python_wire_values() {
        assert_wire_values(&[
            (MissionStatus::Draft, "draft"),
            (MissionStatus::Running, "running"),
            (MissionStatus::Paused, "paused"),
            (MissionStatus::WaitingForDecision, "waiting_for_decision"),
            (MissionStatus::Completed, "completed"),
            (MissionStatus::Failed, "failed"),
            (MissionStatus::Cancelled, "cancelled"),
        ]);
        assert_wire_values(&[
            (ApprovalMode::AskForApproval, "ask_for_approval"),
            (ApprovalMode::ApproveForMe, "approve_for_me"),
            (ApprovalMode::FullAccess, "full_access"),
        ]);
        assert_wire_values(&[
            (BranchStatus::Proposed, "proposed"),
            (BranchStatus::Active, "active"),
            (BranchStatus::Blocked, "blocked"),
            (BranchStatus::Succeeded, "succeeded"),
            (BranchStatus::Failed, "failed"),
            (BranchStatus::Abandoned, "abandoned"),
            (BranchStatus::Superseded, "superseded"),
        ]);
        assert_wire_values(&[
            (UserDirectiveType::AddRequirement, "add_requirement"),
            (UserDirectiveType::Pause, "pause"),
            (UserDirectiveType::Resume, "resume"),
            (UserDirectiveType::PrioritizeBranch, "prioritize_branch"),
            (UserDirectiveType::AbandonBranch, "abandon_branch"),
            (UserDirectiveType::ReopenBranch, "reopen_branch"),
            (UserDirectiveType::RollbackPatch, "rollback_patch"),
            (UserDirectiveType::NarrowScope, "narrow_scope"),
            (UserDirectiveType::ExcludeScope, "exclude_scope"),
            (UserDirectiveType::AskQuestion, "ask_question"),
        ]);
        assert_wire_values(&[
            (UserDirectiveStatus::Pending, "pending"),
            (UserDirectiveStatus::Applied, "applied"),
            (UserDirectiveStatus::Rejected, "rejected"),
            (UserDirectiveStatus::Superseded, "superseded"),
        ]);
        assert_wire_values(&[
            (GoalContractStatus::Resolved, "resolved"),
            (GoalContractStatus::NeedsReview, "needs_review"),
        ]);
        assert_wire_values(&[
            (GoalOutcomeType::FlagCapture, "flag_capture"),
            (GoalOutcomeType::ConfirmedFinding, "confirmed_finding"),
            (GoalOutcomeType::VerifiedEvidence, "verified_evidence"),
            (GoalOutcomeType::Coverage, "coverage"),
            (GoalOutcomeType::Custom, "custom"),
        ]);
        assert_wire_values(&[
            (GoalContractSource::Operator, "operator"),
            (GoalContractSource::IntakeModel, "intake_model"),
            (
                GoalContractSource::DeterministicFallback,
                "deterministic_fallback",
            ),
            (GoalContractSource::Legacy, "legacy"),
        ]);
    }

    #[test]
    fn mission_enums_roundtrip_through_json() {
        for status in [
            MissionStatus::Draft,
            MissionStatus::Running,
            MissionStatus::Paused,
            MissionStatus::WaitingForDecision,
            MissionStatus::Completed,
            MissionStatus::Failed,
            MissionStatus::Cancelled,
        ] {
            assert_roundtrip(&status);
        }
        for mode in [
            ApprovalMode::AskForApproval,
            ApprovalMode::ApproveForMe,
            ApprovalMode::FullAccess,
        ] {
            assert_roundtrip(&mode);
        }
        for status in [
            BranchStatus::Proposed,
            BranchStatus::Active,
            BranchStatus::Blocked,
            BranchStatus::Succeeded,
            BranchStatus::Failed,
            BranchStatus::Abandoned,
            BranchStatus::Superseded,
        ] {
            assert_roundtrip(&status);
        }
    }

    #[test]
    fn unknown_wire_value_is_rejected() {
        let result: Result<MissionStatus, _> = serde_json::from_str("\"warp_speed\"");
        assert!(result.is_err(), "未知 wire 值必须被拒绝而不是静默映射");
    }

    #[test]
    fn goal_contract_default_matches_python_wire_bytes() {
        // 期望串逐字节来自 scripts/probe_parity_wire.py 探针输出。
        let expected = concat!(
            r#"{"schema_version":"goal-contract.v1","status":"needs_review","#,
            r#""outcome_type":"custom","description":"Success condition requires "#,
            r#"classification or operator review","minimum_count":1,"finding_rule_ids":[],"#,
            r#""evidence_kinds":[],"require_confirmed_findings":true,"require_provenance":true,"#,
            r#""auto_complete":false,"source":"legacy","confidence":0.0,"rationale":null}"#
        );
        let json = serde_json::to_string(&MissionGoalContract::default())
            .unwrap_or_else(|error| panic!("契约序列化不会失败: {error}"));
        assert_eq!(json, expected);
    }

    #[test]
    fn goal_contract_flag_capture_gates_cannot_be_weakened() {
        let contract = MissionGoalContract {
            status: GoalContractStatus::Resolved,
            outcome_type: GoalOutcomeType::FlagCapture,
            minimum_count: 2,
            finding_rule_ids: vec!["attempted.bypass".to_string()],
            require_confirmed_findings: false,
            require_provenance: false,
            auto_complete: true,
            confidence: 0.8,
            rationale: Some("capture the flag".to_string()),
            ..MissionGoalContract::default()
        }
        .validated()
        .unwrap_or_else(|error| panic!("合法契约必须通过校验: {error}"));

        assert_eq!(contract.finding_rule_ids, ["web_exploit.flag_capture"]);
        assert!(contract.require_confirmed_findings);
        assert!(contract.require_provenance);
        // resolved 状态允许自动完成。
        assert!(contract.auto_complete);
        assert_eq!(contract.minimum_count, 2);
        // serde 往返必须逐位保留 f64（0.8 无法精确表示，近似比较会漏掉
        // 精度回归；to_bits 是精确断言）。
        assert_eq!(contract.confidence.to_bits(), 0.8_f64.to_bits());

        // 序列化字节与 Python 探针一致。
        let json = serde_json::to_string(&contract)
            .unwrap_or_else(|error| panic!("契约序列化不会失败: {error}"));
        assert!(
            json.contains(r#""finding_rule_ids":["web_exploit.flag_capture"]"#),
            "门禁规则必须出现在 wire 输出: {json}"
        );
    }

    #[test]
    fn goal_contract_custom_forces_needs_review() {
        let contract = MissionGoalContract {
            status: GoalContractStatus::Resolved,
            outcome_type: GoalOutcomeType::Custom,
            auto_complete: true,
            ..MissionGoalContract::default()
        }
        .validated()
        .unwrap_or_else(|error| panic!("合法契约必须通过校验: {error}"));
        assert_eq!(contract.status, GoalContractStatus::NeedsReview);
        assert!(!contract.auto_complete);
    }

    #[test]
    fn goal_contract_coverage_clears_rule_gates() {
        let contract = MissionGoalContract {
            status: GoalContractStatus::Resolved,
            outcome_type: GoalOutcomeType::Coverage,
            finding_rule_ids: vec!["stale".to_string()],
            evidence_kinds: vec![EvidenceKind::ToolOutput],
            require_confirmed_findings: true,
            ..MissionGoalContract::default()
        }
        .validated()
        .unwrap_or_else(|error| panic!("合法契约必须通过校验: {error}"));
        assert!(contract.finding_rule_ids.is_empty());
        assert!(contract.evidence_kinds.is_empty());
        assert!(!contract.require_confirmed_findings);
        assert!(contract.auto_complete);
        // require_provenance 未被 coverage 分支触碰。
        assert!(contract.require_provenance);
    }

    #[test]
    fn goal_contract_rejects_out_of_range_fields() {
        let too_large = MissionGoalContract {
            minimum_count: 1001,
            ..MissionGoalContract::default()
        }
        .validated();
        assert_eq!(
            too_large.err(),
            Some(GoalContractError::MinimumCountOutOfRange { value: 1001 })
        );

        let too_confident = MissionGoalContract {
            confidence: 1.5,
            ..MissionGoalContract::default()
        }
        .validated();
        assert_eq!(
            too_confident.err(),
            Some(GoalContractError::ConfidenceOutOfRange { value: 1.5 })
        );
    }

    #[test]
    fn goal_contract_deserialize_applies_gates() {
        // Python: model_validate_json 同样执行规范化；serde try_from 镜像之。
        let json = r#"{
            "schema_version":"goal-contract.v1","status":"resolved",
            "outcome_type":"flag_capture","minimum_count":1,
            "finding_rule_ids":["attempted.bypass"],
            "evidence_kinds":[],"require_confirmed_findings":false,
            "require_provenance":true,"auto_complete":false,"source":"legacy",
            "confidence":0.5,"rationale":null,
            "description":"Success condition requires classification or operator review"
        }"#;
        let contract: MissionGoalContract = serde_json::from_str(json)
            .unwrap_or_else(|error| panic!("合法 JSON 必须可解析: {error}"));
        assert_eq!(contract.finding_rule_ids, ["web_exploit.flag_capture"]);
        assert!(contract.require_confirmed_findings);
        assert!(contract.auto_complete);
    }

    #[test]
    fn goal_contract_deserialize_rejects_unknown_fields() {
        let result: Result<MissionGoalContract, _> =
            serde_json::from_str(r#"{"outcome_type":"custom","evil":true}"#);
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }

    #[test]
    fn mission_serializes_to_python_wire_bytes() {
        // 期望串逐字节来自 scripts/probe_parity_wire.py 探针输出。
        let expected = concat!(
            r#"{"id":"mission_fix_0001","project_id":"proj_parity","#,
            r#""user_goal":"Find injected sinks","title":null,"#,
            r#""target":{"url":"https://target.example","note":"中文注释"},"#,
            r#""constraints":["stay in scope"],"#,
            r#""success_criteria":["flag captured"],"#,
            r#""goal_contract":{"schema_version":"goal-contract.v1","status":"needs_review","#,
            r#""outcome_type":"custom","description":"Success condition requires "#,
            r#"classification or operator review","minimum_count":1,"finding_rule_ids":[],"#,
            r#""evidence_kinds":[],"require_confirmed_findings":true,"require_provenance":true,"#,
            r#""auto_complete":false,"source":"legacy","confidence":0.0,"rationale":null},"#,
            r#""tags":["web","audit"],"category":"web-audit","#,
            r#""approval_mode":"ask_for_approval","archived":false,"status":"running","#,
            r#""active_run_id":null,"created_at":"2026-08-24T12:00:00.123456Z","#,
            r#""updated_at":"2026-08-24T12:00:00.123456Z","finished_at":null,"#,
            r#""created_by":"user","metadata":{"zz_meta":"last","aa_meta":"first","count":2}}"#
        );
        let target = [("url", "https://target.example"), ("note", "中文注释")]
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect::<StrMap>();
        let mission = Mission {
            id: MissionId::new("mission_fix_0001".to_string()),
            project_id: ProjectId::new("proj_parity".to_string()),
            user_goal: "Find injected sinks".to_string(),
            title: None,
            target,

            constraints: vec!["stay in scope".to_string()],
            success_criteria: vec!["flag captured".to_string()],
            goal_contract: MissionGoalContract::default(),
            tags: vec!["web".to_string(), "audit".to_string()],
            category: Some("web-audit".to_string()),
            approval_mode: ApprovalMode::AskForApproval,
            archived: false,
            status: MissionStatus::Running,
            active_run_id: None,
            created_at: timestamp(),
            updated_at: timestamp(),
            finished_at: None,
            created_by: "user".to_string(),
            metadata: [
                ("zz_meta", Value::from("last")),
                ("aa_meta", Value::from("first")),
                ("count", Value::from(2)),
            ]
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
        };
        let json = serde_json::to_string(&mission)
            .unwrap_or_else(|error| panic!("Mission 序列化不会失败: {error}"));
        assert_eq!(json, expected);

        // 反序列化往返保持相等（含 metadata 键序无关的结构相等）。
        let back: Mission = serde_json::from_str(&json)
            .unwrap_or_else(|error| panic!("自身输出必须可解析: {error}"));
        assert_eq!(back, mission);
    }

    #[test]
    fn branch_serializes_to_python_wire_bytes() {
        // 期望串逐字节来自 scripts/probe_parity_wire.py 探针输出。
        let expected = concat!(
            r#"{"id":"branch_fix_0001","project_id":"proj_parity","#,
            r#""mission_id":"mission_fix_0001","run_id":null,"parent_branch_id":null,"#,
            r#""title":"Sink reachability","hypothesis":"User input reaches eval","#,
            r#""rationale":"","status":"active","priority":70,"confidence":0.75,"#,
            r#""budget_steps":6,"steps_used":2,"related_fact_ids":["fact_1"],"#,
            r#""related_evidence_ids":[],"related_finding_ids":[],"#,
            r#""related_tool_invocation_ids":["tool_1"],"assigned_worker_id":null,"#,
            r#""created_by":"branch_generator","#,
            r#""created_at":"2026-08-24T12:00:00.123456Z","#,
            r#""updated_at":"2026-08-24T12:00:00.123456Z","metadata":{"zz":1,"aa":2}}"#
        );
        let branch = Branch {
            id: BranchId::new("branch_fix_0001".to_string()),
            project_id: ProjectId::new("proj_parity".to_string()),
            mission_id: MissionId::new("mission_fix_0001".to_string()),
            run_id: None,
            parent_branch_id: None,
            title: "Sink reachability".to_string(),
            hypothesis: "User input reaches eval".to_string(),
            rationale: String::new(),
            status: BranchStatus::Active,
            priority: 70,
            confidence: 0.75,
            budget_steps: 6,
            steps_used: 2,
            related_fact_ids: vec!["fact_1".to_string()],
            related_evidence_ids: Vec::new(),
            related_finding_ids: Vec::new(),
            related_tool_invocation_ids: vec!["tool_1".to_string()],
            assigned_worker_id: None,
            created_by: "branch_generator".to_string(),
            created_at: timestamp(),
            updated_at: timestamp(),
            metadata: [("zz", Value::from(1)), ("aa", Value::from(2))]
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        };
        let json = serde_json::to_string(&branch)
            .unwrap_or_else(|error| panic!("Branch 序列化不会失败: {error}"));
        assert_eq!(json, expected);

        let back: Branch = serde_json::from_str(&json)
            .unwrap_or_else(|error| panic!("自身输出必须可解析: {error}"));
        assert_eq!(back, branch);
    }

    #[test]
    fn mission_deserialize_applies_python_defaults_on_missing_fields() {
        let json = r#"{"project_id":"p","user_goal":"g"}"#;
        let mission: Mission = serde_json::from_str(json)
            .unwrap_or_else(|error| panic!("pydantic 接受缺省字段，serde 必须同样接受: {error}"));

        assert_eq!(mission.status, MissionStatus::Draft);
        assert_eq!(mission.approval_mode, ApprovalMode::AskForApproval);
        assert_eq!(mission.created_by, "user");
        assert!(mission.tags.is_empty());
        assert_eq!(mission.goal_contract.outcome_type, GoalOutcomeType::Custom);
        assert!(mission.id.as_str().starts_with("mission_"));
    }

    #[test]
    fn mission_deserialize_rejects_unknown_fields() {
        let result: Result<Mission, _> =
            serde_json::from_str(r#"{"project_id":"p","user_goal":"g","surprise":1}"#);
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }

    #[test]
    fn capability_gap_severity_matches_python_wire_values() {
        assert_wire_values(&[
            (CapabilityGapSeverity::Info, "info"),
            (CapabilityGapSeverity::Low, "low"),
            (CapabilityGapSeverity::Medium, "medium"),
            (CapabilityGapSeverity::High, "high"),
        ]);
    }

    #[test]
    fn capability_dispatch_serializes_to_python_wire_bytes() {
        let expected = concat!(
            r#"{"branch_id":"branch_fix_0001","solver":"web_solver","#,
            r#""audit_domain":"web_sast","config":{"mode":"fast"},"#,
            r#""capability_name":"http_probe","rationale":"route to web solver","#,
            r#""capability_gap":true,"gap_summary":"no binary solver available","#,
            r#""gap_severity":"medium"}"#
        );
        let dispatch = CapabilityDispatch {
            branch_id: BranchId::new("branch_fix_0001".to_string()),
            solver: Some("web_solver".to_string()),
            audit_domain: Some("web_sast".to_string()),
            config: [("mode", Value::from("fast"))]
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
            capability_name: Some("http_probe".to_string()),
            rationale: "route to web solver".to_string(),
            capability_gap: true,
            gap_summary: Some("no binary solver available".to_string()),
            gap_severity: CapabilityGapSeverity::Medium,
        };
        let json = serde_json::to_string(&dispatch)
            .unwrap_or_else(|error| panic!("CapabilityDispatch 序列化不会失败: {error}"));
        assert_eq!(json, expected);

        let back: CapabilityDispatch = serde_json::from_str(&json)
            .unwrap_or_else(|error| panic!("自身输出必须可解析: {error}"));
        assert_eq!(back, dispatch);
    }

    #[test]
    fn capability_dispatch_deserialize_applies_python_defaults() {
        let dispatch: CapabilityDispatch =
            serde_json::from_str(r#"{"branch_id":"b1","rationale":"fallback routing"}"#)
                .unwrap_or_else(|error| {
                    panic!("pydantic 接受缺省字段，serde 必须同样接受: {error}")
                });
        assert_eq!(dispatch.solver, None);
        assert_eq!(dispatch.audit_domain, None);
        assert!(dispatch.config.is_empty());
        assert_eq!(dispatch.capability_name, None);
        assert!(!dispatch.capability_gap);
        assert_eq!(dispatch.gap_summary, None);
        assert_eq!(dispatch.gap_severity, CapabilityGapSeverity::Info);
    }

    #[test]
    fn capability_dispatch_rejects_unknown_fields() {
        let result: Result<CapabilityDispatch, _> =
            serde_json::from_str(r#"{"branch_id":"b1","rationale":"r","unexpected":1}"#);
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }

    #[test]
    fn mission_start_result_roundtrips_with_defaults() {
        let mut mission = Mission::new(
            ProjectId::new("proj_test".to_string()),
            "Find the flag".to_string(),
        );
        mission.created_at = timestamp();
        mission.updated_at = timestamp();
        let mut branch = Branch::new(
            ProjectId::new("proj_test".to_string()),
            MissionId::new("mission_test".to_string()),
            "Route A".to_string(),
            "Input reaches eval".to_string(),
        );
        branch.created_at = timestamp();
        branch.updated_at = timestamp();
        let result = MissionStartResult {
            mission,
            run_id: RunId::new("run_test".to_string()),
            branches: vec![branch],
        };
        let json = serde_json::to_string(&result)
            .unwrap_or_else(|error| panic!("MissionStartResult 序列化不会失败: {error}"));
        let back: MissionStartResult = serde_json::from_str(&json)
            .unwrap_or_else(|error| panic!("自身输出必须可解析: {error}"));
        assert_eq!(back, result);

        // 缺省 branches 时 pydantic 接受空列表。
        let empty: MissionStartResult = serde_json::from_value(serde_json::json!({
            "mission": {
                "project_id": "p",
                "user_goal": "g",
                "id": "mission_x",
                "created_at": "2026-08-24T12:00:00.123456Z",
                "updated_at": "2026-08-24T12:00:00.123456Z"
            },
            "run_id": "run_x"
        }))
        .unwrap_or_else(|error| panic!("合法 JSON 必须可解析: {error}"));
        assert!(empty.branches.is_empty());
    }
}
