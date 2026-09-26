//! Intelligence pipeline：seed → 多源查询（故障隔离）→ 原始留痕 →
//! 归一化 → 实体去重合并 → 关系建图（带 provenance）。
//!
//! confidence 规则（全部确定性，不由 LLM 生成）：
//!
//! - source 归一化给出 `base_confidence`（CT 证书=Confirmed、CT 覆盖
//!   域名=High、Wayback 历史 URL=Medium…）；
//! - 同一实体被 ≥2 个不同 source 命中（`hit_sources` 并集）→ 至少
//!   `High`；
//! - 合并时 confidence 取历史与当前的高者，绝不降级。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{self, StreamExt};
use models::{
    IntelConfidence, IntelEntity, IntelEntityKind, IntelEntityObservation, IntelEntityRecord,
    IntelEntityStatus, IntelIngestBatch, IntelQuery, IntelRelationObservation, IntelRelationRecord,
    IntelSourceResult, MissionAsset, MissionAssetId, MissionAssetSensitivity, MissionAssetSource,
    MissionAssetType, MissionId, ProjectId, Timestamp, new_id, utcnow,
};
use serde::Serialize;
use storage::Repository;

use crate::source::{IntelError, IntelSourceInfo, IntelligenceSource};

/// 一次 seed 展开的汇总（API 响应 + UI 渲染原料）。
#[derive(Debug, Clone, Serialize)]
pub struct IntelExpansionReport {
    /// 本次查询 ID。
    pub query_id: String,
    /// 原始查询。
    pub query: IntelQuery,
    /// 每个被执行 source 的结果（含故障隔离的 errors）。
    pub source_results: Vec<IntelSourceResult>,
    /// 成功完成且成功入库的 source。
    pub successful_sources: Vec<String>,
    /// 查询或入库失败的 source。
    pub failed_sources: Vec<String>,
    /// 是否同时存在成功与失败 source。
    pub partial: bool,
    /// 非致命提示（例如未知 source ID）。
    pub warnings: Vec<String>,
    /// 汇总生成时间。
    pub fetched_at: Timestamp,
    /// 本次落库（新建或合并）的实体。
    pub entities: Vec<IntelEntityRecord>,
    /// 本次落库（新建或刷新）的关系。
    pub relations: Vec<IntelRelationRecord>,
}

/// 实体晋升结果。
#[derive(Debug, Clone, Serialize)]
pub struct PromoteOutcome {
    /// 被晋升的情报实体。
    pub entity: IntelEntity,
    /// 新建/合并后的 Mission 资产。
    pub asset: MissionAsset,
}

/// 单个 source 的执行 future（bounded 并发流元素）。
type SourceFuture =
    Pin<Box<dyn Future<Output = (Arc<dyn IntelligenceSource>, IntelSourceResult)> + Send>>;

/// Intelligence pipeline（source 集 + 仓储）。
pub struct IntelligencePipeline {
    sources: Vec<Arc<dyn IntelligenceSource>>,
    repository: Arc<dyn Repository>,
    source_timeout: Duration,
    global_timeout: Duration,
}

impl IntelligencePipeline {
    /// 构造 pipeline。
    #[must_use]
    pub fn new(sources: Vec<Arc<dyn IntelligenceSource>>, repository: Arc<dyn Repository>) -> Self {
        Self {
            sources,
            repository,
            source_timeout: Duration::from_secs(35),
            global_timeout: Duration::from_secs(45),
        }
    }

    /// 覆盖 pipeline 的 per-source deadline（主要用于测试和受限部署）。
    #[must_use]
    pub fn with_source_timeout(mut self, timeout: Duration) -> Self {
        self.source_timeout = timeout;
        self
    }

    /// 覆盖整次查询 deadline；到期时保留已经完成的 source 结果，取消
    /// 未完成 source（主要用于测试和受限部署）。
    #[must_use]
    pub fn with_global_timeout(mut self, timeout: Duration) -> Self {
        self.global_timeout = timeout;
        self
    }

    /// source 元信息（UI Sources 面板）。
    #[must_use]
    pub fn source_infos(&self) -> Vec<IntelSourceInfo> {
        self.sources
            .iter()
            .map(|source| IntelSourceInfo {
                id: source.id().to_string(),
                display_name: source.display_name().to_string(),
                capabilities: source.capabilities(),
                implemented: true,
            })
            .collect()
    }

