//! Provenance Gate —— `server/core/evidence/provenance_gate.py` 的移植。
//!
//! Finding 升级到 `confirmed` 的唯一显式不可绕过闸门："无证据 → 不确认"
//! 规则集中在此审计，而不是散落在 manager 各处的内联检查。
//!
//! 纯函数：无 I/O、无副作用。Evidence 记录与已知 `ToolInvocation` id 由
//! 调用方取回并传入。

use std::collections::HashSet;

use models::{Evidence, Finding, FindingStatus};

use crate::python_list_repr;

/// Provenance 闸门检查结果（镜像 Python `ProvenanceDecision` frozen
/// dataclass）。
///
/// 字段私有 + 只读访问器：frozen 语义在类型层表达（Python 侧靠
/// `@dataclass(frozen=True)` 抛 `AttributeError`，Rust 侧让赋值根本
/// 编译不过）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceDecision {
    allowed: bool,
    reason: Option<String>,
}

impl ProvenanceDecision {
    /// 构造一个裁决。
    #[must_use]
    pub fn new(allowed: bool, reason: Option<String>) -> Self {
        Self { allowed, reason }
    }

    /// 是否放行。
    #[must_use]
    pub fn allowed(&self) -> bool {
        self.allowed
    }

    /// 拒绝原因（放行时为 `None`）。
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

/// 不可绕过闸门：无证据的 Finding 不能被确认。
pub struct ProvenanceGate;

impl ProvenanceGate {
    /// 检查 *finding* 是否可升级到 `confirmed`。
    ///
    /// 至少引用一个 evidence id 才放行；提供 `evidence_records` 时，每条
    /// 被引用的 Evidence 必须存在并携带工件路径、指纹、ToolInvocation id；
    /// 提供已知 invocation id 集合时，引用会对照该集合校验。
    #[must_use]
    pub fn check_confirmation(
        finding: &Finding,
        evidence_records: Option<&[Evidence]>,
        known_tool_invocation_ids: Option<&HashSet<String>>,
    ) -> ProvenanceDecision {
        if finding.evidence_ids.is_empty() {
            return ProvenanceDecision::new(
                false,
                Some(format!(
                    "provenance gate: finding {} has no evidence chain; confirmation blocked \
                     \u{2014} every confirmed finding must reference at least one Evidence",
                    finding.id
                )),
            );
        }

        if let Some(records) = evidence_records {
            let linked: Vec<&Evidence> = records
                .iter()
                .filter(|e| finding.evidence_ids.iter().any(|id| id == e.id.as_str()))
                .collect();
            let missing_records: Vec<&str> = finding
                .evidence_ids
                .iter()
                .filter(|id| !linked.iter().any(|e| e.id.as_str() == *id))
                .map(String::as_str)
                .collect();
            if !missing_records.is_empty() {
                return ProvenanceDecision::new(
                    false,
                    Some(format!(
                        "provenance gate: finding {} evidence_ids reference unknown Evidence \
                         records {}",
                        finding.id,
                        python_list_repr(missing_records)
                    )),
                );
            }
            let missing_provenance: Vec<&str> = linked
                .iter()
                .filter(|e| {
                    e.evidence_path.as_deref().is_none_or(str::is_empty)
                        || e.produced_by_tool_invocation_id.is_none()
                        || e.fingerprint.as_deref().is_none_or(str::is_empty)
                })
                .map(|e| e.id.as_str())
                .collect();
            if !missing_provenance.is_empty() {
                return ProvenanceDecision::new(
                    false,
                    Some(format!(
                        "provenance gate: evidence {} lacks provenance (evidence_path, \
                         fingerprint, or produced_by_tool_invocation_id missing)",
                        python_list_repr(missing_provenance)
                    )),
                );
            }
            if let Some(known) = known_tool_invocation_ids {
                let unknown_invocations: Vec<&str> = linked
                    .iter()
                    .filter(|e| {
                        // missing_provenance 已保证此处 produced_by 非空；
                        // None 值在此分支不可达。
                        e.produced_by_tool_invocation_id
                            .as_ref()
                            .is_none_or(|id| !known.contains(id.as_str()))
                    })
                    .map(|e| {
                        e.produced_by_tool_invocation_id
                            .as_ref()
                            .map_or("", |id| id.as_str())
                    })
                    .collect();
                if !unknown_invocations.is_empty() {
                    return ProvenanceDecision::new(
                        false,
                        Some(format!(
                            "provenance gate: evidence for finding {} references unknown \
                             ToolInvocation records {}",
                            finding.id,
                            python_list_repr(unknown_invocations)
                        )),
                    );
                }
            }
        }

        ProvenanceDecision::new(true, None)
    }

