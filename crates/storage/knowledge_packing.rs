//! Knowledge Context Packing —— 从检索结果到 prompt 注入的最后一站。
//!
//! 目标不是"top-K 全塞"：
//! * **relevance**：得分降序为基线；
//! * **diversity**：同一 tool / 同一父单元的近似结果受限额约束，避免
//!   "同一工具的 10 条近似命令"占满上下文；
//! * **parent/child 去重**：父子同时高分时优先父单元（父的 summary 已
//!   概括全貌），子单元只在额度内保留最好的；
//! * **budget**：注入正文按字符预算截断选择（chars ≈ tokens/4 的保守
//!   估算），预算内装不满就少装。
//!
//! 全程 deterministic：同一输入永远产出同一注入序列。

use models::KnowledgeRetrievalResult;

/// 打包选项（全部可配，缺省即"6~12 条 + 预算"的推荐形态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackOptions {
    /// 注入条数上限（spec 推荐 6~12）。
    pub max_units: usize,
    /// 同一父单元最多注入的子卡数（父卡本身不计）。
    pub max_children_per_parent: usize,
    /// 同一工具最多注入的卡数。
    pub max_per_tool: usize,
    /// 注入正文总字符预算（summary 优先，不足时不再补 body）。
    pub char_budget: usize,
}

impl Default for PackOptions {
    fn default() -> Self {
        Self {
            max_units: 8,
            max_children_per_parent: 2,
            max_per_tool: 3,
            char_budget: 12_000,
        }
    }
}

/// 打包后的一个注入单元。
#[derive(Debug, Clone, PartialEq)]
pub struct PackedKnowledge<'a> {
    /// 来源检索结果。
    pub item: &'a KnowledgeRetrievalResult,
    /// 注入正文（summary，缺省回退 body 截断）。
    pub injection_text: String,
    /// 预算原因（被截断/被限额时说明，供 trace）。
    pub pack_note: Option<String>,
}

