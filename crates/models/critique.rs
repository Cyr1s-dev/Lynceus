//! Critique 模型 —— `server/core/models/critique.py` 的移植。
//!
//! Critique Agent 位于 Branch Generator 与假设池之间，职责是对抗性的：
//! 拒绝把"断言式结论"当"可证伪假设"的分支，拒绝无法对具体可校验契约
//! 执行的分支。可执行契约四要素齐备（verb / object / artifact path /
//! hit signal）的分支才允许入池——没有 hit signal 的分支无法失败，
//! 无法失败的分支不是假设。

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::common::Timestamp;
use crate::common::new_id;
use crate::ids::BranchId;
use crate::ids::CritiqueReportId;
use crate::ids::MissionId;
use crate::ids::ProjectId;
use crate::ids::RunId;

/// Critic 对一个假设的裁决（`CritiqueVerdict`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CritiqueVerdict {
    /// 可信且可执行：入池。
    Accepted,
    /// 可挽救：改写后重新提交。
    NeedsRevision,
    /// 臆测或无根据：直接拒绝。
    Rejected,
}

impl CritiqueVerdict {
    /// wire 值（Python `str(verdict.value)` 镜像）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            CritiqueVerdict::Accepted => "accepted",
            CritiqueVerdict::NeedsRevision => "needs_revision",
            CritiqueVerdict::Rejected => "rejected",
        }
    }
}

/// 可执行分支契约的四要素（`ContractElement`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractElement {
    /// 要执行的动作。
    Verb,
    /// 动作针对的对象。
    Object,
    /// 产出落点。
    ArtifactPath,
    /// 命中信号——使分支可证伪。
    HitSignal,
}

impl ContractElement {
    /// wire 值（Python `element.value` 镜像，用于理由文案拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ContractElement::Verb => "verb",
            ContractElement::Object => "object",
            ContractElement::ArtifactPath => "artifact_path",
            ContractElement::HitSignal => "hit_signal",
        }
    }
}

/// 每个 Branch 可执行必须满足的四要素契约（`ExecutableContract`）。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutableContract {
    /// 要执行的动作。
    #[serde(default)]
    pub verb: String,
    /// 动作针对的对象。
    #[serde(default)]
    pub object: String,
    /// 产出落点。
    #[serde(default)]
    pub artifact_path: String,
    /// 命中信号。
    #[serde(default)]
    pub hit_signal: String,
}

impl ExecutableContract {
    /// 返回缺失或空白的契约要素（`missing_elements`）。
    #[must_use]
    pub fn missing_elements(&self) -> Vec<ContractElement> {
        let pairs = [
            (ContractElement::Verb, &self.verb),
            (ContractElement::Object, &self.object),
            (ContractElement::ArtifactPath, &self.artifact_path),
            (ContractElement::HitSignal, &self.hit_signal),
        ];
        pairs
            .into_iter()
            .filter(|(_, value)| value.trim().is_empty())
            .map(|(element, _)| element)
            .collect()
    }

    /// 四要素是否齐备（`is_complete`）。
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.missing_elements().is_empty()
    }
}

fn default_critique_id() -> CritiqueReportId {
    CritiqueReportId::new(new_id("critique"))
}

fn default_critique_created_by() -> String {
    "critique_agent".to_string()
}

fn default_critique_verdict_confidence() -> f64 {
    0.0
}

/// CritiqueReport：对单个 Branch 假设的一次独立批判（`CritiqueReport`）。
///
/// append-only 且不论裁决如何都保留：一次拒绝是关于探索如何被引导的
/// 审计证据，不是可丢弃物。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CritiqueReport {
    /// 批判报告标识符。
    #[serde(default = "default_critique_id")]
    pub id: CritiqueReportId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Mission。
    #[serde(default)]
    pub mission_id: Option<MissionId>,
    /// 所属 Run。
    #[serde(default)]
    pub run_id: Option<RunId>,
    /// 被批判的 Branch。
    pub branch_id: BranchId,
    /// 裁决。
    pub verdict: CritiqueVerdict,
    /// 被裁决的假设原文——绝不原地改写。
    pub hypothesis: String,
    /// 理由。
    #[serde(default)]
    pub reasons: Vec<String>,
    /// 让文本成为断言而非假设的逐字短语。
    #[serde(default)]
    pub speculative_phrases: Vec<String>,
    /// 缺失的契约要素。
    #[serde(default)]
    pub missing_contract_elements: Vec<ContractElement>,
    /// 分支声称但图中不存在的悬空引用。
    #[serde(default)]
    pub unknown_fact_ids: Vec<String>,
    /// 是否可证伪。
    #[serde(default)]
    pub falsifiable: bool,
    /// 仅 `NEEDS_REVISION` 出现：假设形式的改写供重新提交。
    #[serde(default)]
    pub restated_hypothesis: Option<String>,
    /// Critic 对自身判断的置信度（Python 侧约束 `[0.0, 1.0]`）。
    #[serde(default = "default_critique_verdict_confidence")]
    pub confidence: f64,
    /// 创建者。
    #[serde(default = "default_critique_created_by")]
    pub created_by: String,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
    /// 附加元数据（键序 = 插入序）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

