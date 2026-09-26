//! Solver bootstrap —— 初始工具集选择的**唯一入口**。
//!
//! 主链（与 Knowledge/Tool 双检索平行的契约一致）：
//!
//! ```text
//! Branch / raw request
//!       ↓
//! 确定性信号（MissionAsset / target / 查询文本 / 已知工具名）
//!       ↓
//! ToolRetrievalQuery
//!       ├──────────────→ KnowledgeRetriever（我应该知道什么？）
//!       ↓
//! ToolRetriever（我能用什么？）→ ToolsetSelector → visible_tool_ids
//!       ↓
//! config 注入（visible_tool_ids / knowledge_hints）
//! ```
//!
//! 职责边界：
//! - bootstrap 只做 discovery 与 config 注入；真正执行授权仍由 Harness
//!   （adapter / allow-list / schema 校验 / 预算 / 风险策略）决定；
//! - 任何一步失败都**绝不阻塞 Mission**——降级为无注入（Harness 保持
//!   静态行为，向后兼容）；
//! - `CapabilityRouter` 仍是 Branch→solver 的 execution dispatch 层；
//!   bootstrap 不替代它，只在其后补充开放世界工具发现；
//! - telemetry 复用 `retrieval_invocations` 表（purpose 区分
//!   `tool_retrieval` / `solver_bootstrap_knowledge`），不落敏感原文。
//!
//! 历史：这里曾有一层 TaskProfiler（任务画像：会话/工具模式分类、
//! fast/strong provider 升级）。分类机制已按产品决策删除——信号提取
//! 只保留确定性部分（资产形态 / 目标键 / 查询文本 / 点名工具），
//! 不再对任务本身做任何模式判定。

use std::time::Instant;

use agents::tool_retrieval::ToolRetriever;
use agents::toolset_selector::ToolsetSelector;
use engines::tool_catalog::detect_tool_catalog_cached;
use engines::tool_catalog::local_tools_config_path;
use engines::tool_retrieval::CatalogToolRetriever;
use engines::tool_retrieval::ToolRetrievalIndex;
use engines::tool_retrieval::all_native_descriptors;
use engines::tool_retrieval::ida_mcp_descriptor;
use models::asset::MissionAssetType;
use models::mission::Branch;
use models::retrieval::RetrievalInvocation;
use models::{RetrievalChunkId, RetrievalSourceKind, RetrievedEvidence};
use models::retrieval::RetrievalStatus;
use models::tool_retrieval::ToolRetrievalQuery;
use models::tool_retrieval::ToolsetSelection;
use models::{AuditRun, Project};
use serde_json::Map;
use serde_json::Value;

use crate::manager::AuditManager;

/// `ToolRetriever` 返回的 compact 候选上限（选择器再收敛到 3~8）。
const CANDIDATE_LIMIT: usize = 20;
/// Knowledge 汇合的紧凑提示上限。
const KNOWLEDGE_LIMIT: i64 = 5;
/// 工具检索 telemetry purpose。
const PURPOSE_TOOL_RETRIEVAL: &str = "tool_retrieval";
/// 知识检索 telemetry purpose。
const PURPOSE_BOOTSTRAP_KNOWLEDGE: &str = "solver_bootstrap_knowledge";
/// telemetry 文本字段的字符上限（不落敏感原文）。
const TELEMETRY_TEXT_CHARS: usize = 400;

/// bootstrap 结果的审计摘要（由 `branch_runtime` 落 branch observation）。
#[derive(Debug, Clone)]
pub(crate) struct BootstrapSummary {
    /// 一句话摘要（observation 文本）。
    pub(crate) text: String,
    /// 结构化细节（observation data）。
    pub(crate) data: Map<String, Value>,
}

/// 文件名后缀 → 工件 kind（查询文本 / 目标值里的后缀信号）。
const EXTENSION_KINDS: &[(&str, &str)] = &[
    (".elf", "elf"),
    (".so", "elf"),
    (".o", "elf"),
    (".bin", "binary"),
    (".exe", "pe"),
    (".dll", "pe"),
    (".pcap", "pcap"),
    (".pcapng", "pcap"),
    (".cap", "pcap"),
    (".har", "traffic"),
    (".zip", "archive"),
    (".tar", "archive"),
    (".gz", "archive"),
    (".7z", "archive"),
    (".rar", "archive"),
    (".apk", "package"),
    (".ipsw", "firmware"),
    (".img", "firmware"),
    (".repo", "source"),
];

