//! 复测只读上下文：把漏洞本体、关联证据与测试约束内联进 prompt，
//! 不给 worker 任何工具。
//!
//! 复测 Agent 只被授予「读取既有证据」的能力：它读漏洞本体、关联
//! 证据、测试约束，然后给结论。Lynceus 之前没有这条只读通道——复测端点
//! 直接复用了 `IntakeService::advise`，那条路会发放
//! `blackboard_read/blackboard_append/knowledge_search` 三件套。这对复测是
//! **错的**，有两个具体后果：
//!
//! 1. **复测会污染白板**：`blackboard_append` 让复测把自己的结论写回任务
//!    白板，之后所有读白板的 worker 都把「复测认为已修复」当成事实。评估
//!    动作不该改写被评估的对象。
//! 2. **grant 发不下来时会静默变成零上下文**：`advise` 在 grant 失败时打
//!    一行 eprintln 就退化到「无 MCP」状态继续跑。对顾问问答这是可接受的
//!    降级；对复测则是要求一个**从未见过证据**的 worker 给 verdict——
//!    这正是编造结论的标准入口。
//!
//! 因此这里不复用 MCP grant，而是**直接从仓储把上下文读出来、内联进
//! prompt**：worker 拿不到任何工具，物理上无法读也无法写，唯一的信息源
//! 就是这份快照。只读由构造保证，不靠约定。
//!
//! 设计取舍：
//! - 常见做法用组织维度给复测加上下文；Lynceus 没有公司实体，
//!   对应字段不设。
//! - 复测可以异步轮询；Lynceus 的复测是一次性评估，同步收口。
//! - 上下文的**大小上限**是本模块自己定的（交由 LLM 侧截断），因为
//!   Lynceus 的 worker 指令长度直接影响 CLI 参数与费用。
//!
//! 本模块**只读**：没有一个方法会写仓储、发 grant 或起 worker。

use models::{Branch, Evidence, Fact, Finding, ToolInvocation};

/// 上下文体积上限。超限时按优先级丢弃尾部并置 `truncated`。
pub(crate) const MAX_EVIDENCE: usize = 24;
pub(crate) const MAX_FACTS: usize = 16;
pub(crate) const MAX_BRANCHES: usize = 8;
pub(crate) const MAX_TOOL_CALLS: usize = 12;
/// 单个字段进 prompt 前截断到的字符数。
pub(crate) const MAX_FIELD_CHARS: usize = 600;
/// 整份上下文的字符预算。
pub(crate) const MAX_TOTAL_CHARS: usize = 24_000;
/// 一条已登记证据的只读投影。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RetestEvidence {
    pub id: String,
    pub kind: String,
    pub summary: String,
    /// `Evidence.content` 是自由 JSON；压成单行摘要，不整体塞进 prompt。
    pub content: String,
    pub locations: Vec<String>,
}

/// 复测只读上下文快照。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RetestContext {
    /// 漏洞 id（回显，防止 prompt 与记录错配）。
    pub finding_id: String,
    /// 漏洞标题。
    pub finding_title: String,
    /// 严重度 wire 值。
    pub severity: String,
    /// 当前状态 wire 值。
    pub status: String,
    /// 漏洞描述（可能为空）。
    pub description: String,
    /// 数据流 source → sink（缺失任一侧时不显示）。
    pub dataflow: Option<String>,
    /// CWE 编号。
    pub cwe: Vec<String>,
    /// 已登记证据，按 id 稳定排序。
    pub evidence: Vec<RetestEvidence>,
    /// 关联事实标题。
    pub facts: Vec<String>,
    /// 关联分支（假设链）。
    pub branches: Vec<String>,
    /// 关联工具调用摘要。
    pub tool_calls: Vec<String>,
    /// 是否因体积上限丢弃了内容——**必须**让 worker 知道，否则它会以为
    /// 「没列出的就是不存在的」，从而给出过于乐观的结论。
    pub truncated: bool,
}

