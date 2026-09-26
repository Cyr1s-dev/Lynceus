//! 收口链模型 —— `server/core/models/closure.py` 的移植。
//!
//! 收口链（COV → META → MGATE → EGUARD）拒绝让终止判定独自宣布 Mission
//! 完成：`complete` 之前必须经过覆盖检查、元认知发散与出口判定；任何新
//! 方向只能经 EGUARD 的 max-branch / max-budget 闸回到 Branch Generator。
//! 四类记录全部是 append-only 审计工件。

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::common::Timestamp;
use crate::common::new_id;
use crate::common::utcnow;
use crate::domain::AuditDomain;
use crate::ids::CoverageAssessmentId;
use crate::ids::EscalationGuardVerdictId;
use crate::ids::ExitGateDecisionId;
use crate::ids::MetacognitionAssessmentId;
use crate::ids::MissionId;
use crate::ids::ProjectId;
use crate::ids::RunId;
use crate::ids::TerminationAssessmentId;

/// 单个审计域的确定性覆盖状态（`CoverageCategoryStatus`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageCategoryStatus {
    /// 至少一个真实信号行使过该域。
    Covered,
    /// 与 Mission 相关但从未行使。
    NotCovered,
    /// 在该 Mission 的攻击面之外。
    NotRelevant,
}

/// 覆盖信号来源（`CoverageSignalSource`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageSignalSource {
    /// 工具调用。
    ToolInvocation,
    /// 证据。
    Evidence,
    /// 分支。
    Branch,
    /// 发现。
    Finding,
}

/// 一个审计域相对一个 Mission 的覆盖状态（`CoverageDomainEntry`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageDomainEntry {
    /// 审计域。
    pub domain: AuditDomain,
    /// 覆盖状态。
    #[serde(default = "default_entry_status")]
    pub status: CoverageCategoryStatus,
    /// 相关性理由。
    #[serde(default)]
    pub relevance_reason: String,
    /// 工具调用计数。
    #[serde(default)]
    pub tool_invocation_count: i64,
    /// 证据计数。
    #[serde(default)]
    pub evidence_count: i64,
    /// 分支计数。
    #[serde(default)]
    pub branch_count: i64,
    /// 发现计数。
    #[serde(default)]
    pub finding_count: i64,
    /// 短信号出处（如观察到的工具名或证据类型）。
    #[serde(default)]
    pub signal_sources: Vec<String>,
}

fn default_entry_status() -> CoverageCategoryStatus {
    CoverageCategoryStatus::NotRelevant
}

/// COV：一轮收口的确定性攻击面覆盖检查（`CoverageAssessment`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageAssessment {
    /// 覆盖评估标识符。
    #[serde(default = "default_coverage_id")]
    pub id: CoverageAssessmentId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Mission。
    #[serde(default)]
    pub mission_id: Option<MissionId>,
    /// 所属 Run。
    pub run_id: RunId,
    /// 收口轮次。
    #[serde(default)]
    pub round_index: i64,
    /// 各域覆盖明细。
    #[serde(default)]
    pub entries: Vec<CoverageDomainEntry>,
    /// 与 Mission 相关的域。
    #[serde(default)]
    pub relevant_domains: Vec<AuditDomain>,
    /// 已行使的域。
    #[serde(default)]
    pub covered_domains: Vec<AuditDomain>,
    /// 零真实信号的相关域——诚实的盲区。
    #[serde(default)]
    pub blind_spots: Vec<AuditDomain>,
    /// 摘要。
    #[serde(default)]
    pub summary: String,
    /// 创建者。
    #[serde(default = "default_coverage_created_by")]
    pub created_by: String,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
    /// 附加元数据（键序 = 插入序）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

fn default_coverage_id() -> CoverageAssessmentId {
    CoverageAssessmentId::new(new_id("cov"))
}

fn default_coverage_created_by() -> String {
    "coverage_checker".to_string()
}

impl CoverageAssessment {
    /// 以 Python 默认值构造（`CoverageAssessment(project_id=..., run_id=...)`）。
    #[must_use]
    pub fn new(project_id: ProjectId, run_id: RunId) -> Self {
        Self {
            id: default_coverage_id(),
            project_id,
            mission_id: None,
            run_id,
            round_index: 0,
            entries: Vec::new(),
            relevant_domains: Vec::new(),
            covered_domains: Vec::new(),
            blind_spots: Vec::new(),
            summary: String::new(),
            created_by: default_coverage_created_by(),
            created_at: utcnow(),
            metadata: Map::new(),
        }
    }
}