/// 资产类别 → 工件 kind（MissionAsset 是持久化的权威工件信号）。
fn asset_artifact_kind(asset_type: MissionAssetType) -> Option<&'static str> {
    match asset_type {
        MissionAssetType::Url | MissionAssetType::Endpoint | MissionAssetType::Api => Some("url"),
        MissionAssetType::Domain => Some("domain"),
        MissionAssetType::Host | MissionAssetType::Ip | MissionAssetType::Service => Some("host"),
        MissionAssetType::Repository | MissionAssetType::SourcePath => Some("source"),
        MissionAssetType::Binary => Some("binary"),
        MissionAssetType::TrafficCapture => Some("pcap"),
        MissionAssetType::Package => Some("package"),
        MissionAssetType::Container => Some("container"),
        MissionAssetType::CloudResource => Some("cloud"),
        MissionAssetType::Unknown => Some("unknown"),
        MissionAssetType::Secret | MissionAssetType::Credential | MissionAssetType::Account => None,
    }
}

/// 查询文本里出现过的后缀 → kinds（`binary` 太泛，不参与检索加成）。
fn extension_kinds_in_text(lower_query: &str) -> Vec<&'static str> {
    EXTENSION_KINDS
        .iter()
        .filter(|(extension, _)| lower_query.contains(extension))
        .map(|(_, kind)| *kind)
        .filter(|kind| !matches!(*kind, "binary"))
        .collect()
}

/// 值是否像 URL（`scheme://` 或以 `www.` / `http` 开头）。
fn looks_like_url(lower_value: &str) -> bool {
    lower_value.contains("://")
        || lower_value.starts_with("www.")
        || lower_value.starts_with("http")
}

/// 词边界包含匹配（避免 `ida` 命中 `idea` 这类子串误报）。
fn word_contains(haystack: &str, needle: &str) -> bool {
    let mut start = 0;
    let chars: Vec<char> = haystack.chars().collect();
    let needle_chars: Vec<char> = needle.chars().collect();
    if needle_chars.is_empty() || chars.len() < needle_chars.len() {
        return false;
    }
    while start + needle_chars.len() <= chars.len() {
        if chars[start..start + needle_chars.len()] == needle_chars[..] {
            let before_ok = start == 0 || !is_word_char(chars[start - 1]);
            let end = start + needle_chars.len();
            let after_ok = end == chars.len() || !is_word_char(chars[end]);
            if before_ok && after_ok {
                return true;
            }
        }
        start += 1;
    }
    false
}

fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// 去重 push（保序）。
fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

/// 从分支与持久化信号拼装确定性检索查询。
///
/// 输入只有形状信号（资产类别 / 目标键形态 / 查询文本 / 点名工具），
/// 没有任何任务模式分类。
fn bootstrap_query(
    project: &Project,
    branch: &Branch,
    assets: &[models::MissionAsset],
    known_tool_names: &[String],
) -> (ToolRetrievalQuery, Vec<String>) {
    let raw_query = if branch.hypothesis.trim().is_empty() {
        branch.title.clone()
    } else {
        format!("{} — {}", branch.title, branch.hypothesis)
    };
    let lower_query = raw_query.to_lowercase();

    let mut artifact_kinds: Vec<String> = Vec::new();
    // MissionAsset 是持久化的权威工件信号。
    for asset in assets {
        if let Some(kind) = asset_artifact_kind(asset.asset_type.clone()) {
            push_unique(&mut artifact_kinds, kind.to_string());
        }
    }
    // 目标键里的 URL / 后缀信号。
    for (_key, value) in project.target.iter() {
        let lower = value.to_lowercase();
        if looks_like_url(&lower) {
            push_unique(&mut artifact_kinds, "url".to_string());
        } else if let Some(kind) = EXTENSION_KINDS
            .iter()
            .find(|(extension, _)| lower.ends_with(extension))
            .map(|(_, kind)| *kind)
        {
            push_unique(&mut artifact_kinds, kind.to_string());
        }
    }
    // 查询文本里的 URL / 文件名后缀。
    if looks_like_url(&lower_query) {
        push_unique(&mut artifact_kinds, "url".to_string());
    }
    for kind in extension_kinds_in_text(&lower_query) {
        push_unique(&mut artifact_kinds, kind.to_string());
    }

    // 显式工具：查询里出现已知工具名（词边界匹配）。
    let mut explicit_tools: Vec<String> = Vec::new();
    for name in known_tool_names {
        let lower_name = name.to_lowercase();
        if !lower_name.is_empty() && word_contains(&lower_query, &lower_name) {
            push_unique(&mut explicit_tools, name.clone());
        }
    }

    let query = ToolRetrievalQuery {
        text: raw_query,
        capability_queries: Vec::new(),
        artifact_kinds,
        explicit_tools: explicit_tools.clone(),
        // 画像风险是任务属性而非工具授权；候选集不被风险上限饿减，
        // 执行授权由 Harness 风险策略最终把关。
        risk_limit: None,
        limit: CANDIDATE_LIMIT,
    };
    (query, explicit_tools)
}

