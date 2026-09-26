//! External Worker Runtime API 集成测试：
//!
//! 1. 未 attach worker registry 的独立 ApiState 下 `/worker-runtimes`
//!    返回 503（子系统未装配，不伪造可用性）；
//! 2. 组合根装配后（build_production_manager），端点返回四个显式
//!    runtime 的真实探测结论（本机未安装的如实显示 not_installed）；
//! 3. Profile 绑定 CRUD 与 wire 契约（未知 runtime type → 422）；
//! 4. `/worker-runs` 列表与详情（含 404）。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use storage::SqliteRepository;
use tower::ServiceExt;

fn production_app(directory: &std::path::Path) -> Router {
    let repository =
        Arc::new(SqliteRepository::open(directory.join("worker-api.sqlite3")).expect("repository"));
    let manager = api::build_production_manager(repository).expect("composition root");
    api::router(api::ApiState::new(manager).expect("api state"))
}

fn bare_app() -> Router {
    let repository = Arc::new(SqliteRepository::open(":memory:").expect("repository"));
    let manager = Arc::new(runtime::AuditManager::new(
        repository,
        engines::default_solver_registry(),
        Arc::new(runtime::InMemoryTaskBackend::default()),
    ));
    api::router(api::ApiState::new(manager).expect("api state"))
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
        .expect("request builds");
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn worker_runtimes_endpoint_lifecycle() {
    // global registry 是进程级单例（OnceLock 先到先得），因此 503→200
    // 的生命周期必须在同一个测试内顺序验证：未 attach → 503（子系统未
    // 装配，不伪造可用性）；组合根 attach 后 → 四个显式 adapter 的真实
    // 探测结论。真实 `--version` 探测语义由 engines 层 hermetic 测试锁定。
    let bare = bare_app();
    let (status, body) = call(&bare, "GET", "/worker-runtimes", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(
        body["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("worker runtime subsystem")),
        "{body}"
    );

    let directory = tempfile::tempdir().expect("temp dir");
    let repository: Arc<dyn storage::Repository> = Arc::new(
        SqliteRepository::open(directory.path().join("registry.sqlite3")).expect("repository"),
    );
    engines::worker::attach_global_registry(repository);
    let app = production_app(directory.path());
    let (status, body) = call(&app, "GET", "/worker-runtimes", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let probes = body.as_array().expect("probe array");
    assert_eq!(probes.len(), 4, "four explicit adapters, got {probes:?}");
    let mut runtimes: Vec<&str> = probes
        .iter()
        .filter_map(|probe| probe["runtime"].as_str())
        .collect();
    runtimes.sort_unstable();
    assert_eq!(
        runtimes,
        vec!["claude_code", "codex", "deepseek_harness", "pi"]
    );
    // 每个 probe 都携带 wire 合法的可用性值与探测时间戳。
    for probe in probes {
        assert!(
            [
                "available",
                "not_installed",
                "unavailable",
                "unsupported",
                "not_ready",
                "error"
            ]
            .contains(&probe["availability"].as_str().unwrap_or_default()),
            "{probe}"
        );
        assert!(probe["checked_at"].as_str().is_some(), "{probe}");
    }
}

#[tokio::test]
async fn worker_runtime_profile_crud_and_validation() {
    let directory = tempfile::tempdir().expect("temp dir");
    let app = production_app(directory.path());

    // 未知 runtime type → 422。
    let (status, _) = call(
        &app,
        "POST",
        "/worker-runtime-profiles",
        Some(json!({
            "runtime_type": "definitely_not_a_runtime",
            "connection_id": "prov_x"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // 空白 connection_id → 422（Profile 不变量）。
    let (status, _) = call(
        &app,
        "POST",
        "/worker-runtime-profiles",
        Some(json!({
            "runtime_type": "claude_code",
            "connection_id": "  "
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // 合法创建。
    let (status, body) = call(
        &app,
        "POST",
        "/worker-runtime-profiles",
        Some(json!({
            "runtime_type": "claude_code",
            "connection_id": "prov_anthropic",
            "model_override": "claude-opus-5"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["runtime_type"], "claude_code");
    assert_eq!(body["connection_id"], "prov_anthropic");
    assert_eq!(body["model_override"], "claude-opus-5");
    assert_eq!(body["execution_environment"], "local");
    let profile_id = body["id"].as_str().expect("profile id").to_string();

    // 列表可见。
    let (status, list) = call(&app, "GET", "/worker-runtime-profiles", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().map(Vec::len), Some(1));

    // 删除。
    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/worker-runtime-profiles/{profile_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // 重复删除 → 404。
    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/worker-runtime-profiles/{profile_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn worker_runs_list_and_detail_404() {
    let directory = tempfile::tempdir().expect("temp dir");
    let app = production_app(directory.path());
    let (status, runs) = call(&app, "GET", "/worker-runs", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(runs.as_array().map(Vec::len), Some(0));

    let (status, body) = call(&app, "GET", "/worker-runs/wkrun_missing", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        body["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("worker run")),
        "{body}"
    );
}