/// 截断到 `MAX_FIELD_CHARS`，保留头尾并在中间标注省略量。
fn clip(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= MAX_FIELD_CHARS {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(MAX_FIELD_CHARS * 3 / 4).collect();
    let tail: String = trimmed
        .chars()
        .rev()
        .take(MAX_FIELD_CHARS / 4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!(
        "{head}\n…[省略 {} 字符]…\n{tail}",
        trimmed.chars().count() - MAX_FIELD_CHARS
    )
}

/// `Finding.cwe` 是 `Option<String>`（兼容 string 与 array 两种历史存储形态）。
fn cwe_of(finding: &Finding) -> Vec<String> {
    let Some(raw) = finding.cwe.as_deref() else {
        return Vec::new();
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    // 老 payload 可能把数组序列化成了 JSON 字符串，试着解析一下。
    if trimmed.starts_with('[')
        && let Ok(serde_json::Value::Array(items)) =
            serde_json::from_str::<serde_json::Value>(trimmed)
    {
        let parsed: Vec<String> = items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect();
        if !parsed.is_empty() {
            return parsed;
        }
    }
    vec![trimmed.to_string()]
}

/// 取 `finding.evidence_ids` 对应的证据实体。
fn evidence_of(finding: &Finding, all: &[Evidence]) -> Vec<RetestEvidence> {
    let mut wanted: Vec<&str> = finding
        .evidence_ids
        .iter()
        .map(String::as_str)
        .collect();
    wanted.sort_unstable();
    let mut seen: Vec<&str> = Vec::with_capacity(wanted.len());
    let mut out: Vec<RetestEvidence> = Vec::new();
    for id in wanted {
        if seen.contains(&id) {
            continue;
        }
        seen.push(id);
        if let Some(evidence) = all.iter().find(|item| item.id.as_str() == id) {
            let locations = evidence
                .locations
                .iter()
                .map(|location| {
                    let range = match (location.start_line, location.end_line) {
                        (Some(start), Some(end)) if start != end => format!("{start}-{end}"),
                        (Some(start), _) => start.to_string(),
                        _ => String::new(),
                    };
                    let where_ = if location.artifact.trim().is_empty() {
                        location
                            .symbol
                            .clone()
                            .unwrap_or_else(|| "(unknown location)".to_string())
                    } else {
                        location.artifact.clone()
                    };
                    if range.is_empty() {
                        where_
                    } else {
                        format!("{where_}:{range}")
                    }
                })
                .collect();
            // content 是自由 JSON：压成单行，只取前若干字符。
            let content = clip(&serde_json::to_string(&evidence.content).unwrap_or_default());
            out.push(RetestEvidence {
                id: evidence.id.as_str().to_string(),
                kind: evidence.kind.as_str().to_string(),
                summary: clip(&evidence.summary),
                content,
                locations,
            });
        }
    }
    out
}

/// 复测上下文里出现的事实：`Finding.related_fact_ids` 命中的排前面。
fn facts_of(all: &[Fact], finding: &Finding) -> Vec<String> {
    let mut linked: Vec<&Fact> = all
        .iter()
        .filter(|fact| {
            finding
                .related_fact_ids
                .iter()
                .any(|id| id == fact.id.as_str())
        })
        .collect();
    linked.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    let mut rest: Vec<&Fact> = all
        .iter()
        .filter(|fact| {
            !finding
                .related_fact_ids
                .iter()
                .any(|id| id == fact.id.as_str())
        })
        .collect();
    rest.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    linked
        .into_iter()
        .chain(rest)
        .map(|fact| clip(&fact.statement))
        .collect()
}

/// 复测上下文里出现的分支：假设链说明「这条漏洞是怎么被推出来的」。
fn branches_of(all: &[Branch], finding: &Finding) -> Vec<String> {
    let finding_id = finding.id.as_str();
    let mut linked: Vec<&Branch> = all
        .iter()
        .filter(|branch| {
            branch
                .related_finding_ids
                .iter()
                .any(|id| id.as_str() == finding_id)
        })
        .collect();
    linked.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    let mut rest: Vec<&Branch> = all
        .iter()
        .filter(|branch| {
            !branch
                .related_finding_ids
                .iter()
                .any(|id| id.as_str() == finding_id)
        })
        .collect();
    rest.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    linked
        .into_iter()
        .chain(rest)
        .map(|branch| format!("[{}] {}", branch.status.as_str(), clip(&branch.title)))
        .collect()
}

/// 复测上下文里出现的工具调用：证明「证据是怎么采到的」。
///
/// `ToolInvocation` 没有 `finding_ids` 反向索引，只能按 mission/branch 关联；
/// 因此这里按「同 mission → 同 branch」排序，把最可能相关的前置。
fn tool_calls_of(all: &[ToolInvocation], finding: &Finding) -> Vec<String> {
    let mission_id = finding.mission_id.as_ref();
    let branch_id = finding.branch_id.as_ref();
    let rank = |invocation: &ToolInvocation| -> u8 {
        if branch_id.is_some()
            && invocation.branch_id.as_ref() == branch_id
        {
            0
        } else if mission_id.is_some() && invocation.mission_id.as_ref() == mission_id {
            1
        } else {
            2
        }
    };
    let mut sorted: Vec<&ToolInvocation> = all.iter().collect();
    sorted.sort_by(|left, right| {
        rank(left)
            .cmp(&rank(right))
            .then_with(|| left.id.as_str().cmp(right.id.as_str()))
    });
    sorted
        .into_iter()
        .map(|invocation| {
            let tool = invocation.tool_name.trim();
            let tool = if tool.is_empty() { "(unknown tool)" } else { tool };
            let status = invocation.status.as_str();
            let summary = clip(&invocation.output_summary);
            format!("{tool} → {status}: {summary}")
        })
        .collect()
}

/// 组装复测只读上下文。
///
/// 只读仓储，**不写任何东西**。超限时按上面四个上限截断并置 `truncated`。
pub(crate) fn build_retest_context(
    repository: &dyn storage::Repository,
    project_id: &str,
    finding: &Finding,
) -> RetestContext {
    let evidence_all = repository.list_evidence(project_id).unwrap_or_default();
    let facts_all = repository.list_facts(project_id).unwrap_or_default();
    let tool_all = repository
        .list_tool_invocations(Some(project_id))
        .unwrap_or_default();
    let branches_all = repository
        .list_branches(Some(project_id), None, None)
        .unwrap_or_default();

    let mut evidence = evidence_of(finding, &evidence_all);
    let mut facts = facts_of(&facts_all, finding);
    let mut branches = branches_of(&branches_all, finding);
    let mut tool_calls = tool_calls_of(&tool_all, finding);

    let truncated = trim_to_budget(&mut evidence, &mut facts, &mut branches, &mut tool_calls);

    let dataflow = match (
        finding.source_label.as_deref().filter(|value| !value.trim().is_empty()),
        finding.sink_label.as_deref().filter(|value| !value.trim().is_empty()),
    ) {
        (Some(source), Some(sink)) => Some(format!("{source} → {sink}")),
        _ => None,
    };

    RetestContext {
        finding_id: finding.id.as_str().to_string(),
        finding_title: clip(&finding.title),
        severity: finding.severity.as_str().to_string(),
        status: finding.status.as_str().to_string(),
        description: clip(finding.description.as_deref().unwrap_or("")),
        dataflow,
        cwe: cwe_of(finding),
        evidence,
        facts,
        branches,
        tool_calls,
        truncated,
    }
}

/// 按四条上限 + 整份字符预算裁剪四段上下文，返回是否有东西被丢掉。
///
/// 单独抽出来是因为：**渲染不是最后一道防线，裁剪才是**。把裁剪做成
/// `&mut` 纯函数，调用方（`build_retest_context`）和测试都能直接看到它
/// 真的生效，而不是只能看渲染出来的字符串猜。
///
/// 裁剪顺序：先按条数上限截，再按整份预算从最不重要的尾部开始丢
/// （工具调用 → 分支 → 事实 → 证据）。证据是结论的依据，最后才动。
/// 每一段都至少留一条：只剩一条证据时，宁可超预算也不能把依据全丢光——
/// 那会让 `truncated` 谎报"没东西可看"。
fn trim_to_budget(
    evidence: &mut Vec<RetestEvidence>,
    facts: &mut Vec<String>,
    branches: &mut Vec<String>,
    tool_calls: &mut Vec<String>,
) -> bool {
    let mut truncated = false;
    if evidence.len() > MAX_EVIDENCE {
        evidence.truncate(MAX_EVIDENCE);
        truncated = true;
    }
    if facts.len() > MAX_FACTS {
        facts.truncate(MAX_FACTS);
        truncated = true;
    }
    if branches.len() > MAX_BRANCHES {
        branches.truncate(MAX_BRANCHES);
        truncated = true;
    }
    if tool_calls.len() > MAX_TOOL_CALLS {
        tool_calls.truncate(MAX_TOOL_CALLS);
        truncated = true;
    }
    while render_budget(evidence, facts, branches, tool_calls) > MAX_TOTAL_CHARS
        && (facts.len() > 1
            || branches.len() > 1
            || tool_calls.len() > 1
            || evidence.len() > 1)
    {
        if tool_calls.len() > 1 {
            tool_calls.pop();
        } else if branches.len() > 1 {
            branches.pop();
        } else if facts.len() > 1 {
            facts.pop();
        } else if evidence.len() > 1 {
            evidence.pop();
        } else {
            break;
        }
        truncated = true;
    }
    truncated
}

/// 渲染成 prompt 片段之前的字符预算粗估（只算字段长度和，不真的渲染）。
fn render_budget(
    evidence: &[RetestEvidence],
    facts: &[String],
    branches: &[String],
    tool_calls: &[String],
) -> usize {
    let mut total = 220; // 头部固定段落
    total += evidence
        .iter()
        .map(|item| item.id.len() + item.kind.len() + item.summary.len() + item.content.len() + 24)
        .sum::<usize>();
    total += evidence
        .iter()
        .map(|item| item.locations.iter().map(String::len).sum::<usize>())
        .sum::<usize>();
    total += facts.iter().map(String::len).sum::<usize>() + facts.len() * 4;
    total += branches.iter().map(String::len).sum::<usize>() + branches.len() * 4;
    total += tool_calls.iter().map(String::len).sum::<usize>() + tool_calls.len() * 4;
    total
}

/// 渲染成 prompt 片段。
///
/// 三条硬规则写进正文，因为它们直接决定结论可信度：
/// 1. **没有列出的证据就是不存在的**——不允许假设还有别的证据；
/// 2. `truncated` 时必须说明上下文不完整， verdict 只能是 inconclusive；
/// 3. 证据不足就直说，不许为了让报告好看而挑一个结论。
pub(crate) fn render_retest_context(context: &RetestContext) -> String {
    let mut out = String::new();
    out.push_str(&format!("漏洞 id：{}\n", context.finding_id));
    out.push_str(&format!("标题：{}\n", context.finding_title));
    out.push_str(&format!("严重度：{}\n", context.severity));
    out.push_str(&format!("当前状态：{}\n", context.status));
    if !context.cwe.is_empty() {
        out.push_str(&format!("CWE：{}\n", context.cwe.join(", ")));
    }
    if let Some(dataflow) = &context.dataflow {
        out.push_str(&format!("数据流：{dataflow}\n"));
    }
    if !context.description.is_empty() {
        out.push_str(&format!("原始描述：{}\n", context.description));
    }

    out.push_str("\n已登记证据（唯一事实来源，没有列出的就是不存在的）：\n");
    if context.evidence.is_empty() {
        out.push_str("  （无）\n");
    } else {
        for evidence in &context.evidence {
            out.push_str(&format!(
                "  - [{}] {} — {}\n",
                evidence.kind, evidence.id, evidence.summary
            ));
            if !evidence.content.is_empty() {
                out.push_str(&format!("    内容：{}\n", evidence.content));
            }
            if !evidence.locations.is_empty() {
                out.push_str(&format!(
                    "    位置：{}\n",
                    evidence.locations.join("; ")
                ));
            }
        }
    }

    out.push_str("\n相关事实：\n");
    if context.facts.is_empty() {
        out.push_str("  （无）\n");
    } else {
        for fact in &context.facts {
            out.push_str(&format!("  - {fact}\n"));
        }
    }

    out.push_str("\n相关分支（假设链）：\n");
    if context.branches.is_empty() {
        out.push_str("  （无）\n");
    } else {
        for branch in &context.branches {
            out.push_str(&format!("  - {branch}\n"));
        }
    }

    out.push_str("\n相关工具调用：\n");
    if context.tool_calls.is_empty() {
        out.push_str("  （无）\n");
    } else {
        for call in &context.tool_calls {
            out.push_str(&format!("  - {call}\n"));
        }
    }

    if context.truncated {
        out.push_str(
            "\n⚠ 上下文因体积上限被截断：上面只是部分证据。此时不得给出 reproduced 或 fixed 结论，\
             只能给出 inconclusive 并要求补充上下文。\n",
        );
    }

    out
}

/// 上下文渲染结果的字符数（测试与预算断言用）。
#[cfg(test)]
pub(crate) fn context_budget(context: &RetestContext) -> usize {
    render_retest_context(context).chars().count()
}

/// 上下文是否空到无法评估。
///
/// 判据是**漏洞自己有没有已登记证据**，不是整个项目有没有事实/分支/工具调用。
/// 理由：复测评估的是「这个漏洞现在还成立吗」，依据只能是它自己的证据。
/// 项目级事实和别的分支的假设再丰富，也不能替代这个漏洞的证据——拿它们拼一个
/// verdict 就是编造。空证据下调用方应直接判失败，不起 worker。
pub(crate) fn context_has_no_evidence(context: &RetestContext) -> bool {
    context.evidence.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding() -> Finding {
        serde_json::from_value(serde_json::json!({
            "id": "find_eval",
            "project_id": "proj_x",
            "title": "Eval injection",
            "description": "user input reaches eval()",
            "severity": "high",
            "status": "confirmed",
            "source_label": "request.body",
            "sink_label": "eval()",
            "cwe": "CWE-95",
            "evidence_ids": ["evd_1"],
            "created_at": "2026-08-24T12:00:00.123456Z",
            "updated_at": "2026-08-24T12:00:00.123456Z",
        }))
        .expect("Finding fixture must parse")
    }

    #[test]
    fn render_includes_every_section_and_the_no_evidence_rule() {
        let context = RetestContext {
            finding_id: "find_eval".to_string(),
            finding_title: "Eval injection".to_string(),
            severity: "high".to_string(),
            status: "confirmed".to_string(),
            description: "user input reaches eval()".to_string(),
            dataflow: Some("request.body → eval()".to_string()),
            cwe: vec!["CWE-95".to_string()],
            evidence: vec![RetestEvidence {
                id: "evd_1".to_string(),
                kind: "taint_path".to_string(),
                summary: "tainted flow to eval".to_string(),
                content: "request.body -> eval(src/app.py:42)".to_string(),
                locations: vec!["src/app.py:42".to_string()],
            }],
            facts: vec!["user input is unsanitised".to_string()],
            branches: vec!["[active] Sink reachability".to_string()],
            tool_calls: vec!["semgrep → ok: 3 findings".to_string()],
            truncated: false,
        };
        let rendered = render_retest_context(&context);

        assert!(rendered.contains("漏洞 id：find_eval"));
        assert!(rendered.contains("CWE：CWE-95"));
        assert!(rendered.contains("数据流：request.body → eval()"));
        assert!(rendered.contains("[taint_path] evd_1 — tainted flow to eval"));
        assert!(rendered.contains("位置：src/app.py:42"));
        // 「没有列出的就是不存在的」必须写进去——这是防止幻觉的主规则。
        assert!(rendered.contains("没有列出的就是不存在的"));
        assert!(!rendered.contains("截断"));
        assert!(context_budget(&context) > 0);
    }

    #[test]
    fn truncated_context_forbids_a_conclusive_verdict() {
        let context = RetestContext {
            finding_id: "f".to_string(),
            finding_title: "t".to_string(),
            severity: "low".to_string(),
            status: "candidate".to_string(),
            description: String::new(),
            dataflow: None,
            cwe: Vec::new(),
            evidence: Vec::new(),
            facts: Vec::new(),
            branches: Vec::new(),
            tool_calls: Vec::new(),
            truncated: true,
        };
        let rendered = render_retest_context(&context);
        assert!(rendered.contains("不得给出 reproduced 或 fixed 结论"));
        assert!(rendered.contains("（无）"));
        assert!(!rendered.contains("数据流："));
        assert!(!rendered.contains("CWE："));
    }

    #[test]
    fn a_finding_without_evidence_cannot_be_assessed() {
        // 项目级事实再多，也不能替代漏洞自己的证据。
        let context = RetestContext {
            finding_id: "f".to_string(),
            finding_title: "t".to_string(),
            severity: "high".to_string(),
            status: "confirmed".to_string(),
            description: String::new(),
            dataflow: None,
            cwe: Vec::new(),
            evidence: Vec::new(),
            facts: vec!["unrelated fact".to_string()],
            branches: vec!["[active] some branch".to_string()],
            tool_calls: vec!["nuclei → ok: 3".to_string()],
            truncated: false,
        };
        assert!(context_has_no_evidence(&context));
    }

    #[test]
    fn a_finding_with_evidence_is_assessable_even_without_facts() {
        let context = RetestContext {
            finding_id: "f".to_string(),
            finding_title: "t".to_string(),
            severity: "high".to_string(),
            status: "confirmed".to_string(),
            description: String::new(),
            dataflow: None,
            cwe: Vec::new(),
            evidence: vec![RetestEvidence {
                id: "evd_1".to_string(),
                kind: "taint_path".to_string(),
                summary: "s".to_string(),
                content: "c".to_string(),
                locations: Vec::new(),
            }],
            facts: Vec::new(),
            branches: Vec::new(),
            tool_calls: Vec::new(),
            truncated: false,
        };
        assert!(!context_has_no_evidence(&context));
    }

    #[test]
    fn empty_context_is_detected() {
        let context = RetestContext {
            finding_id: "f".to_string(),
            finding_title: "t".to_string(),
            severity: "info".to_string(),
            status: "candidate".to_string(),
            description: String::new(),
            dataflow: None,
            cwe: Vec::new(),
            evidence: Vec::new(),
            facts: Vec::new(),
            branches: Vec::new(),
            tool_calls: Vec::new(),
            truncated: false,
        };
        assert!(context_has_no_evidence(&context));
    }

    #[test]
    fn clip_keeps_head_and_tail_of_oversized_fields() {
        let long = "x".repeat(MAX_FIELD_CHARS * 3);
        let clipped = clip(&long);
        assert!(clipped.contains("省略"));
        assert!(clipped.chars().count() < long.chars().count());
        assert!(clipped.starts_with('x'));
        assert!(clipped.ends_with('x'));
    }

    #[test]
    fn oversized_sections_are_dropped_inside_the_budget() {
        // 每条证据都顶到 MAX_FIELD_CHARS，条数顶到上限：远超 24k 预算。
        let mut evidence: Vec<RetestEvidence> = (0..MAX_EVIDENCE * 2)
            .map(|index| RetestEvidence {
                id: format!("evd_{index}"),
                kind: "taint_path".to_string(),
                summary: "s".repeat(MAX_FIELD_CHARS),
                content: "c".repeat(MAX_FIELD_CHARS),
                locations: vec!["l".repeat(MAX_FIELD_CHARS)],
            })
            .collect();
        let mut facts: Vec<String> = (0..MAX_FACTS * 2).map(|index| "f".repeat(400)).collect();
        let mut branches: Vec<String> =
            (0..MAX_BRANCHES * 2).map(|index| "b".repeat(400)).collect();
        let mut tool_calls: Vec<String> =
            (0..MAX_TOOL_CALLS * 2).map(|index| "t".repeat(400)).collect();

        let truncated = trim_to_budget(
            &mut evidence,
            &mut facts,
            &mut branches,
            &mut tool_calls,
        );
        assert!(truncated, "明显超限必须报告被截断");
        // 条数上限先生效。
        assert!(evidence.len() <= MAX_EVIDENCE);
        assert!(facts.len() <= MAX_FACTS);
        assert!(branches.len() <= MAX_BRANCHES);
        assert!(tool_calls.len() <= MAX_TOOL_CALLS);
        // 每段至少留一条：依据不能全丢光。
        assert!(!evidence.is_empty(), "证据不能被清空");
        // 预算估算必须收进上限内（估算偏乐观，放宽一倍作为余量）。
        let estimated = render_budget(&evidence, &facts, &branches, &tool_calls);
        assert!(
            estimated <= MAX_TOTAL_CHARS * 2,
            "预算必须把上下文压住：{estimated} > {}",
            MAX_TOTAL_CHARS * 2
        );

        // 真的渲染一遍，确认上下文片段本身也没失控。
        let context = RetestContext {
            finding_id: "f".to_string(),
            finding_title: "t".to_string(),
            severity: "high".to_string(),
            status: "confirmed".to_string(),
            description: String::new(),
            dataflow: None,
            cwe: Vec::new(),
            evidence,
            facts,
            branches,
            tool_calls,
            truncated,
        };
        assert!(context_budget(&context) <= MAX_TOTAL_CHARS * 2);
    }

    #[test]
    fn small_contexts_are_left_untouched() {
        let mut evidence: Vec<RetestEvidence> = (0..3)
            .map(|index| RetestEvidence {
                id: format!("evd_{index}"),
                kind: "taint_path".to_string(),
                summary: "summary".to_string(),
                content: "content".to_string(),
                locations: vec!["main.rs".to_string()],
            })
            .collect();
        let mut facts = vec!["fact".to_string()];
        let mut branches = vec!["[active] b".to_string()];
        let mut tool_calls = vec!["nuclei → ok".to_string()];

        let truncated = trim_to_budget(
            &mut evidence,
            &mut facts,
            &mut branches,
            &mut tool_calls,
        );
        assert!(!truncated, "小上下文不该被误伤");
        assert_eq!(evidence.len(), 3);
    }

    #[test]
    fn cwe_of_accepts_both_storage_shapes() {
        let array_shape = serde_json::from_value::<Finding>(serde_json::json!({
            "id": "f1", "project_id": "p", "title": "t", "severity": "low",
            "status": "candidate", "cwe": "[\"CWE-79\", \"CWE-80\"]",
            "created_at": "2026-08-24T12:00:00.123456Z",
            "updated_at": "2026-08-24T12:00:00.123456Z",
        }))
        .expect("json-array cwe fixture");
        assert_eq!(cwe_of(&array_shape), vec!["CWE-79", "CWE-80"]);

        let string_shape = serde_json::from_value::<Finding>(serde_json::json!({
            "id": "f2", "project_id": "p", "title": "t", "severity": "low",
            "status": "candidate", "cwe": "CWE-89",
            "created_at": "2026-08-24T12:00:00.123456Z",
            "updated_at": "2026-08-24T12:00:00.123456Z",
        }))
        .expect("string cwe fixture");
        assert_eq!(cwe_of(&string_shape), vec!["CWE-89"]);

        let empty = serde_json::from_value::<Finding>(serde_json::json!({
            "id": "f3", "project_id": "p", "title": "t", "severity": "low",
            "status": "candidate", "cwe": "   ",
            "created_at": "2026-08-24T12:00:00.123456Z",
            "updated_at": "2026-08-24T12:00:00.123456Z",
        }))
        .expect("blank cwe fixture");
        assert!(cwe_of(&empty).is_empty());

        let missing = serde_json::from_value::<Finding>(serde_json::json!({
            "id": "f4", "project_id": "p", "title": "t", "severity": "low",
            "status": "candidate",
            "created_at": "2026-08-24T12:00:00.123456Z",
            "updated_at": "2026-08-24T12:00:00.123456Z",
        }))
        .expect("missing cwe fixture");
        assert!(cwe_of(&missing).is_empty());
    }
}
