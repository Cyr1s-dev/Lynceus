//! Guardian 质量门 —— `server/core/evidence/guardian.py` 的移植。
//!
//! 双重证据门的第一道（防"乱写"），与 Provenance Gate（防"洗稿"）相对。
//! 四条确定性规则：
//!
//! 1. 验证谓词存在（evidence 链非空，且引用的 Evidence 可解析）；
//! 2. CONFIRMED 状态的 Finding 标题/描述不含猜测措辞；
//! 3. 关联的 Evidence 必须携带工件路径与指纹；
//! 4. 必填字段齐全（title、severity、至少一个 evidence id）。
//!
//! 纯函数：无 I/O、无 LLM、硬编码、不可绕过。未过门的 Finding 降级
//! （[`FindingStatus::NeedsReview`]），不丢弃。

use models::{Evidence, Finding, FindingStatus};

use crate::python_list_repr;

/// 猜测措辞黑名单：CONFIRMED 的 Finding 不得包含。
const SPECULATIVE_PHRASES: [&str; 14] = [
    "may be",
    "might be",
    "could be",
    "possibly",
    "potentially",
    "i think",
    "i believe",
    "seems to",
    "appears to",
    "likely",
    "uncertain",
    "not sure",
    "guess",
    "assume",
];

/// Guardian 质量门检查结果（镜像 Python `GuardianVerdict` frozen dataclass）。
///
/// 字段私有 + 只读访问器：frozen 语义在类型层表达。失败时按
/// `demoted_status` 降级（保留，不丢弃）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardianVerdict {
    passed: bool,
    reasons: Vec<String>,
    demoted_status: FindingStatus,
}

impl GuardianVerdict {
    /// 是否通过全部确定性检查。
    #[must_use]
    pub fn passed(&self) -> bool {
        self.passed
    }

    /// 失败原因列表（通过时为空）。
    #[must_use]
    pub fn reasons(&self) -> &[String] {
        &self.reasons
    }

    /// 失败时应用的状态：降级不丢弃。
    #[must_use]
    pub fn demoted_status(&self) -> FindingStatus {
        self.demoted_status
    }

    /// 原因拼接文本（与 Python `reason_text` 一致：空列表输出 "ok"）。
    #[must_use]
    pub fn reason_text(&self) -> String {
        if self.reasons.is_empty() {
            "ok".to_string()
        } else {
            self.reasons.join("; ")
        }
    }
}

/// Finding 的确定性、非 LLM 质量门。
pub struct Guardian;

