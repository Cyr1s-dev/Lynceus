//! CRITIC：独立批判 —— `server/core/agents/critique.py` 的移植。
//!
//! Critic 卡在假设生成与执行之间，与 Co-RedTeam 的对抗评审同构：
//! 拦下一个幻觉漏洞最便宜的位置，是在任何预算花在追它之前。
//!
//! Critic 是确定性、无模型的——这是刻意为之。LLM critic 自身就是
//! 幻觉源，而这里真正要紧的检查是结构性的而非语义的：
//!
//! 1. **这是猜想还是结论？** 已经断言漏洞存在的分支跳过了取证步骤，
//!    即 README 直拒的"臆测"。
//! 2. **可证伪吗？** 没有办法出错的假设不是假设——实践中就是可执行
//!    契约的 hit signal。
//! 3. **可执行吗？** 契约四要素（verb / object / artifact path / hit
//!    signal）缺一不可，否则 worker 无法据之行动。
//! 4. **依据真实吗？** 引用图中不存在的事实，等于凭空编造前提。
//!
//! 裁决永不丢弃：一次拒绝是探索如何被引导的审计证据，与被接受的
//! 分支一同落盘。

use std::collections::HashSet;
use std::sync::OnceLock;

use models::critique::ContractElement;
use models::critique::CritiqueReport;
use models::critique::CritiqueVerdict;
use models::critique::ExecutableContract;
use models::mission::Branch;

/// 把假设写成既成事实的措辞（Python `ASSERTIVE_PATTERNS`，(regex, label) 对）。
///
/// 用这种口吻说话的假设，已经预设了自己的结论。
const ASSERTIVE_PATTERN_TEXTS: [(&str, &str); 8] = [
    (
        r"\bis (?:definitely |certainly |clearly )?vulnerable\b",
        "asserts vulnerability as fact",
    ),
    (r"\bis exploitable\b", "asserts exploitability as fact"),
    (
        r"\bwill (?:definitely |certainly )?(?:succeed|work|bypass)\b",
        "asserts outcome as fact",
    ),
    (
        r"\b(?:definitely|certainly|obviously|undoubtedly|guaranteed)\b",
        "unqualified certainty",
    ),
    (r"\bproves?\b", "claims proof without evidence"),
    (r"\bconfirmed\b", "claims confirmation before execution"),
    (
        r"\bwe know (?:that )?\b",
        "claims knowledge without a source",
    ),
    (
        r"\bthere is a (?:critical |high )?vulnerability\b",
        "asserts a finding as fact",
    ),
];

/// 让陈述成为猜想、因此可检验的标记词（Python `CONJECTURE_PATTERNS`）。
const CONJECTURE_PATTERN_TEXTS: [&str; 11] = [
    r"\bmay\b",
    r"\bmight\b",
    r"\bcould\b",
    r"\bcan\b",
    r"\bpossibly\b",
    r"\bpotentially\b",
    r"\bsuspected\b",
    r"\bwhether\b",
    r"\bif\b",
    r"\blikely\b",
    r"\bappears? to\b",
];

/// 假设的最小可行动长度（Python `MIN_HYPOTHESIS_LENGTH`，按字符计）。
pub const MIN_HYPOTHESIS_LENGTH: usize = 20;

/// 各裁决的置信度（Python `_VERDICT_CONFIDENCE`）。
///
/// 表达的是 critic 对**自身判断**的把握，不是假设为真的概率。
#[must_use]
pub const fn verdict_confidence(verdict: CritiqueVerdict) -> f64 {
    match verdict {
        CritiqueVerdict::Accepted => 0.8,
        CritiqueVerdict::NeedsRevision => 0.6,
        CritiqueVerdict::Rejected => 0.9,
    }
}

fn assertive_patterns() -> &'static [(regex::Regex, &'static str)] {
    static PATTERNS: OnceLock<Vec<(regex::Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        ASSERTIVE_PATTERN_TEXTS
            .iter()
            .map(|(text, label)| {
                let compiled = regex::Regex::new(text).unwrap_or_else(|error| {
                    panic!("内置正则 {text} 必须可编译（静态字面量）: {error}")
                });
                (compiled, *label)
            })
            .collect()
    })
}

fn conjecture_patterns() -> &'static [regex::Regex] {
    static PATTERNS: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        CONJECTURE_PATTERN_TEXTS
            .iter()
            .map(|text| {
                regex::Regex::new(text).unwrap_or_else(|error| {
                    panic!("内置正则 {text} 必须可编译（静态字面量）: {error}")
                })
            })
            .collect()
    })
}

