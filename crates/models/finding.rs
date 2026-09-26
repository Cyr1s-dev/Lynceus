//! Finding 模型 —— `server/core/models/finding.py` 的移植。
//!
//! Python 侧的 `model_validator`（CONFIRMED 必须携带证据）通过
//! `validated()`（构造路径）与 serde `try_from`（解析路径）双入口镜像，
//! 与 `MissionGoalContract` 的模式一致。

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::common::Timestamp;
use crate::common::new_id;
use crate::common::utcnow;
use crate::ids::BranchId;
use crate::ids::FindingId;
use crate::ids::MissionId;
use crate::ids::ProjectId;
use crate::ids::RunId;
use crate::ids::TaskId;
use crate::lifecycle::FindingStatus;
use crate::lifecycle::Severity;

fn default_finding_id() -> FindingId {
    FindingId::new(new_id("find"))
}

fn default_severity() -> Severity {
    Severity::Medium
}

fn default_finding_status() -> FindingStatus {
    FindingStatus::Candidate
}

/// Finding 校验失败（镜像 pydantic `model_validator`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FindingError {
    /// CONFIRMED 状态必须引用至少一个 Evidence。
    #[error("a CONFIRMED finding must reference at least one Evidence")]
    ConfirmedWithoutEvidence,
}

/// Finding：漏洞结论（`Finding`），必须始终有 Evidence 支撑。
///
/// 不变量：非 candidate 状态（尤其 CONFIRMED）必须引用至少一个 Evidence
/// ID。`Observer` 负责在状态间迁移（`candidate` → `confirmed` /
/// `needs_review` / `false_positive` / `duplicate`）；`fingerprint` 用于去重，`dedup_of` 把
/// 重复项指向规范 Finding。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "FindingWire")]
pub struct Finding {
    /// Finding 标识符。
    pub id: FindingId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Mission。
    pub mission_id: Option<MissionId>,
    /// 所属 Branch。
    pub branch_id: Option<BranchId>,
    /// 所属 Run。
    pub run_id: Option<RunId>,
    /// 标题。
    pub title: String,
    /// 描述。
    pub description: Option<String>,
    /// 严重级别。
    pub severity: Severity,
    /// Observer 评审状态。
    pub status: FindingStatus,
    /// CWE 分类（如 `CWE-89`）。
    pub cwe: Option<String>,
    /// 求解器/工具规则标识。
    pub rule_id: Option<String>,
    /// 证据链（非 candidate 状态必须非空）。
    pub evidence_ids: Vec<String>,
    /// 关联 Fact ID 列表。
    pub related_fact_ids: Vec<String>,
    /// Observer 校验完整性用的 source 标记。
    pub source_label: Option<String>,
    /// Observer 校验完整性用的 sink 标记。
    pub sink_label: Option<String>,
    /// 去重指纹。
    pub fingerprint: Option<String>,
    /// 重复项指向的规范 Finding。
    pub dedup_of: Option<String>,
    /// Observer 的结构化评审记录（键序 = 插入序）。
    pub review: Map<String, Value>,
    /// 产生该 Finding 的 Task。
    pub produced_by_task_id: Option<TaskId>,
    /// 创建时间。
    pub created_at: Timestamp,
    /// 最后更新时间。
    pub updated_at: Timestamp,
}

impl Finding {
    /// 以 Python 默认值构造（`Finding(project_id=..., title=...)`）。
    #[must_use]
    pub fn new(project_id: ProjectId, title: String) -> Self {
        Self {
            id: default_finding_id(),
            project_id,
            mission_id: None,
            branch_id: None,
            run_id: None,
            title,
            description: None,
            severity: default_severity(),
            status: default_finding_status(),
            cwe: None,
            rule_id: None,
            evidence_ids: Vec::new(),
            related_fact_ids: Vec::new(),
            source_label: None,
            sink_label: None,
            fingerprint: None,
            dedup_of: None,
            review: Map::new(),
            produced_by_task_id: None,
            created_at: utcnow(),
            updated_at: utcnow(),
        }
    }

