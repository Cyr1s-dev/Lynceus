//! [`IntelligenceSource`] 抽象：一切外部数据源的统一接口。
//!
//! 业务代码（pipeline / API / UI）只面向这个 trait；新增数据源 =
//! 新增一个实现并注册，**不允许**在业务代码里写 source 名分支。

use models::{
    IntelConfidence, IntelEntityKind, IntelQuery, IntelRawRecord, IntelRelationKind,
    IntelSourceCapabilities, IntelSourceResult,
};

/// Source 层错误（网络 / 协议 / 凭据）。pipeline 把它转成
/// [`IntelSourceResult::failed`] 做故障隔离，不向调用方抛出。
#[derive(Debug, thiserror::Error)]
pub enum IntelError {
    /// HTTP 传输失败。
    #[error("transport: {0}")]
    Transport(String),
    /// source 返回非 2xx。
    #[error("source returned HTTP {status}: {detail}")]
    Status {
        /// HTTP 状态码。
        status: u16,
        /// 截断后的响应摘要（绝不包含凭据）。
        detail: String,
    },
    /// 响应解析失败。
    #[error("parse: {0}")]
    Parse(String),
    /// 查询类型不受支持（调用前应用 capabilities 预检，属编程错误）。
    #[error("unsupported query type: {0}")]
    UnsupportedQuery(String),
    /// 凭据缺失（source 声明 `requires_credentials`）。
    #[error("credentials not configured for source {0}")]
    MissingCredentials(String),
    /// source 在配置的 deadline 内没有完成。
    #[error("timeout querying source {0}")]
    Timeout(String),
    /// source payload 超过安全上限。
    #[error("source {source_id} response exceeds {limit} bytes")]
    ResponseTooLarge {
        /// Source ID。
        source_id: String,
        /// 允许的最大字节数。
        limit: u64,
    },
}

/// 有界读取 HTTP body，避免先把任意大小的外部 payload 全部载入内存。
pub(crate) async fn read_limited_body(
    mut response: reqwest::Response,
    source: &str,
    limit: u64,
) -> Result<Vec<u8>, IntelError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(IntelError::ResponseTooLarge {
            source_id: source.to_string(),
            limit,
        });
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| map_reqwest_error(source, &error))?
    {
        if u64::try_from(body.len().saturating_add(chunk.len())).unwrap_or(u64::MAX) > limit {
            return Err(IntelError::ResponseTooLarge {
                source_id: source.to_string(),
                limit,
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub(crate) fn map_reqwest_error(source: &str, error: &reqwest::Error) -> IntelError {
    if error.is_timeout() {
        IntelError::Timeout(source.to_string())
    } else {
        IntelError::Transport(error.to_string())
    }
}

/// 归一化产出的实体草稿（入库前的候选）。
#[derive(Debug, Clone, PartialEq)]
pub struct EntityDraft {
    /// 类别。
    pub kind: IntelEntityKind,
    /// 展示值（首个观测形态）。
    pub value: String,
    /// canonical 归一值（去重键）。
    pub normalized_value: String,
    /// source 给出的基础置信度（pipeline 还会叠加跨源规则）。
    pub base_confidence: IntelConfidence,
}

/// 归一化产出的关系草稿：端点用 `(kind, normalized_value)` 引用
/// （pipeline 先落实体再解析成实体 ID）。
#[derive(Debug, Clone, PartialEq)]
pub struct RelationDraft {
    /// 起点实体键。
    pub from: (IntelEntityKind, String),
    /// 关系类别。
    pub relation: IntelRelationKind,
    /// 终点实体键。
    pub to: (IntelEntityKind, String),
    /// 置信度。
    pub confidence: IntelConfidence,
}

/// 一条原始记录的归一化结果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NormalizedRecord {
    /// 抽出的实体草稿。
    pub entities: Vec<EntityDraft>,
    /// 抽出的关系草稿。
    pub relations: Vec<RelationDraft>,
}

/// 数据源元信息（UI 的 Sources 面板）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct IntelSourceInfo {
    /// 数据源 ID。
    pub id: String,
    /// 展示名。
    pub display_name: String,
    /// 能力声明。
    pub capabilities: IntelSourceCapabilities,
    /// 是否为本轮真实实现的 source（false = 接口预留）。
    pub implemented: bool,
}

/// 外部数据源统一接口。
#[async_trait::async_trait]
pub trait IntelligenceSource: Send + Sync {
    /// 稳定 ID（小写，如 `crtsh`）。
    fn id(&self) -> &'static str;
    /// 展示名（如 `crt.sh Certificate Transparency`）。
    fn display_name(&self) -> &'static str;
    /// 能力声明：可接受的查询类型 / filters / 凭据需求。
    fn capabilities(&self) -> IntelSourceCapabilities;
    /// 执行查询，返回原始记录（未归一化）。错误经 [`IntelError`]
    /// 上抛，由 pipeline 做故障隔离。
    async fn query(&self, query: &IntelQuery) -> Result<IntelSourceResult, IntelError>;
    /// 把一条原始记录归一化为实体/关系草稿（纯函数，无 IO）。
    fn normalize(&self, record: &IntelRawRecord) -> NormalizedRecord;
}