    /// seed 展开：对每个声明支持该查询类型的 source 发查询。
    ///
    /// 故障隔离：任何 source 的错误只记录在该 source 的
    /// [`IntelSourceResult::errors`]，其余 source 照常执行。
    pub async fn expand(&self, query: &IntelQuery) -> IntelExpansionReport {
        let mut report = IntelExpansionReport {
            query_id: new_id("iqry"),
            query: query.clone(),
            source_results: Vec::new(),
            successful_sources: Vec::new(),
            failed_sources: Vec::new(),
            partial: false,
            warnings: Vec::new(),
            fetched_at: utcnow(),
            entities: Vec::new(),
            relations: Vec::new(),
        };
        let requested_sources = &query.source_ids;
        for requested in requested_sources {
            if !self.sources.iter().any(|source| source.id() == requested) {
                report
                    .warnings
                    .push(format!("unknown intelligence source: {requested}"));
            }
        }
        let query_for_sources = query.clone();
        let source_timeout = self.source_timeout;
        let applicable_sources = self.applicable_sources(query);
        let applicable_source_ids = applicable_sources
            .iter()
            .map(|source| source.id().to_string())
            .collect::<Vec<_>>();
        let futures = applicable_sources
            .into_iter()
            .map(|source| {
                let query = query_for_sources.clone();
                Box::pin(async move {
                    let result = match tokio::time::timeout(source_timeout, source.query(&query))
                        .await
                    {
                        Ok(Ok(result)) => result,
                        Ok(Err(error)) => IntelSourceResult::failed(source.id(), error.to_string()),
                        Err(_) => IntelSourceResult::failed(
                            source.id(),
                            IntelError::Timeout(source.id().to_string()).to_string(),
                        ),
                    };
                    (source, result)
                }) as SourceFuture
            })
            .collect::<Vec<_>>();
        let mut executions = Vec::new();
        let mut execution_stream = stream::iter(futures).buffer_unordered(4);
        let completed_within_global_deadline = tokio::time::timeout(self.global_timeout, async {
            while let Some(execution) = execution_stream.next().await {
                executions.push(execution);
            }
        })
        .await
        .is_ok();
        let mut entities = HashMap::new();
        let mut relations = HashMap::new();
        for (source, mut result) in executions {
            if result.errors.is_empty() {
                match self.ingest(source.as_ref(), &result.records) {
                    Ok(outcome) => {
                        report.successful_sources.push(source.id().to_string());
                        for entity in outcome.entities {
                            entities.insert(entity.entity.id.clone(), entity);
                        }
                        for relation in outcome.relations {
                            relations.insert(relation.relation.id.clone(), relation);
                        }
                    }
                    Err(error) => result.errors.push(format!("persist: {error}")),
                }
            }
            if !result.errors.is_empty() {
                report.failed_sources.push(source.id().to_string());
            }
            report.source_results.push(result);
        }
        if !completed_within_global_deadline {
            self.record_global_timeout(&mut report, applicable_source_ids);
        }
        report.entities = entities.into_values().collect();
        report.relations = relations.into_values().collect();
        report.partial = !report.successful_sources.is_empty() && !report.failed_sources.is_empty();
        report
    }

    fn record_global_timeout(
        &self,
        report: &mut IntelExpansionReport,
        applicable_source_ids: Vec<String>,
    ) {
        report.warnings.push(format!(
            "intelligence query exceeded global timeout of {} seconds",
            self.global_timeout.as_secs_f64()
        ));
        for source_id in applicable_source_ids {
            if report
                .source_results
                .iter()
                .any(|result| result.source_id == source_id)
            {
                continue;
            }
            report.failed_sources.push(source_id.clone());
            report.source_results.push(IntelSourceResult::failed(
                &source_id,
                format!("global intelligence query timeout: {source_id}"),
            ));
        }
    }

    /// 单 source 结果的入库路径：原始留痕 → 归一化 → 实体/关系
    /// upsert。整个 source result 由 repository 在一个事务内提交。
    fn ingest(
        &self,
        source: &dyn IntelligenceSource,
        records: &[models::IntelRawRecord],
    ) -> Result<models::IntelIngestOutcome, storage::StorageError> {
        let mut batch = IntelIngestBatch {
            records: records.to_vec(),
            ..IntelIngestBatch::default()
        };
        for record in records {
            let normalized = source.normalize(record);
            for draft in normalized.entities {
                batch.entities.push(IntelEntityObservation {
                    raw_record_id: record.id.clone(),
                    entity: IntelEntity::new(
                        draft.kind,
                        draft.value,
                        draft.normalized_value,
                        draft.base_confidence,
                    ),
                });
            }
            for draft in normalized.relations {
                batch.relations.push(IntelRelationObservation {
                    raw_record_id: record.id.clone(),
                    from_kind: draft.from.0,
                    from_normalized_value: draft.from.1,
                    relation: draft.relation,
                    to_kind: draft.to.0,
                    to_normalized_value: draft.to.1,
                    confidence: draft.confidence,
                });
            }
        }
        self.repository.ingest_intel_batch(&batch)
    }

