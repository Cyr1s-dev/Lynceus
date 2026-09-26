//! Intelligence pipeline provenance、故障隔离和事务回归测试。

#![cfg_attr(test, allow(clippy::expect_used))]

use std::sync::Arc;
use std::time::Duration;

use intelligence::IntelligencePipeline;
use intelligence::source::{
    EntityDraft, IntelError, IntelligenceSource, NormalizedRecord, RelationDraft,
};
use models::{
    IntelConfidence, IntelEntity, IntelEntityKind, IntelEntityObservation, IntelIngestBatch,
    IntelQuery, IntelQueryType, IntelRawRecord, IntelRelationKind, IntelRelationObservation,
    IntelSourceCapabilities, IntelSourceResult,
};
use serde_json::json;
use storage::{Repository, SqliteRepository};

struct StaticSource {
    id: &'static str,
    fail: bool,
    hang: bool,
}

#[async_trait::async_trait]
impl IntelligenceSource for StaticSource {
    fn id(&self) -> &'static str {
        self.id
    }

    fn display_name(&self) -> &'static str {
        self.id
    }

    fn capabilities(&self) -> IntelSourceCapabilities {
        IntelSourceCapabilities {
            query_types: vec![IntelQueryType::Domain],
            filter_keys: Vec::new(),
            requires_credentials: false,
        }
    }

    async fn query(&self, query: &IntelQuery) -> Result<IntelSourceResult, IntelError> {
        if self.hang {
            std::future::pending::<()>().await;
        }
        if self.fail {
            return Err(IntelError::Transport("fixture failure".to_string()));
        }
        Ok(IntelSourceResult::ok(
            self.id,
            vec![IntelRawRecord::new(
                self.id,
                format!("{}-record", self.id),
                query.clone(),
                json!({"fixture": self.id}),
            )],
        ))
    }

    fn normalize(&self, _record: &IntelRawRecord) -> NormalizedRecord {
        NormalizedRecord {
            entities: vec![
                EntityDraft {
                    kind: IntelEntityKind::Domain,
                    value: "api.example.com".to_string(),
                    normalized_value: "api.example.com".to_string(),
                    base_confidence: IntelConfidence::Medium,
                },
                EntityDraft {
                    kind: IntelEntityKind::Url,
                    value: "https://api.example.com/v1".to_string(),
                    normalized_value: "https://api.example.com/v1".to_string(),
                    base_confidence: IntelConfidence::Medium,
                },
            ],
            relations: vec![RelationDraft {
                from: (IntelEntityKind::Domain, "api.example.com".to_string()),
                relation: IntelRelationKind::References,
                to: (
                    IntelEntityKind::Url,
                    "https://api.example.com/v1".to_string(),
                ),
                confidence: IntelConfidence::Medium,
            }],
        }
    }
}

fn repository() -> Arc<dyn Repository> {
    Arc::new(SqliteRepository::open(":memory:").expect("repository opens"))
}

#[tokio::test]
async fn dedup_preserves_all_entity_and_relation_provenance() {
    let pipeline = IntelligencePipeline::new(
        vec![
            Arc::new(StaticSource {
                id: "source_a",
                fail: false,
                hang: false,
            }),
            Arc::new(StaticSource {
                id: "source_b",
                fail: false,
                hang: false,
            }),
        ],
        repository(),
    );

    let report = pipeline
        .expand(&IntelQuery::new("example.com", IntelQueryType::Domain))
        .await;

    assert_eq!(report.entities.len(), 2);
    assert_eq!(report.relations.len(), 1);
    assert!(!report.partial);
    for entity in &report.entities {
        assert_eq!(entity.entity.source_count, 2);
        assert_eq!(entity.provenance.len(), 2);
        assert_eq!(entity.entity.confidence, IntelConfidence::High);
    }
    let relation = &report.relations[0];
    assert_eq!(relation.relation.source_count, 2);
    assert_eq!(relation.provenance.len(), 2);
    assert_ne!(
        relation.provenance[0].raw_record_id,
        relation.provenance[1].raw_record_id
    );
}

#[tokio::test]
async fn one_source_failure_returns_partial_success() {
    let pipeline = IntelligencePipeline::new(
        vec![
            Arc::new(StaticSource {
                id: "working",
                fail: false,
                hang: false,
            }),
            Arc::new(StaticSource {
                id: "broken",
                fail: true,
                hang: false,
            }),
        ],
        repository(),
    );

    let report = pipeline
        .expand(&IntelQuery::new("example.com", IntelQueryType::Domain))
        .await;

    assert!(report.partial);
    assert_eq!(report.successful_sources, vec!["working"]);
    assert_eq!(report.failed_sources, vec!["broken"]);
    assert!(!report.entities.is_empty());
}

#[tokio::test]
async fn pipeline_enforces_per_source_timeout() {
    let pipeline = IntelligencePipeline::new(
        vec![Arc::new(StaticSource {
            id: "hanging",
            fail: false,
            hang: true,
        })],
        repository(),
    )
    .with_source_timeout(Duration::from_millis(20));

    let report = pipeline
        .expand(&IntelQuery::new("example.com", IntelQueryType::Domain))
        .await;

    assert_eq!(report.failed_sources, vec!["hanging"]);
    assert!(report.source_results[0].errors[0].contains("timeout"));
}

#[tokio::test]
async fn global_timeout_keeps_completed_sources_and_marks_pending_sources_failed() {
    let pipeline = IntelligencePipeline::new(
        vec![
            Arc::new(StaticSource {
                id: "working",
                fail: false,
                hang: false,
            }),
            Arc::new(StaticSource {
                id: "hanging",
                fail: false,
                hang: true,
            }),
        ],
        repository(),
    )
    .with_source_timeout(Duration::from_secs(2))
    .with_global_timeout(Duration::from_millis(40));

    let report = pipeline
        .expand(&IntelQuery::new("example.com", IntelQueryType::Domain))
        .await;

    assert!(report.partial);
    assert_eq!(report.successful_sources, vec!["working"]);
    assert_eq!(report.failed_sources, vec!["hanging"]);
    assert_eq!(report.source_results.len(), 2);
    assert!(report.warnings[0].contains("global timeout"));
    assert!(!report.entities.is_empty());
}

#[test]
fn failed_batch_leaves_no_raw_entity_or_relation_orphans() {
    let repository = repository();
    let query = IntelQuery::new("example.com", IntelQueryType::Domain);
    let raw = IntelRawRecord::new("fixture", "record", query, json!({"fixture": true}));
    let batch = IntelIngestBatch {
        records: vec![raw.clone()],
        entities: vec![IntelEntityObservation {
            raw_record_id: raw.id.clone(),
            entity: IntelEntity::new(
                IntelEntityKind::Domain,
                "example.com",
                "example.com",
                IntelConfidence::High,
            ),
        }],
        relations: vec![IntelRelationObservation {
            raw_record_id: raw.id,
            from_kind: IntelEntityKind::Domain,
            from_normalized_value: "example.com".to_string(),
            relation: IntelRelationKind::References,
            to_kind: IntelEntityKind::Url,
            to_normalized_value: "https://missing.example.com".to_string(),
            confidence: IntelConfidence::Medium,
        }],
    };

    assert!(repository.ingest_intel_batch(&batch).is_err());
    assert!(
        repository
            .list_intel_raw_records(None, 10)
            .expect("raw list")
            .is_empty()
    );
    assert!(
        repository
            .list_intel_entities(None, None, 10)
            .expect("entity list")
            .is_empty()
    );
    assert!(
        repository
            .list_intel_relations(None)
            .expect("relation list")
            .is_empty()
    );
}
