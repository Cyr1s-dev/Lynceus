//! Intelligence Hub 领域模型：外部 / 被动 / OSINT 数据源产生的线索。
//!
//! 三类概念严格分离（违反即返工）：
//!
//! - **Evidence**：Lynceus `ToolInvocation` 真实执行产生的证据
//!   （`crate::evidence`），是 Finding 的唯一支撑。
//! - **Intelligence**（本模块）：外部数据源（CT、pDNS、Wayback、FOFA…）
//!   的线索，只有 provenance，**永远不能**直接成为 Finding Evidence。
//! - **Knowledge**：可复用的策略 / 工具 / 漏洞知识
//!   （`crate::knowledge`）。
//!
//! 数据流（Seed != Asset：资产发现是关系展开，不是点枚举）：
//!
//! ```text
//! Seed → Multi-source query → IntelRawRecord（原始留痕）
//!      → normalize → IntelEntity（canonicalize 后合并去重）
//!      → IntelRelation（带 source provenance）→ Attack Surface Graph
//!      → candidate →（确定性高置信才自动）promote → Asset
//! ```

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::common::Timestamp;
use crate::common::new_id;
use crate::common::utcnow;

// ---------------------------------------------------------------------------
// 查询
// ---------------------------------------------------------------------------

/// 情报查询类型（seed 的语义解释）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntelQueryType {
    /// 域名（example.com）。
    Domain,
    /// IP 地址。
    Ip,
    /// URL。
    Url,
    /// 证书（SHA-256 指纹 / 序列号）。
    Certificate,
    /// 组织名。
    Organization,
    /// 自由关键词（source 各自解释）。
    Keyword,
}

impl IntelQueryType {
    /// wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            IntelQueryType::Domain => "domain",
            IntelQueryType::Ip => "ip",
            IntelQueryType::Url => "url",
            IntelQueryType::Certificate => "certificate",
            IntelQueryType::Organization => "organization",
            IntelQueryType::Keyword => "keyword",
        }
    }
}

/// 一次情报查询。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntelQuery {
    /// 种子值（域名 / IP / URL / 指纹 / 组织…）。
    pub seed: String,
    /// 查询类型。
    pub query_type: IntelQueryType,
    /// 可选 source 白名单；空表示执行所有支持该查询类型的 source。
    #[serde(default)]
    pub source_ids: Vec<String>,
    /// source 特定过滤参数（原样透传，键由 source capabilities 声明）。
    #[serde(default)]
    pub filters: Map<String, Value>,
    /// 单 source 返回上限。
    #[serde(default = "default_query_limit")]
    pub limit: u32,
    /// 可选关联上下文（仅记录，不影响查询）。
    #[serde(default)]
    pub project_id: Option<String>,
    /// 可选关联上下文（仅记录，不影响查询）。
    #[serde(default)]
    pub mission_id: Option<String>,
}

fn default_query_limit() -> u32 {
    100
}

impl IntelQuery {
    /// 构造查询。
    #[must_use]
    pub fn new(seed: impl Into<String>, query_type: IntelQueryType) -> Self {
        Self {
            seed: seed.into(),
            query_type,
            source_ids: Vec::new(),
            filters: Map::new(),
            limit: default_query_limit(),
            project_id: None,
            mission_id: None,
        }
    }
}

/// 不含 source 原始 payload 的 provenance 摘要。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntelProvenance {
    /// 内部原始记录 ID。
    pub raw_record_id: String,
    /// 数据源 ID。
    pub source: String,
    /// source 自身的记录 ID。
    pub source_record_id: String,
    /// 产生该记录的查询快照。
    pub query: IntelQuery,
    /// 拉取时间。
    pub fetched_at: Timestamp,
}

impl From<&IntelRawRecord> for IntelProvenance {
    fn from(record: &IntelRawRecord) -> Self {
        Self {
            raw_record_id: record.id.clone(),
            source: record.source.clone(),
            source_record_id: record.source_record_id.clone(),
            query: record.query.clone(),
            fetched_at: record.fetched_at,
        }
    }
}

// ---------------------------------------------------------------------------
// 原始记录（Raw 留痕：外部 API 原始结果不是可信 Asset）
// ---------------------------------------------------------------------------