/// 一次批判评审的输入（Python `review` 的 keyword-only 参数镜像）。
#[derive(Debug)]
pub struct CritiqueInput<'a> {
    /// 被评审的 Branch。
    pub branch: &'a Branch,
    /// 审计图中已知的 Fact ID（grounding 检查）。
    pub known_fact_ids: &'a [String],
    /// 显式契约；`None` 时从 `branch.metadata["contract"]` 读取。
    pub contract: Option<ExecutableContract>,
}

impl<'a> CritiqueInput<'a> {
    /// 以最小输入构造（无已知事实，契约取自分支元数据）。
    #[must_use]
    pub fn new(branch: &'a Branch) -> Self {
        Self {
            branch,
            known_fact_ids: &[],
            contract: None,
        }
    }

    /// 指定已知 Fact ID（Python `known_fact_ids=`）。
    #[must_use]
    pub fn with_known_fact_ids(mut self, known_fact_ids: &'a [String]) -> Self {
        self.known_fact_ids = known_fact_ids;
        self
    }

    /// 显式指定契约（Python `contract=`）。
    #[must_use]
    pub fn with_contract(mut self, contract: ExecutableContract) -> Self {
        self.contract = Some(contract);
        self
    }
}

/// 确定性对抗评审器，裁决生成的 Branch 假设。
#[derive(Debug, Default)]
pub struct CritiqueAgent;

impl CritiqueAgent {
    /// 评审一个 Branch 假设，产出 append-only 裁决（纯计算，无 I/O）。
    ///
    /// 裁决优先级：
    /// 1. `REJECTED` —— 断言式结论，或引用图中不存在的事实（臆测）；
    /// 2. `NEEDS_REVISION` —— 可挽救，但契约不完整 / 不可证伪 / 过短；
    /// 3. `ACCEPTED` —— 可证伪猜想 + 完整契约 + 真实事实依据。
    ///
    /// # Panics
    /// 契约为纯字符串结构，`serde_json::to_value` 的 Err 分支在类型上
    /// 不可达；若到达属编程错误，立即失败优于静默落盘残缺契约。
    #[must_use]
    pub fn review(&self, input: &CritiqueInput<'_>) -> CritiqueReport {
        let hypothesis = input.branch.hypothesis.trim().to_string();
        let resolved_contract = input
            .contract
            .clone()
            .unwrap_or_else(|| contract_from_branch(input.branch));

        let speculative = assertive_phrases(&hypothesis);
        let missing = resolved_contract.missing_elements();
        let unknown_facts = unknown_facts(input.branch, input.known_fact_ids);
        let falsifiable = is_falsifiable(&hypothesis, &resolved_contract);

        let (verdict, reasons) = determine_verdict_and_reasons(
            &hypothesis,
            &speculative,
            &unknown_facts,
            &missing,
            falsifiable,
        );

        tracing::info!(
            branch = %input.branch.id,
            verdict = verdict.as_str(),
            reason = reasons.first().map(String::as_str).unwrap_or_default(),
            "critique verdict"
        );

        let mut report = CritiqueReport::new(
            input.branch.project_id.clone(),
            input.branch.id.clone(),
            verdict,
            // 报告保存假设原文，绝不原地改写。
            input.branch.hypothesis.clone(),
        );
        report.mission_id = Some(input.branch.mission_id.clone());
        report.run_id.clone_from(&input.branch.run_id);
        report.reasons = reasons;
        report.speculative_phrases = speculative
            .iter()
            .map(|(phrase, _)| phrase.clone())
            .collect();
        report.missing_contract_elements.clone_from(&missing);
        report.unknown_fact_ids = unknown_facts;
        report.falsifiable = falsifiable;
        report.restated_hypothesis = if verdict == CritiqueVerdict::NeedsRevision {
            Some(restate(&hypothesis, &resolved_contract, &missing))
        } else {
            None
        };
        report.confidence = verdict_confidence(verdict);
        report.metadata.insert(
            "branch_kind".to_string(),
            input
                .branch
                .metadata
                .get("branch_kind")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        );
        report.metadata.insert(
            "contract".to_string(),
            serde_json::to_value(&resolved_contract)
                .unwrap_or_else(|error| panic!("契约序列化不会失败: {error}")),
        );
        report
    }

