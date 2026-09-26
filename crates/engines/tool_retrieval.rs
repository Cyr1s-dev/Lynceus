//! `ToolRetriever` 的 catalog 实现：词法打分 + 能力匹配 + 工件兼容 +
//! 可用性过滤 + 显式工具加成（P1 阶段刻意不引入向量库）。
//!
//! 复用既有 [`ToolCatalogEntry`]（嵌入 YAML + 本地检测）作为唯一数据源，
//! 不建第二套 Catalog。检索只是 discovery：返回 compact 描述符候选，
//! 绝不执行工具——执行授权仍由 Harness（adapter / allow-list / schema
//! 校验 / 风险策略）决定。
//!
//! [`ToolRetrievalIndex`] 是纯计算索引：catalog CLI 工具 + harness 原生/
//! 远程工具可合并建索引（同一打分实现，两处装配），查询是同步且不可
//! 失败的（空索引返回空候选——空目录优雅降级）。
//! 【统一工具系统核心】开放世界工具检索：`tool_search` MCP 工具的
//! 实现基础（词法打分索引，返回候选摘要）。

use std::collections::HashSet;
use std::sync::Arc;

use agents::tool_retrieval::ToolRetriever;
use models::tool_retrieval::CompactToolDescriptor;
use models::tool_retrieval::ToolCandidate;
use models::tool_retrieval::ToolCandidateAvailability;
use models::tool_retrieval::ToolRetrievalQuery;
use models::tool_retrieval::ToolRetrievalResult;

use crate::tool_catalog::ToolAvailability;
use crate::tool_catalog::ToolCatalogEntry;

/// 单 token 命中描述的最大计分上限（防单个长描述刷分）。
const DESCRIPTION_TOKEN_CAP: f64 = 3.0;
/// 工具名 token 命中上限。
const NAME_TOKEN_CAP: f64 = 3.0;
/// 领域 token 命中上限（weak signal）。
const DOMAIN_TOKEN_CAP: f64 = 1.5;
/// 显式工具加成。
const EXPLICIT_BOOST: f64 = 3.0;
/// 工件兼容加成。
const ARTIFACT_BOOST: f64 = 2.0;
/// 已安装且健康的加成。
const INSTALLED_BOOST: f64 = 0.5;
/// domain hint 弱加成（绝不作为硬前提）。
const DOMAIN_HINT_BOOST: f64 = 0.25;
/// 查询侧停用词：纯会话问句（what/how/does/and…）绝不允许仅凭功能词
/// 命中工具描述——No-tool accuracy 是检索契约的一等指标。
const QUERY_STOPWORDS: &[&str] = &[
    "and", "the", "for", "with", "this", "that", "what", "how", "does", "why", "are", "was",
    "were", "from", "into", "your", "you", "all", "any", "can", "could", "should", "would", "use",
    "using", "used", "out", "about", "tell", "show", "give", "please", "help", "there", "here",
    "have", "has", "its", "it", "is", "to", "of", "in", "on", "a", "an",
];
/// 工件 kind → 兼容审计域（确定性弱信号表）。
const ARTIFACT_DOMAINS: &[(&str, &[&str])] = &[
    (
        "elf",
        &["binary_static", "binary_dynamic", "binary_analysis"],
    ),
    (
        "binary",
        &["binary_static", "binary_dynamic", "binary_analysis"],
    ),
    (
        "pe",
        &["binary_static", "binary_dynamic", "binary_analysis"],
    ),
    ("pcap", &["traffic_intelligence"]),
    ("traffic", &["traffic_intelligence"]),
    ("url", &["web_recon", "content_discovery", "web_dast"]),
    ("domain", &["asset_recon"]),
    ("source", &["web_sast", "code_deep_sast"]),
    ("host", &["internal_surface", "asset_recon"]),
];
/// 高风险域（risk class 过滤用；弱策略，仅约束候选集）。
const HIGH_RISK_DOMAINS: [&str; 2] = ["web_exploit", "exploitability_validation"];
/// 中风险域。
const MEDIUM_RISK_DOMAINS: [&str; 5] = [
    "web_dast",
    "web_validation",
    "exposure_intelligence",
    "internal_surface",
    "web_recon",
];