    /// 校验不变量，返回可持久化的 Finding。
    ///
    /// 消费 `self`：与 Python `model_validator(mode="after")` 对齐，
    /// 构造与解析两条路径都必须经过这里。
    ///
    /// # Errors
    /// - [`FindingError::ConfirmedWithoutEvidence`]。
    pub fn validated(self) -> Result<Self, FindingError> {
        if self.status == FindingStatus::Confirmed && self.evidence_ids.is_empty() {
            return Err(FindingError::ConfirmedWithoutEvidence);
        }
        Ok(self)
    }
}

/// [`Finding`] 的解析镜像：缺省字段取 Python 默认值，未知字段拒绝
/// （`extra="forbid"`），解析后走 `validated()`。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FindingWire {
    id: FindingId,
    project_id: ProjectId,
    mission_id: Option<MissionId>,
    branch_id: Option<BranchId>,
    run_id: Option<RunId>,
    title: String,
    description: Option<String>,
    severity: Severity,
    status: FindingStatus,
    cwe: Option<String>,
    rule_id: Option<String>,
    evidence_ids: Vec<String>,
    related_fact_ids: Vec<String>,
    source_label: Option<String>,
    sink_label: Option<String>,
    fingerprint: Option<String>,
    dedup_of: Option<String>,
    review: Map<String, Value>,
    produced_by_task_id: Option<TaskId>,
    created_at: Timestamp,
    updated_at: Timestamp,
}

impl Default for FindingWire {
    fn default() -> Self {
        let base = Finding::new(ProjectId::new(String::new()), String::new());
        Self {
            id: base.id,
            project_id: base.project_id,
            mission_id: base.mission_id,
            branch_id: base.branch_id,
            run_id: base.run_id,
            title: base.title,
            description: base.description,
            severity: base.severity,
            status: base.status,
            cwe: base.cwe,
            rule_id: base.rule_id,
            evidence_ids: base.evidence_ids,
            related_fact_ids: base.related_fact_ids,
            source_label: base.source_label,
            sink_label: base.sink_label,
            fingerprint: base.fingerprint,
            dedup_of: base.dedup_of,
            review: base.review,
            produced_by_task_id: base.produced_by_task_id,
            created_at: base.created_at,
            updated_at: base.updated_at,
        }
    }
}

impl TryFrom<FindingWire> for Finding {
    type Error = FindingError;