    /// 批量评审，保持输入顺序（Python `review_all`）。
    ///
    /// `known_fact_ids` 对整批共享——同一 grounding 上下文一次给出，
    /// 而不是每个分支重查一遍图。
    #[must_use]
    pub fn review_all(
        &self,
        branches: &[Branch],
        known_fact_ids: &[String],
    ) -> Vec<CritiqueReport> {
        branches
            .iter()
            .map(|branch| {
                self.review(&CritiqueInput::new(branch).with_known_fact_ids(known_fact_ids))
            })
            .collect()
    }
}

/// 裁决判定与理由收集（Python `review` 的判定段镜像）。
///
/// 拒绝优先于修订：断言式结论或悬空事实引用直接 `REJECTED`；已拒绝的
/// 分支不再叠记修订理由（Python `if verdict != REJECTED` 卫语句）。
/// 全部检查通过时补默认接受理由——报告的 reasons 永不为空。
fn determine_verdict_and_reasons(
    hypothesis: &str,
    speculative: &[(String, &'static str)],
    unknown_facts: &[String],
    missing: &[ContractElement],
    falsifiable: bool,
) -> (CritiqueVerdict, Vec<String>) {
    let mut reasons: Vec<String> = Vec::new();
    let mut verdict = CritiqueVerdict::Accepted;

    // 拒绝：断言自己的结论，或编造前提。
    if !speculative.is_empty() {
        verdict = CritiqueVerdict::Rejected;
        let mut labels: Vec<&str> = speculative.iter().map(|(_, label)| *label).collect();
        labels.sort_unstable();
        labels.dedup();
        reasons.push(format!(
            "hypothesis asserts a conclusion instead of proposing a testable \
             conjecture ({})",
            labels.join(", ")
        ));
    }
    if !unknown_facts.is_empty() {
        verdict = CritiqueVerdict::Rejected;
        reasons.push(format!(
            "hypothesis cites facts that do not exist in the audit graph: {}",
            unknown_facts.join(", ")
        ));
    }

    // 修订：可挽救，但尚不可执行或不可证伪。
    if verdict != CritiqueVerdict::Rejected {
        if !missing.is_empty() {
            verdict = CritiqueVerdict::NeedsRevision;
            reasons.push(format!(
                "executable contract is incomplete; missing {}",
                missing
                    .iter()
                    .map(|element| element.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !falsifiable {
            verdict = CritiqueVerdict::NeedsRevision;
            reasons.push(
                "hypothesis is not falsifiable: it states no condition that could come \
                 out false"
                    .to_string(),
            );
        }
        if hypothesis.chars().count() < MIN_HYPOTHESIS_LENGTH {
            verdict = CritiqueVerdict::NeedsRevision;
            reasons.push(format!(
                "hypothesis is too short to be actionable ({} < {} characters)",
                hypothesis.chars().count(),
                MIN_HYPOTHESIS_LENGTH
            ));
        }
    }

    if reasons.is_empty() {
        reasons.push(
            "hypothesis is a falsifiable conjecture with a complete executable contract \
             and grounded fact references"
                .to_string(),
        );
    }

    (verdict, reasons)
}

/// 逐字短语 + 分类标签：把假设写成既成事实的每一处措辞。
fn assertive_phrases(hypothesis: &str) -> Vec<(String, &'static str)> {
    let lowered = hypothesis.to_lowercase();
    let mut found: Vec<(String, &'static str)> = Vec::new();
    for (pattern, label) in assertive_patterns() {
        if let Some(matched) = pattern.find(&lowered) {
            found.push((matched.as_str().to_string(), label));
        }
    }
    found
}

/// 假设可证伪 = 猜想式措辞 + 非空 hit signal（Python `_is_falsifiable`）。
fn is_falsifiable(hypothesis: &str, contract: &ExecutableContract) -> bool {
    let lowered = hypothesis.to_lowercase();
    let conjectural = conjecture_patterns()
        .iter()
        .any(|pattern| pattern.is_match(&lowered));
    conjectural && !contract.hit_signal.trim().is_empty()
}

/// 分支引用了图中不存在的哪些事实（Python `_unknown_facts`）。
///
/// 已知集与引用集**同时为空**时直接放行——没有引用就没有悬空引用；
/// 这与"引用了事实但图中一个都没有"（全部悬空）不同。
fn unknown_facts(branch: &Branch, known_fact_ids: &[String]) -> Vec<String> {
    if known_fact_ids.is_empty() && branch.related_fact_ids.is_empty() {
        return Vec::new();
    }
    let known: HashSet<&str> = known_fact_ids.iter().map(String::as_str).collect();
    let mut unknown: Vec<String> = branch
        .related_fact_ids
        .iter()
        .filter(|fact_id| !known.contains(fact_id.as_str()))
        .cloned()
        .collect();
    unknown.sort();
    unknown
}

/// 把可挽救的假设改写为显式猜想形式（Python `_restate`）。
///
/// 原文逐字保留在改写中；critic 补上缺失的结构，而不是转述分析师。
fn restate(hypothesis: &str, contract: &ExecutableContract, missing: &[ContractElement]) -> String {
    let mut parts = vec![format!(
        "It is unverified whether {}.",
        hypothesis.trim_end_matches('.')
    )];
    if !contract.verb.is_empty() && !contract.object.is_empty() {
        parts.push(format!(
            "Test by {} against {}.",
            contract.verb, contract.object
        ));
    }
    if contract.hit_signal.is_empty() {
        parts.push(
            "A hit signal must be defined before this branch can fail, and a branch that \
             cannot fail is not a hypothesis."
                .to_string(),
        );
    } else {
        parts.push(format!("Treat as a hit only if {}.", contract.hit_signal));
    }
    if !missing.is_empty() {
        parts.push(format!(
            "Supply the missing contract element(s): {}.",
            missing
                .iter()
                .map(|element| element.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    parts.join(" ")
}

/// 从 `branch.metadata["contract"]` 读取可执行契约（Python 模块函数）。
///
/// 分支不携带契约时返回空契约——critic 把缺失报告为 missing elements，
/// 而不是抛异常。
#[must_use]
pub fn contract_from_branch(branch: &Branch) -> ExecutableContract {
    let Some(raw) = branch.metadata.get("contract") else {
        return ExecutableContract::default();
    };
    let Some(fields) = raw.as_object() else {
        return ExecutableContract::default();
    };
    ExecutableContract {
        verb: python_str_field(fields, "verb"),
        object: python_str_field(fields, "object"),
        artifact_path: python_str_field(fields, "artifact_path"),
        hit_signal: python_str_field(fields, "hit_signal"),
    }
}

/// Python `str(raw.get(key) or "")` 的镜像。
///
/// 缺失 / `null` / 假值（`""`、`0`、`false`）折算为空串；真值标量取其
/// `str()` 形式。非标量（数组/对象）按空串处理：契约字段按构造只能是
/// 字符串，非标量值两边都产生不了有效契约。
fn python_str_field(fields: &serde_json::Map<String, serde_json::Value>, key: &str) -> String {
    match fields.get(key) {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Number(number)) => {
            if number.as_f64().is_some_and(|value| value == 0.0) {
                String::new()
            } else {
                number.to_string()
            }
        }
        Some(serde_json::Value::Bool(true)) => "True".to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::ids::MissionId;
    use models::ids::ProjectId;

    fn complete_contract() -> ExecutableContract {
        ExecutableContract {
            verb: "trace input".to_string(),
            object: "request handlers".to_string(),
            artifact_path: "artifacts/trace.json".to_string(),
            hit_signal: "an unvalidated path reaches a dangerous sink".to_string(),
        }
    }

    fn critique_branch() -> Branch {
        let mut branch = Branch::new(
            ProjectId::new("project_1".to_string()),
            MissionId::new("mission_1".to_string()),
            "Trace candidate".to_string(),
            "User input may reach a dangerous sink without validation.".to_string(),
        );
        branch.related_fact_ids = vec!["fact_1".to_string()];
        branch.metadata.insert(
            "contract".to_string(),
            serde_json::to_value(complete_contract())
                .unwrap_or_else(|error| panic!("测试契约序列化不会失败: {error}")),
        );
        branch
    }

    fn known_facts() -> Vec<String> {
        vec!["fact_1".to_string()]
    }

    /// Python `test_critique_agent_enforces_conjecture_grounding_and_contract`。
    #[test]
    fn enforces_conjecture_grounding_and_contract() {
        let agent = CritiqueAgent;
        let mut branch = critique_branch();

        let accepted =
            agent.review(&CritiqueInput::new(&branch).with_known_fact_ids(&known_facts()));
        assert_eq!(accepted.verdict, CritiqueVerdict::Accepted);
        assert!(accepted.admitted());

        branch.hypothesis = "There is a critical vulnerability in this handler.".to_string();
        let rejected =
            agent.review(&CritiqueInput::new(&branch).with_known_fact_ids(&known_facts()));
        assert_eq!(rejected.verdict, CritiqueVerdict::Rejected);
        assert!(!rejected.speculative_phrases.is_empty());

        branch.hypothesis = "Input may reach a dangerous sink.".to_string();
        branch.related_fact_ids = vec!["fact_missing".to_string()];
        let unknown =
            agent.review(&CritiqueInput::new(&branch).with_known_fact_ids(&known_facts()));
        assert_eq!(unknown.verdict, CritiqueVerdict::Rejected);
        assert_eq!(unknown.unknown_fact_ids, vec!["fact_missing".to_string()]);

        branch.related_fact_ids = Vec::new();
        let incomplete = agent.review(&CritiqueInput::new(&branch).with_contract(
            ExecutableContract {
                verb: "trace input".to_string(),
                ..ExecutableContract::default()
            },
        ));
        assert_eq!(incomplete.verdict, CritiqueVerdict::NeedsRevision);
        assert!(
            incomplete
                .missing_contract_elements
                .contains(&ContractElement::HitSignal)
        );
        assert!(incomplete.restated_hypothesis.is_some());
    }

    /// ACCEPTED 报告的完整形态：默认理由 / 置信度 / 元数据 / 原文假设。
    #[test]
    fn accepted_report_shape() {
        let branch = critique_branch();
        let report =
            CritiqueAgent.review(&CritiqueInput::new(&branch).with_known_fact_ids(&known_facts()));

        assert_eq!(
            report.mission_id.as_ref().map(MissionId::as_str),
            Some("mission_1")
        );
        assert!(report.falsifiable);
        assert!(report.speculative_phrases.is_empty());
        assert!(report.missing_contract_elements.is_empty());
        assert!(report.unknown_fact_ids.is_empty());
        assert!((report.confidence - 0.8).abs() < f64::EPSILON);
        assert_eq!(
            report.reasons,
            vec![
                "hypothesis is a falsifiable conjecture with a complete executable \
                 contract and grounded fact references"
                    .to_string()
            ]
        );
        // 假设保存原文，绝不原地改写。
        assert_eq!(
            report.hypothesis,
            "User input may reach a dangerous sink without validation."
        );
        // 元数据：branch_kind（缺失即 null）+ 契约 dump。
        assert_eq!(
            report.metadata.get("branch_kind"),
            Some(&serde_json::Value::Null)
        );
        assert_eq!(
            report
                .metadata
                .get("contract")
                .and_then(|c| c.get("hit_signal")),
            Some(&serde_json::json!(
                "an unvalidated path reaches a dangerous sink"
            ))
        );
    }

    /// 断言式假设：命中的逐字短语 + 排序去重后的标签理由 + 0.9 置信度。
    #[test]
    fn assertive_hypothesis_reports_verbatim_phrase_and_labels() {
        let mut branch = critique_branch();
        branch.hypothesis = "The endpoint is definitely vulnerable; this proves it and it \
                             is confirmed."
            .to_string();
        let report =
            CritiqueAgent.review(&CritiqueInput::new(&branch).with_known_fact_ids(&known_facts()));

        assert_eq!(report.verdict, CritiqueVerdict::Rejected);
        assert!((report.confidence - 0.9).abs() < f64::EPSILON);
        assert_eq!(
            report.speculative_phrases,
            vec![
                // 顺序 = 断言模式的定义序（Python 按序迭代收集）。
                "is definitely vulnerable".to_string(),
                "definitely".to_string(),
                "proves".to_string(),
                "confirmed".to_string(),
            ]
        );
        assert!(report.reasons.first().is_some_and(|reason| reason.contains(
            "asserts vulnerability as fact, claims confirmation before execution, \
                 claims proof without evidence, unqualified certainty"
        )));
        // REJECTED 不产出改写。
        assert_eq!(report.restated_hypothesis, None);
    }

    /// `NEEDS_REVISION` 的改写格式：显式猜想 + 缺失结构，原文逐字保留。
    #[test]
    fn restatement_adds_missing_structure_around_original_text() {
        let mut branch = critique_branch();
        branch.hypothesis = "Input may reach a dangerous sink.".to_string();
        // 与 Python 测试一致：聚焦契约完整性，清空事实引用。
        branch.related_fact_ids = Vec::new();
        let report = CritiqueAgent.review(&CritiqueInput::new(&branch).with_contract(
            ExecutableContract {
                verb: "trace input".to_string(),
                object: "request handlers".to_string(),
                artifact_path: String::new(),
                hit_signal: String::new(),
            },
        ));

        assert_eq!(report.verdict, CritiqueVerdict::NeedsRevision);
        assert!((report.confidence - 0.6).abs() < f64::EPSILON);
        assert_eq!(
            report.restated_hypothesis,
            Some(
                "It is unverified whether Input may reach a dangerous sink. Test by \
                 trace input against request handlers. A hit signal must be defined \
                 before this branch can fail, and a branch that cannot fail is not a \
                 hypothesis. Supply the missing contract element(s): artifact_path, \
                 hit_signal."
                    .to_string()
            )
        );
    }

    /// 过短的假设：可行动性检查（按字符计，含缺失理由）。
    #[test]
    fn too_short_hypothesis_needs_revision() {
        let mut branch = critique_branch();
        branch.hypothesis = "may be bad".to_string(); // 10 字符 < 20。
        branch.related_fact_ids = Vec::new();
        let report =
            CritiqueAgent.review(&CritiqueInput::new(&branch).with_contract(complete_contract()));
        assert_eq!(report.verdict, CritiqueVerdict::NeedsRevision);
        assert!(report.reasons.iter().any(|reason| {
            reason.contains("hypothesis is too short to be actionable (10 < 20 characters)")
        }));
    }

    /// 无猜想标记的假设不可证伪——哪怕契约齐备。
    #[test]
    fn non_conjectural_hypothesis_is_not_falsifiable() {
        let mut branch = critique_branch();
        branch.hypothesis = "This branch traces request handler dataflow end to end.".to_string();
        branch.related_fact_ids = Vec::new();
        let report =
            CritiqueAgent.review(&CritiqueInput::new(&branch).with_contract(complete_contract()));
        assert_eq!(report.verdict, CritiqueVerdict::NeedsRevision);
        assert!(!report.falsifiable);
        assert!(
            report
                .reasons
                .iter()
                .any(|reason| reason.contains("hypothesis is not falsifiable"))
        );
    }

    /// `contract_from_branch`：缺失 / 非对象 / 空值字段全部折算为空契约要素。
    #[test]
    fn contract_from_branch_falls_back_to_empty() {
        let mut branch = critique_branch();
        branch.metadata.remove("contract");
        assert_eq!(contract_from_branch(&branch), ExecutableContract::default());

        branch.metadata.insert(
            "contract".to_string(),
            serde_json::json!({"verb": "scan", "object": null, "hit_signal": ""}),
        );
        let contract = contract_from_branch(&branch);
        assert_eq!(contract.verb, "scan");
        assert_eq!(contract.object, "");
        assert_eq!(contract.hit_signal, "");
        assert_eq!(
            contract.missing_elements(),
            vec![
                ContractElement::Object,
                ContractElement::ArtifactPath,
                ContractElement::HitSignal
            ]
        );

        // 非对象契约（如误存字符串）→ 整体折算为空契约。
        branch
            .metadata
            .insert("contract".to_string(), serde_json::json!("not-a-contract"));
        assert_eq!(contract_from_branch(&branch), ExecutableContract::default());
    }

    /// `review_all`：保持输入顺序，grounding 上下文整批共享。
    #[test]
    fn review_all_preserves_order_and_shares_fact_ids() {
        let first = critique_branch();
        let mut second = critique_branch();
        second.hypothesis = "The login flow is exploitable.".to_string();

        let reports = CritiqueAgent.review_all(&[first, second], &["fact_1".to_string()]);
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].verdict, CritiqueVerdict::Accepted);
        assert_eq!(reports[1].verdict, CritiqueVerdict::Rejected);
        assert_eq!(
            reports[1].speculative_phrases,
            vec!["is exploitable".to_string()]
        );
    }

    /// grounding 边界：无已知事实且无引用 → 放行（没有引用就没有悬空引用）。
    #[test]
    fn empty_known_and_empty_related_passes_grounding() {
        let mut branch = critique_branch();
        branch.related_fact_ids = Vec::new();
        let report =
            CritiqueAgent.review(&CritiqueInput::new(&branch).with_contract(complete_contract()));
        assert_eq!(report.verdict, CritiqueVerdict::Accepted);
        assert!(report.unknown_fact_ids.is_empty());
    }
}
