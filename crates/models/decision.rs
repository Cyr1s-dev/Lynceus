//! `DecisionGate` 模型 —— `server/core/models/decision.py` 的移植。
//!
//! 人机协同（HITL）审计控制点：可能暂停一个 `AuditRun` 的用户决策点。

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::common::Timestamp;
use crate::common::new_id;
use crate::common::utcnow;
use crate::ids::DecisionGateId;
use crate::ids::ProjectId;
use crate::ids::RunId;

/// 用户决策点生命周期（`DecisionGateStatus`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionGateStatus {
    /// 待回答。
    Pending,
    /// 已回答。
    Answered,
    /// 已过期。
    Expired,
    /// 已取消。
    Cancelled,
}

impl DecisionGateStatus {
    /// Python wire 值（`str` 枚举序列化）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Answered => "answered",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        }
    }
}

/// 决策对编排的影响强度（`DecisionGateKind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionGateKind {
    /// 阻塞。
    Blocking,
    /// 咨询。
    Advisory,
    /// 评审。
    Review,
}

/// 决策的审计风险严重级别（`DecisionSeverity`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSeverity {
    /// 低。
    Low,
    /// 中。
    Medium,
    /// 高。
    High,
    /// 严重。
    Critical,
}

impl DecisionSeverity {
    /// Python wire 值（`str` 枚举序列化）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

impl DecisionGateKind {
    /// Python wire 值（`str` 枚举序列化）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blocking => "blocking",
            Self::Advisory => "advisory",
            Self::Review => "review",
        }
    }
}

fn default_option_id() -> String {
    new_id("opt")
}

/// DecisionOption：呈现给分析员的具体选项（`DecisionOption`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionOption {
    /// 选项标识符。
    #[serde(default = "default_option_id")]
    pub id: String,
    /// 标签。
    pub label: String,
    /// 描述。
    pub description: String,
    /// 影响。
    pub impact: String,
    /// 风险。
    pub risk: String,
    /// 是否推荐。
    #[serde(default)]
    pub is_recommended: bool,
}

fn default_answered_by() -> String {
    "user".to_string()
}

/// `DecisionAnswer`：分析员对一个 `DecisionGate` 的回答。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionAnswer {
    /// 选中的选项。
    #[serde(default)]
    pub option_id: Option<String>,
    /// 自由文本回答。
    #[serde(default)]
    pub freeform_text: Option<String>,
    /// 回答者。
    #[serde(default = "default_answered_by")]
    pub answered_by: String,
    /// 理由。
    #[serde(default)]
    pub rationale: Option<String>,
}

/// `DecisionGate` 校验失败（镜像 pydantic `model_validator`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DecisionGateError {
    /// `recommended_option_id` 未引用 options 中的选项。
    #[error("recommended_option_id must reference one of options")]
    RecommendedNotInOptions,
    /// 推荐选项未标记 `is_recommended=true`。
    #[error("recommended option must have is_recommended=True")]
    RecommendedNotMarked,
    /// 标记为推荐的选项数不为一。
    #[error("exactly one option must be marked as recommended")]
    RecommendedCount,
}

fn default_gate_id() -> DecisionGateId {
    DecisionGateId::new(new_id("decision"))
}

fn default_gate_created_by() -> String {
    "manager".to_string()
}

fn default_gate_kind() -> DecisionGateKind {
    DecisionGateKind::Blocking
}

fn default_gate_severity() -> DecisionSeverity {
    DecisionSeverity::Medium
}

fn default_gate_status() -> DecisionGateStatus {
    DecisionGateStatus::Pending
}