/// 单条外部原始记录：保留 source 返回的原始 payload，一切实体/关系
/// 抽取都必须能回溯到这里（provenance 链的起点）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntelRawRecord {
    /// 记录 ID（`raw_<hex>`）。
    pub id: String,
    /// 数据源 ID（`crtsh` / `wayback` / `fofa`…）。
    pub source: String,
    /// source 内部的记录 ID（无则空字符串）。
    #[serde(default)]
    pub source_record_id: String,
    /// 产生本记录的查询快照。
    pub query: IntelQuery,
    /// 原始 payload（未归一化的 source 原生 JSON）。
    pub raw_payload: Value,
    /// 拉取时间。
    pub fetched_at: Timestamp,
}

impl IntelRawRecord {
    /// 构造原始记录。
    #[must_use]
    pub fn new(
        source: impl Into<String>,
        source_record_id: impl Into<String>,
        query: IntelQuery,
        raw_payload: Value,
    ) -> Self {
        Self {
            id: new_id("raw"),
            source: source.into(),
            source_record_id: source_record_id.into(),
            query,
            raw_payload,
            fetched_at: utcnow(),
        }
    }
}

// ---------------------------------------------------------------------------
// 实体
// ---------------------------------------------------------------------------

/// 情报实体类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntelEntityKind {
    /// 域名。
    Domain,
    /// IP。
    Ip,
    /// URL。
    Url,
    /// 服务（host:port[/proto]）。
    Service,
    /// 证书。
    Certificate,
    /// 技术栈。
    Technology,
    /// 指纹（favicon / cert / banner hash）。
    Fingerprint,
    /// 组织。
    Organization,
    /// 代码仓库。
    Repository,
    /// 应用。
    Application,
}

impl IntelEntityKind {
    /// wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            IntelEntityKind::Domain => "domain",
            IntelEntityKind::Ip => "ip",
            IntelEntityKind::Url => "url",
            IntelEntityKind::Service => "service",
            IntelEntityKind::Certificate => "certificate",
            IntelEntityKind::Technology => "technology",
            IntelEntityKind::Fingerprint => "fingerprint",
            IntelEntityKind::Organization => "organization",
            IntelEntityKind::Repository => "repository",
            IntelEntityKind::Application => "application",
        }
    }
}

/// 确定性置信度（**不由 LLM 生成**）：规则见
/// `crates/intelligence/src/pipeline.rs` 的 confidence 章节。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntelConfidence {
    /// 单源间接线索。
    Weak,
    /// 单源直接记录。
    Medium,
    /// 多源交叉或直接关系证据。
    High,
    /// 高置信确定性来源（如 CT 日志证书本身）。
    Confirmed,
}

impl IntelConfidence {
    /// wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            IntelConfidence::Weak => "weak",
            IntelConfidence::Medium => "medium",
            IntelConfidence::High => "high",
            IntelConfidence::Confirmed => "confirmed",
        }
    }
}

/// 情报实体的资产晋升状态（2.11：Intel 不应自动全部成为正式
/// 资产——默认 candidate，只有确定性高置信或人工 promote 才晋升）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntelEntityStatus {
    /// 候选（默认）。
    Candidate,
    /// 已晋升为正式资产。
    Promoted,
    /// 已排除。
    Rejected,
}

impl IntelEntityStatus {
    /// wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            IntelEntityStatus::Candidate => "candidate",
            IntelEntityStatus::Promoted => "promoted",
            IntelEntityStatus::Rejected => "rejected",
        }
    }
}

/// 情报实体：canonicalize 后按 `(kind, normalized_value)` 全局去重
/// 合并——5 个 source 命中同一资产只产生 1 个实体，`source_count`
/// 记录命中源数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntelEntity {
    /// 实体 ID（`ient_<hex>`）。
    pub id: String,
    /// 类别。
    pub kind: IntelEntityKind,
    /// 首次观测到的原始值（展示用）。
    pub value: String,
    /// canonical 归一值（去重键）。
    pub normalized_value: String,
    /// 置信度（确定性规则推导）。
    pub confidence: IntelConfidence,
    /// 资产晋升状态。
    #[serde(default = "default_entity_status")]
    pub status: IntelEntityStatus,
    /// 首次入库时间。
    pub first_seen: Timestamp,
    /// 最近一次命中时间。
    pub last_seen: Timestamp,
    /// 命中过本实体的 source ID 集（`source_count` 的精确依据：
    /// 同源重复命中不重复计数）。
    #[serde(default)]
    pub hit_sources: Vec<String>,
    /// 命中本实体的不同 source 数（= `hit_sources.len()`）。
    #[serde(default = "default_source_count")]
    pub source_count: u32,
}

