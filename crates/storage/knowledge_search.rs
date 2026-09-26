//! 知识卡仓储的确定性检索辅助 —— `server/core/storage/knowledge_search.py`
//! 的移植。

use models::{KnowledgeCard, KnowledgeRetrievalQuery, KnowledgeRetrievalResult};

/// 以确定性的关键词/标签/类型打分检索卡片（Python `search_cards`）。
#[must_use]
#[allow(clippy::cast_precision_loss)] // 计数与 priority 都远小于 2^53，无精度损失。
pub fn search_cards(
    cards: &[KnowledgeCard],
    query: &KnowledgeRetrievalQuery,
) -> Vec<KnowledgeRetrievalResult> {
    let query_terms = terms(&query.text);
    let tag_terms: Vec<String> = query.tags.iter().map(|tag| tag.to_lowercase()).collect();
    let kind_filter: Vec<_> = query.kinds.clone();
    let mut results: Vec<KnowledgeRetrievalResult> = Vec::new();

    for card in cards {
        if !kind_filter.is_empty() && !kind_filter.contains(&card.kind) {
            continue;
        }
        let card_tags: Vec<String> = card.tags.iter().map(|tag| tag.to_lowercase()).collect();
        let searchable =
            format!("{} {} {}", card.title, card.content, card.tags.join(" ")).to_lowercase();
        let matched: Vec<String> = query_terms
            .iter()
            .filter(|term| searchable.contains(term.as_str()))
            .cloned()
            .collect();
        let mut tag_matches: Vec<String> = tag_terms
            .iter()
            .filter(|tag| card_tags.contains(tag))
            .cloned()
            .collect();
        tag_matches.sort();
        if (!query_terms.is_empty() || !tag_terms.is_empty())
            && matched.is_empty()
            && tag_matches.is_empty()
        {
            continue;
        }
        let score = (matched.len() as f64) * 2.0
            + (tag_matches.len() as f64) * 3.0
            + (card.priority as f64) / 100.0;
        let mut combined = matched;
        combined.extend(tag_matches);
        results.push(KnowledgeRetrievalResult {
            card: card.clone(),
            score,
            matched_terms: combined,
            retrieval_reason: None,
        });
    }

    // Python `sorted(key=(score, priority, created_at), reverse=True)`：
    // 稳定排序，同键保持原（seq 升序）相对位置。
    results.sort_by(|a, b| {
        let key = |item: &KnowledgeRetrievalResult| {
            (item.score, item.card.priority, item.card.created_at)
        };
        key(b)
            .partial_cmp(&key(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(usize::try_from(query.limit.max(0)).unwrap_or(usize::MAX));
    results
}

/// Python `_terms`：`[A-Za-z0-9_.:/-]+` 抽词、小写、长度 ≥2。
fn terms(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let is_term_char =
        |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '/' | '-');
    for ch in text.chars() {
        if is_term_char(ch) {
            current.push(ch);
        } else if !current.is_empty() {
            push_term(&mut result, &current);
            current.clear();
        }
    }
    if !current.is_empty() {
        push_term(&mut result, &current);
    }
    result
}

fn push_term(result: &mut Vec<String>, raw: &str) {
    let term = raw.to_lowercase();
    if term.chars().count() >= 2 {
        result.push(term);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::KnowledgeCardKind;

    fn card(id: &str, title: &str, content: &str, tags: &[&str], priority: i64) -> KnowledgeCard {
        serde_json::from_value(serde_json::json!({
            "kind": "vulnerability_pattern", "title": title, "content": content,
            "tags": tags, "priority": priority,
        }))
        .map_or_else(
            |error| panic!("测试卡必须可构造: {error}"),
            |mut card: KnowledgeCard| {
                card.id = models::KnowledgeCardId::new(id.to_string());
                card
            },
        )
    }

    #[test]
    fn search_scores_terms_and_tags_deterministically() {
        let cards = vec![
            card(
                "k_1",
                "SQL injection",
                "classic union select",
                &["web", "sqli"],
                50,
            ),
            card("k_2", "XSS", "reflected payload", &["web"], 80),
            card("k_3", "Deserialization", "java gadget chain", &["java"], 20),
        ];
        let mut query = KnowledgeRetrievalQuery::new();
        query.text = "sql injection".to_string();
        let results = search_cards(&cards, &query);
        assert_eq!(results.len(), 1, "只有 k_1 命中关键词");
        assert_eq!(results[0].card.id.as_str(), "k_1");
        // 2 词 × 2 + 0 tag × 3 + 50/100 = 4.5。
        assert!((results[0].score - 4.5).abs() < 1e-9);
        assert_eq!(results[0].matched_terms, ["sql", "injection"]);

        let mut tag_query = KnowledgeRetrievalQuery::new();
        tag_query.tags = vec!["web".to_string()];
        let tagged = search_cards(&cards, &tag_query);
        assert_eq!(tagged.len(), 2, "tag 命中两条");
        // k_2 priority 80 → 3.8 分高于 k_1 的 3.5。
        assert_eq!(tagged[0].card.id.as_str(), "k_2");
    }

    #[test]
    fn search_with_no_terms_matches_all_up_to_limit() {
        let cards = vec![
            card("k_1", "a", "c", &[], 50),
            card("k_2", "b", "c", &[], 60),
        ];
        let mut query = KnowledgeRetrievalQuery::new();
        query.limit = 1;
        let results = search_cards(&cards, &query);
        assert_eq!(results.len(), 1, "limit 截断");
        assert_eq!(results[0].card.id.as_str(), "k_2", "priority 高者在前");
    }

    #[test]
    fn search_filters_by_kind() {
        let mut cards = vec![card("k_1", "SQL", "u", &[], 50)];
        cards[0].kind = KnowledgeCardKind::CaseReference;
        let mut query = KnowledgeRetrievalQuery::new();
        query.kinds = vec![KnowledgeCardKind::VulnerabilityPattern];
        let results = search_cards(&cards, &query);
        assert!(results.is_empty(), "kind 不匹配必须被过滤");
    }

    #[test]
    fn terms_extractor_matches_python_regex() {
        assert_eq!(
            terms("SQL injection-explained"),
            ["sql", "injection-explained"]
        );
        assert_eq!(terms("a b cc"), ["cc"], "单字符词被丢弃");
        assert!(terms("配置文件").is_empty(), "非 ASCII 不入词");
    }
}