impl Guardian {
    /// 执行四条确定性检查。永不 panic、永不返回 Err（镜像 Python "Never
    /// raises" 契约）。
    ///
    /// `evidence_records` 为 `None` 时只检查 Finding 自身可判定的规则
    /// （规则 1 退化为 `evidence_ids` 非空，规则 3 跳过）。
    #[must_use]
    pub fn check(finding: &Finding, evidence_records: Option<&[Evidence]>) -> GuardianVerdict {
        let mut reasons: Vec<String> = Vec::new();

        // 规则 1：验证谓词存在。
        if finding.evidence_ids.is_empty() {
            reasons.push("no evidence chain (evidence_ids empty)".to_string());
        } else if let Some(records) = evidence_records {
            let linked = records
                .iter()
                .any(|e| finding.evidence_ids.iter().any(|id| id == e.id.as_str()));
            if !linked {
                reasons.push("evidence_ids reference no known Evidence records".to_string());
            }
        }

        // 规则 4（廉价）：必填字段齐全。
        if finding.title.trim().is_empty() {
            reasons.push("title is empty".to_string());
        }
        // Python 侧 `not finding.severity` 对枚举成员恒为假（防御性死代码）；
        // Rust 的 `Severity` 是非 Optional 枚举，"未设置"状态不可表示，
        // 语义等价地省略该检查。

        // 规则 2：CONFIRMED 不得含猜测措辞。
        if finding.status == FindingStatus::Confirmed {
            let haystack = format!(
                "{} {}",
                finding.title,
                finding.description.as_deref().unwrap_or("")
            )
            .to_lowercase();
            let hits: Vec<&str> = SPECULATIVE_PHRASES
                .iter()
                .copied()
                .filter(|phrase| haystack.contains(phrase))
                .collect();
            if !hits.is_empty() {
                reasons.push(format!(
                    "speculative wording in confirmed finding: {}",
                    python_list_repr(hits)
                ));
            }
        }

        // 规则 3：关联 Evidence 必须带工件路径与指纹。
        if let Some(records) = evidence_records
            && !records.is_empty()
            && !finding.evidence_ids.is_empty()
        {
            let linked: Vec<&Evidence> = records
                .iter()
                .filter(|e| finding.evidence_ids.iter().any(|id| id == e.id.as_str()))
                .collect();
            let empty_path: Vec<&str> = linked
                .iter()
                .filter(|e| e.evidence_path.as_deref().is_none_or(str::is_empty))
                .map(|e| e.id.as_str())
                .collect();
            if !empty_path.is_empty() {
                reasons.push(format!(
                    "linked evidence missing evidence_path: {}",
                    python_list_repr(empty_path)
                ));
            }
            let empty_fingerprint: Vec<&str> = linked
                .iter()
                .filter(|e| e.fingerprint.as_deref().is_none_or(str::is_empty))
                .map(|e| e.id.as_str())
                .collect();
            if !empty_fingerprint.is_empty() {
                reasons.push(format!(
                    "linked evidence missing fingerprint: {}",
                    python_list_repr(empty_fingerprint)
                ));
            }
        }

        let passed = reasons.is_empty();
        GuardianVerdict {
            passed,
            reasons,
            demoted_status: FindingStatus::NeedsReview,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::{EvidenceKind, ProjectId, Severity};

    fn finding(
        status: FindingStatus,
        title: &str,
        evidence_ids: &[&str],
        description: &str,
    ) -> Finding {
        let mut f = Finding::new(ProjectId::new("p1".to_string()), title.to_string());
        f.id = models::FindingId::new("f1".to_string());
        f.description = Some(description.to_string());
        f.severity = Severity::High;
        f.status = status;
        f.evidence_ids = evidence_ids.iter().map(|s| (*s).to_string()).collect();
        f
    }

    fn evidence(
        eid: &str,
        evidence_path: Option<&str>,
        tool_invocation_id: Option<&str>,
        fingerprint: Option<&str>,
    ) -> Evidence {
        let mut e = Evidence::new(
            ProjectId::new("p1".to_string()),
            EvidenceKind::ToolOutput,
            "tool output".to_string(),
        );
        e.id = models::EvidenceId::new(eid.to_string());
        e.evidence_path = evidence_path.map(str::to_string);
        e.fingerprint = fingerprint.map(str::to_string);
        if let Some(tool) = tool_invocation_id {
            e.produced_by_tool_invocation_id =
                Some(models::ToolInvocationId::new(tool.to_string()));
        }
        e
    }

    #[test]
    fn passes_with_evidence_and_path() {
        let verdict = Guardian::check(
            &finding(
                FindingStatus::Confirmed,
                "SQL injection in /login",
                &["e1"],
                "verified via tool output",
            ),
            Some(&[evidence(
                "e1",
                Some("/ws/m1/evidence/e1.json"),
                Some("ti1"),
                Some("sha256:deadbeef"),
            )]),
        );
        assert!(verdict.passed());
        assert!(verdict.reasons().is_empty());
        assert_eq!(verdict.reason_text(), "ok");
    }

    #[test]
    fn fails_no_evidence() {
        // CONFIRMED + 空 evidence_ids 会被 Finding 模型校验拒绝（行为正确），
        // 故用 NEEDS_REVIEW 触发同一 Guardian 规则。
        let verdict = Guardian::check(
            &finding(
                FindingStatus::NeedsReview,
                "SQL injection in /login",
                &[],
                "verified via tool output",
            ),
            Some(&[]),
        );
        assert!(!verdict.passed());
        assert!(
            verdict.reasons()[0].contains("no evidence chain"),
            "首条原因必须是证据链缺失: {:?}",
            verdict.reasons()
        );
    }

    #[test]
    fn demotes_speculative_confirmed() {
        let verdict = Guardian::check(
            &finding(
                FindingStatus::Confirmed,
                "SQL injection in /login",
                &["e1"],
                "this may be vulnerable, could be exploitable",
            ),
            Some(&[evidence(
                "e1",
                Some("/ws/m1/evidence/e1.json"),
                Some("ti1"),
                Some("sha256:deadbeef"),
            )]),
        );
        assert!(!verdict.passed());
        assert_eq!(verdict.demoted_status(), FindingStatus::NeedsReview);
        assert!(
            verdict.reasons().iter().any(|r| r.contains("speculative")),
            "猜测措辞必须被点名: {:?}",
            verdict.reasons()
        );
    }

    #[test]
    fn fails_missing_evidence_path() {
        let verdict = Guardian::check(
            &finding(
                FindingStatus::Confirmed,
                "SQL injection in /login",
                &["e1"],
                "verified via tool output",
            ),
            Some(&[evidence("e1", None, Some("ti1"), Some("sha256:deadbeef"))]),
        );
        assert!(!verdict.passed());
        assert!(
            verdict
                .reasons()
                .iter()
                .any(|r| r.contains("evidence_path")),
            "缺失 evidence_path 必须被点名: {:?}",
            verdict.reasons()
        );
    }

    #[test]
    fn fails_missing_fingerprint() {
        let verdict = Guardian::check(
            &finding(
                FindingStatus::Confirmed,
                "SQL injection in /login",
                &["e1"],
                "verified via tool output",
            ),
            Some(&[evidence(
                "e1",
                Some("/ws/m1/evidence/e1.json"),
                Some("ti1"),
                None,
            )]),
        );
        assert!(!verdict.passed());
        assert!(
            verdict.reasons().iter().any(|r| r.contains("fingerprint")),
            "缺失 fingerprint 必须被点名: {:?}",
            verdict.reasons()
        );
    }

    #[test]
    fn empty_title_fails() {
        let verdict = Guardian::check(
            &finding(FindingStatus::NeedsReview, "  ", &["e1"], "desc"),
            Some(&[evidence(
                "e1",
                Some("/ws/m1/evidence/e1.json"),
                Some("ti1"),
                Some("sha256:deadbeef"),
            )]),
        );
        assert!(!verdict.passed());
        assert!(
            verdict.reasons().iter().any(|r| r.contains("title")),
            "空白标题必须被点名: {:?}",
            verdict.reasons()
        );
    }

    #[test]
    fn unknown_evidence_reference_fails() {
        let verdict = Guardian::check(
            &finding(
                FindingStatus::NeedsReview,
                "SQL injection in /login",
                &["e-missing"],
                "verified via tool output",
            ),
            Some(&[evidence(
                "e1",
                Some("/ws/m1/evidence/e1.json"),
                Some("ti1"),
                Some("sha256:deadbeef"),
            )]),
        );
        assert!(!verdict.passed());
        assert!(
            verdict
                .reasons()
                .iter()
                .any(|r| r.contains("reference no known Evidence")),
            "引用不存在的 Evidence 必须被点名: {:?}",
            verdict.reasons()
        );
    }

    #[test]
    fn speculative_phrases_match_python_list_repr() {
        // 双跑一致性：Python f"{hits}" 是列表 repr（单引号）。
        let verdict = Guardian::check(
            &finding(
                FindingStatus::Confirmed,
                "SQL injection in /login",
                &["e1"],
                "this may be vulnerable, could be exploitable",
            ),
            Some(&[evidence(
                "e1",
                Some("/ws/m1/evidence/e1.json"),
                Some("ti1"),
                Some("sha256:deadbeef"),
            )]),
        );
        let speculative = verdict
            .reasons()
            .iter()
            .find(|r| r.contains("speculative"))
            .cloned()
            .unwrap_or_else(|| panic!("必须包含猜测措辞原因: {:?}", verdict.reasons()));
        assert!(
            speculative.contains("['may be', 'could be']"),
            "hits 列表必须按 Python repr 格式化: {speculative}"
        );
    }
}