impl AuditManager {
    /// Solver bootstrap：确定性信号 → 平行 Knowledge/Tool 检索 → 初始
    /// visible toolset → config 注入。
    ///
    /// 这是初始工具集选择的唯一入口；Branch Generator / `StrategyBoard` /
    /// Runtime 其他位置不得各自再做工具选择。失败一律降级为无注入。
    pub(crate) async fn solver_bootstrap(
        &self,
        project: &Project,
        run: &AuditRun,
        branch: &Branch,
        config: &mut Map<String, Value>,
    ) -> Option<BootstrapSummary> {
        // 1. 装配检索索引（catalog + 全部 profile 原生工具 + IDA 远程能力
        //    描述符）。catalog 检测失败 → 无注入降级（不阻塞 Mission）。
        let catalog = detect_tool_catalog_cached(&local_tools_config_path())
            .await
            .ok()?;
        let mut extra = all_native_descriptors();
        extra.push(ida_mcp_descriptor());
        let index = ToolRetrievalIndex::from_catalog(&catalog).with_extra_descriptors(extra);
        let retriever = CatalogToolRetriever::from_index(index);
        let known_tool_names = retriever.known_tool_names();

        // 2. 确定性信号（MissionAsset / target / 查询文本 / 已知工具名）
        //    → 检索查询。无任何任务模式分类。
        let assets = self
            .repository()
            .list_mission_assets(
                Some(branch.mission_id.as_str()),
                Some(project.id.as_str()),
                None,
                None,
            )
            .unwrap_or_default();
        let (query, explicit_tools) =
            bootstrap_query(project, branch, &assets, &known_tool_names);

        // 3. 平行检索：Knowledge（我应该知道什么）与 Tools（我能用什么）
        //    职责正交，互不调用。
        let knowledge_hints = self.bootstrap_knowledge(project, run, &query.text);
        let retrieval = retriever.retrieve(&query);

        // 4. 初始 visible toolset 选择（确定性 fast path + 模型辅助路径）。
        let selection = ToolsetSelector::new()
            .select(
                &explicit_tools,
                &retrieval.candidates,
                self.provider_runtime().as_ref(),
                self.resolve_provider_id(&run.config)
                    .ok()
                    .flatten()
                    .as_deref(),
            )
            .await;
        self.record_tool_retrieval_telemetry(
            project,
            run,
            &query,
            &retrieval.candidates,
            &selection,
        );

        // 5. config 注入（compact 投影；完整 schema 由 Harness 按选择加载）。
        inject_bootstrap_config(config, &selection, &knowledge_hints);
        Some(bootstrap_summary(&retrieval, &selection))
    }