/// 带完整多来源 provenance 的实体 API/storage view。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntelEntityRecord {
    /// 逻辑实体（按 kind + normalized value 去重）。
    #[serde(flatten)]
    pub entity: IntelEntity,
    /// 支持该实体的全部原始记录摘要。
    pub provenance: Vec<IntelProvenance>,
}

fn default_entity_status() -> IntelEntityStatus {
    IntelEntityStatus::Candidate
}

fn default_source_count() -> u32 {
    1
}

impl IntelEntity {
    /// 构造实体（`source_count` 初始 1、状态 candidate，confidence
    /// 由调用方按确定性规则给出）。
    #[must_use]
    pub fn new(
        kind: IntelEntityKind,
        value: impl Into<String>,
        normalized_value: impl Into<String>,
        confidence: IntelConfidence,
    ) -> Self {
        let value = value.into();
        Self {
            id: new_id("ient"),
            kind,
            normalized_value: normalized_value.into(),
            value,
            confidence,
            status: default_entity_status(),
            first_seen: utcnow(),
            last_seen: utcnow(),
            hit_sources: Vec::new(),
            source_count: default_source_count(),
        }
    }
}

// ---------------------------------------------------------------------------
// 关系
// ---------------------------------------------------------------------------

/// 情报关系类别（攻击面图的边）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntelRelationKind {
    /// 当前解析到。
    ResolvesTo,
    /// 历史解析到。
    HistoricallyResolvedTo,
    /// 证书包含（SAN/CN）。
    CertificateContains,
    /// 引用（页面/JS 引用另一实体）。
    References,
    /// 同 favicon。
    SameFavicon,
    /// 同指纹。
    SameFingerprint,
    /// 同 IP。
    SameIp,
    /// 使用技术。
    UsesTechnology,
    /// 由…提供服务。
    ServedBy,
    /// 可能是…的环境（dev/staging 猜测）。
    PossibleEnvironmentOf,
}

impl IntelRelationKind {
    /// wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            IntelRelationKind::ResolvesTo => "resolves_to",
            IntelRelationKind::HistoricallyResolvedTo => "historically_resolved_to",
            IntelRelationKind::CertificateContains => "certificate_contains",
            IntelRelationKind::References => "references",
            IntelRelationKind::SameFavicon => "same_favicon",
            IntelRelationKind::SameFingerprint => "same_fingerprint",
            IntelRelationKind::SameIp => "same_ip",
            IntelRelationKind::UsesTechnology => "uses_technology",
            IntelRelationKind::ServedBy => "served_by",
            IntelRelationKind::PossibleEnvironmentOf => "possible_environment_of",
        }
    }
}

/// 情报关系：逻辑边按 `(from, relation, to)` 去重；具体来源由独立
/// provenance association 保存，避免把多来源事实压缩成单值字段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntelRelation {
    /// 关系 ID（`irel_<hex>`）。
    pub id: String,
    /// 起点实体 ID。
    pub from_entity_id: String,
    /// 关系类别。
    pub relation: IntelRelationKind,
    /// 终点实体 ID。
    pub to_entity_id: String,
    /// 置信度（确定性规则推导）。
    pub confidence: IntelConfidence,
    /// 命中过该关系的 source ID 集。
    #[serde(default)]
    pub hit_sources: Vec<String>,
    /// 命中该关系的不同 source 数。
    #[serde(default)]
    pub source_count: u32,
    /// 首次观测时间。
    pub first_seen: Timestamp,
    /// 最近观测时间。
    pub last_seen: Timestamp,
}

impl IntelRelation {
    /// 构造关系。
    #[must_use]
    pub fn new(
        from_entity_id: impl Into<String>,
        relation: IntelRelationKind,
        to_entity_id: impl Into<String>,
        confidence: IntelConfidence,
    ) -> Self {
        Self {
            id: new_id("irel"),
            from_entity_id: from_entity_id.into(),
            relation,
            to_entity_id: to_entity_id.into(),
            confidence,
            hit_sources: Vec::new(),
            source_count: 0,
            first_seen: utcnow(),
            last_seen: utcnow(),
        }
    }
}

/// 带完整多来源 provenance 的关系 API/storage view。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntelRelationRecord {
    /// 逻辑关系。
    #[serde(flatten)]
    pub relation: IntelRelation,
    /// 支持该关系的全部原始记录摘要。
    pub provenance: Vec<IntelProvenance>,
}

/// 单个 raw record 归一化出的实体观测。
#[derive(Debug, Clone, PartialEq)]
pub struct IntelEntityObservation {
    /// 原始记录 ID。
    pub raw_record_id: String,
    /// 实体草稿。
    pub entity: IntelEntity,
}