/// DecisionGate：可能暂停 `AuditRun` 的用户决策点（`DecisionGate`）。
///
/// 推荐一致性经 [`Self::validated`]（构造路径）与 serde `try_from`
/// （解析路径）双入口镜像。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "DecisionGateWire")]
pub struct DecisionGate {
    /// 决策点标识符。
    pub id: DecisionGateId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 关联的 `AuditRun`。
    pub audit_run_id: RunId,
    /// 创建者。
    pub created_by: String,
    /// 决策影响强度。
    pub kind: DecisionGateKind,
    /// 审计风险严重级别。
    pub severity: DecisionSeverity,
    /// 问题。
    pub question: String,
    /// 上下文摘要（Python 侧上限 4000 字符）。
    pub context_summary: String,
    /// 推荐选项。
    pub recommended_option_id: String,
    /// 选项列表（至少一项）。
    pub options: Vec<DecisionOption>,
    /// 生命周期状态。
    pub status: DecisionGateStatus,
    /// 分析员的回答。
    pub answer: Option<DecisionAnswer>,
    /// 创建时间。
    pub created_at: Timestamp,
    /// 回答时间。
    pub answered_at: Option<Timestamp>,
    /// 过期时间。
    pub expires_at: Option<Timestamp>,
    /// 关联 Fact ID 列表。
    pub related_fact_ids: Vec<String>,
    /// 关联 Evidence ID 列表。
    pub related_evidence_ids: Vec<String>,
    /// 关联 Finding ID 列表。
    pub related_finding_ids: Vec<String>,
    /// 附加元数据（键序 = 插入序）。
    pub metadata: Map<String, Value>,
}

impl DecisionGate {
    /// `option_id` 是否为该决策点的合法选项（`has_option`）。
    #[must_use]
    pub fn has_option(&self, option_id: &str) -> bool {
        self.options.iter().any(|option| option.id == option_id)
    }

    /// 校验推荐一致性，返回可持久化决策点。
    ///
    /// # Errors
    /// 见 [`DecisionGateError`]；错误文本与 Python 逐字节一致。
    pub fn validated(self) -> Result<Self, DecisionGateError> {
        let option_ids: Vec<&str> = self.options.iter().map(|o| o.id.as_str()).collect();
        if !option_ids.contains(&self.recommended_option_id.as_str()) {
            return Err(DecisionGateError::RecommendedNotInOptions);
        }
        let recommended: Vec<&str> = self
            .options
            .iter()
            .filter(|option| option.is_recommended)
            .map(|option| option.id.as_str())
            .collect();
        if !recommended.contains(&self.recommended_option_id.as_str()) {
            return Err(DecisionGateError::RecommendedNotMarked);
        }
        if recommended.len() != 1 {
            return Err(DecisionGateError::RecommendedCount);
        }
        Ok(self)
    }
}

/// [`DecisionGate`] 的解析镜像：缺省字段取 Python 默认值，未知字段拒绝
/// （`extra="forbid"`），解析后走 `validated()`。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DecisionGateWire {
    id: DecisionGateId,
    project_id: ProjectId,
    audit_run_id: RunId,
    created_by: String,
    kind: DecisionGateKind,
    severity: DecisionSeverity,
    question: String,
    context_summary: String,
    recommended_option_id: String,
    options: Vec<DecisionOption>,
    status: DecisionGateStatus,
    answer: Option<DecisionAnswer>,
    created_at: Timestamp,
    answered_at: Option<Timestamp>,
    expires_at: Option<Timestamp>,
    related_fact_ids: Vec<String>,
    related_evidence_ids: Vec<String>,
    related_finding_ids: Vec<String>,
    metadata: Map<String, Value>,
}

impl Default for DecisionGateWire {
    fn default() -> Self {
        Self {
            id: default_gate_id(),
            project_id: ProjectId::new(String::new()),
            audit_run_id: RunId::new(String::new()),
            created_by: default_gate_created_by(),
            kind: default_gate_kind(),
            severity: default_gate_severity(),
            question: String::new(),
            context_summary: String::new(),
            recommended_option_id: String::new(),
            options: Vec::new(),
            status: default_gate_status(),
            answer: None,
            created_at: utcnow(),
            answered_at: None,
            expires_at: None,
            related_fact_ids: Vec::new(),
            related_evidence_ids: Vec::new(),
            related_finding_ids: Vec::new(),
            metadata: Map::new(),
        }
    }
}

impl TryFrom<DecisionGateWire> for DecisionGate {
    type Error = DecisionGateError;

