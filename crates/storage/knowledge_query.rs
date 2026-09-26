//! 知识检索的统一 Query Normalizer —— `knowledge_search`（旧）与
//! `knowledge_fts`（新）共用同一条归一化管线，替代各调用点自维护。
//!
//! 词法规则：
//! * ASCII 词元 `[A-Za-z0-9_.:/-]+` **原样保留**（`CVE-2025-1234`、
//!   `SeImpersonatePrivilege`、`--risk`、URL 路径不被破坏），小写化，
//!   长度 ≥2；
//! * CJK 连续段整体成词并附加**二元组**（unicode61 下二元组是中文召回的基础）；
//! * 命中别名注册表时追加同组其余词面（标记 `expanded_from`）。

use std::sync::OnceLock;

use regex::Regex;

use crate::knowledge_aliases::AliasRegistry;

static TOKEN_RE: OnceLock<Regex> = OnceLock::new();
static CJK_RE: OnceLock<Regex> = OnceLock::new();

/// ASCII 词元或 CJK 连续段。
fn token_re() -> &'static Regex {
    TOKEN_RE.get_or_init(|| {
        Regex::new(r"[A-Za-z0-9_.:/-]+|[\x{3400}-\x{9fff}]+")
            .unwrap_or_else(|error| panic!("内置正则必须可编译（静态字面量）: {error}"))
    })
}

/// 整段是否全为 CJK。
fn cjk_re() -> &'static Regex {
    CJK_RE.get_or_init(|| {
        Regex::new(r"^[\x{3400}-\x{9fff}]+$")
            .unwrap_or_else(|error| panic!("内置正则必须可编译（静态字面量）: {error}"))
    })
}

/// 归一化后的检索查询。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NormalizedKnowledgeQuery {
    /// ASCII 词元（小写、保序去重）。
    pub ascii_terms: Vec<String>,
    /// CJK 段整体词 + 二元组（小写、保序去重）。
    pub cjk_terms: Vec<String>,
    /// 别名展开追加的 `(term, canonical)`。
    pub alias_expansions: Vec<(String, String)>,
    /// `ascii_terms + cjk_terms + 展开词` 合并去重后的最终词项集
    /// （`FTS MATCH` 与 `matched_terms` 归因都用它）。
    pub all_terms: Vec<String>,
}

impl NormalizedKnowledgeQuery {
    /// 是否没有任何可用词项（纯过滤查询）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.all_terms.is_empty()
    }
}

/// 归一化查询文本（小写化、抽词、CJK 二元组、别名展开）。
#[must_use]
pub fn normalize_query(text: &str, registry: &AliasRegistry) -> NormalizedKnowledgeQuery {
    let mut ascii_terms: Vec<String> = Vec::new();
    let mut cjk_terms: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let push = |term: String,
                cjk: bool,
                ascii: &mut Vec<String>,
                cjk_terms: &mut Vec<String>,
                seen: &mut std::collections::HashSet<String>| {
        if seen.insert(term.clone()) {
            if cjk {
                cjk_terms.push(term);
            } else {
                ascii.push(term);
            }
        }
    };

    for raw in token_re().find_iter(text) {
        let token = raw.as_str().to_lowercase();
        if cjk_re().is_match(&token) {
            push(
                token.clone(),
                true,
                &mut ascii_terms,
                &mut cjk_terms,
                &mut seen,
            );
            let chars: Vec<char> = token.chars().collect();
            if chars.len() > 2 {
                for index in 0..chars.len() - 1 {
                    let bigram: String = chars[index..index + 2].iter().collect();
                    push(bigram, true, &mut ascii_terms, &mut cjk_terms, &mut seen);
                }
            }
        } else if token.chars().count() >= 2 {
            push(token, false, &mut ascii_terms, &mut cjk_terms, &mut seen);
        }
    }

    let alias_expansions = registry.expand(text);
    let mut all_terms = ascii_terms.clone();
    all_terms.extend(cjk_terms.iter().cloned());
    for (term, _) in &alias_expansions {
        if seen.insert(term.clone()) {
            all_terms.push(term.clone());
        }
    }

    NormalizedKnowledgeQuery {
        ascii_terms,
        cjk_terms,
        alias_expansions,
        all_terms,
    }
}

