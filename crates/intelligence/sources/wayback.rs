//! Wayback Machine CDX source：Internet Archive 历史 URL 检索，无需
//! 凭据。`GET <base>/cdx/search/cdx?url=<domain>/*&output=json&...`
//! 返回行数组（首行是表头），每行 `original` 展开为 URL 实体，
//! host 建域名实体，域名与 URL 之间建 `references` 边。

use std::collections::BTreeSet;
use std::time::Duration;

use models::{
    IntelConfidence, IntelEntityKind, IntelQuery, IntelQueryType, IntelRawRecord,
    IntelRelationKind, IntelSourceCapabilities, IntelSourceResult,
};
use serde_json::Value;

use crate::normalize::{normalize_domain, normalize_url, url_host};
use crate::source::{
    EntityDraft, IntelError, IntelligenceSource, NormalizedRecord, RelationDraft,
    map_reqwest_error, read_limited_body,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const DEFAULT_ROW_LIMIT: u32 = 200;

/// Wayback Machine CDX source。
pub struct WaybackSource {
    base_url: String,
    client: reqwest::Client,
}

impl WaybackSource {
    /// 生产实例（`https://web.archive.org`）。
    #[must_use]
    pub fn production() -> Self {
        Self::with_base_url("https://web.archive.org")
    }

    /// 指定 base URL（测试注入本地 mock server）。
    #[must_use]
    pub fn with_base_url(base_url: &str) -> Self {
        Self::with_base_url_and_timeout(base_url, REQUEST_TIMEOUT)
    }

    /// 指定 base URL 与请求超时（测试和受限部署使用）。
    ///
    /// 客户端构建失败时退回默认 client（rustls 初始化异常，实际
    /// 不可达）；即便退回，pipeline 的 per-source deadline 仍然兜底。
    #[must_use]
    pub fn with_base_url_and_timeout(base_url: &str, timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .unwrap_or_default();
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
        }
    }
}

#[async_trait::async_trait]
impl IntelligenceSource for WaybackSource {
    fn id(&self) -> &'static str {
        "wayback"
    }

    fn display_name(&self) -> &'static str {
        "Internet Archive Wayback CDX"
    }

    fn capabilities(&self) -> IntelSourceCapabilities {
        IntelSourceCapabilities {
            query_types: vec![IntelQueryType::Domain],
            filter_keys: vec!["collapse".to_string(), "filter".to_string()],
            requires_credentials: false,
        }
    }

    async fn query(&self, query: &IntelQuery) -> Result<IntelSourceResult, IntelError> {
        if query.query_type != IntelQueryType::Domain {
            return Err(IntelError::UnsupportedQuery(
                query.query_type.as_str().to_string(),
            ));
        }
        let domain = normalize_domain(&query.seed)
            .ok_or_else(|| IntelError::Parse(format!("invalid domain seed: {}", query.seed)))?;
        let limit = if query.limit == 0 {
            DEFAULT_ROW_LIMIT
        } else {
            query.limit.min(1000)
        };
        let url = format!("{}/cdx/search/cdx", self.base_url);
        let collapse = query
            .filters
            .get("collapse")
            .and_then(Value::as_str)
            .unwrap_or("urlkey")
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || *character == '_')
            .take(64)
            .collect::<String>();
        let mut request = self.client.get(&url).query(&[
            ("url", format!("{domain}/*")),
            ("output", "json".to_string()),
            ("limit", limit.to_string()),
            ("collapse", collapse),
        ]);
        if let Some(filter) = query.filters.get("filter").and_then(Value::as_str) {
            request = request.query(&[("filter", filter.chars().take(256).collect::<String>())]);
        }
        let response = request
            .send()
            .await
            .map_err(|error| map_reqwest_error(self.id(), &error))?;
        let status = response.status();
        if !status.is_success() {
            return Err(IntelError::Status {
                status: status.as_u16(),
                detail: format!("wayback CDX query for {domain}"),
            });
        }
        let bytes = read_limited_body(response, self.id(), MAX_RESPONSE_BYTES).await?;
        let rows: Vec<Vec<Value>> = if bytes.is_empty() {
            Vec::new()
        } else {
            serde_json::from_slice(&bytes)
                .map_err(|error| IntelError::Parse(format!("wayback CDX JSON: {error}")))?
        };
        // 首行是表头（["urlkey","timestamp","original",...]），跳过。
        let records = rows
            .into_iter()
            .skip(1)
            .filter(|row| !row.is_empty())
            .map(|row| {
                let record_id = row
                    .first()
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                IntelRawRecord::new(self.id(), record_id, query.clone(), Value::Array(row))
            })
            .collect();
        Ok(IntelSourceResult::ok(self.id(), records))
    }

    fn normalize(&self, record: &IntelRawRecord) -> NormalizedRecord {
        let mut normalized = NormalizedRecord::default();
        let row = &record.raw_payload;
        // CDX 列序固定：urlkey, timestamp, original, mimetype, ...
        let Some(original) = row.get(2).and_then(Value::as_str) else {
            return normalized;
        };
        let Some(url_normalized) = normalize_url(original) else {
            return normalized;
        };
        let mut domains = BTreeSet::new();
        if let Some(host) = url_host(original)
            && normalize_domain(&host).is_some()
        {
            domains.insert(host);
        }
        for domain in &domains {
            normalized.entities.push(EntityDraft {
                kind: IntelEntityKind::Domain,
                value: domain.clone(),
                normalized_value: domain.clone(),
                base_confidence: IntelConfidence::Medium,
            });
        }
        normalized.entities.push(EntityDraft {
            kind: IntelEntityKind::Url,
            value: original.to_string(),
            normalized_value: url_normalized.clone(),
            // 历史快照中的 URL：单源中等置信。
            base_confidence: IntelConfidence::Medium,
        });
        for domain in &domains {
            normalized.relations.push(RelationDraft {
                from: (IntelEntityKind::Domain, domain.clone()),
                relation: IntelRelationKind::References,
                to: (IntelEntityKind::Url, url_normalized.clone()),
                confidence: IntelConfidence::Medium,
            });
        }
        normalized
    }
}