/// 一条索引记录：compact 描述符 + 分层词法 token 集。
#[derive(Debug, Clone)]
struct IndexedTool {
    descriptor: CompactToolDescriptor,
    name_tokens: HashSet<String>,
    summary_tokens: HashSet<String>,
    all_tokens: HashSet<String>,
    explicit_aliases: Vec<String>,
}

/// 工具检索索引（catalog + 原生/远程扩展，纯词法打分）。
#[derive(Debug, Clone)]
pub struct ToolRetrievalIndex {
    tools: Vec<IndexedTool>,
}

impl ToolRetrievalIndex {
    /// 从 catalog 条目构建（不含原生/远程工具）。
    #[must_use]
    pub fn from_catalog(catalog: &[ToolCatalogEntry]) -> Self {
        let tools = catalog
            .iter()
            .map(|entry| {
                let availability = availability_of(entry);
                let descriptor = CompactToolDescriptor {
                    tool_id: entry.id.clone(),
                    title: entry.name.clone(),
                    summary: entry.description.clone(),
                    capabilities: vec![entry.domain.clone()],
                    domain: entry.domain.clone(),
                    availability,
                    risk: risk_class_of(&entry.domain).to_string(),
                    adapter_status: entry.adapter_status.clone(),
                };
                let name_tokens = tokenize(&entry.name);
                let summary_tokens = tokenize(&entry.description);
                let mut all_tokens = name_tokens.clone();
                all_tokens.extend(summary_tokens.iter().cloned());
                all_tokens.extend(tokenize(&entry.domain));
                let mut explicit_aliases = vec![entry.id.to_lowercase(), entry.name.to_lowercase()];
                explicit_aliases.extend(
                    entry
                        .executable_names
                        .iter()
                        .map(|name| name.to_lowercase()),
                );
                IndexedTool {
                    descriptor,
                    name_tokens,
                    summary_tokens,
                    all_tokens,
                    explicit_aliases,
                }
            })
            .collect();
        Self { tools }
    }

    /// 追加 harness 原生/远程工具的 compact 描述符（同一打分实现）。
    #[must_use]
    pub fn with_extra_descriptors(mut self, extra: Vec<CompactToolDescriptor>) -> Self {
        for descriptor in extra {
            let name_tokens = tokenize(&descriptor.title);
            let summary_tokens = tokenize(&descriptor.summary);
            let mut all_tokens = name_tokens.clone();
            all_tokens.extend(summary_tokens.iter().cloned());
            all_tokens.extend(tokenize(&descriptor.domain));
            all_tokens.extend(
                descriptor
                    .capabilities
                    .iter()
                    .flat_map(|capability| tokenize(capability))
                    .collect::<HashSet<String>>(),
            );
            let explicit_aliases = vec![
                descriptor.tool_id.to_lowercase(),
                descriptor.title.to_lowercase(),
            ];
            self.tools.push(IndexedTool {
                descriptor,
                name_tokens,
                summary_tokens,
                all_tokens,
                explicit_aliases,
            });
        }
        self
    }

    /// 检索：打分 → 过滤 → 排序（relevance 降序，tie-break id 升序）→ 限流。
    #[must_use]
    pub fn search(&self, query: &ToolRetrievalQuery) -> ToolRetrievalResult {
        let query_tokens: Vec<String> = tokenize(&query.retrieval_text())
            .into_iter()
            .filter(|token| token.chars().count() >= 3)
            .filter(|token| !QUERY_STOPWORDS.contains(&token.as_str()))
            .collect();
        let risk_limit = query.risk_limit.as_deref().and_then(risk_rank);

        let mut candidates: Vec<ToolCandidate> = self
            .tools
            .iter()
            .filter_map(|tool| score_tool(tool, query, &query_tokens, risk_limit))
            .collect();
        candidates.sort_by(|left, right| {
            right
                .relevance
                .partial_cmp(&left.relevance)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.descriptor.tool_id.cmp(&right.descriptor.tool_id))
        });
        candidates.truncate(query.limit);
        ToolRetrievalResult { candidates }
    }

    /// 索引覆盖的工具名（id + 展示名；供 bootstrap 显式工具识别，
    /// 绝不含参数 schema）。
    #[must_use]
    pub fn known_tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .tools
            .iter()
            .flat_map(|tool| {
                [
                    tool.descriptor.tool_id.clone(),
                    tool.descriptor.title.clone(),
                ]
            })
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

