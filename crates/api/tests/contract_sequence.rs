//! Rust-only replacement for the deleted `scripts/contract_parity.py`.
//!
//! The original 24-step deterministic sequence compared the Python
//! `TestClient` with the Rust axum API byte-for-byte. Python is gone, so the
//! same frozen sequence now locks the Rust runtime's wire contract:
//! per-step status codes, structural response invariants, and the
//! mission-start run-epoch idempotency guarantee.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use engines::default_solver_registry;
use engines::tool_catalog::ToolInstallCoordinator;
use runtime::{AuditManager, ExecutionControlPlane, InMemoryTaskBackend};
use storage::SqliteRepository;
use tower::ServiceExt;

type Json = serde_json::Value;

struct Sequence {
    app: Router,
}

impl Sequence {
    fn new(app: Router) -> Self {
        Self { app }
    }

    async fn call(&self, method: &str, path: &str, json: Option<String>) -> (StatusCode, Json) {
        let mut builder = Request::builder().method(method).uri(path);
        if json.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        let request = builder
            .body(Body::from(json.unwrap_or_default()))
            .unwrap_or_else(|error| panic!("request `{method} {path}` must build: {error}"));
        let response = self
            .app
            .clone()
            .oneshot(request)
            .await
            .unwrap_or_else(|error| panic!("`{method} {path}` must respond: {error}"));
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap_or_else(|error| panic!("`{method} {path}` body must read: {error}"));
        let body: Json = serde_json::from_slice(&bytes)
            .unwrap_or_else(|error| panic!("`{method} {path}` body must parse: {error}"));
        (status, body)
    }
}

/// 分支生成用的假 provider：恒返回一组通用 web 分支。
///
/// 分支生成改为 LLM 驱动后，契约序列同样必须显式提供 runtime，否则
/// mission start 会以 `ProviderConfigError` 失败关闭。
struct SequenceBranchStub;

#[async_trait::async_trait]
impl agents::llm::ProviderRuntime for SequenceBranchStub {
    async fn list_providers(
        &self,
    ) -> Result<Vec<models::provider::ProviderConfig>, agents::llm::ProviderCallError> {
        Ok(Vec::new())
    }

    async fn get_provider(
        &self,
        _provider_id: &str,
    ) -> Result<Option<models::provider::ProviderConfig>, agents::llm::ProviderCallError> {
        Ok(None)
    }

    async fn resolve_default_provider(
        &self,
    ) -> Result<Option<models::provider::ProviderConfig>, agents::llm::ProviderCallError> {
        Ok(None)
    }

    async fn health_check(
        &self,
        _provider_id: &str,
    ) -> Result<models::provider::ProviderHealthResult, agents::llm::ProviderCallError> {
        unreachable!("sequence branch stub never health-checks")
    }

    async fn generate_text(
        &self,
        _request: agents::llm::TextGenerationRequest<'_>,
    ) -> Result<agents::llm::LlmResponse, agents::llm::ProviderCallError> {
        unreachable!("sequence branch stub only serves structured calls")
    }

    async fn generate_structured(
        &self,
        _request: agents::llm::StructuredGenerationRequest<'_>,
    ) -> Result<serde_json::Map<String, serde_json::Value>, agents::llm::ProviderCallError> {
        let branch = |title: &str, kind: &str| {
            serde_json::json!({
                "title": title,
                "hypothesis": "The target exposes at least one untested assumption.",
                "rationale": "Grounding later branches.",
                "branch_kind": kind,
                "priority": 80,
                "confidence": 0.6
            })
        };
        serde_json::json!({ "branches": [
            branch("Map externally reachable routes", "url.surface_mapping"),
            branch("Trace untrusted input to sinks", "url.input_validation"),
        ]})
        .as_object()
        .cloned()
        .ok_or_else(|| agents::llm::ProviderCallError::new("stub branches must be an object"))
    }
}

