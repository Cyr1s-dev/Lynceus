//! 安全领域别名注册表 —— 检索归一化的同义词/双语扩展源。
//!
//! 注册表以 JSON 资源内嵌（`resources/knowledge/security-aliases.json`，与
//! `tool_catalog` 的 `include_str!` 惯例一致），**不在代码里 hardcode**：
//! 扩词只改资产文件，行为随构建确定。
//!
//! 设计约束：
//! * deterministic —— 同一注册表 + 同一 query 永远产出同一扩展集；
//! * exact identifiers 不进注册表 —— `CVE-2025-1234`、`CWE-78`、
//!   `SeImpersonatePrivilege`、`--risk` 等由 tokenizer 原样保留；
//! * 展开是"命中任一词 → 追加同组全部词"，原文词项永远保留。

use std::sync::OnceLock;

use serde::Deserialize;

const EMBEDDED_REGISTRY: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../resources/knowledge/security-aliases.json"
));

/// 单个同义词条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasTerm {
    /// 原始词面（保留大小写，展示用）。
    pub text: String,
    /// 小写检索形态。
    pub lower: String,
}

/// 一个概念的同义词组（`canonical` + 全部等价词面）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasGroup {
    /// 规范名（小写检索形态）。
    pub canonical: String,
    /// 组内全部词面（含规范名本身）。
    pub terms: Vec<AliasTerm>,
}

/// 内嵌别名注册表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasRegistry {
    /// 全部同义词组（保持 JSON 声明顺序）。
    pub groups: Vec<AliasGroup>,
}

#[derive(Debug, Deserialize)]
struct RegistryWire {
    #[allow(dead_code)]
    version: i64,
    groups: Vec<GroupWire>,
}

#[derive(Debug, Deserialize)]
struct GroupWire {
    canonical: String,
    terms: Vec<String>,
}

impl AliasRegistry {
    /// 解析注册表 JSON（`include_str!` 静态资产 + 测试注入共用）。
    ///
    /// # Errors
    /// JSON 非法或词面全空白。
    pub fn parse(json: &str) -> Result<Self, String> {
        let wire: RegistryWire = serde_json::from_str(json)
            .map_err(|error| format!("alias registry invalid: {error}"))?;
        let mut groups = Vec::with_capacity(wire.groups.len());
        for group in wire.groups {
            let mut terms = Vec::with_capacity(group.terms.len() + 1);
            let canonical_lower = group.canonical.trim().to_lowercase();
            if canonical_lower.is_empty() {
                return Err("alias group canonical must not be blank".to_string());
            }
            for term in group.terms {
                let text = term.trim().to_string();
                if text.is_empty() {
                    continue;
                }
                terms.push(AliasTerm {
                    lower: text.to_lowercase(),
                    text,
                });
            }
            if terms.is_empty() {
                return Err(format!(
                    "alias group '{canonical_lower}' must define at least one term"
                ));
            }
            if !terms.iter().any(|term| term.lower == canonical_lower) {
                terms.push(AliasTerm {
                    lower: canonical_lower.clone(),
                    text: canonical_lower.clone(),
                });
            }
            groups.push(AliasGroup {
                canonical: canonical_lower,
                terms,
            });
        }
        if groups.is_empty() {
            return Err("alias registry must define at least one group".to_string());
        }
        Ok(Self { groups })
    }

    /// 内嵌静态注册表（构建期确定）。
    ///
    /// # Panics
    /// 内嵌字面量非法（编程错误）：静态字符串写错时立即失败优于静默
    /// 降级，panic 文本携带解析错误。
    #[must_use]
    pub fn embedded() -> &'static Self {
        static REGISTRY: OnceLock<AliasRegistry> = OnceLock::new();
        REGISTRY.get_or_init(|| {
            AliasRegistry::parse(EMBEDDED_REGISTRY)
                .unwrap_or_else(|error| panic!("内嵌别名注册表必须合法: {error}"))
        })
    }

    /// 展开 query 文本中命中的同义词组。
    ///
    /// 追加词 = 命中组的**其余全部词面**（去重、保序），命中判定：
    /// ASCII 词面按"非字母数字边界 + 包含"，CJK 词面按直接包含；均
    /// 小写比较。返回值附 `canonical` 供 `retrieval_reason` 归因。
    #[must_use]
    pub fn expand(&self, query: &str) -> Vec<(String, String)> {
        let query_lower = query.to_lowercase();
        let query_chars: Vec<char> = query_lower.chars().collect();
        let mut expanded: Vec<(String, String)> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for group in &self.groups {
            let hit = group.terms.iter().any(|term| {
                if is_cjk(&term.lower) {
                    query_lower.contains(&term.lower)
                } else {
                    contains_word(&query_chars, &term.lower)
                }
            });
            if !hit {
                continue;
            }
            for term in &group.terms {
                if term.lower == query_lower.trim() {
                    continue;
                }
                if seen.insert(term.lower.clone()) {
                    expanded.push((term.lower.clone(), group.canonical.clone()));
                }
            }
        }
        expanded
    }
}