    fn try_from(wire: FindingWire) -> Result<Self, Self::Error> {
        Self {
            id: wire.id,
            project_id: wire.project_id,
            mission_id: wire.mission_id,
            branch_id: wire.branch_id,
            run_id: wire.run_id,
            title: wire.title,
            description: wire.description,
            severity: wire.severity,
            status: wire.status,
            cwe: wire.cwe,
            rule_id: wire.rule_id,
            evidence_ids: wire.evidence_ids,
            related_fact_ids: wire.related_fact_ids,
            source_label: wire.source_label,
            sink_label: wire.sink_label,
            fingerprint: wire.fingerprint,
            dedup_of: wire.dedup_of,
            review: wire.review,
            produced_by_task_id: wire.produced_by_task_id,
            created_at: wire.created_at,
            updated_at: wire.updated_at,
        }
        .validated()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timestamp() -> Timestamp {
        "2026-08-24T12:00:00.123456Z"
            .parse()
            .unwrap_or_else(|error| panic!("固定时间必须可解析: {error}"))
    }

    #[test]
    fn finding_serializes_to_python_wire_bytes() {
        // 期望串逐字节来自 scripts/probe_parity_wire.py 探针输出。
        let expected = concat!(
            r#"{"id":"find_fix_0001","project_id":"proj_parity","#,
            r#""mission_id":"mission_fix_0001","branch_id":"branch_fix_0001","#,
            r#""run_id":"run_fix_0001","title":"Eval injection","#,
            r#""description":"user input reaches eval","severity":"high","#,
            r#""status":"confirmed","cwe":"CWE-95","#,
            r#""rule_id":"web_sast.eval_injection","#,
            r#""evidence_ids":["evd_fix_0001"],"related_fact_ids":["fact_1"],"#,
            r#""source_label":"request.data","sink_label":"eval","#,
            r#""fingerprint":"sha256:def456","dedup_of":null,"#,
            r#""review":{"zz":1,"aa":2},"#,
            r#""produced_by_task_id":"task_fix_0001","#,
            r#""created_at":"2026-08-24T12:00:00.123456Z","#,
            r#""updated_at":"2026-08-24T12:00:00.123456Z"}"#
        );
        let finding = Finding {
            id: FindingId::new("find_fix_0001".to_string()),
            project_id: ProjectId::new("proj_parity".to_string()),
            mission_id: Some(MissionId::new("mission_fix_0001".to_string())),
            branch_id: Some(BranchId::new("branch_fix_0001".to_string())),
            run_id: Some(RunId::new("run_fix_0001".to_string())),
            title: "Eval injection".to_string(),
            description: Some("user input reaches eval".to_string()),
            severity: Severity::High,
            status: FindingStatus::Confirmed,
            cwe: Some("CWE-95".to_string()),
            rule_id: Some("web_sast.eval_injection".to_string()),
            evidence_ids: vec!["evd_fix_0001".to_string()],
            related_fact_ids: vec!["fact_1".to_string()],
            source_label: Some("request.data".to_string()),
            sink_label: Some("eval".to_string()),
            fingerprint: Some("sha256:def456".to_string()),
            dedup_of: None,
            review: [("zz", Value::from(1)), ("aa", Value::from(2))]
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
            produced_by_task_id: Some(TaskId::new("task_fix_0001".to_string())),
            created_at: timestamp(),
            updated_at: timestamp(),
        };
        let json = serde_json::to_string(&finding)
            .unwrap_or_else(|error| panic!("Finding 序列化不会失败: {error}"));
        assert_eq!(json, expected);

        let back: Finding = serde_json::from_str(&json)
            .unwrap_or_else(|error| panic!("自身输出必须可解析: {error}"));
        assert_eq!(back, finding);
    }

    #[test]
    fn finding_candidate_defaults_match_python() {
        // 镜像 pytest：test_candidate_finding_allows_empty_evidence。
        let finding = Finding::new(
            ProjectId::new("proj_x".to_string()),
            "candidate".to_string(),
        )
        .validated()
        .unwrap_or_else(|error| panic!("candidate 允许空证据: {error}"));
        assert_eq!(finding.status, FindingStatus::Candidate);
        assert_eq!(finding.severity, Severity::Medium);
        assert!(finding.id.as_str().starts_with("find_"));
    }

    #[test]
    fn finding_confirmed_without_evidence_is_rejected() {
        // 镜像 pytest：test_confirmed_finding_requires_evidence。
        let finding = Finding {
            status: FindingStatus::Confirmed,
            evidence_ids: Vec::new(),
            ..Finding::new(
                ProjectId::new("proj_x".to_string()),
                "no evidence".to_string(),
            )
        }
        .validated();
        assert_eq!(
            finding.err(),
            Some(FindingError::ConfirmedWithoutEvidence),
            "CONFIRMED 无证据必须被拒绝"
        );

        // 解析路径同样拒绝。
        let result: Result<Finding, _> = serde_json::from_str(
            r#"{"project_id":"proj_x","title":"no evidence","status":"confirmed","evidence_ids":[]}"#,
        );
        assert!(result.is_err(), "model_validator 在解析路径同样生效");
    }

    #[test]
    fn finding_deserialize_applies_python_defaults_on_missing_fields() {
        let json = r#"{"project_id":"p","title":"t"}"#;
        let finding: Finding = serde_json::from_str(json)
            .unwrap_or_else(|error| panic!("pydantic 接受缺省字段，serde 必须同样接受: {error}"));
        assert_eq!(finding.status, FindingStatus::Candidate);
        assert_eq!(finding.severity, Severity::Medium);
        assert!(finding.evidence_ids.is_empty());
        assert!(finding.review.is_empty());
        assert!(finding.id.as_str().starts_with("find_"));
    }

    #[test]
    fn finding_deserialize_rejects_unknown_fields() {
        let result: Result<Finding, _> =
            serde_json::from_str(r#"{"project_id":"p","title":"t","surprise":1}"#);
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }
}
