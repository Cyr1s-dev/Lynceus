//! Observer：Finding 与审计证据链的旁路监督 ——
//! `server/core/agents/observer.py` 的移植。
//!
//! Observer 不跑扫描器。它检视 findings + evidence 并返回**结构化**
//! [`ObserverReport`]（绝不只是自由文本）。检查项：
//! - Finding 是否至少有一条 Evidence？
//! - source 与 sink 是否都已标注？
//! - 证据链是否完整（Evidence 引用了真实存在的 Fact）？
//! - 这是否可能是误报/重复？
//! - 是否应发起新的验证 Intent？
//! - Run 是否已可出报告？
//!
//! MVP 阶段这些都是确定性规则检查。生产环境会由 LLM 增补判定，但必须
//! 依然产出同一份结构化报告。

use std::collections::HashMap;
use std::collections::HashSet;

use models::evidence::Evidence;
use models::fact::Fact;
use models::finding::Finding;
use models::ids::RunId;
use models::lifecycle::FindingStatus;
use serde::Deserialize;
use serde::Serialize;

/// Observer 对单条 Finding 可给出的判定（`ObserverVerdict`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObserverVerdict {
    /// 确认。
    Confirm,
    /// 需要更多证据。
    NeedsMoreEvidence,
    /// 疑似误报。
    LikelyFalsePositive,
    /// 重复。
    Duplicate,
}

impl ObserverVerdict {
    /// wire 值（Python `.value` 镜像）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ObserverVerdict::Confirm => "confirm",
            ObserverVerdict::NeedsMoreEvidence => "needs_more_evidence",
            ObserverVerdict::LikelyFalsePositive => "likely_false_positive",
            ObserverVerdict::Duplicate => "duplicate",
        }
    }
}

/// 对单条 Finding 的结构化评审（`FindingReview`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingReview {
    /// 被评审的 Finding 标识符。
    pub finding_id: String,
    /// 判定。
    pub verdict: ObserverVerdict,
    /// 理由列表。
    #[serde(default)]
    pub reasons: Vec<String>,
    /// 缺失项（如 `["source", "sink", "evidence"]`）。
    #[serde(default)]
    pub missing: Vec<String>,
    /// Observer 想要更多验证时建议的下一步 Intent 标题。
    #[serde(default)]
    pub suggested_intent: Option<String>,
}

/// 一次评审回合的 Observer 结构化输出（`ObserverReport`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObserverReport {
    /// 所属 Run。
    pub run_id: String,
    /// 逐条 Finding 的评审。
    #[serde(default)]
    pub reviews: Vec<FindingReview>,
    /// Run 是否已可出报告。
    #[serde(default)]
    pub ready_to_report: bool,
    /// 汇总文本。
    #[serde(default)]
    pub summary: String,
}

/// 一次评审的输入（Python `review` 的 keyword-only 参数镜像）。
pub struct ReviewInput<'a> {
    /// 所属 Run。
    pub run_id: &'a RunId,
    /// 待评审的候选 Finding 列表。
    pub findings: &'a [Finding],
    /// 全量 Evidence（Finding 的证据必须在此可解析）。
    pub evidence: &'a [Evidence],
    /// 全量 Fact（Evidence 的 `supports_fact_ids` 必须链回这里）。
    pub facts: &'a [Fact],
}

/// 确定性、结构化的 Finding/证据链评审者（MVP）。
#[derive(Debug, Default)]
pub struct Observer;