impl CritiqueReport {
    /// 以 Python 默认值构造（`CritiqueReport(project_id=..., branch_id=...,
    /// verdict=..., hypothesis=...)`，其余字段取模型默认）。
    #[must_use]
    pub fn new(
        project_id: ProjectId,
        branch_id: BranchId,
        verdict: CritiqueVerdict,
        hypothesis: String,
    ) -> Self {
        Self {
            id: default_critique_id(),
            project_id,
            mission_id: None,
            run_id: None,
            branch_id,
            verdict,
            hypothesis,
            reasons: Vec::new(),
            speculative_phrases: Vec::new(),
            missing_contract_elements: Vec::new(),
            unknown_fact_ids: Vec::new(),
            falsifiable: false,
            restated_hypothesis: None,
            confidence: default_critique_verdict_confidence(),
            created_by: default_critique_created_by(),
            created_at: crate::common::utcnow(),
            metadata: Map::new(),
        }
    }

    /// 该裁决是否允许分支进入假设池（`admitted` 属性）。
    #[must_use]
    pub fn admitted(&self) -> bool {
        self.verdict == CritiqueVerdict::Accepted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{assert_roundtrip, assert_wire_values};

    #[test]
    fn critique_enums_match_python_wire_values() {
        assert_wire_values(&[
            (CritiqueVerdict::Accepted, "accepted"),
            (CritiqueVerdict::NeedsRevision, "needs_revision"),
            (CritiqueVerdict::Rejected, "rejected"),
        ]);
        assert_wire_values(&[
            (ContractElement::Verb, "verb"),
            (ContractElement::Object, "object"),
            (ContractElement::ArtifactPath, "artifact_path"),
            (ContractElement::HitSignal, "hit_signal"),
        ]);
    }

    #[test]
    fn contract_missing_elements_reports_blank_fields() {
        let empty = ExecutableContract::default();
        assert_eq!(
            empty.missing_elements(),
            vec![
                ContractElement::Verb,
                ContractElement::Object,
                ContractElement::ArtifactPath,
                ContractElement::HitSignal,
            ]
        );
        assert!(!empty.is_complete());

        let partial = ExecutableContract {
            verb: "trace input".to_string(),
            object: "   ".to_string(),
            artifact_path: "artifacts/trace.json".to_string(),
            hit_signal: "path reaches sink".to_string(),
        };
        assert_eq!(partial.missing_elements(), vec![ContractElement::Object]);

        let complete = ExecutableContract {
            verb: "v".to_string(),
            object: "o".to_string(),
            artifact_path: "a".to_string(),
            hit_signal: "h".to_string(),
        };
        assert!(complete.is_complete());
        assert_roundtrip(&complete);
    }

    #[test]
    fn critique_report_admitted_follows_verdict() {
        let report = CritiqueReport {
            id: CritiqueReportId::new("critique_1".to_string()),
            project_id: ProjectId::new("p".to_string()),
            mission_id: None,
            run_id: None,
            branch_id: BranchId::new("branch_1".to_string()),
            verdict: CritiqueVerdict::Accepted,
            hypothesis: "h".to_string(),
            reasons: Vec::new(),
            speculative_phrases: Vec::new(),
            missing_contract_elements: Vec::new(),
            unknown_fact_ids: Vec::new(),
            falsifiable: true,
            restated_hypothesis: None,
            confidence: 0.8,
            created_by: "critique_agent".to_string(),
            created_at: "2026-08-24T12:00:00Z"
                .parse()
                .unwrap_or_else(|error| panic!("固定时间必须可解析: {error}")),
            metadata: Map::new(),
        };
        assert!(report.admitted());

        let json = serde_json::to_string(&report)
            .unwrap_or_else(|error| panic!("序列化不会失败: {error}"));
        assert!(json.contains(r#""created_by":"critique_agent""#));
        let back: CritiqueReport = serde_json::from_str(&json)
            .unwrap_or_else(|error| panic!("自身输出必须可解析: {error}"));
        assert_eq!(back, report);

        let mut rejected = report;
        rejected.verdict = CritiqueVerdict::Rejected;
        assert!(!rejected.admitted());
    }

    #[test]
    fn critique_as_str_matches_wire_values() {
        assert_eq!(CritiqueVerdict::Accepted.as_str(), "accepted");
        assert_eq!(CritiqueVerdict::NeedsRevision.as_str(), "needs_revision");
        assert_eq!(CritiqueVerdict::Rejected.as_str(), "rejected");
        assert_eq!(ContractElement::Verb.as_str(), "verb");
        assert_eq!(ContractElement::Object.as_str(), "object");
        assert_eq!(ContractElement::ArtifactPath.as_str(), "artifact_path");
        assert_eq!(ContractElement::HitSignal.as_str(), "hit_signal");
    }

    #[test]
    fn critique_report_new_takes_python_defaults() {
        let report = CritiqueReport::new(
            ProjectId::new("p".to_string()),
            BranchId::new("b".to_string()),
            CritiqueVerdict::NeedsRevision,
            "h".to_string(),
        );
        assert_eq!(report.mission_id, None);
        assert_eq!(report.run_id, None);
        assert!(report.reasons.is_empty());
        assert!(report.speculative_phrases.is_empty());
        assert!(report.missing_contract_elements.is_empty());
        assert!(report.unknown_fact_ids.is_empty());
        assert!(!report.falsifiable);
        assert_eq!(report.restated_hypothesis, None);
        assert!(report.confidence.abs() < f64::EPSILON);
        assert_eq!(report.created_by, "critique_agent");
        assert!(report.metadata.is_empty());
        assert!(!report.admitted());
    }

    #[test]
    fn critique_report_rejects_unknown_fields() {
        let result: Result<CritiqueReport, _> = serde_json::from_str(
            r#"{"project_id":"p","branch_id":"b","verdict":"rejected","hypothesis":"h","extra":1}"#,
        );
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }
}