/// Catalog 检索器（`agents::tool_retrieval::ToolRetriever` 的 catalog 实现）。
#[derive(Debug, Clone)]
pub struct CatalogToolRetriever {
    index: ToolRetrievalIndex,
}

impl CatalogToolRetriever {
    /// 从 catalog 条目构建。
    #[must_use]
    pub fn new(catalog: &[ToolCatalogEntry]) -> Self {
        Self {
            index: ToolRetrievalIndex::from_catalog(catalog),
        }
    }

    /// 从预装配索引构建（catalog + 原生/远程扩展描述符）。
    #[must_use]
    pub fn from_index(index: ToolRetrievalIndex) -> Self {
        Self { index }
    }
}

impl ToolRetriever for CatalogToolRetriever {
    fn retrieve(&self, query: &ToolRetrievalQuery) -> ToolRetrievalResult {
        self.index.search(query)
    }

    fn known_tool_names(&self) -> Vec<String> {
        self.index.known_tool_names()
    }
}

/// 共享句柄便捷构造（api / runtime 装配用）。
#[must_use]
pub fn catalog_tool_retriever(catalog: &[ToolCatalogEntry]) -> Arc<CatalogToolRetriever> {
    Arc::new(CatalogToolRetriever::new(catalog))
}

/// Harness profile 声明的原生工具 → compact 描述符（注册 adapter 即视为
/// 可用，可用性与 adapter 同生共死；domain 取 profile 首个审计域）。
#[must_use]
pub fn native_descriptors_for(
    profile: &crate::harness::profile::DomainProfile,
) -> Vec<CompactToolDescriptor> {
    let domain = profile
        .audit_domains
        .first()
        .map_or("unknown", |domain| domain.as_str())
        .to_string();
    profile
        .native_tools
        .iter()
        .map(|spec| CompactToolDescriptor {
            tool_id: spec.id.to_string(),
            title: spec.title.to_string(),
            summary: spec.description.to_string(),
            capabilities: vec![domain.clone()],
            domain: domain.clone(),
            availability: ToolCandidateAvailability::Native,
            risk: profile.risk.as_str().to_string(),
            adapter_status: "implemented".to_string(),
        })
        .collect()
}

/// 全部 profile 的原生工具描述符（runtime bootstrap 的全量检索索引用）。
#[must_use]
pub fn all_native_descriptors() -> Vec<CompactToolDescriptor> {
    crate::harness::profile::profiles()
        .iter()
        .flat_map(native_descriptors_for)
        .collect()
}

/// IDA MCP 远程能力的 compact 描述符（capability schema 运行时才发现，
/// 检索层只声明「存在这样一种远程二进制分析能力」；可用性由 Harness
/// 绑定决定）。
#[must_use]
pub fn ida_mcp_descriptor() -> CompactToolDescriptor {
    CompactToolDescriptor {
        tool_id: "ida_mcp".to_string(),
        title: "IDA".to_string(),
        summary:
            "Remote IDA Pro MCP capability: decompile, disassemble and inspect native binaries"
                .to_string(),
        capabilities: vec!["binary_static".to_string(), "binary_analysis".to_string()],
        domain: "binary_static".to_string(),
        availability: ToolCandidateAvailability::Remote,
        risk: "low".to_string(),
        adapter_status: "remote".to_string(),
    }
}

fn availability_of(entry: &ToolCatalogEntry) -> ToolCandidateAvailability {
    if !entry.detection.available {
        return ToolCandidateAvailability::Missing;
    }
    match entry.detection.availability {
        ToolAvailability::Configured => ToolCandidateAvailability::Configured,
        ToolAvailability::Path => ToolCandidateAvailability::Path,
        ToolAvailability::Unknown | ToolAvailability::Missing => ToolCandidateAvailability::Missing,
    }
}