    /// `KnowledgeRetriever` 侧平行检索：查询文本 → 知识查询 → 紧凑提示。
    /// 失败/空库一律降级为空（不阻塞 Mission）。
    fn bootstrap_knowledge(
        &self,
        project: &Project,
        run: &AuditRun,
        query_text: &str,
    ) -> Vec<String> {
        let mut query = models::KnowledgeRetrievalQuery::new();
        query.text = query_text.to_string();
        query.limit = KNOWLEDGE_LIMIT;
        let started = Instant::now();
        let results = self
            .repository()
            .search_knowledge_cards(&query)
            .unwrap_or_default();
        let hints: Vec<String> = results
            .iter()
            .map(|result| {
                let summary: String = result.card.summary.chars().take(160).collect();
                if summary.is_empty() {
                    result.card.title.clone()
                } else {
                    format!("{}: {}", result.card.title, summary)
                }
            })
            .collect();
        // 知识检索 telemetry（目的与工具检索平行，审计可查）。
        let mut invocation =
            RetrievalInvocation::new(project.id.clone(), truncate_telemetry(&query.text));
        invocation.run_id = Some(run.id.clone());
        invocation.purpose = PURPOSE_BOOTSTRAP_KNOWLEDGE.to_string();
        invocation.top_k = KNOWLEDGE_LIMIT;
        invocation.status = if hints.is_empty() {
            RetrievalStatus::Empty
        } else {
            RetrievalStatus::Hit
        };
        invocation.candidate_count = i64::try_from(results.len()).unwrap_or(i64::MAX);
        invocation.filtered_count = invocation.candidate_count;
        invocation.max_score = results.first().map_or(0.0, |result| result.score);
        invocation.duration_ms =
            Some(i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX));
        // 命中列表必须真的落库：此前这个字段从来没被赋值，恒为 `[]`，于是一条
        // `status=hit`、`candidate_count=5`、`max_score=76.5` 的记录看上去像
        // "检索什么都没返回"，直接导致"知识库没被调用"的误判。
        invocation.retrieved = retrieved_from_results(&results);
        let _ = self.repository().add_retrieval_invocation(&invocation);
        hints
    }

    /// 工具检索 telemetry：query / 候选与选择 id / 选择方式 / 分数 /
    /// 归因（不落敏感 prompt）。
    fn record_tool_retrieval_telemetry(
        &self,
        project: &Project,
        run: &AuditRun,
        query: &ToolRetrievalQuery,
        candidates: &[models::ToolCandidate],
        selection: &ToolsetSelection,
    ) {
        let mut invocation = RetrievalInvocation::new(
            project.id.clone(),
            truncate_telemetry(&query.retrieval_text()),
        );
        invocation.run_id = Some(run.id.clone());
        invocation.purpose = PURPOSE_TOOL_RETRIEVAL.to_string();
        invocation.top_k = i64::try_from(query.limit).unwrap_or(i64::MAX);
        invocation.status = if candidates.is_empty() {
            RetrievalStatus::Empty
        } else {
            RetrievalStatus::Hit
        };
        invocation.candidate_count = i64::try_from(candidates.len()).unwrap_or(i64::MAX);
        invocation.filtered_count = invocation.candidate_count;
        invocation.max_score = candidates
            .first()
            .map_or(0.0, |candidate| candidate.relevance);
        invocation.reason = serde_json::to_string(&serde_json::json!({
            "candidate_tool_ids": candidates
                .iter()
                .map(models::ToolCandidate::tool_id)
                .collect::<Vec<_>>(),
            "selected_tool_ids": selection.selected_tool_ids,
            "selection_method": selection.method.as_str(),
            "scores": candidates
                .iter()
                .map(|candidate| (candidate.tool_id(), candidate.relevance))
                .collect::<Vec<_>>(),
            "rationale": selection.rationale,
        }))
        .unwrap_or_default();
        let _ = self.repository().add_retrieval_invocation(&invocation);
    }
}

/// config 注入（compact 投影；完整 schema 由 Harness 按选择加载）。
/// 空选择/空提示不注入对应键——收紧语义只对非空集合生效。
/// 把知识检索命中投影为 telemetry 的 `retrieved` 列表。
///
/// 这个字段曾经从来没被赋值，恒为 `[]`：于是 `status=hit`、
/// `candidate_count=5`、`max_score=76.5` 的记录看上去像"检索什么都没返回"，
/// 直接让人误判成"知识库没被调用"。rank 从 1 起，与命中顺序一致。
fn retrieved_from_results(
    results: &[models::knowledge::KnowledgeRetrievalResult],
) -> Vec<RetrievedEvidence> {
    results
        .iter()
        .enumerate()
        .map(|(index, result)| RetrievedEvidence {
            chunk_id: RetrievalChunkId::new(format!("knowledge:{}", result.card.id.as_str())),
            source_kind: RetrievalSourceKind::KnowledgeCard,
            source_id: result.card.id.as_str().to_string(),
            score: result.score,
            rank: i64::try_from(index).unwrap_or(i64::MAX).saturating_add(1),
            snippet: result.card.effective_summary().chars().take(600).collect(),
            title: result.card.title.clone(),
            artifact_record_id: None,
            artifact_uri: None,
            artifact_sha256: None,
            evidence_id: None,
            finding_id: None,
            tool_invocation_id: None,
            model_invocation_id: None,
            location: Map::new(),
            metadata: Map::new(),
        })
        .collect()
}