impl Observer {
    /// 评审候选 Finding 并返回结构化判定。
    ///
    /// 去重顺序：先按指纹（或 `rule_id::sink_label` 复合键）判重，再判
    /// 缺失证据——重复项立即定 `duplicate`，不再触发补证建议。
    #[must_use]
    pub fn review(&self, input: &ReviewInput<'_>) -> ObserverReport {
        let evidence_by_id: HashMap<&str, &Evidence> = input
            .evidence
            .iter()
            .map(|item| (item.id.as_str(), item))
            .collect();
        let fact_ids: HashSet<&str> = input.facts.iter().map(|fact| fact.id.as_str()).collect();
        // Python 侧为指纹 → 首个确认 Finding 的 id 的映射。
        let mut seen_fingerprints: HashMap<String, String> = HashMap::new();
        let mut reviews: Vec<FindingReview> = Vec::new();

        for finding in input.findings {
            let mut missing: Vec<&str> = Vec::new();
            let mut reasons: Vec<String> = Vec::new();

            if finding.evidence_ids.is_empty() {
                missing.push("evidence");
            } else {
                // Evidence 必须存在且能链回真实 Fact。
                let dangling: Vec<&str> = finding
                    .evidence_ids
                    .iter()
                    .filter(|evidence_id| !evidence_by_id.contains_key(evidence_id.as_str()))
                    .map(String::as_str)
                    .collect();
                if dangling.is_empty() {
                    let chained = finding.evidence_ids.iter().any(|evidence_id| {
                        evidence_by_id[evidence_id.as_str()]
                            .supports_fact_ids
                            .iter()
                            .any(|fact_id| fact_ids.contains(fact_id.as_str()))
                    });
                    if !chained {
                        reasons.push("evidence does not reference any known fact".to_string());
                    }
                } else {
                    missing.push("evidence");
                    reasons.push(format!("evidence not found: {}", py_repr(&dangling)));
                }
            }

            if finding.source_label.as_deref().unwrap_or("").is_empty() {
                missing.push("source");
            }
            if finding.sink_label.as_deref().unwrap_or("").is_empty() {
                missing.push("sink");
            }

            // 按指纹或 (rule_id, sink) 对判重。
            let fingerprint = match finding.fingerprint.as_deref() {
                Some(value) if !value.is_empty() => value.to_string(),
                _ => format!(
                    "{}::{}",
                    py_str(finding.rule_id.as_deref()),
                    py_str(finding.sink_label.as_deref())
                ),
            };
            let verdict;
            let mut suggested: Option<String> = None;
            if let Some(original_id) = seen_fingerprints.get(&fingerprint) {
                verdict = ObserverVerdict::Duplicate;
                reasons.push(format!("duplicate of {original_id}"));
            } else if !missing.is_empty() {
                verdict = ObserverVerdict::NeedsMoreEvidence;
                let joined = missing.join(", ");
                suggested = Some(format!(
                    "Collect missing context ({joined}) for: {}",
                    finding.title
                ));
            } else {
                verdict = ObserverVerdict::Confirm;
                seen_fingerprints.insert(fingerprint, finding.id.to_string());
            }

            reviews.push(FindingReview {
                finding_id: finding.id.to_string(),
                verdict,
                reasons,
                missing: missing.into_iter().map(str::to_string).collect(),
                suggested_intent: suggested,
            });
        }

        // 没有任何 Finding 还需要补证时才可出报告。
        let ready = reviews
            .iter()
            .all(|review| review.verdict != ObserverVerdict::NeedsMoreEvidence);
        let confirmed = reviews
            .iter()
            .filter(|review| review.verdict == ObserverVerdict::Confirm)
            .count();
        ObserverReport {
            run_id: input.run_id.to_string(),
            reviews,
            ready_to_report: ready,
            summary: format!("{confirmed}/{} findings confirmed", input.findings.len()),
        }
    }

    /// 把 Observer 判定映射为 Finding 状态（`verdict_to_status`）。
    #[must_use]
    pub const fn verdict_to_status(verdict: ObserverVerdict) -> FindingStatus {
        match verdict {
            ObserverVerdict::Confirm => FindingStatus::Confirmed,
            ObserverVerdict::NeedsMoreEvidence => FindingStatus::NeedsReview,
            ObserverVerdict::LikelyFalsePositive => FindingStatus::FalsePositive,
            ObserverVerdict::Duplicate => FindingStatus::Duplicate,
        }
    }
}

/// Python `f"{value}"` 对 `None` 的渲染。
fn py_str(value: Option<&str>) -> &str {
    value.unwrap_or("None")
}