/// 单条索引记录打分（`None` 表示被过滤：风险超限 / 无命中 / 缺失且未点名）。
fn score_tool(
    tool: &IndexedTool,
    query: &ToolRetrievalQuery,
    query_tokens: &[String],
    risk_limit: Option<u8>,
) -> Option<ToolCandidate> {
    let descriptor = &tool.descriptor;
    if let Some(limit) = risk_limit
        && risk_rank(&descriptor.risk).is_some_and(|rank| rank > limit)
    {
        return None;
    }
    let mut relevance = 0.0_f64;
    let mut rationale: Vec<String> = Vec::new();

    // 显式工具加成：query.explicit_tools 列表命中，或查询文本以词
    // 边界点名工具别名（仍需通过 Catalog/policy，不绕过 allow-list）。
    let explicit = query
        .explicit_tools
        .iter()
        .any(|tool_name| tool.explicit_aliases.contains(&tool_name.to_lowercase()))
        || word_contains(&query.text.to_lowercase(), &tool.explicit_aliases);
    if explicit {
        relevance += EXPLICIT_BOOST;
        rationale.push("explicitly requested by user".to_string());
    }

    // 词法命中：名字 > 描述 > 领域（weak）。
    let mut name_score = 0.0_f64;
    let mut description_score = 0.0_f64;
    let mut domain_score = 0.0_f64;
    let mut matched_token: Option<&String> = None;
    for token in query_tokens {
        if !tool.all_tokens.contains(token) {
            continue;
        }
        if tool.name_tokens.contains(token) {
            name_score += 1.5;
        } else if tool.summary_tokens.contains(token) {
            description_score += 0.75;
            if matched_token.is_none() {
                matched_token = Some(token);
            }
        } else {
            domain_score += 0.5;
        }
    }
    name_score = name_score.min(NAME_TOKEN_CAP);
    description_score = description_score.min(DESCRIPTION_TOKEN_CAP);
    domain_score = domain_score.min(DOMAIN_TOKEN_CAP);
    if name_score > 0.0 {
        relevance += name_score;
    }
    if description_score > 0.0 {
        relevance += description_score;
        if let Some(token) = matched_token {
            rationale.push(format!("matched capability: {token}"));
        }
    }
    relevance += domain_score;

    // 工件兼容性（确定性弱信号，绝不是硬前提）。
    let artifact_hit = query
        .artifact_kinds
        .iter()
        .any(|kind| compatible_with_artifact(&descriptor.domain, kind));
    if artifact_hit {
        relevance += ARTIFACT_BOOST;
        if let Some(kind) = query
            .artifact_kinds
            .iter()
            .find(|kind| compatible_with_artifact(&descriptor.domain, kind))
        {
            rationale.push(format!("accepts current artifact: {kind}"));
        }
    }

    // domain hint 弱加成（domain 永远不是硬前提）。
    if query
        .text
        .to_lowercase()
        .contains(&descriptor.domain.replace('_', ""))
    {
        relevance += DOMAIN_HINT_BOOST;
    }

    // 已安装加成只作用于已有语义命中的候选（缺失工具默认排除，
    // 除非用户显式点名——如实反馈，但 Harness 仍不会加载它）。
    if !descriptor.availability.locally_executable() && !explicit {
        return None;
    }
    if relevance <= 0.0 {
        return None;
    }
    if descriptor.availability.locally_executable() {
        relevance += INSTALLED_BOOST;
        rationale.push("installed and healthy".to_string());
    }
    Some(ToolCandidate {
        descriptor: descriptor.clone(),
        relevance,
        rationale,
    })
}

fn risk_class_of(domain: &str) -> &'static str {
    if HIGH_RISK_DOMAINS.contains(&domain) {
        "high"
    } else if MEDIUM_RISK_DOMAINS.contains(&domain) {
        "medium"
    } else {
        "low"
    }
}

fn risk_rank(risk: &str) -> Option<u8> {
    match risk {
        "none" => Some(0),
        "low" => Some(1),
        "medium" => Some(2),
        "high" => Some(3),
        _ => None,
    }
}