/// 元认知每轮必须施加的五个发散框架（`MetacognitionFramework`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetacognitionFramework {
    /// 类比：复用在别处奏效的技术。
    Analogy,
    /// 反向：假设失败并解释它。
    Inversion,
    /// 极端：已覆盖面的边界条件。
    Extremes,
    /// 组合：合并两个已覆盖域的产出。
    Combination,
    /// 降维：剥离假设重新推导。
    DimensionReduction,
}

impl MetacognitionFramework {
    /// Python 枚举定义序全集（`for framework in MetacognitionFramework` 镜像）。
    pub const ALL: [MetacognitionFramework; 5] = [
        MetacognitionFramework::Analogy,
        MetacognitionFramework::Inversion,
        MetacognitionFramework::Extremes,
        MetacognitionFramework::Combination,
        MetacognitionFramework::DimensionReduction,
    ];

    /// wire 值（Python `.value` 镜像，用于文本拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            MetacognitionFramework::Analogy => "analogy",
            MetacognitionFramework::Inversion => "inversion",
            MetacognitionFramework::Extremes => "extremes",
            MetacognitionFramework::Combination => "combination",
            MetacognitionFramework::DimensionReduction => "dimension_reduction",
        }
    }
}

/// 元认知提出的一个候选探索方向（`MetacognitionDirection`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetacognitionDirection {
    /// 标题。
    pub title: String,
    /// 可证伪的假设。
    pub hypothesis: String,
    /// 立论理由。
    #[serde(default)]
    pub rationale: String,
    /// 所属发散框架。
    #[serde(default = "default_direction_framework")]
    pub framework: MetacognitionFramework,
    /// 关联盲区。
    #[serde(default)]
    pub related_blind_spots: Vec<AuditDomain>,
    /// 关联未满足目标需求。
    #[serde(default)]
    pub related_unmet_requirements: Vec<String>,
}

fn default_direction_framework() -> MetacognitionFramework {
    MetacognitionFramework::Analogy
}

impl MetacognitionDirection {
    /// 稳定比较键（重复方向检测用）：小写化 + 空白规范化。
    ///
    /// Python：`" ".join(self.hypothesis.lower().split())`。
    #[must_use]
    pub fn normalized_hypothesis(&self) -> String {
        self.hypothesis
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// 元认知方向的产出方式（`MetacognitionMode`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetacognitionMode {
    /// Provider 经 LLM 路径提出方向。
    Llm,
    /// 盲区结构化回退。
    Deterministic,
}

/// 元认知评估的触发原因（`MetacognitionTrigger`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetacognitionTrigger {
    /// 收敛：终止判定提出 complete 前触发。
    Convergence,
    /// 枚举停滞。
    EnumerationStall,
    /// 周期触发。
    Periodic,
    /// 人工触发。
    Manual,
}

/// META：对 Mission 已探索状态的一次发散评估（`MetacognitionAssessment`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetacognitionAssessment {
    /// 元认知评估标识符。
    #[serde(default = "default_meta_id")]
    pub id: MetacognitionAssessmentId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Mission。
    #[serde(default)]
    pub mission_id: Option<MissionId>,
    /// 所属 Run。
    pub run_id: RunId,
    /// 收口轮次。
    #[serde(default)]
    pub round_index: i64,
    /// 触发原因。
    #[serde(default = "default_meta_trigger")]
    pub trigger: MetacognitionTrigger,
    /// 方向产出方式。
    #[serde(default = "default_meta_mode")]
    pub mode: MetacognitionMode,
    /// 施加的框架列表。
    #[serde(default)]
    pub frameworks_applied: Vec<MetacognitionFramework>,
    /// 提出的方向。
    #[serde(default)]
    pub directions: Vec<MetacognitionDirection>,
    /// 覆盖的盲区。
    #[serde(default)]
    pub blind_spots_addressed: Vec<AuditDomain>,
    /// 备注。
    #[serde(default)]
    pub notes: Vec<String>,
    /// 模型调用标识符。
    #[serde(default)]
    pub model_invocation_id: Option<String>,
    /// 创建者。
    #[serde(default = "default_meta_created_by")]
    pub created_by: String,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
    /// 附加元数据（键序 = 插入序）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

