//! crt.sh Certificate Transparency source：公开 CT 日志检索，无需
//! 凭据。`GET <base>/?q=%25.<domain>&output=json` 返回证书条目数组，
//! `name_value`（SAN，换行分隔）与 `common_name` 展开为域名实体，
//! 证书本身以序列号建实体，`certificate_contains` 建边。

use std::collections::BTreeSet;
use std::time::Duration;

use models::{
    IntelConfidence, IntelEntityKind, IntelQuery, IntelQueryType, IntelRawRecord,
    IntelRelationKind, IntelSourceCapabilities, IntelSourceResult,
};
use serde_json::Value;

use crate::normalize::normalize_domain;
use crate::source::{
    EntityDraft, IntelError, IntelligenceSource, NormalizedRecord, RelationDraft,
    map_reqwest_error, read_limited_body,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_NAME_VALUES: usize = 200;

/// crt.sh Certificate Transparency source。
pub struct CrtShSource {
    base_url: String,
    client: reqwest::Client,
}

impl CrtShSource {
    /// 生产实例（`https://crt.sh`）。
    #[must_use]
    pub fn production() -> Self {
        Self::with_base_url("https://crt.sh")
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
impl IntelligenceSource for CrtShSource {
    fn id(&self) -> &'static str {
        "crtsh"
    }

    fn display_name(&self) -> &'static str {
        "crt.sh Certificate Transparency"
    }

    fn capabilities(&self) -> IntelSourceCapabilities {
        IntelSourceCapabilities {
            query_types: vec![IntelQueryType::Domain],
            filter_keys: Vec::new(),
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
        let url = format!("{}/", self.base_url);
        let response = self
            .client
            .get(&url)
            .query(&[("q", format!("%.{domain}")), ("output", "json".to_string())])
            .send()
            .await
            .map_err(|error| map_reqwest_error(self.id(), &error))?;
        let status = response.status();
        if !status.is_success() {
            return Err(IntelError::Status {
                status: status.as_u16(),
                detail: format!("crt.sh query for {domain}"),
            });
        }
        let bytes = read_limited_body(response, self.id(), MAX_RESPONSE_BYTES).await?;
        let entries: Vec<Value> = if bytes.is_empty() {
            Vec::new()
        } else {
            serde_json::from_slice(&bytes)
                .map_err(|error| IntelError::Parse(format!("crt.sh JSON: {error}")))?
        };
        let records = entries
            .into_iter()
            .take(query.limit as usize)
            .map(|entry| {
                let record_id = entry
                    .get("id")
                    .and_then(Value::as_i64)
                    .map_or(String::new(), |id| id.to_string());
                IntelRawRecord::new(self.id(), record_id, query.clone(), entry)
            })
            .collect();
        Ok(IntelSourceResult::ok(self.id(), records))
    }

    fn normalize(&self, record: &IntelRawRecord) -> NormalizedRecord {
        let mut normalized = NormalizedRecord::default();
        let payload = &record.raw_payload;
        let serial = payload
            .get("serial_number")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase();
        let mut domains = BTreeSet::new();
        if let Some(name_value) = payload.get("name_value").and_then(Value::as_str) {
            for line in name_value.lines().take(MAX_NAME_VALUES) {
                if let Some(domain) = normalize_domain(line) {
                    domains.insert(domain);
                }
            }
        }
        if let Some(common_name) = payload.get("common_name").and_then(Value::as_str)
            && let Some(domain) = normalize_domain(common_name)
        {
            domains.insert(domain);
        }
        for domain in &domains {
            normalized.entities.push(EntityDraft {
                kind: IntelEntityKind::Domain,
                value: domain.clone(),
                normalized_value: domain.clone(),
                // CT 日志直接记录该域名被证书覆盖：高置信单源线索。
                base_confidence: IntelConfidence::High,
            });
        }
        // 证书实体（有序列号才建）+ certificate_contains 边。
        if !serial.is_empty() {
            normalized.entities.push(EntityDraft {
                kind: IntelEntityKind::Certificate,
                value: serial.clone(),
                normalized_value: serial.clone(),
                // 证书本身就在 CT 日志里：确定性事实。
                base_confidence: IntelConfidence::Confirmed,
            });
            for domain in &domains {
                normalized.relations.push(RelationDraft {
                    from: (IntelEntityKind::Certificate, serial.clone()),
                    relation: IntelRelationKind::CertificateContains,
                    to: (IntelEntityKind::Domain, domain.clone()),
                    confidence: IntelConfidence::Confirmed,
                });
            }
        }
        // 签发组织（有issuer_name 时）。
        if let Some(issuer) = payload.get("issuer_name").and_then(Value::as_str) {
            let issuer = issuer.trim();
            if !issuer.is_empty() && !serial.is_empty() {
                let org_key = issuer.to_lowercase();
                normalized.entities.push(EntityDraft {
                    kind: IntelEntityKind::Organization,
                    value: issuer.to_string(),
                    normalized_value: org_key.clone(),
                    base_confidence: IntelConfidence::Confirmed,
                });
                normalized.relations.push(RelationDraft {
                    from: (IntelEntityKind::Certificate, serial.clone()),
                    relation: IntelRelationKind::ServedBy,
                    to: (IntelEntityKind::Organization, org_key),
                    confidence: IntelConfidence::Confirmed,
                });
            }
        }
        normalized
    }
}