fn compatible_with_artifact(domain: &str, kind: &str) -> bool {
    ARTIFACT_DOMAINS
        .iter()
        .filter(|(artifact, _)| *artifact == kind)
        .any(|(_, domains)| domains.contains(&domain))
}

/// 查询文本是否以词边界点名任一工具别名（避免 `nuclei_template` 之类
/// 子串误报）。
fn word_contains(haystack: &str, needles: &[String]) -> bool {
    needles.iter().any(|needle| {
        let needle = needle.trim();
        if needle.is_empty() {
            return false;
        }
        haystack.match_indices(needle).any(|(index, _)| {
            let end = index + needle.len();
            let before_ok = index == 0
                || !haystack[..index]
                    .chars()
                    .next_back()
                    .is_some_and(is_word_char);
            let after_ok =
                end == haystack.len() || !haystack[end..].chars().next().is_some_and(is_word_char);
            before_ok && after_ok
        })
    })
}

fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// 词法归一：小写 + 按非字母数字切分（中文连续段落保持为单 token）。
fn tokenize(text: &str) -> HashSet<String> {
    let mut tokens = HashSet::new();
    let mut current = String::new();
    for character in text.chars() {
        if character.is_alphanumeric() {
            current.extend(character.to_lowercase());
        } else if !current.is_empty() {
            tokens.insert(current.clone());
            current.clear();
        }
    }
    if !current.is_empty() {
        tokens.insert(current);
    }
    tokens
}

#[cfg(test)]
mod tests {
    #![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

    use super::*;
    use crate::tool_catalog::ToolDetection;

    fn entry(id: &str, domain: &str, description: &str, available: bool) -> ToolCatalogEntry {
        ToolCatalogEntry {
            enabled: true,
            id: id.to_string(),
            name: id.to_string(),
            domain: domain.to_string(),
            description: description.to_string(),
            upstream_url: String::new(),
            executable_names: vec![id.to_string()],
            version_args: Vec::new(),
            supported_platforms: Vec::new(),
            output_format: "json".to_string(),
            adapter_status: "implemented".to_string(),
            risk_notes: Vec::new(),
            install: None,
            invocation: None,
            target_flag: Vec::new(),
            output_flags: Vec::new(),
            detection: if available {
                ToolDetection {
                    available: true,
                    availability: ToolAvailability::Path,
                    executable_path: Some(format!("/usr/bin/{id}")),
                    source: None,
                    version: None,
                    sha256: None,
                    source_url: None,
                    integrity_status: "unverified".to_string(),
                    integrity_message: None,
                }
            } else {
                ToolDetection {
                    available: false,
                    availability: ToolAvailability::Missing,
                    executable_path: None,
                    source: None,
                    version: None,
                    sha256: None,
                    source_url: None,
                    integrity_status: "unverified".to_string(),
                    integrity_message: None,
                }
            },
            configured_settings: None,
        }
    }

    fn catalog() -> Vec<ToolCatalogEntry> {
        vec![
            entry(
                "nuclei",
                "web_dast",
                "template based web vulnerability scanner",
                true,
            ),
            entry(
                "ffuf",
                "content_discovery",
                "fast web fuzzer for content discovery",
                true,
            ),
            entry(
                "subfinder",
                "asset_recon",
                "passive subdomain discovery tool",
                true,
            ),
            entry(
                "semgrep",
                "web_sast",
                "static source code analysis scanner",
                true,
            ),
            entry("gdb", "binary_dynamic", "binary debugger", false),
        ]
    }

    fn retriever() -> CatalogToolRetriever {
        CatalogToolRetriever::new(&catalog())
    }

    fn query(text: &str) -> ToolRetrievalQuery {
        ToolRetrievalQuery::from_text(text, 10)
    }

    #[test]
    fn explicit_tool_outranks_lexical_matches() {
        let result = retriever().retrieve(&query("use gdb to inspect the program"));
        assert!(!result.candidates.is_empty());
        assert_eq!(result.candidates[0].tool_id(), "gdb");
        assert!(
            result.candidates[0]
                .rationale
                .iter()
                .any(|reason| reason.contains("explicitly requested"))
        );
        // gdb 本地缺失但被点名 → 如实保留候选（Harness 仍不会加载）。
        assert!(!result.candidates[0].locally_executable());
    }