fn default_meta_id() -> MetacognitionAssessmentId {
    MetacognitionAssessmentId::new(new_id("meta"))
}

fn default_meta_trigger() -> MetacognitionTrigger {
    MetacognitionTrigger::Convergence
}

fn default_meta_mode() -> MetacognitionMode {
    MetacognitionMode::Deterministic
}

fn default_meta_created_by() -> String {
    "metacognition_agent".to_string()
}

impl MetacognitionAssessment {
    /// 以 Python 默认值构造（`MetacognitionAssessment(project_id=...,
    /// run_id=...)`）。
    #[must_use]
    pub fn new(project_id: ProjectId, run_id: RunId) -> Self {
        Self {
            id: default_meta_id(),
            project_id,
            mission_id: None,
            run_id,
            round_index: 0,
            trigger: default_meta_trigger(),
            mode: default_meta_mode(),
            frameworks_applied: Vec::new(),
            directions: Vec::new(),
            blind_spots_addressed: Vec::new(),
            notes: Vec::new(),
            model_invocation_id: None,
            created_by: default_meta_created_by(),
            created_at: utcnow(),
            metadata: Map::new(),
        }
    }
}

/// MGATE 判定值（`ExitGateDecisionValue`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitGateDecisionValue {
    /// 收口轮且无新方向：Mission 可以完成。
    Done,
    /// 发现新方向：经 EGUARD 重新进入探索。
    Escalate,
}

impl ExitGateDecisionValue {
    /// wire 值（Python `decision.value` 的镜像，用于事件与 journal 文本）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ExitGateDecisionValue::Done => "done",
            ExitGateDecisionValue::Escalate => "escalate",
        }
    }
}

/// MGATE：META 的唯一下游，裁决 DONE 与 ESCALATE（`ExitGateDecision`）。
///
/// 收口轮无新方向即 DONE；发散或新发现方向即 ESC。MGATE 绝不自行创建
/// 分支——升级必须流经 EGUARD。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExitGateDecision {
    /// 出口判定标识符。
    #[serde(default = "default_gate_id")]
    pub id: ExitGateDecisionId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Mission。
    #[serde(default)]
    pub mission_id: Option<MissionId>,
    /// 所属 Run。
    pub run_id: RunId,
    /// 收口轮次。
    #[serde(default)]
    pub round_index: i64,
    /// 判定值。
    #[serde(default = "default_gate_decision")]
    pub decision: ExitGateDecisionValue,
    /// 是否收口轮。
    #[serde(default = "default_gate_closure_round")]
    pub closure_round: bool,
    /// 新方向标题列表。
    #[serde(default)]
    pub new_direction_titles: Vec<String>,
    /// 盲区。
    #[serde(default)]
    pub blind_spots: Vec<AuditDomain>,
    /// 理由。
    #[serde(default)]
    pub reasons: Vec<String>,
    /// 来源终止评估。
    #[serde(default)]
    pub termination_assessment_id: Option<TerminationAssessmentId>,
    /// 来源覆盖评估。
    #[serde(default)]
    pub coverage_assessment_id: Option<CoverageAssessmentId>,
    /// 来源元认知评估。
    #[serde(default)]
    pub metacognition_assessment_id: Option<MetacognitionAssessmentId>,
    /// 创建者。
    #[serde(default = "default_gate_created_by")]
    pub created_by: String,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
    /// 附加元数据（键序 = 插入序）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

fn default_gate_id() -> ExitGateDecisionId {
    ExitGateDecisionId::new(new_id("mgate"))
}

fn default_gate_decision() -> ExitGateDecisionValue {
    ExitGateDecisionValue::Done
}

fn default_gate_closure_round() -> bool {
    true
}

fn default_gate_created_by() -> String {
    "metacog_exit_gate".to_string()
}

impl ExitGateDecision {
    /// 以 Python 默认值构造（`ExitGateDecision(project_id=..., run_id=...)`）。
    #[must_use]
    pub fn new(project_id: ProjectId, run_id: RunId) -> Self {
        Self {
            id: default_gate_id(),
            project_id,
            mission_id: None,
            run_id,
            round_index: 0,
            decision: default_gate_decision(),
            closure_round: default_gate_closure_round(),
            new_direction_titles: Vec::new(),
            blind_spots: Vec::new(),
            reasons: Vec::new(),
            termination_assessment_id: None,
            coverage_assessment_id: None,
            metacognition_assessment_id: None,
            created_by: default_gate_created_by(),
            created_at: utcnow(),
            metadata: Map::new(),
        }
    }
}

