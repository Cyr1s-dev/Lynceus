//! `/projects/{project_id}/agent-narratives` 契约回归：前端 Agent 叙事
//! 面板依赖该端点，缺失时任何任务页都报 404。锁定 GET 列表（run /
//! mission / branch 过滤 + limit）与 POST 服务端生成 `id`/`created_at`。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use engines::tool_catalog::ToolInstallCoordinator;
use runtime::{AuditManager, ExecutionControlPlane};
use serde_json::{Value, json};
use storage::{Repository, SqliteRepository};
use tower::ServiceExt;

fn test_app(directory: &std::path::Path) -> (Router, Arc<AuditManager>) {
    let repository: Arc<dyn Repository> =
        Arc::new(SqliteRepository::open(":memory:").expect("in-memory repository"));
    let manager = api::build_production_manager(repository).expect("production manager");
    let execution_control = Arc::new(
        ExecutionControlPlane::safe_local(Arc::clone(manager.repository()))
            .expect("execution control"),
    );
    let tool_installs = Arc::new(
        ToolInstallCoordinator::new(directory.join("local-tools.json")).expect("tool coordinator"),
    );
    let app = api::router(api::ApiState::with_services(
        manager.clone(),
        execution_control,
        tool_installs,
        directory.join("local-tools.json"),
        directory.join("uploads"),
        directory.join("missions"),
    ));
    (app, manager)
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    let request = if let Some(body) = body {
        builder.body(Body::from(body.to_string()))
    } else {
        builder.body(Body::empty())
    }
    .expect("request builds");
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, parsed)
}

fn narrative_payload(mission_id: &str, branch_id: Option<&str>, text: &str) -> Value {
    let mut payload = json!({
        "source_agent": "orchestrator",
        "event_kind": "progress",
        "original_text": text,
        "original_language": "zh-CN",
        "mission_id": mission_id,
        "metadata": {"phase": "startup"},
    });
    if let Some(branch_id) = branch_id {
        payload["branch_id"] = json!(branch_id);
    }
    payload
}

#[tokio::test]
async fn create_and_list_agent_narratives_roundtrip() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (app, _manager) = test_app(directory.path());

    let (status, created) = request(
        &app,
        "POST",
        "/projects/proj_narr/agent-narratives",
        Some(narrative_payload("mission_a", None, "任务已启动")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create must respond: {created}");
    // id / created_at 由服务端生成，调用方不可伪造。
    assert!(
        created["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("narr_")),
        "server-generated id: {created}"
    );
    assert!(
        created["created_at"]
            .as_str()
            .is_some_and(|at| !at.is_empty()),
        "server-generated created_at: {created}"
    );
    assert_eq!(created["source_agent"], "orchestrator");
    assert_eq!(created["event_kind"], "progress");
    assert_eq!(created["original_text"], "任务已启动");

    let (status, listed) = request(&app, "GET", "/projects/proj_narr/agent-narratives", None).await;
    assert_eq!(status, StatusCode::OK, "list must respond: {listed}");
    assert_eq!(listed.as_array().expect("array").len(), 1);

    // 未知 project 返回空列表（契约只有 200/422，无 404）。
    let (_, empty) = request(&app, "GET", "/projects/proj_other/agent-narratives", None).await;
    assert_eq!(empty.as_array().expect("array").len(), 0);
}

#[tokio::test]
async fn list_agent_narratives_filters_and_limit() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (app, _manager) = test_app(directory.path());
    for (branch, text) in [
        (None, "first"),
        (Some("branch_x"), "second"),
        (Some("branch_x"), "third"),
    ] {
        let (status, body) = request(
            &app,
            "POST",
            "/projects/proj_narr/agent-narratives",
            Some(narrative_payload("mission_a", branch, text)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "seed create: {body}");
    }

    // branch_id 内存过滤。
    let (_, filtered) = request(
        &app,
        "GET",
        "/projects/proj_narr/agent-narratives?branch_id=branch_x",
        None,
    )
    .await;
    let items = filtered.as_array().expect("array");
    assert_eq!(items.len(), 2, "branch filter: {filtered}");
    assert!(items.iter().all(|item| item["branch_id"] == "branch_x"));

    // mission_id 内存过滤 + limit 截断（seq 升序取前 N 条）。
    let (_, limited) = request(
        &app,
        "GET",
        "/projects/proj_narr/agent-narratives?mission_id=mission_a&limit=2",
        None,
    )
    .await;
    let items = limited.as_array().expect("array");
    assert_eq!(items.len(), 2, "limit: {limited}");
    assert_eq!(items[0]["original_text"], "first");

    // 不存在的 mission 过滤为空。
    let (_, none) = request(
        &app,
        "GET",
        "/projects/proj_narr/agent-narratives?mission_id=mission_z",
        None,
    )
    .await;
    assert_eq!(none.as_array().expect("array").len(), 0);
}

#[tokio::test]
async fn create_agent_narrative_rejects_unknown_fields() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (app, _manager) = test_app(directory.path());
    let mut payload = narrative_payload("mission_a", None, "note");
    payload["model_invocation_id"] = json!("model_forbidden");
    let (status, body) = request(
        &app,
        "POST",
        "/projects/proj_narr/agent-narratives",
        Some(payload),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "deny_unknown_fields must hold: {body}"
    );
}