/// 打包检索结果：relevance → diversity → parent/child 去重 → budget。
#[must_use]
pub fn pack_knowledge<'a>(
    results: &'a [KnowledgeRetrievalResult],
    options: &PackOptions,
) -> Vec<PackedKnowledge<'a>> {
    // 结果按得分降序进入（调用方已排序；这里再稳排一次保证契约）。
    let mut ranked: Vec<&KnowledgeRetrievalResult> = results.iter().collect();
    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut used_budget = 0usize;
    let mut child_count_per_parent: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut tool_count: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut packed: Vec<PackedKnowledge<'a>> = Vec::new();

    for result in &ranked {
        if packed.len() >= options.max_units {
            break;
        }
        let card = &result.card;

        // —— parent/child 去重：父单元在包内时，子卡让位（除非父卡未被
        // 检索到而子卡极高分，此时子卡顶上并记录 note）。
        let parent_key = card.parent_id.as_ref().map(|id| id.as_str().to_string());
        if let Some(parent) = &parent_key {
            let count = child_count_per_parent.get(parent).copied().unwrap_or(0);
            if count >= options.max_children_per_parent {
                continue;
            }
        }

        // —— 工具多样性限额（无 tool 字段的卡不受限）。
        let tool_key = card.tool.first().cloned();
        if let Some(tool) = &tool_key {
            let count = tool_count.get(tool).copied().unwrap_or(0);
            if count >= options.max_per_tool {
                continue;
            }
        }

        // —— 预算：summary（缺省回退 body）超预算时截断，整条放不下跳过。
        let summary_full = card.effective_summary();
        let summary = if summary_full.is_empty() {
            card.body.as_str()
        } else {
            summary_full
        };
        if summary.is_empty() {
            continue;
        }
        let chars: Vec<char> = summary.chars().collect();
        let allowed = options.char_budget.saturating_sub(used_budget);
        if allowed == 0 {
            continue;
        }
        let (injection_text, pack_note) = if chars.len() > allowed {
            let truncated: String = chars[..allowed.saturating_sub(3)].iter().collect();
            (
                format!("{truncated}..."),
                Some("summary truncated to fit budget".to_string()),
            )
        } else {
            (summary.to_string(), None)
        };
        used_budget += injection_text.chars().count();

        if let Some(parent) = &parent_key {
            *child_count_per_parent.entry(parent.clone()).or_insert(0) += 1;
        }
        if let Some(tool) = &tool_key {
            *tool_count.entry(tool.clone()).or_insert(0) += 1;
        }

        packed.push(PackedKnowledge {
            item: result,
            injection_text,
            pack_note,
        });
    }
    packed
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use models::KnowledgeCard;
    use models::KnowledgeCardId;

    fn result(id: &str, title: &str, summary: &str, score: f64) -> KnowledgeRetrievalResult {
        KnowledgeRetrievalResult {
            card: serde_json::from_value::<KnowledgeCard>(serde_json::json!({
                "kind": "tool_usage", "title": title, "summary": summary, "content": summary,
            }))
            .map(|mut card: KnowledgeCard| {
                card.id = KnowledgeCardId::new(id.to_string());
                card
            })
            .unwrap(),
            score,
            matched_terms: Vec::new(),
            retrieval_reason: None,
        }
    }

    fn with_parent(mut item: KnowledgeRetrievalResult, parent: &str) -> KnowledgeRetrievalResult {
        item.card.parent_id = Some(KnowledgeCardId::new(parent.to_string()));
        item
    }

    fn with_tool(mut item: KnowledgeRetrievalResult, tool: &str) -> KnowledgeRetrievalResult {
        item.card.tool = vec![tool.to_string()];
        item
    }

    #[test]
    fn packs_top_within_limits_and_budget() {
        let results = vec![
            result("a", "A", "summary a", 5.0),
            result("b", "B", "summary b", 4.0),
            result("c", "C", "summary c", 3.0),
        ];
        let options = PackOptions {
            max_units: 2,
            ..PackOptions::default()
        };
        let packed = pack_knowledge(&results, &options);
        assert_eq!(packed.len(), 2);
        assert_eq!(packed[0].item.card.title, "A");
    }

    #[test]
    fn diversity_caps_per_tool() {
        let results = vec![
            with_tool(result("a", "A", "s", 9.0), "sqlmap"),
            with_tool(result("b", "B", "s", 8.0), "sqlmap"),
            with_tool(result("c", "C", "s", 7.0), "sqlmap"),
            with_tool(result("d", "D", "s", 6.0), "sqlmap"),
            result("e", "E", "s", 5.0),
        ];
        let options = PackOptions {
            max_per_tool: 3,
            ..PackOptions::default()
        };
        let packed = pack_knowledge(&results, &options);
        assert_eq!(packed.len(), 4, "sqlmap 限 3 + 其他 1");
        let titles: Vec<&str> = packed
            .iter()
            .map(|item| item.item.card.title.as_str())
            .collect();
        assert!(!titles.contains(&"D"), "第 4 条 sqlmap 被多样性限额排除");
    }

    #[test]
    fn parent_children_dedup_prefers_parent_order() {
        let parent = result("p", "Tool Summary", "parent summary", 9.0);
        let child_a = with_parent(result("c1", "Command A", "cmd a", 8.0), "p");
        let child_b = with_parent(result("c2", "Command B", "cmd b", 7.0), "p");
        let child_c = with_parent(result("c3", "Command C", "cmd c", 6.0), "p");
        let results = vec![parent, child_a, child_b, child_c];
        let options = PackOptions {
            max_children_per_parent: 2,
            max_units: 4,
            ..PackOptions::default()
        };
        let packed = pack_knowledge(&results, &options);
        assert_eq!(packed.len(), 3, "父 + 2 子，第 3 子被去重");
    }

    #[test]
    fn budget_truncates_and_stops() {
        let long = "x".repeat(100);
        let results = vec![result("a", "A", &long, 9.0), result("b", "B", &long, 8.0)];
        let options = PackOptions {
            char_budget: 120,
            ..PackOptions::default()
        };
        let packed = pack_knowledge(&results, &options);
        assert_eq!(packed.len(), 2);
        assert!(
            packed[0].pack_note.is_none(),
            "首条 100 字符在 120 预算内完整注入"
        );
        assert!(packed[1].pack_note.is_some(), "第二条只剩 20 预算被截断");
        assert!(
            packed[1].injection_text.chars().count() <= 20,
            "截断后不超过剩余预算"
        );
    }

    #[test]
    fn empty_summary_with_body_falls_back() {
        let mut item = result("a", "A", "", 5.0);
        item.card.summary.clear();
        item.card.content.clear();
        item.card.body = "body text here".to_string();
        let packed = pack_knowledge(std::slice::from_ref(&item), &PackOptions::default());
        assert_eq!(packed[0].injection_text, "body text here");
    }
}