    fn try_from(wire: DecisionGateWire) -> Result<Self, Self::Error> {
        Self {
            id: wire.id,
            project_id: wire.project_id,
            audit_run_id: wire.audit_run_id,
            created_by: wire.created_by,
            kind: wire.kind,
            severity: wire.severity,
            question: wire.question,
            context_summary: wire.context_summary,
            recommended_option_id: wire.recommended_option_id,
            options: wire.options,
            status: wire.status,
            answer: wire.answer,
            created_at: wire.created_at,
            answered_at: wire.answered_at,
            expires_at: wire.expires_at,
            related_fact_ids: wire.related_fact_ids,
            related_evidence_ids: wire.related_evidence_ids,
            related_finding_ids: wire.related_finding_ids,
            metadata: wire.metadata,
        }
        .validated()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::assert_wire_values;

    fn option(id: &str, recommended: bool) -> DecisionOption {
        DecisionOption {
            id: id.to_string(),
            label: "yes".to_string(),
            description: "go".to_string(),
            impact: "go".to_string(),
            risk: "low".to_string(),
            is_recommended: recommended,
        }
    }

    fn gate(recommended: &str, options: Vec<DecisionOption>) -> DecisionGate {
        DecisionGate {
            id: DecisionGateId::new("decision_1".to_string()),
            project_id: ProjectId::new("p".to_string()),
            audit_run_id: RunId::new("r".to_string()),
            created_by: "manager".to_string(),
            kind: DecisionGateKind::Blocking,
            severity: DecisionSeverity::Medium,
            question: "continue?".to_string(),
            context_summary: String::new(),
            recommended_option_id: recommended.to_string(),
            options,
            status: DecisionGateStatus::Pending,
            answer: None,
            created_at: "2026-08-24T12:00:00Z"
                .parse()
                .unwrap_or_else(|error| panic!("固定时间必须可解析: {error}")),
            answered_at: None,
            expires_at: None,
            related_fact_ids: Vec::new(),
            related_evidence_ids: Vec::new(),
            related_finding_ids: Vec::new(),
            metadata: Map::new(),
        }
    }

    #[test]
    fn decision_enums_match_python_wire_values() {
        assert_wire_values(&[
            (DecisionGateStatus::Pending, "pending"),
            (DecisionGateStatus::Answered, "answered"),
            (DecisionGateStatus::Expired, "expired"),
            (DecisionGateStatus::Cancelled, "cancelled"),
        ]);
        assert_wire_values(&[
            (DecisionGateKind::Blocking, "blocking"),
            (DecisionGateKind::Advisory, "advisory"),
            (DecisionGateKind::Review, "review"),
        ]);
        assert_wire_values(&[
            (DecisionSeverity::Low, "low"),
            (DecisionSeverity::Medium, "medium"),
            (DecisionSeverity::High, "high"),
            (DecisionSeverity::Critical, "critical"),
        ]);
    }

    #[test]
    fn decision_gate_validated_enforces_recommendation() {
        let valid = gate("yes", vec![option("yes", true), option("no", false)]);
        assert!(valid.clone().validated().is_ok());
        assert!(valid.has_option("yes"));
        assert!(!valid.has_option("maybe"));

        assert_eq!(
            gate("missing", vec![option("yes", true)])
                .validated()
                .unwrap_err(),
            DecisionGateError::RecommendedNotInOptions
        );
        assert_eq!(
            gate("yes", vec![option("yes", false)])
                .validated()
                .unwrap_err(),
            DecisionGateError::RecommendedNotMarked
        );
        assert_eq!(
            gate("yes", vec![option("yes", true), option("also", true)])
                .validated()
                .unwrap_err(),
            DecisionGateError::RecommendedCount
        );
    }

    #[test]
    fn decision_gate_wire_validates_and_rejects_unknown_fields() {
        let json = concat!(
            r#"{"project_id":"p","audit_run_id":"r","question":"continue?","#,
            r#""recommended_option_id":"yes","options":[{"id":"yes","label":"yes","#,
            r#""description":"go","impact":"go","risk":"low","is_recommended":true}]}"#
        );
        let gate_parsed: DecisionGate = serde_json::from_str(json)
            .unwrap_or_else(|error| panic!("合法输入必须可解析: {error}"));
        assert_eq!(gate_parsed.kind, DecisionGateKind::Blocking);
        assert_eq!(gate_parsed.created_by, "manager");

        let bad = concat!(
            r#"{"project_id":"p","audit_run_id":"r","question":"q","#,
            r#""recommended_option_id":"other","options":[{"id":"yes","label":"y","#,
            r#""description":"d","impact":"i","risk":"r","is_recommended":true}]}"#
        );
        let result: Result<DecisionGate, _> = serde_json::from_str(bad);
        assert!(result.is_err(), "推荐不一致必须被拒绝");

        let result: Result<DecisionGate, _> = serde_json::from_str(
            r#"{"project_id":"p","audit_run_id":"r","question":"q","recommended_option_id":"x","options":[],"extra":1}"#,
        );
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }
}
