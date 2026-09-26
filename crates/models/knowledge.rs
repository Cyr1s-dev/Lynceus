//! `KnowledgeCard` —— `server/core/models/knowledge.py` 的移植 + Retrieval
//! Substrate 扩展。
//!
//! 面向策略与规划的紧凑检索记录，存储在 Facts/Evidence 真值之外的小型
//! 持久知识单元。在原扁平卡之上演进为可分层的 `KnowledgeUnit` 语义：
//!
//! * `summary` 负责注入（context budget 侧约束），`body` 存完整知识
//!   ——**限制注入，不限制存储**；
//! * `parent_id` 支持 Tool→Command、Payload→子步骤的父子粒度；
//! * `tool/technique/platform/protocol` 是结构化过滤字段（ingestion 期
//!   也会被收编进 `search_terms`）；
//! * `source`（`source_id`）+ `source_locator` + `content_hash` 构成知识
//!   provenance；知识不等于 Evidence，不得进入 Finding 引用链。

use serde::Deserialize;
use serde::Serialize;

use crate::common::Timestamp;
use crate::common::new_id;
use crate::ids::KnowledgeCardId;

/// 注入用 `summary` 的软上限（字符）。写库校验用；context packing 的实际
/// 预算由检索侧选项控制，不写死在存储层。
pub const MAX_SUMMARY_CHARS: usize = 2_000;
/// 完整知识 `body` 的存储上限（字符）。防失控导入，不是注入预算。
pub const MAX_BODY_CHARS: usize = 512_000;
/// 每个词条类字段（aliases/tool/technique/platform/protocol/prerequisites/
/// `tags` / `search_terms`）的最大条数。
pub const MAX_STRUCTURED_TERMS: usize = 32;
/// `source_locator` 上限（字符）。
pub const MAX_SOURCE_LOCATOR_CHARS: usize = 300;

/// Lynceus 可检索进提示词或规划的知识类型（`KnowledgeCardKind`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeCardKind {
    /// 工具用法。
    #[default]
    ToolUsage,
    /// 漏洞模式。
    VulnerabilityPattern,
    /// 案例参考。
    CaseReference,
    /// Payload 策略。
    PayloadStrategy,
    /// 误报模式。
    FalsePositivePattern,
    /// 修复模式。
    RemediationPattern,
    /// 云攻击路径。
    CloudAttackPath,
    /// 二进制模式。
    BinaryPattern,
}

fn default_card_id() -> KnowledgeCardId {
    KnowledgeCardId::new(new_id("kcard"))
}

fn default_card_priority() -> i64 {
    50
}

/// 知识卡语料/索引状态（`empty → ready → stale`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeCorpusState {
    /// 无知识卡，索引为空。
    Empty,
    /// 索引与语料一致，可直接检索。
    Ready,
    /// 语料相对索引已变化（新增/更新/删除未同步），需 index-sync。
    Stale,
}

/// 知识语料与 FTS 索引的整体状态（`Knowledge Corpus State`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeCorpusStatus {
    /// 当前状态。
    pub state: KnowledgeCorpusState,
    /// 知识卡总数。
    pub card_count: i64,
    /// FTS 索引行数。
    pub indexed_count: i64,
    /// 上次全量同步时间（ISO-8601；从未同步为 `None`）。
    pub last_synced_at: Option<Timestamp>,
    /// 状态说明（人读，deterministic 生成）。
    pub reason: String,
}

