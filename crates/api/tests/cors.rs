//! CORS 集成测试：frontend 与 API 不同源，跨域预检与响应头必须生效。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use engines::default_solver_registry;
use engines::tool_catalog::ToolInstallCoordinator;
use runtime::{AuditManager, ExecutionControlPlane, InMemoryTaskBackend};
use storage::SqliteRepository;
use tower::ServiceExt;

fn app() -> axum::Router {
    let repository = Arc::new(
        SqliteRepository::open(":memory:")
            .unwrap_or_else(|error| panic!("in-memory repository must open: {error}")),
    );
    let manager = Arc::new(AuditManager::new(
        repository,
        default_solver_registry(),
        Arc::new(InMemoryTaskBackend::default()),
    ));
    let execution_control = Arc::new(
        ExecutionControlPlane::safe_local(Arc::clone(manager.repository()))
            .unwrap_or_else(|error| panic!("execution control must initialize: {error}")),
    );
    let tool_installs = Arc::new(
        ToolInstallCoordinator::new(std::env::temp_dir().join("cors-test-local-tools.json"))
            .unwrap_or_else(|error| panic!("tool coordinator must initialize: {error}")),
    );
    let root = std::env::temp_dir().join("api-cors-test");
    api::router(api::ApiState::with_services(
        manager,
        execution_control,
        tool_installs,
        root.join("local-tools.json"),
        root.join("uploads"),
        root.join("missions"),
    ))
}

async fn preflight(origin: &str) -> (StatusCode, Option<String>) {
    let request = Request::builder()
        .method("OPTIONS")
        .uri("/health")
        .header(header::ORIGIN, origin)
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("preflight request must build: {error}"));
    let response = app()
        .oneshot(request)
        .await
        .unwrap_or_else(|error| panic!("preflight must respond: {error}"));
    let status = response.status();
    let allowed = response
        .headers()
        .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    (status, allowed)
}

#[tokio::test]
async fn preflight_from_dev_console_is_allowed() {
    for origin in ["http://127.0.0.1:5173", "http://localhost:5173"] {
        let (status, allowed) = preflight(origin).await;
        assert_eq!(status, StatusCode::OK, "preflight for {origin} must pass");
        assert_eq!(
            allowed.as_deref(),
            Some(origin),
            "allow-origin must echo {origin}"
        );
    }
}

#[tokio::test]
async fn preflight_from_unknown_origin_is_rejected() {
    let (_, allowed) = preflight("http://evil.example").await;
    assert!(
        allowed.is_none(),
        "unknown origin must not receive allow-origin"
    );
}

#[tokio::test]
async fn health_response_carries_allow_origin_for_console() {
    let request = Request::builder()
        .uri("/health")
        .header(header::ORIGIN, "http://127.0.0.1:5173")
        .body(Body::empty())
        .unwrap_or_else(|error| panic!("health request must build: {error}"));
    let response = app()
        .oneshot(request)
        .await
        .unwrap_or_else(|error| panic!("health must respond: {error}"));
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|value| value.to_str().ok()),
        Some("http://127.0.0.1:5173")
    );
}