    #[test]
    fn capability_text_ranks_domain_tools() {
        let result = retriever().retrieve(&query("web vulnerability scanning"));
        let ids: Vec<&str> = result
            .candidates
            .iter()
            .map(ToolCandidate::tool_id)
            .collect();
        assert_eq!(ids.first().copied(), Some("nuclei"));
        assert!(ids.contains(&"ffuf"), "ffuf 描述含 web/fuzzer 词: {ids:?}");
    }

    #[test]
    fn artifact_compatibility_boosts_traffic_tools() {
        let mut query = query("analyze packet capture");
        query.artifact_kinds = vec!["pcap".to_string()];
        let result = retriever().retrieve(&query);
        // catalog 无 traffic 工具 → 空结果是合法降级（非阻塞）。
        assert!(result.candidates.is_empty());
    }

    #[test]
    fn missing_tools_are_hidden_without_explicit_request() {
        let result = retriever().retrieve(&query("debug a binary program"));
        let ids: Vec<&str> = result
            .candidates
            .iter()
            .map(ToolCandidate::tool_id)
            .collect();
        assert!(!ids.contains(&"gdb"), "缺失工具不应出现: {ids:?}");
    }

    #[test]
    fn risk_limit_filters_high_risk_domains() {
        let high = entry(
            "web_exploit_campaign",
            "web_exploit",
            "bounded exploit campaign",
            true,
        );
        let retriever = CatalogToolRetriever::new(&[high]);
        let mut query = query("run the campaign");
        query.risk_limit = Some("low".to_string());
        let result = retriever.retrieve(&query);
        assert!(result.candidates.is_empty());
        query.risk_limit = Some("high".to_string());
        let result = retriever.retrieve(&query);
        assert_eq!(result.candidates.len(), 1);
    }

    #[test]
    fn empty_catalog_degrades_to_empty_candidates() {
        let retriever = CatalogToolRetriever::new(&[]);
        assert!(retriever.retrieve(&query("anything")).candidates.is_empty());
        assert!(retriever.known_tool_names().is_empty());
    }

    #[test]
    fn known_tool_names_cover_ids_and_titles() {
        let names = retriever().known_tool_names();
        assert!(names.contains(&"nuclei".to_string()));
        assert!(names.contains(&"subfinder".to_string()));
    }

    #[test]
    fn extra_descriptors_share_the_same_scoring() {
        let index = ToolRetrievalIndex::from_catalog(&catalog()).with_extra_descriptors(vec![
            CompactToolDescriptor {
                tool_id: "traffic_artifact_import".to_string(),
                title: "Traffic Artifact Import".to_string(),
                summary: "Import HAR Burp Chrome network artifacts into normalized requests"
                    .to_string(),
                capabilities: vec!["traffic_intelligence".to_string()],
                domain: "traffic_intelligence".to_string(),
                availability: ToolCandidateAvailability::Native,
                risk: "low".to_string(),
                adapter_status: "implemented".to_string(),
            },
        ]);
        let mut query = query("analyze packet capture and extract network streams");
        query.artifact_kinds = vec!["pcap".to_string()];
        let result = index.search(&query);
        assert_eq!(result.candidates[0].tool_id(), "traffic_artifact_import");
        assert!(
            result.candidates[0]
                .rationale
                .iter()
                .any(|reason| reason.contains("accepts current artifact: pcap"))
        );
    }

    #[test]
    fn results_are_deterministic_on_equal_scores() {
        let catalog = vec![
            entry("alpha", "web_dast", "scanner", true),
            entry("beta", "web_dast", "scanner", true),
        ];
        let result = CatalogToolRetriever::new(&catalog).retrieve(&query("scanner"));
        assert_eq!(result.candidates.len(), 2);
        assert_eq!(result.candidates[0].tool_id(), "alpha");
        assert_eq!(result.candidates[1].tool_id(), "beta");
        assert!(
            (result.candidates[0].relevance - result.candidates[1].relevance).abs() < f64::EPSILON
        );
    }
}