/// 存储在事实/证据真值之外的小型持久知识单元（`KnowledgeCard`）。
///
/// 兼容约定：`content` 是 `summary` 的历史镜像字段——写入路径恒等维护
/// `content == summary`，老读者（旧 frontend / 旧测试）无需感知拆分。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeCard {
    /// 卡片标识符。
    #[serde(default = "default_card_id")]
    pub id: KnowledgeCardId,
    /// 知识类型。
    pub kind: KnowledgeCardKind,
    /// 标题（Python 侧约束 `[1, 160]`）。
    pub title: String,
    /// 注入用摘要（写入路径校验 `≤ MAX_SUMMARY_CHARS`；老数据默认空串）。
    #[serde(default)]
    pub summary: String,
    /// 完整知识正文（命令、步骤、OPSEC、攻击链；`≤ MAX_BODY_CHARS`）。
    #[serde(default)]
    pub body: String,
    /// 历史字段：恒等于 `summary` 的兼容镜像，新代码勿再消费。
    #[serde(default)]
    pub content: String,
    /// 父知识单元（Tool Summary Unit ← Command Unit）。
    #[serde(default)]
    pub parent_id: Option<KnowledgeCardId>,
    /// 检索别名（同义词/缩写/双语词条，ingestion 期可由注册表补齐）。
    #[serde(default)]
    pub aliases: Vec<String>,
    /// 检索标签（Python 侧约束 ≤16）。
    #[serde(default)]
    pub tags: Vec<String>,
    /// 关联工具名（结构化过滤，如 `sqlmap`）。
    #[serde(default)]
    pub tool: Vec<String>,
    /// 关联技术/战术（结构化过滤，如 `sqli`、`privesc`）。
    #[serde(default)]
    pub technique: Vec<String>,
    /// 适用平台（结构化过滤，如 `windows`/`linux`/`web`）。
    #[serde(default)]
    pub platform: Vec<String>,
    /// 涉及协议（结构化过滤，如 `http`/`smb`/`rdp`）。
    #[serde(default)]
    pub protocol: Vec<String>,
    /// 前置条件（执行该知识需要的工具/权限/访问面）。
    #[serde(default)]
    pub prerequisites: Vec<String>,
    /// 来源（Python 侧约束 ≤300；provenance 的 `source_id`）。
    #[serde(default)]
    pub source: Option<String>,
    /// 来源内定位（源文件路径 + 条目 id 等；≤300）。
    #[serde(default)]
    pub source_locator: Option<String>,
    /// `body`（优先）或 `summary` 的 SHA-256（hex）。
    #[serde(default)]
    pub content_hash: Option<String>,
    /// ingestion 期生成的归一化检索词（含 CJK 切词与别名展开）。
    #[serde(default)]
    pub search_terms: Vec<String>,
    /// 优先级（`[0, 100]`，默认 50）。
    #[serde(default = "default_card_priority")]
    pub priority: i64,
    /// 向量索引版本（Phase 2 预留；`None` = 未向量化）。
    #[serde(default)]
    pub embedding_version: Option<i64>,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
    /// 更新时间。
    #[serde(default = "crate::common::utcnow")]
    pub updated_at: Timestamp,
}

impl KnowledgeCard {
    /// 注入用摘要（`summary` 优先，缺省回退 legacy `content`）。
    #[must_use]
    pub fn effective_summary(&self) -> &str {
        if self.summary.is_empty() {
            self.content.as_str()
        } else {
            self.summary.as_str()
        }
    }

    /// provenance 引用串（`source#locator` 风格；均空返回 `None`）。
    #[must_use]
    pub fn provenance_label(&self) -> Option<String> {
        match (self.source.as_deref(), self.source_locator.as_deref()) {
            (None, None) => None,
            (Some(source), None) => Some(source.to_string()),
            (source, locator) => Some(format!(
                "{}#{}",
                source.unwrap_or(""),
                locator.unwrap_or("")
            )),
        }
    }
}

/// 知识单元写入草稿（manager/importer 的完整字段入口；字段校验在
/// manager 层执行，与 Python 契约一致的边界集中在一处）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct KnowledgeCardDraft {
    /// 预置 id（importer 的确定性 id）；`None` = 生成 `kcard_` 随机 id。
    pub id: Option<KnowledgeCardId>,
    /// 知识类型。
    pub kind: KnowledgeCardKind,
    /// 标题。
    pub title: String,
    /// 注入用摘要。
    pub summary: String,
    /// 完整正文。
    pub body: String,
    /// 父单元。
    pub parent_id: Option<KnowledgeCardId>,
    /// 检索别名。
    pub aliases: Vec<String>,
    /// 检索标签。
    pub tags: Vec<String>,
    /// 工具名。
    pub tool: Vec<String>,
    /// 技术/战术。
    pub technique: Vec<String>,
    /// 平台。
    pub platform: Vec<String>,
    /// 协议。
    pub protocol: Vec<String>,
    /// 前置条件。
    pub prerequisites: Vec<String>,
    /// 来源（`source_id`）。
    pub source: Option<String>,
    /// 来源内定位。
    pub source_locator: Option<String>,
    /// 优先级。
    pub priority: i64,
}

fn default_retrieval_limit() -> i64 {
    10
}

/// 知识卡检索查询（`KnowledgeRetrievalQuery`）。
///
/// `text` 走归一化 + FTS；`kinds/tags/tool/technique/platform/protocol`
/// 是结构化过滤，空集合 = 不限。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeRetrievalQuery {
    /// 查询文本（空串 = 只按过滤条件检索）。
    #[serde(default)]
    pub text: String,
    /// 限定知识类型（空 = 不限）。
    #[serde(default)]
    pub kinds: Vec<KnowledgeCardKind>,
    /// 限定标签。
    #[serde(default)]
    pub tags: Vec<String>,
    /// 限定工具名。
    #[serde(default)]
    pub tools: Vec<String>,
    /// 限定技术条目。
    #[serde(default)]
    pub techniques: Vec<String>,
    /// 限定平台。
    #[serde(default)]
    pub platforms: Vec<String>,
    /// 限定协议。
    #[serde(default)]
    pub protocols: Vec<String>,
    /// 返回条数上限（Python 侧约束 `[1, 50]`）。
    #[serde(default = "default_retrieval_limit")]
    pub limit: i64,
}