/// 整段是否全为 CJK 表意文字。
#[must_use]
pub fn is_cjk(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|ch| ('\u{3400}'..='\u{9fff}').contains(&ch))
}

/// 词边界包含判定：term 两侧须为非 ASCII 字母数字（或串首尾）。
fn contains_word(haystack: &[char], needle: &str) -> bool {
    let needle_chars: Vec<char> = needle.chars().collect();
    if needle_chars.is_empty() || needle_chars.len() > haystack.len() {
        return false;
    }
    let is_word = |ch: char| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.');
    for start in 0..=(haystack.len() - needle_chars.len()) {
        if haystack[start..start + needle_chars.len()] != needle_chars[..] {
            continue;
        }
        let left_ok = start == 0 || !is_word(haystack[start - 1]);
        let end = start + needle_chars.len();
        let right_ok = end == haystack.len() || !is_word(haystack[end]);
        if left_ok && right_ok {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const SAMPLE: &str = r#"{"version":1,"groups":[
        {"canonical":"sql injection","terms":["SQL injection","SQLi","SQL注入"]},
        {"canonical":"reverse shell","terms":["reverse shell","反弹 shell"]}
    ]}"#;

    #[test]
    fn parse_normalizes_and_appends_canonical() {
        let registry = AliasRegistry::parse(SAMPLE).unwrap();
        assert_eq!(registry.groups.len(), 2);
        assert_eq!(registry.groups[0].canonical, "sql injection");
        let lowers: Vec<&str> = registry.groups[0]
            .terms
            .iter()
            .map(|term| term.lower.as_str())
            .collect();
        assert!(lowers.contains(&"sql injection"), "canonical 自动入组");
    }

    #[test]
    fn parse_rejects_blank_groups() {
        assert!(AliasRegistry::parse(r#"{"version":1,"groups":[]}"#).is_err());
        assert!(
            AliasRegistry::parse(r#"{"version":1,"groups":[{"canonical":"x","terms":["  "]}]}"#)
                .is_err()
        );
    }

    #[test]
    fn embedded_registry_is_valid_and_covers_spec_groups() {
        let registry = AliasRegistry::embedded();
        let canonicals: Vec<&str> = registry
            .groups
            .iter()
            .map(|group| group.canonical.as_str())
            .collect();
        for expected in [
            "sql injection",
            "remote code execution",
            "privilege escalation",
            "lateral movement",
            "reverse shell",
            "directory traversal",
            "command injection",
            "server-side request forgery",
        ] {
            assert!(
                canonicals.contains(&expected),
                "缺少 spec 必备组 {expected}"
            );
        }
    }

    #[test]
    fn expand_hits_ascii_boundary_and_cjk() {
        let registry = AliasRegistry::embedded();
        let expanded = registry.expand("detect SQLi in login form");
        let terms: Vec<&str> = expanded.iter().map(|(term, _)| term.as_str()).collect();
        assert!(terms.contains(&"sql injection"), "命中 SQLi 展开全组");
        assert!(terms.contains(&"sql注入"));

        let cjk = registry.expand("目标存在任意文件上传漏洞");
        let cjk_terms: Vec<&str> = cjk.iter().map(|(term, _)| term.as_str()).collect();
        assert!(cjk_terms.contains(&"file upload"), "中文命中反向展开英文");

        let none = registry.expand("unrelated query about dns");
        assert!(none.is_empty(), "未命中不展开");
    }

    #[test]
    fn expand_preserves_exact_identifier_queries() {
        let registry = AliasRegistry::embedded();
        // CVE/CWE/privilege 名不被别名系统改写：不含命中词时零展开。
        assert!(registry.expand("CVE-2025-12345").is_empty());
        assert!(registry.expand("SeImpersonatePrivilege").is_empty());
    }

    #[test]
    fn is_cjk_detects_pure_ideographic_runs() {
        assert!(is_cjk("文件上传"));
        assert!(!is_cjk("文件上传RCE"));
        assert!(!is_cjk(""));
    }
}