/// EGUARD 拒绝一个候选方向时的记录（`EscalationGuardRejection`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalationGuardRejection {
    /// 被拒方向标题。
    pub title: String,
    /// 拒绝理由。
    pub reason: String,
}

/// EGUARD：ESC 与 Branch Generator 之间强制预算闸（`EscalationGuardVerdict`）。
///
/// 任何方向不经过 max-branch 与 max-budget 检查不得成为 Branch；已探索
/// 假设的重复项被丢弃，停滞的 Mission 无法循环升级。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscalationGuardVerdict {
    /// 升级闸裁决标识符。
    #[serde(default = "default_guard_id")]
    pub id: EscalationGuardVerdictId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Mission。
    #[serde(default)]
    pub mission_id: Option<MissionId>,
    /// 所属 Run。
    pub run_id: RunId,
    /// 收口轮次。
    #[serde(default)]
    pub round_index: i64,
    /// 放行的方向。
    #[serde(default)]
    pub admitted_directions: Vec<MetacognitionDirection>,
    /// 拒绝的方向。
    #[serde(default)]
    pub rejected_directions: Vec<EscalationGuardRejection>,
    /// 升级前分支数。
    #[serde(default)]
    pub branch_count_before: i64,
    /// 升级后分支数。
    #[serde(default)]
    pub branch_count_after: i64,
    /// 分支数上限。
    #[serde(default)]
    pub max_branches: i64,
    /// 剩余预算步数。
    #[serde(default)]
    pub budget_steps_remaining: i64,
    /// 是否被预算阻塞。
    #[serde(default)]
    pub blocked_by_budget: bool,
    /// 理由。
    #[serde(default)]
    pub reasons: Vec<String>,
    /// 来源出口判定。
    #[serde(default)]
    pub exit_gate_decision_id: Option<ExitGateDecisionId>,
    /// 创建者。
    #[serde(default = "default_guard_created_by")]
    pub created_by: String,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
    /// 附加元数据（键序 = 插入序）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

fn default_guard_id() -> EscalationGuardVerdictId {
    EscalationGuardVerdictId::new(new_id("eguard"))
}

fn default_guard_created_by() -> String {
    "escalation_guard".to_string()
}