impl KnowledgeRetrievalQuery {
    /// 以 Python 默认值构造（`KnowledgeRetrievalQuery()`）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            text: String::new(),
            kinds: Vec::new(),
            tags: Vec::new(),
            tools: Vec::new(),
            techniques: Vec::new(),
            platforms: Vec::new(),
            protocols: Vec::new(),
            limit: default_retrieval_limit(),
        }
    }
}

impl Default for KnowledgeRetrievalQuery {
    fn default() -> Self {
        Self::new()
    }
}

/// 一张被检索到的卡片及其确定性得分（`KnowledgeRetrievalResult`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeRetrievalResult {
    /// 命中的卡片。
    pub card: KnowledgeCard,
    /// 得分（BM25 转正后，越高越好）。
    pub score: f64,
    /// 命中的词项。
    #[serde(default)]
    pub matched_terms: Vec<String>,
    /// 检索原因（deterministic 分类，供调试 / 评测 / mission trace）。
    #[serde(default)]
    pub retrieval_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::utcnow;
    use crate::testutil::assert_wire_values;

    fn minimal_card_json() -> &'static str {
        // 旧版 wire 形态：只有老字段。
        r#"{"kind":"tool_usage","title":"t","content":"c"}"#
    }

    #[test]
    fn knowledge_card_kind_matches_python_wire_values() {
        assert_wire_values(&[
            (KnowledgeCardKind::ToolUsage, "tool_usage"),
            (
                KnowledgeCardKind::VulnerabilityPattern,
                "vulnerability_pattern",
            ),
            (KnowledgeCardKind::CaseReference, "case_reference"),
            (KnowledgeCardKind::PayloadStrategy, "payload_strategy"),
            (
                KnowledgeCardKind::FalsePositivePattern,
                "false_positive_pattern",
            ),
            (KnowledgeCardKind::RemediationPattern, "remediation_pattern"),
            (KnowledgeCardKind::CloudAttackPath, "cloud_attack_path"),
            (KnowledgeCardKind::BinaryPattern, "binary_pattern"),
        ]);
    }

    #[test]
    fn legacy_payload_parses_with_defaults() {
        let card: KnowledgeCard =
            serde_json::from_str(minimal_card_json()).expect("旧 wire 必须可解析");
        assert_eq!(card.title, "t");
        assert_eq!(card.content, "c");
        assert_eq!(card.effective_summary(), "c", "summary 缺省回退 content");
        assert!(card.body.is_empty());
        assert!(card.parent_id.is_none());
        assert!(card.search_terms.is_empty());
        assert_eq!(card.embedding_version, None);
        assert_eq!(card.priority, 50);
    }

    #[test]
    fn knowledge_card_roundtrips() {
        let card = KnowledgeCard {
            id: default_card_id(),
            kind: KnowledgeCardKind::CaseReference,
            title: "t".to_string(),
            summary: "s".to_string(),
            body: "b".to_string(),
            content: "s".to_string(),
            parent_id: None,
            aliases: vec!["sqli".to_string()],
            tags: Vec::new(),
            tool: vec!["sqlmap".to_string()],
            technique: vec!["sqli".to_string()],
            platform: vec!["web".to_string()],
            protocol: vec!["http".to_string()],
            prerequisites: Vec::new(),
            source: Some("security-wiki".to_string()),
            source_locator: Some("webPayloads.json#sqli-mysql-basic".to_string()),
            content_hash: Some("deadbeef".to_string()),
            search_terms: vec!["sql".to_string()],
            priority: 50,
            embedding_version: None,
            created_at: utcnow(),
            updated_at: utcnow(),
        };
        let json = serde_json::to_string(&card).unwrap();
        let back: KnowledgeCard = serde_json::from_str(&json).unwrap();
        assert_eq!(back, card);
        assert_eq!(
            back.provenance_label().as_deref(),
            Some("security-wiki#webPayloads.json#sqli-mysql-basic")
        );
    }

    #[test]
    fn retrieval_query_defaults_match_python() {
        let query = KnowledgeRetrievalQuery::new();
        assert_eq!(query.limit, 10);
        assert!(query.text.is_empty());
        assert!(query.kinds.is_empty() && query.tools.is_empty() && query.platforms.is_empty());
    }

    #[test]
    fn provenance_label_variants() {
        let mut card: KnowledgeCard =
            serde_json::from_str(minimal_card_json()).expect("旧 wire 必须可解析");
        assert_eq!(card.provenance_label(), None);
        card.source = Some("github".to_string());
        assert_eq!(card.provenance_label().as_deref(), Some("github"));
        card.source_locator = Some("README#L12".to_string());
        assert_eq!(
            card.provenance_label().as_deref(),
            Some("github#README#L12")
        );
    }
}