/// Python `repr(list[str])`：`['a', 'b']`（reasons 文本进审计链，需逐字节一致）。
fn py_repr(items: &[&str]) -> String {
    let joined = items
        .iter()
        .map(|item| format!("'{item}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{joined}]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::ids::ProjectId;
    use models::lifecycle::EvidenceKind;

    fn project_id() -> ProjectId {
        ProjectId::new("proj_test".to_string())
    }

    fn run_id() -> RunId {
        RunId::new("run_x".to_string())
    }

    fn finding(title: &str) -> Finding {
        let mut item = Finding::new(project_id(), title.to_string());
        item.id = models::ids::FindingId::new("find_test".to_string());
        item
    }

    fn evidence(summary: &str, supports: &[&str]) -> Evidence {
        let mut item = Evidence::new(project_id(), EvidenceKind::ToolOutput, summary.to_string());
        item.id = models::ids::EvidenceId::new("evd_1".to_string());
        item.supports_fact_ids = supports.iter().map(|id| (*id).to_string()).collect();
        item
    }

    fn fact() -> Fact {
        let mut item = Fact::new(project_id(), "k".to_string(), "s".to_string());
        item.id = models::ids::FactId::new("fact_1".to_string());
        item
    }

    #[test]
    fn observer_verdicts_match_python_wire_values() {
        assert_eq!(ObserverVerdict::Confirm.as_str(), "confirm");
        assert_eq!(
            ObserverVerdict::NeedsMoreEvidence.as_str(),
            "needs_more_evidence"
        );
        assert_eq!(
            ObserverVerdict::LikelyFalsePositive.as_str(),
            "likely_false_positive"
        );
        assert_eq!(ObserverVerdict::Duplicate.as_str(), "duplicate");
        assert_eq!(
            serde_json::to_string(&ObserverVerdict::NeedsMoreEvidence)
                .unwrap_or_else(|error| panic!("序列化不会失败: {error}")),
            "\"needs_more_evidence\""
        );
    }

    #[test]
    fn verdict_to_status_maps_like_python() {
        assert_eq!(
            Observer::verdict_to_status(ObserverVerdict::Confirm),
            FindingStatus::Confirmed
        );
        assert_eq!(
            Observer::verdict_to_status(ObserverVerdict::NeedsMoreEvidence),
            FindingStatus::NeedsReview
        );
        assert_eq!(
            Observer::verdict_to_status(ObserverVerdict::LikelyFalsePositive),
            FindingStatus::FalsePositive
        );
        assert_eq!(
            Observer::verdict_to_status(ObserverVerdict::Duplicate),
            FindingStatus::Duplicate
        );
    }

    #[test]
    fn review_confirms_fully_grounded_finding() {
        let mut grounded = finding("SQLi in login");
        grounded.id = models::ids::FindingId::new("find_ok".to_string());
        grounded.evidence_ids = vec!["evd_1".to_string()];
        grounded.source_label = Some("request.user".to_string());
        grounded.sink_label = Some("db.query".to_string());
        let evidence = evidence("s", &["fact_1"]);
        let report = Observer.review(&ReviewInput {
            run_id: &run_id(),
            findings: &[grounded],
            evidence: &[evidence],
            facts: &[fact()],
        });
        assert_eq!(report.reviews.len(), 1);
        assert_eq!(report.reviews[0].verdict, ObserverVerdict::Confirm);
        assert!(report.reviews[0].missing.is_empty());
        assert_eq!(report.reviews[0].suggested_intent, None);
        assert!(report.ready_to_report);
        assert_eq!(report.summary, "1/1 findings confirmed");
        assert_eq!(report.run_id, "run_x");
    }

    #[test]
    fn review_flags_missing_context_and_blocks_reporting() {
        let incomplete = finding("XSS in search");
        let report = Observer.review(&ReviewInput {
            run_id: &run_id(),
            findings: &[incomplete],
            evidence: &[],
            facts: &[],
        });
        assert_eq!(
            report.reviews[0].verdict,
            ObserverVerdict::NeedsMoreEvidence
        );
        assert_eq!(
            report.reviews[0].missing,
            ["evidence", "source", "sink"]
                .iter()
                .map(|item| (*item).to_string())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            report.reviews[0].suggested_intent.as_deref(),
            Some("Collect missing context (evidence, source, sink) for: XSS in search")
        );
        assert!(!report.ready_to_report);
        assert_eq!(report.summary, "0/1 findings confirmed");
    }

    #[test]
    fn review_reports_dangling_evidence_in_python_repr() {
        let mut dangling = finding("RCE in upload");
        dangling.evidence_ids = vec!["evd_missing".to_string()];
        dangling.source_label = Some("file".to_string());
        dangling.sink_label = Some("exec".to_string());
        let report = Observer.review(&ReviewInput {
            run_id: &run_id(),
            findings: &[dangling],
            evidence: &[],
            facts: &[],
        });
        assert!(
            report.reviews[0]
                .reasons
                .contains(&"evidence not found: ['evd_missing']".to_string())
        );
        assert_eq!(
            report.reviews[0].verdict,
            ObserverVerdict::NeedsMoreEvidence
        );
    }

    #[test]
    fn review_flags_evidence_that_chains_to_no_known_fact() {
        let mut unchained = finding("SSRF in webhook");
        unchained.evidence_ids = vec!["evd_1".to_string()];
        unchained.source_label = Some("url".to_string());
        unchained.sink_label = Some("fetch".to_string());
        let evidence = evidence("s", &["fact_unknown"]);
        let report = Observer.review(&ReviewInput {
            run_id: &run_id(),
            findings: &[unchained],
            evidence: &[evidence],
            facts: &[fact()],
        });
        assert_eq!(report.reviews[0].verdict, ObserverVerdict::Confirm);
        assert!(
            report.reviews[0]
                .reasons
                .contains(&"evidence does not reference any known fact".to_string()),
            "未链到已知 fact 只记 reason，不计入 missing（Python 语义）"
        );
    }

    #[test]
    fn review_marks_second_finding_with_same_fingerprint_duplicate() {
        let mut first = finding("SQLi in login");
        first.id = models::ids::FindingId::new("find_first".to_string());
        first.evidence_ids = vec!["evd_1".to_string()];
        first.source_label = Some("a".to_string());
        first.sink_label = Some("b".to_string());
        let mut second = finding("SQLi again");
        second.id = models::ids::FindingId::new("find_second".to_string());
        second.evidence_ids = vec!["evd_1".to_string()];
        second.source_label = Some("a".to_string());
        second.sink_label = Some("b".to_string());
        let evidence = evidence("s", &["fact_1"]);
        let report = Observer.review(&ReviewInput {
            run_id: &run_id(),
            findings: &[first, second],
            evidence: &[evidence],
            facts: &[fact()],
        });
        assert_eq!(report.reviews[0].verdict, ObserverVerdict::Confirm);
        assert_eq!(report.reviews[1].verdict, ObserverVerdict::Duplicate);
        assert_eq!(
            report.reviews[1].reasons,
            ["duplicate of find_first".to_string()]
        );
        // 重复项不算"需要补证"，因此不阻塞出报告。
        assert!(report.ready_to_report);
        assert_eq!(report.summary, "1/2 findings confirmed");
    }
}