impl EscalationGuardVerdict {
    /// 以 Python 默认值构造（`EscalationGuardVerdict(project_id=...,
    /// run_id=...)`）。
    #[must_use]
    pub fn new(project_id: ProjectId, run_id: RunId) -> Self {
        Self {
            id: default_guard_id(),
            project_id,
            mission_id: None,
            run_id,
            round_index: 0,
            admitted_directions: Vec::new(),
            rejected_directions: Vec::new(),
            branch_count_before: 0,
            branch_count_after: 0,
            max_branches: 0,
            budget_steps_remaining: 0,
            blocked_by_budget: false,
            reasons: Vec::new(),
            exit_gate_decision_id: None,
            created_by: default_guard_created_by(),
            created_at: utcnow(),
            metadata: Map::new(),
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

    fn timestamp() -> Timestamp {
        "2026-08-24T12:00:00.123456Z"
            .parse()
            .unwrap_or_else(|error| panic!("固定时间必须可解析: {error}"))
    }

    #[test]
    fn closure_enums_match_python_wire_values() {
        assert_wire_values(&[
            (CoverageCategoryStatus::Covered, "covered"),
            (CoverageCategoryStatus::NotCovered, "not_covered"),
            (CoverageCategoryStatus::NotRelevant, "not_relevant"),
        ]);
        assert_wire_values(&[
            (CoverageSignalSource::ToolInvocation, "tool_invocation"),
            (CoverageSignalSource::Evidence, "evidence"),
            (CoverageSignalSource::Branch, "branch"),
            (CoverageSignalSource::Finding, "finding"),
        ]);
        assert_wire_values(&[
            (MetacognitionFramework::Analogy, "analogy"),
            (MetacognitionFramework::Inversion, "inversion"),
            (MetacognitionFramework::Extremes, "extremes"),
            (MetacognitionFramework::Combination, "combination"),
            (
                MetacognitionFramework::DimensionReduction,
                "dimension_reduction",
            ),
        ]);
        assert_wire_values(&[
            (MetacognitionMode::Llm, "llm"),
            (MetacognitionMode::Deterministic, "deterministic"),
        ]);
        assert_wire_values(&[
            (MetacognitionTrigger::Convergence, "convergence"),
            (MetacognitionTrigger::EnumerationStall, "enumeration_stall"),
            (MetacognitionTrigger::Periodic, "periodic"),
            (MetacognitionTrigger::Manual, "manual"),
        ]);
        assert_wire_values(&[
            (ExitGateDecisionValue::Done, "done"),
            (ExitGateDecisionValue::Escalate, "escalate"),
        ]);
    }

    #[test]
    fn closure_structs_default_like_python() {
        let coverage = CoverageAssessment::new(project_id(), run_id());
        assert_eq!(coverage.created_by, "coverage_checker");
        assert!(coverage.id.as_str().starts_with("cov_"));
        assert_eq!(coverage.entries.len(), 0);

        let meta = MetacognitionAssessment::new(project_id(), run_id());
        assert_eq!(meta.created_by, "metacognition_agent");
        assert!(meta.id.as_str().starts_with("meta_"));
        assert_eq!(meta.trigger, MetacognitionTrigger::Convergence);
        assert_eq!(meta.mode, MetacognitionMode::Deterministic);

        let gate = ExitGateDecision::new(project_id(), run_id());
        assert_eq!(gate.created_by, "metacog_exit_gate");
        assert!(gate.id.as_str().starts_with("mgate_"));
        assert_eq!(gate.decision, ExitGateDecisionValue::Done);
        assert!(gate.closure_round);

        let guard = EscalationGuardVerdict::new(project_id(), run_id());
        assert_eq!(guard.created_by, "escalation_guard");
        assert!(guard.id.as_str().starts_with("eguard_"));
        assert!(!guard.blocked_by_budget);
    }

    #[test]
    fn direction_normalized_hypothesis_matches_python() {
        let direction = MetacognitionDirection {
            title: "t".to_string(),
            hypothesis: "  An   Unexamined\nSURFACE may  hold ".to_string(),
            rationale: String::new(),
            framework: MetacognitionFramework::Analogy,
            related_blind_spots: Vec::new(),
            related_unmet_requirements: Vec::new(),
        };
        // Python: " ".join("  An   Unexamined\nSURFACE may  hold ".lower().split())
        assert_eq!(
            direction.normalized_hypothesis(),
            "an unexamined surface may hold"
        );
        assert_roundtrip(&MetacognitionFramework::Inversion);
    }

    #[test]
    fn coverage_assessment_wire_roundtrip_keeps_field_order() {
        let mut coverage = CoverageAssessment::new(project_id(), run_id());
        coverage.mission_id = Some(MissionId::new("mission_x".to_string()));
        coverage.created_at = timestamp();
        coverage.blind_spots = vec![AuditDomain::WebDast];
        coverage.entries.push(CoverageDomainEntry {
            domain: AuditDomain::WebDast,
            status: CoverageCategoryStatus::NotCovered,
            relevance_reason: "relevant for target type url".to_string(),
            tool_invocation_count: 0,
            evidence_count: 0,
            branch_count: 0,
            finding_count: 0,
            signal_sources: Vec::new(),
        });
        let json = serde_json::to_string(&coverage)
            .unwrap_or_else(|error| panic!("序列化不会失败: {error}"));
        assert!(json.contains(r#""created_by":"coverage_checker""#));
        assert!(json.find(r#""blind_spots":["web_dast"]"#).is_some());
        let back: CoverageAssessment = serde_json::from_str(&json)
            .unwrap_or_else(|error| panic!("自身输出必须可解析: {error}"));
        assert_eq!(back, coverage);
    }

    #[test]
    fn closure_structs_reject_unknown_fields() {
        let result: Result<CoverageAssessment, _> =
            serde_json::from_str(r#"{"project_id":"p","run_id":"r","extra":1}"#);
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
        let result: Result<ExitGateDecision, _> =
            serde_json::from_str(r#"{"project_id":"p","run_id":"r","extra":1}"#);
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }
}