fn inject_bootstrap_config(
    config: &mut Map<String, Value>,
    selection: &ToolsetSelection,
    knowledge_hints: &[String],
) {
    if !selection.selected_tool_ids.is_empty() {
        config.insert(
            models::CONFIG_VISIBLE_TOOL_IDS.to_string(),
            Value::Array(
                selection
                    .selected_tool_ids
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    if !knowledge_hints.is_empty() {
        config.insert(
            models::CONFIG_KNOWLEDGE_HINTS.to_string(),
            Value::Array(knowledge_hints.iter().cloned().map(Value::String).collect()),
        );
    }
}

/// bootstrap 审计摘要（observation 文本 + 结构化细节）。
fn bootstrap_summary(
    retrieval: &models::tool_retrieval::ToolRetrievalResult,
    selection: &ToolsetSelection,
) -> BootstrapSummary {
    BootstrapSummary {
        text: format!(
            "Solver bootstrap: {} candidate(s) → {} visible tool(s) [{}]",
            retrieval.candidates.len(),
            selection.selected_tool_ids.len(),
            selection.selected_tool_ids.join(", ")
        ),
        data: Map::from_iter([
            (
                "selection_method".to_string(),
                Value::from(selection.method.as_str()),
            ),
            (
                "selected_tool_ids".to_string(),
                serde_json::to_value(&selection.selected_tool_ids).unwrap_or(Value::Null),
            ),
        ]),
    }
}

/// telemetry 文本截断（不落超长/敏感原文）。
fn truncate_telemetry(text: &str) -> String {
    text.chars().take(TELEMETRY_TEXT_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::knowledge::{KnowledgeCard, KnowledgeRetrievalResult};

    fn card(id: &str, title: &str, summary: &str) -> KnowledgeCard {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "kind": "vulnerability_pattern",
            "title": title,
            "summary": summary,
        }))
        .expect("card json must be valid")
    }

    fn result(card: KnowledgeCard, score: f64) -> KnowledgeRetrievalResult {
        KnowledgeRetrievalResult {
            card,
            score,
            matched_terms: Vec::new(),
            retrieval_reason: None,
        }
    }

    /// 回归：`RetrievalInvocation::retrieved` 曾经从来没被赋值，恒为 `[]`，
    /// 于是一条 `status=hit` / `candidate_count=5` / `max_score=76.5` 的记录
    /// 看上去像"检索什么都没返回"，直接让人误判成"知识库没被调用"。
    #[test]
    fn retrieved_from_results_projects_every_hit_with_one_based_rank() {
        let results = vec![
            result(card("kcard_a", "Session Fixation", "Login rotation hygiene"), 76.5),
            result(card("kcard_b", "Token Fixation", "Source precedence"), 59.4),
        ];

        let retrieved = retrieved_from_results(&results);

        assert_eq!(retrieved.len(), 2, "every hit must be projected");
        assert_eq!(retrieved[0].rank, 1, "rank starts at 1");
        assert_eq!(retrieved[1].rank, 2);
        assert_eq!(retrieved[0].source_id, "kcard_a");
        assert_eq!(retrieved[0].score, 76.5);
        assert_eq!(retrieved[0].title, "Session Fixation");
        assert_eq!(retrieved[0].snippet, "Login rotation hygiene");
        assert_eq!(
            retrieved[0].chunk_id.as_str(),
            "knowledge:kcard_a",
            "chunk id is namespaced so it never collides with other source kinds"
        );
        assert!(matches!(
            retrieved[0].source_kind,
            RetrievalSourceKind::KnowledgeCard
        ));
    }

    #[test]
    fn retrieved_from_results_empty_stays_empty() {
        assert!(
            retrieved_from_results(&[]).is_empty(),
            "no hits must stay an empty list, not a placeholder entry"
        );
    }
}