    /// 当前查询类型适用的 source（capabilities 预检）。
    fn applicable_sources(&self, query: &IntelQuery) -> Vec<Arc<dyn IntelligenceSource>> {
        self.sources
            .iter()
            .filter(|source| {
                source
                    .capabilities()
                    .query_types
                    .contains(&query.query_type)
                    && (query.source_ids.is_empty()
                        || query.source_ids.iter().any(|id| id == source.id()))
            })
            .cloned()
            .collect()
    }

    /// 确定性自动晋升资格：只有 Confirmed 且多源交叉的实体才允许
    /// 自动晋升（2.11）；其余实体只能人工 promote。
    #[must_use]
    pub fn eligible_for_auto_promote(entity: &IntelEntity) -> bool {
        entity.confidence == IntelConfidence::Confirmed && entity.source_count >= 2
    }

    /// 把情报实体晋升为 Mission 资产（2.11 candidate → asset；
    /// 2.12：Intelligence 不产生 Evidence，assets 的 `evidence_ids`
    /// 恒为空）。
    ///
    /// # Errors
    /// 实体不存在、类别不可晋升、mission 不存在。
    pub fn promote_entity(
        &self,
        entity_id: &str,
        mission_id: &str,
    ) -> Result<PromoteOutcome, IntelError> {
        let entity = self
            .repository
            .get_intel_entity(entity_id)
            .map_err(|error| IntelError::Transport(error.to_string()))?
            .ok_or_else(|| IntelError::Parse(format!("intel entity not found: {entity_id}")))?;
        let asset_type = promotable_asset_type(entity.kind).ok_or_else(|| {
            IntelError::UnsupportedQuery(format!(
                "entity kind '{}' cannot be promoted to an asset",
                entity.kind.as_str()
            ))
        })?;
        let mission = self
            .repository
            .get_mission(mission_id)
            .map_err(|error| IntelError::Transport(error.to_string()))?
            .ok_or_else(|| IntelError::Parse(format!("mission not found: {mission_id}")))?;
        let confidence = match entity.confidence {
            IntelConfidence::Confirmed => 0.95,
            IntelConfidence::High => 0.8,
            IntelConfidence::Medium => 0.6,
            IntelConfidence::Weak => 0.3,
        };
        let asset = MissionAsset {
            id: MissionAssetId::new(new_id("asset")),
            project_id: ProjectId::new(mission.project_id.as_str().to_string()),
            mission_id: MissionId::new(mission_id.to_string()),
            asset_type,
            value: entity.normalized_value.clone(),
            label: Some(entity.value.clone()),
            sensitivity: MissionAssetSensitivity::Unknown,
            confidence,
            source: MissionAssetSource::Intelligence,
            source_id: Some(entity.id.clone()),
            branch_id: None,
            run_id: None,
            evidence_ids: Vec::new(),
            finding_ids: Vec::new(),
            tool_invocation_ids: Vec::new(),
            tags: Vec::new(),
            metadata: serde_json::Map::new(),
            created_at: utcnow(),
            updated_at: utcnow(),
        };
        let asset = self
            .repository
            .upsert_mission_asset(&asset)
            .map_err(|error| IntelError::Transport(error.to_string()))?;
        let entity = self
            .repository
            .set_intel_entity_status(entity_id, IntelEntityStatus::Promoted)
            .map_err(|error| IntelError::Transport(error.to_string()))?
            .ok_or_else(|| IntelError::Parse(format!("intel entity not found: {entity_id}")))?;
        Ok(PromoteOutcome { entity, asset })
    }
}

/// 可晋升类别映射（其余类别——证书/技术/组织等——不直接成为
/// Mission 资产）。
fn promotable_asset_type(kind: IntelEntityKind) -> Option<MissionAssetType> {
    match kind {
        IntelEntityKind::Domain => Some(MissionAssetType::Domain),
        IntelEntityKind::Ip => Some(MissionAssetType::Ip),
        IntelEntityKind::Url => Some(MissionAssetType::Url),
        IntelEntityKind::Service => Some(MissionAssetType::Service),
        IntelEntityKind::Repository => Some(MissionAssetType::Repository),
        _ => None,
    }
}