/// 单个 raw record 归一化出的关系观测；端点以实体去重键引用，由
/// storage 在事务内解析为稳定实体 ID。
#[derive(Debug, Clone, PartialEq)]
pub struct IntelRelationObservation {
    /// 原始记录 ID。
    pub raw_record_id: String,
    /// 起点实体类别。
    pub from_kind: IntelEntityKind,
    /// 起点归一值。
    pub from_normalized_value: String,
    /// 关系类别。
    pub relation: IntelRelationKind,
    /// 终点实体类别。
    pub to_kind: IntelEntityKind,
    /// 终点归一值。
    pub to_normalized_value: String,
    /// 本次观测置信度。
    pub confidence: IntelConfidence,
}

/// 一个 source result 的原子写入批次。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IntelIngestBatch {
    /// 必须先持久化的 source 原始记录。
    pub records: Vec<IntelRawRecord>,
    /// 从 records 派生的实体观测。
    pub entities: Vec<IntelEntityObservation>,
    /// 从 records 派生的关系观测。
    pub relations: Vec<IntelRelationObservation>,
}

/// 一个事务完成后的可见逻辑记录。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IntelIngestOutcome {
    /// 新建或合并后的实体。
    pub entities: Vec<IntelEntityRecord>,
    /// 新建或合并后的关系。
    pub relations: Vec<IntelRelationRecord>,
}

// ---------------------------------------------------------------------------
// Source 结果
// ---------------------------------------------------------------------------

/// 单个 source 一次查询的归一化结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntelSourceResult {
    /// 数据源 ID。
    pub source_id: String,
    /// 拉到的原始记录。
    #[serde(skip_serializing, default)]
    pub records: Vec<IntelRawRecord>,
    /// 返回的 raw record 数量（payload 默认不随查询响应返回）。
    #[serde(default)]
    pub record_count: u32,
    /// 拉取时间。
    pub fetched_at: Timestamp,
    /// 非致命错误（source 故障隔离：错误记录在此，绝不中断其他
    /// source 的查询）。
    #[serde(default)]
    pub errors: Vec<String>,
}

impl IntelSourceResult {
    /// 构造成功结果。
    #[must_use]
    pub fn ok(source_id: impl Into<String>, records: Vec<IntelRawRecord>) -> Self {
        let record_count = u32::try_from(records.len()).unwrap_or(u32::MAX);
        Self {
            source_id: source_id.into(),
            records,
            record_count,
            fetched_at: utcnow(),
            errors: Vec::new(),
        }
    }

    /// 构造失败结果（故障隔离：错误进 `errors`，不抛出）。
    #[must_use]
    pub fn failed(source_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            source_id: source_id.into(),
            records: Vec::new(),
            record_count: 0,
            fetched_at: utcnow(),
            errors: vec![error.into()],
        }
    }
}

/// Source 能力声明（UI / 调度据此决定可接受的查询类型）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntelSourceCapabilities {
    /// 支持的查询类型。
    pub query_types: Vec<IntelQueryType>,
    /// 支持的 filters 键（原样透传给 source）。
    #[serde(default)]
    pub filter_keys: Vec<String>,
    /// 是否需要凭据（API key）。
    #[serde(default)]
    pub requires_credentials: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_dedup_key_is_kind_plus_normalized_value() {
        let first = IntelEntity::new(
            IntelEntityKind::Domain,
            "Example.COM",
            "example.com",
            IntelConfidence::Medium,
        );
        let second = IntelEntity::new(
            IntelEntityKind::Domain,
            "example.com",
            "example.com",
            IntelConfidence::Medium,
        );
        assert_eq!(first.kind, second.kind);
        assert_eq!(first.normalized_value, second.normalized_value);
        assert_ne!(first.id, second.id, "ids are unique per insert attempt");
    }

    #[test]
    fn confidence_orders_weak_to_confirmed() {
        assert!(IntelConfidence::Weak < IntelConfidence::Medium);
        assert!(IntelConfidence::Medium < IntelConfidence::High);
        assert!(IntelConfidence::High < IntelConfidence::Confirmed);
    }

    #[test]
    fn wire_values_are_stable() {
        assert_eq!(IntelQueryType::Domain.as_str(), "domain");
        assert_eq!(IntelEntityKind::Certificate.as_str(), "certificate");
        assert_eq!(
            IntelRelationKind::HistoricallyResolvedTo.as_str(),
            "historically_resolved_to"
        );
        assert_eq!(IntelConfidence::Confirmed.as_str(), "confirmed");
    }
}