    /// 检查 *finding* 是否可迁移到 `new_status`。
    ///
    /// 只有 `confirmed` 需要证据；其他迁移一律放行。这是所有状态升级的
    /// 单一入口。
    #[must_use]
    pub fn check_status_transition(
        finding: &Finding,
        new_status: FindingStatus,
        evidence_records: Option<&[Evidence]>,
        known_tool_invocation_ids: Option<&HashSet<String>>,
    ) -> ProvenanceDecision {
        if new_status != FindingStatus::Confirmed {
            return ProvenanceDecision::new(true, None);
        }
        Self::check_confirmation(finding, evidence_records, known_tool_invocation_ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::{EvidenceId, EvidenceKind, FindingId, ProjectId, Severity};

    fn make_finding(status: FindingStatus, evidence_ids: &[&str]) -> Finding {
        let mut finding = Finding::new(
            ProjectId::new("proj-1".to_string()),
            "Test SQL Injection".to_string(),
        );
        finding.id = FindingId::new("find-test-001".to_string());
        finding.severity = Severity::High;
        finding.status = status;
        finding.evidence_ids = evidence_ids.iter().map(|s| (*s).to_string()).collect();
        finding
    }

    fn linked_evidence(
        eid: &str,
        evidence_path: Option<&str>,
        tool_invocation_id: Option<&str>,
        fingerprint: Option<&str>,
    ) -> Evidence {
        let mut evidence = Evidence::new(
            ProjectId::new("p1".to_string()),
            EvidenceKind::ToolOutput,
            "tool output".to_string(),
        );
        evidence.id = EvidenceId::new(eid.to_string());
        evidence.evidence_path = evidence_path.map(str::to_string);
        evidence.fingerprint = fingerprint.map(str::to_string);
        if let Some(tool) = tool_invocation_id {
            evidence.produced_by_tool_invocation_id =
                Some(models::ToolInvocationId::new(tool.to_string()));
        }
        evidence
    }

    #[test]
    fn allows_confirmation_with_evidence() {
        let finding = make_finding(FindingStatus::Candidate, &["ev-1", "ev-2"]);
        let decision = ProvenanceGate::check_confirmation(&finding, None, None);
        assert!(decision.allowed());
        assert_eq!(decision.reason(), None);
    }

    #[test]
    fn blocks_confirmation_without_evidence() {
        let finding = make_finding(FindingStatus::Candidate, &[]);
        let decision = ProvenanceGate::check_confirmation(&finding, None, None);
        assert!(!decision.allowed());
        let reason = decision.reason().unwrap_or_default().to_string();
        assert!(
            reason.to_lowercase().contains("provenance gate"),
            "原因必须点名闸门: {reason}"
        );
        assert!(
            reason.contains(finding.id.as_str()),
            "原因必须携带 finding id: {reason}"
        );
    }

    #[test]
    fn blocks_confirmation_with_empty_evidence_list() {
        let finding = make_finding(FindingStatus::Candidate, &[]);
        let decision = ProvenanceGate::check_confirmation(&finding, None, None);
        assert!(!decision.allowed());
    }

    #[test]
    fn decision_is_frozen() {
        // frozen 语义由私有字段在编译期保证（赋值代码无法编译），
        // 此处锁定构造后取值不被意外篡改。
        let decision = ProvenanceGate::check_confirmation(
            &make_finding(FindingStatus::Candidate, &["ev-1"]),
            None,
            None,
        );
        assert!(decision.allowed());
        assert_eq!(decision.reason(), None);
        let copy = decision.clone();
        assert_eq!(copy, decision);
    }

    #[test]
    fn non_confirmed_transition_always_allowed() {
        let finding = make_finding(FindingStatus::Candidate, &[]);
        for status in [
            FindingStatus::NeedsReview,
            FindingStatus::FalsePositive,
            FindingStatus::Duplicate,
            FindingStatus::Candidate,
        ] {
            let decision = ProvenanceGate::check_status_transition(&finding, status, None, None);
            assert!(decision.allowed(), "{status:?} 不应被闸门拦截");
        }
    }

    #[test]
    fn confirmed_transition_blocked_without_evidence() {
        let finding = make_finding(FindingStatus::Candidate, &[]);
        let decision =
            ProvenanceGate::check_status_transition(&finding, FindingStatus::Confirmed, None, None);
        assert!(!decision.allowed());
        assert!(
            decision
                .reason()
                .unwrap_or_default()
                .to_lowercase()
                .contains("evidence"),
            "原因必须点名 evidence: {:?}",
            decision.reason()
        );
    }

    #[test]
    fn confirmed_transition_allowed_with_evidence() {
        let finding = make_finding(FindingStatus::Candidate, &["ev-1"]);
        let decision =
            ProvenanceGate::check_status_transition(&finding, FindingStatus::Confirmed, None, None);
        assert!(decision.allowed());
    }

    #[test]
    fn default_reason_is_none() {
        let decision = ProvenanceDecision::new(true, None);
        assert_eq!(decision.reason(), None);
    }

    #[test]
    fn rejection_has_reason() {
        let decision = ProvenanceDecision::new(false, Some("blocked".to_string()));
        assert!(!decision.allowed());
        assert_eq!(decision.reason(), Some("blocked"));
    }

    #[test]
    fn blocks_when_evidence_lacks_provenance() {
        let finding = make_finding(FindingStatus::Candidate, &["e1"]);
        let decision = ProvenanceGate::check_confirmation(
            &finding,
            Some(&[linked_evidence("e1", None, None, None)]),
            None,
        );
        assert!(!decision.allowed());
        assert!(
            decision.reason().unwrap_or_default().contains("provenance"),
            "原因必须点名 provenance: {:?}",
            decision.reason()
        );
    }

    #[test]
    fn allows_with_full_provenance() {
        let finding = make_finding(FindingStatus::Candidate, &["e1"]);
        let decision = ProvenanceGate::check_confirmation(
            &finding,
            Some(&[linked_evidence(
                "e1",
                Some("/ws/m1/evidence/e1.json"),
                Some("ti1"),
                Some("sha256:deadbeef"),
            )]),
            None,
        );
        assert!(decision.allowed());
    }

    #[test]
    fn blocks_when_evidence_lacks_fingerprint() {
        let finding = make_finding(FindingStatus::Candidate, &["e1"]);
        let decision = ProvenanceGate::check_confirmation(
            &finding,
            Some(&[linked_evidence(
                "e1",
                Some("/ws/m1/evidence/e1.json"),
                Some("ti1"),
                None,
            )]),
            None,
        );
        assert!(!decision.allowed());
        assert!(
            decision
                .reason()
                .unwrap_or_default()
                .contains("fingerprint"),
            "原因必须点名 fingerprint: {:?}",
            decision.reason()
        );
    }

    #[test]
    fn blocks_unknown_tool_invocation() {
        let finding = make_finding(FindingStatus::Candidate, &["e1"]);
        let known: HashSet<String> = ["other".to_string()].into_iter().collect();
        let decision = ProvenanceGate::check_confirmation(
            &finding,
            Some(&[linked_evidence(
                "e1",
                Some("/ws/m1/evidence/e1.json"),
                Some("ti1"),
                Some("sha256:deadbeef"),
            )]),
            Some(&known),
        );
        assert!(!decision.allowed());
        assert!(
            decision
                .reason()
                .unwrap_or_default()
                .contains("ToolInvocation"),
            "原因必须点名 ToolInvocation: {:?}",
            decision.reason()
        );
    }

    #[test]
    fn reason_lists_match_python_repr() {
        // 双跑一致性：Python f"{missing_records}" 是列表 repr（单引号）。
        let finding = make_finding(FindingStatus::Candidate, &["ev-9"]);
        let decision = ProvenanceGate::check_confirmation(
            &finding,
            Some(&[linked_evidence(
                "e1",
                Some("/ws/m1/evidence/e1.json"),
                Some("ti1"),
                Some("sha256:deadbeef"),
            )]),
            None,
        );
        assert!(
            decision.reason().unwrap_or_default().contains("['ev-9']"),
            "缺失列表必须按 Python repr 格式化: {:?}",
            decision.reason()
        );
    }
}