/// 生成 FTS5 `MATCH` 表达式：全部词项 OR 连接，词面内部的双引号剥除，
/// 其余字符在引号内按字面匹配。空词项集返回 `None`（纯过滤查询）。
#[must_use]
pub fn fts_match_expr(query: &NormalizedKnowledgeQuery) -> Option<String> {
    if query.all_terms.is_empty() {
        return None;
    }
    let clauses: Vec<String> = query
        .all_terms
        .iter()
        .map(|term| format!("\"{}\"", term.replace('"', "")))
        .collect();
    Some(clauses.join(" OR "))
}

/// 归一化知识卡检索词（ingestion 期 `search_terms` 的生成入口）。
///
/// 对标题/摘要/别名/结构化字段做归一化，追加别名注册表展开；结果保序
/// 去重。写入 `KnowledgeCard.search_terms` 与 FTS 列。空字段自动跳过。
#[must_use]
pub fn derive_search_terms(fields: &[&str], registry: &AliasRegistry) -> Vec<String> {
    let joined = fields
        .iter()
        .filter(|field| !field.trim().is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    normalize_query(&joined, registry).all_terms
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn ascii_tokens_preserve_identifiers() {
        let normalized = normalize_query(
            "exploit CVE-2025-12345 with --risk=3",
            AliasRegistry::embedded(),
        );
        assert!(
            normalized
                .ascii_terms
                .contains(&"cve-2025-12345".to_string())
        );
        assert!(
            normalized.ascii_terms.contains(&"--risk".to_string()),
            "flag 词面原样保留（'=' 不属词元类，值单独成词）"
        );
        assert!(normalized.ascii_terms.contains(&"exploit".to_string()));
    }

    #[test]
    fn cjk_segments_become_whole_and_bigrams() {
        let normalized = normalize_query("利用文件上传实现RCE", AliasRegistry::embedded());
        let whole = "利用文件上传实现";
        assert!(
            normalized.cjk_terms.contains(&whole.to_string()),
            "CJK 连续段整体成词"
        );
        for bigram in ["利用", "用文", "文件", "件上", "上传", "传实", "实现"] {
            assert!(
                normalized.cjk_terms.contains(&bigram.to_string()),
                "缺少二元组 {bigram}"
            );
        }
        assert!(normalized.ascii_terms.contains(&"rce".to_string()));
        assert!(
            normalized
                .cjk_terms
                .iter()
                .all(|term| term.chars().count() <= whole.chars().count()),
            "无超段词"
        );
    }

    #[test]
    fn alias_expansion_appends_group_terms() {
        let normalized = normalize_query("SQLi payload for mysql", AliasRegistry::embedded());
        let expanded: Vec<&str> = normalized
            .alias_expansions
            .iter()
            .map(|(term, _)| term.as_str())
            .collect();
        assert!(expanded.contains(&"sql injection"), "命中 SQLi 展开全组");
        assert!(expanded.contains(&"sql注入"));
        assert!(normalized.all_terms.contains(&"sql injection".to_string()));
    }

    #[test]
    fn match_expr_quotes_and_ors() {
        let normalized = normalize_query("sql 注入", AliasRegistry::embedded());
        let expr = fts_match_expr(&normalized).expect("非空词项必有表达式");
        assert!(
            expr.starts_with('"') && expr.ends_with('"'),
            "词面一律加引号"
        );
        assert!(expr.contains(" OR "));
        // tokenizer 词面类不含引号，此处验证防御性剥除逻辑本身。
        assert_eq!(format!("\"{}\"", "ha\"ck".replace('"', "")), "\"hack\"");
    }

    #[test]
    fn empty_text_yields_empty_query() {
        let normalized = normalize_query("  ", AliasRegistry::embedded());
        assert!(normalized.is_empty());
        assert!(fts_match_expr(&normalized).is_none());
    }

    #[test]
    fn derive_search_terms_merges_fields_and_aliases() {
        let terms = derive_search_terms(
            &["MySQL 联合查询注入", "union based", "sqli"],
            AliasRegistry::embedded(),
        );
        assert!(terms.contains(&"联合".to_string()) && terms.contains(&"注入".to_string()));
        assert!(terms.contains(&"union".to_string()));
        assert!(
            terms.contains(&"sql injection".to_string()),
            "别名展开进 search_terms"
        );
    }
}