fn test_app(directory: &std::path::Path) -> Router {
    let repository = Arc::new(
        SqliteRepository::open(":memory:")
            .unwrap_or_else(|error| panic!("in-memory repository must open: {error}")),
    );
    let manager = Arc::new(AuditManager::new(
        repository,
        default_solver_registry(),
        Arc::new(InMemoryTaskBackend::default()),
    ));
    manager.set_provider_runtime(Some(Arc::new(SequenceBranchStub)));
    let execution_control = Arc::new(
        ExecutionControlPlane::safe_local(Arc::clone(manager.repository()))
            .unwrap_or_else(|error| panic!("execution control must initialize: {error}")),
    );
    let tool_installs = Arc::new(
        ToolInstallCoordinator::new(directory.join("local-tools.json"))
            .unwrap_or_else(|error| panic!("tool coordinator must initialize: {error}")),
    );
    api::router(api::ApiState::with_services(
        manager,
        execution_control,
        tool_installs,
        directory.join("local-tools.json"),
        directory.join("uploads"),
        directory.join("missions"),
    ))
}

// The frozen sequence is one linear audit trail; splitting it into helpers
// would scatter the step numbers and weaken the contract report.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn frozen_contract_sequence_24_steps_stays_green() {
    let directory = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir: {error}"));
    let seq = Sequence::new(test_app(directory.path()));

    // Steps 01-02: health + capabilities.
    let (status, health) = seq.call("GET", "/health", None).await;
    assert_eq!(status, StatusCode::OK, "[01] /health");
    assert_eq!(health["status"], "ok");
    let (status, capabilities) = seq.call("GET", "/capabilities", None).await;
    assert_eq!(status, StatusCode::OK, "[02] /capabilities");
    assert!(capabilities.is_array(), "capabilities must be an array");

    // Steps 03-05: project CRUD.
    let (status, project) = seq
        .call(
            "POST",
            "/projects",
            Some(
                serde_json::json!({
                    "name": "parity-proj",
                    "audit_domain": "web_recon",
                    "target": {"domain": "example.test"},
                    "goal": "Audit example.test surface"
                })
                .to_string(),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "[03] POST /projects");
    let project_id = project["id"].as_str().expect("project id").to_string();
    let (status, fetched) = seq
        .call("GET", &format!("/projects/{project_id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "[04] GET /projects/{{id}}");
    assert_eq!(fetched["id"], project["id"]);
    assert_eq!(fetched["name"], "parity-proj");
    let (status, projects) = seq.call("GET", "/projects", None).await;
    assert_eq!(status, StatusCode::OK, "[05] GET /projects");
    assert!(
        projects
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["id"] == project["id"]))
    );

    // Steps 06-09: mission CRUD + branches.
    let (status, mission) = seq
        .call(
            "POST",
            "/missions",
            Some(
                serde_json::json!({
                    "project_id": project_id,
                    "user_goal": "Audit https://app.example.test/login",
                    "target": {"url": "https://app.example.test/login"},
                    "target_type": "url"
                })
                .to_string(),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "[06] POST /missions");
    let mission_id = mission["id"].as_str().expect("mission id").to_string();
    let (status, fetched) = seq
        .call("GET", &format!("/missions/{mission_id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "[07] GET /missions/{{id}}");
    assert_eq!(fetched["id"], mission["id"]);
    let (status, missions) = seq
        .call("GET", &format!("/missions?project_id={project_id}"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "[08] GET /missions?project_id");
    assert!(
        missions
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["id"] == mission["id"]))
    );
    let (status, branches) = seq
        .call("GET", &format!("/missions/{mission_id}/branches"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "[09] GET /missions/{{id}}/branches");
    assert!(branches.is_array(), "branches must be an array");

    // Steps 10-12: mission start idempotency reuses the same run epoch.
    let start_body = serde_json::json!({
        "auto_start_runtime": false,
        "config": {"web_exploit": {"engine": "native"}}
    })
    .to_string();
    let (status, first) = seq
        .call(
            "POST",
            &format!("/missions/{mission_id}/start"),
            Some(start_body.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "[10] start #1");
    let first_run_id = first["run"]["id"].as_str().expect("run id").to_string();
    let (status, second) = seq
        .call(
            "POST",
            &format!("/missions/{mission_id}/start"),
            Some(start_body),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "[11] start #2");
    let second_run_id = second["run"]["id"].as_str().expect("run id").to_string();
    assert_eq!(
        first_run_id, second_run_id,
        "[12] repeated mission start reuses the same run epoch"
    );

    // Steps 13-14: facts + findings.
    let (status, fact) = seq
        .call(
            "POST",
            &format!("/projects/{project_id}/facts"),
            Some(
                serde_json::json!({
                    "kind": "intake.plan",
                    "statement": "parity fact",
                    "data": {"source": "parity"},
                    "confidence": 0.9
                })
                .to_string(),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "[13] POST facts");
    assert_eq!(fact["kind"], "intake.plan");
    let (status, finding) = seq
        .call(
            "POST",
            &format!("/projects/{project_id}/findings"),
            Some(
                serde_json::json!({
                    "title": "parity finding",
                    "description": "found by the parity harness",
                    "severity": "low",
                    "rule_id": "parity.rule"
                })
                .to_string(),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "[14] POST findings");
    assert_eq!(finding["rule_id"], "parity.rule");

    // Steps 15-16: graph + events.
    let (status, graph) = seq
        .call("GET", &format!("/projects/{project_id}/graph"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "[15] GET graph");
    assert!(graph.is_object(), "graph must be an object");
    let (status, events) = seq
        .call("GET", &format!("/projects/{project_id}/events"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "[16] GET events");
    assert!(events.is_array(), "events must be an array");

    // Steps 17-18: error semantics.
    let (status, missing) = seq.call("GET", "/missions/mission_missing", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "[17] unknown mission");
    assert!(
        missing["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("mission not found")),
        "404 detail must preserve the contract message: {missing}"
    );
    let (status, invalid) = seq
        .call(
            "POST",
            "/missions",
            Some(serde_json::json!({"user_goal": ""}).to_string()),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "[18] blank goal");
    assert!(
        invalid["detail"]
            .as_array()
            .is_some_and(|issues| !issues.is_empty()),
        "422 detail must carry validation issues"
    );

    // Steps 19-20: intake analyze + create-project.
    let (status, plan) = seq
        .call(
            "POST",
            "/intake/analyze",
            Some(
                serde_json::json!({"prompt": "Audit https://app.example.test/login thoroughly"})
                    .to_string(),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "[19] POST /intake/analyze");
    assert!(plan["plan"].is_object(), "analyze must return a plan");
    let (status, intake) = seq
        .call(
            "POST",
            "/intake/create-project",
            Some(serde_json::json!({"plan": plan["plan"]}).to_string()),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "[20] POST /intake/create-project"
    );
    let intake_mission = intake["mission"]["id"].as_str().expect("intake mission id");
    assert!(!intake_mission.is_empty(), "intake must create a mission");

    // Steps 21-24: tool catalog configure / installations / unknown install.
    let (status, catalog) = seq.call("GET", "/tool-catalog", None).await;
    assert_eq!(status, StatusCode::OK, "[21] GET /tool-catalog");
    assert!(catalog.is_array(), "tool catalog must be an array");
    let (status, configured) = seq
        .call(
            "POST",
            "/tool-catalog/ffuf/configure",
            Some(
                serde_json::json!({"executable_path": "C:/Tools/ffuf.exe", "enabled": true})
                    .to_string(),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "[22] configure ffuf");
    assert!(
        configured.is_object(),
        "configure must echo the tool entry object"
    );
    let (status, installations) = seq.call("GET", "/tool-catalog/installations", None).await;
    assert_eq!(status, StatusCode::OK, "[23] GET installations");
    assert!(installations.is_array(), "installations must be an array");
    let (status, _unknown) = seq
        .call(
            "POST",
            "/tool-catalog/not-a-tool/install",
            Some(serde_json::json!({}).to_string()),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "[24] unknown tool install");
}
