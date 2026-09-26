//! Intelligence Hub API integration tests with in-process sources and SQLite.

#![cfg_attr(test, allow(clippy::expect_used))]

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use engines::tool_catalog::ToolInstallCoordinator;
use intelligence::source::{
    EntityDraft, IntelError, IntelligenceSource, NormalizedRecord, RelationDraft,
};
use models::{
    AuditDomain, IntelConfidence, IntelEntityKind, IntelQuery, IntelQueryType, IntelRawRecord,
    IntelRelationKind, IntelSourceCapabilities, IntelSourceResult, Mission, Project,
};
use runtime::ExecutionControlPlane;
use serde_json::{Value, json};
use storage::{Repository, SqliteRepository};
use tower::ServiceExt;

struct ApiSource {
    id: &'static str,
    fail: bool,
}

#[async_trait::async_trait]
impl IntelligenceSource for ApiSource {
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
        if self.fail {
            return Err(IntelError::Transport("fixture unavailable".to_string()));
        }
        Ok(IntelSourceResult::ok(
            self.id,
            vec![IntelRawRecord::new(
                self.id,
                format!("{}-record", self.id),
                query.clone(),
                json!({"secret_raw_fixture": self.id}),
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

fn test_app(directory: &std::path::Path) -> (Router, Arc<dyn Repository>, String) {
    let repository: Arc<dyn Repository> =
        Arc::new(SqliteRepository::open(":memory:").expect("repository"));
    let project = repository
        .create_project(&Project::new(
            "Intelligence fixture".to_string(),
            AuditDomain::AssetRecon,
        ))
        .expect("project");
    let mission = repository
        .create_mission(&Mission::new(
            project.id.clone(),
            "promote intelligence candidate".to_string(),
        ))
        .expect("mission");
    let manager = api::build_production_manager(Arc::clone(&repository)).expect("composition");
    let execution_control = Arc::new(
        ExecutionControlPlane::safe_local(Arc::clone(&repository)).expect("execution control"),
    );
    let tool_installs =
        Arc::new(ToolInstallCoordinator::new(directory.join("local-tools.json")).expect("tools"));
    let state = api::ApiState::with_services(
        manager,
        execution_control,
        tool_installs,
        directory.join("local-tools.json"),
        directory.join("uploads"),
        directory.join("missions"),
    )
    .with_intelligence_sources(vec![
        Arc::new(ApiSource {
            id: "source_a",
            fail: false,
        }),
        Arc::new(ApiSource {
            id: "source_b",
            fail: false,
        }),
        Arc::new(ApiSource {
            id: "source_failed",
            fail: true,
        }),
    ]);
    (
        api::router(state),
        repository,
        mission.id.as_str().to_string(),
    )
}

async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Body::from(
            body.map_or_else(String::new, |value| value.to_string()),
        ))
        .expect("request");
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn query_lists_deduped_records_and_reports_partial_source_failure() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (app, _repository, _mission_id) = test_app(directory.path());
    let (status, body) = call(
        &app,
        "POST",
        "/intelligence/query",
        Some(json!({"seed":"example.com","query_type":"domain","limit":20})),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "query response: {body}");
    assert_eq!(body["partial"], true);
    assert_eq!(body["entities"].as_array().expect("entities").len(), 2);
    assert_eq!(body["relations"].as_array().expect("relations").len(), 1);
    assert_eq!(
        body["relations"][0]["provenance"]
            .as_array()
            .expect("provenance")
            .len(),
        2
    );
    assert!(!body.to_string().contains("secret_raw_fixture"));

    let (_, entities) = call(&app, "GET", "/intelligence/entities", None).await;
    let (_, relations) = call(&app, "GET", "/intelligence/relations", None).await;
    assert_eq!(entities.as_array().expect("entities").len(), 2);
    assert_eq!(relations.as_array().expect("relations").len(), 1);
}

#[tokio::test]
async fn promotion_is_idempotent_and_never_creates_evidence_or_findings() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (app, repository, mission_id) = test_app(directory.path());
    let (_, query) = call(
        &app,
        "POST",
        "/intelligence/query",
        Some(json!({
            "seed":"example.com",
            "query_type":"domain",
            "source_ids":["source_a"],
            "limit":20
        })),
    )
    .await;
    let entity = query["entities"]
        .as_array()
        .expect("entities")
        .iter()
        .find(|entity| entity["kind"] == "domain")
        .expect("domain");
    let entity_id = entity["id"].as_str().expect("entity id");
    let path = format!("/intelligence/entities/{entity_id}/promote");

    let (first_status, first) = call(
        &app,
        "POST",
        &path,
        Some(json!({"mission_id": mission_id.clone()})),
    )
    .await;
    let (_, second) = call(&app, "POST", &path, Some(json!({"mission_id": mission_id}))).await;
    assert_eq!(first_status, StatusCode::OK, "promote: {first}");
    assert_eq!(first["asset"]["id"], second["asset"]["id"]);
    let assets = repository
        .list_mission_assets(Some(&mission_id), None, None, None)
        .expect("assets");
    assert_eq!(assets.len(), 1);
    assert!(assets[0].evidence_ids.is_empty());
    assert!(assets[0].finding_ids.is_empty());
    assert!(
        repository
            .list_evidence(assets[0].project_id.as_str())
            .expect("evidence")
            .is_empty()
    );
    assert!(
        repository
            .list_findings(assets[0].project_id.as_str())
            .expect("findings")
            .is_empty()
    );
}

#[tokio::test]
async fn invalid_seed_and_oversized_filters_are_rejected() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (app, _repository, _mission_id) = test_app(directory.path());
    let (invalid_status, _) = call(
        &app,
        "POST",
        "/intelligence/query",
        Some(json!({"seed":"not a domain","query_type":"domain","limit":20})),
    )
    .await;
    assert_eq!(invalid_status, StatusCode::UNPROCESSABLE_ENTITY);

    let (oversized_status, _) = call(
        &app,
        "POST",
        "/intelligence/query",
        Some(json!({
            "seed":"example.com",
            "query_type":"domain",
            "filters":{"fixture":"x".repeat(17 * 1024)},
            "limit":20
        })),
    )
    .await;
    assert_eq!(oversized_status, StatusCode::UNPROCESSABLE_ENTITY);
}
