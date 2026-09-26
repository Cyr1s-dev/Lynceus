//! Tool Catalog 探测拆分回归测试（spec PART 4/7）：
//!
//! 改动前：`GET /tool-catalog` 每次全量扫描 PATH（curated catalog 全量工具
//! × 候选名 × PATHEXT）并为 httpx spawn `-h` 身份校验进程，页面打开即卡顿。
//! 改动后语义由本文件锁定：
//!
//! 1. list/status 绝不触发探测（无快照时如实 Missing + stale，且
//!    **不创建**快照文件——创建文件即证明发生了探测/写盘）；
//! 2. `POST /tool-catalog/refresh` 是唯一全量探测入口，结果持久化；
//! 3. 快照可被后续 list 复用（mtime 不变 = 没有重复探测/写盘）；
//! 4. configure 事件驱动失效：配置后该工具的 detection 立即反映到
//!    list 与快照，无需全量 refresh。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use engines::default_solver_registry;
use engines::tool_catalog::ToolInstallCoordinator;
use runtime::{AuditManager, ExecutionControlPlane, InMemoryTaskBackend};
use serde_json::{Value, json};
use storage::SqliteRepository;
use tower::ServiceExt;

/// curated catalog 的规模下限。真值是 `resources/tool-catalog/curated_tools.yaml`
/// （当前 17 条，docs 的 tool_catalog 计数与之同步）；这里取下整到十位，
/// 增删个别工具不必改测试，但 catalog 被抽空会立刻红。
const MIN_CATALOG_TOOLS: usize = 10;

fn test_app(directory: &std::path::Path) -> Router {
    let repository = Arc::new(SqliteRepository::open(":memory:").expect("in-memory repository"));
    let manager = Arc::new(AuditManager::new(
        repository,
        default_solver_registry(),
        Arc::new(InMemoryTaskBackend::default()),
    ));
    let execution_control = Arc::new(
        ExecutionControlPlane::safe_local(Arc::clone(manager.repository()))
            .expect("execution control"),
    );
    let tool_installs = Arc::new(
        ToolInstallCoordinator::new(directory.join("local-tools.json")).expect("tool coordinator"),
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

async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Body::from(body.map_or_default(|v| v.to_string())))
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

fn snapshot_file(directory: &std::path::Path) -> std::path::PathBuf {
    directory.join("tool-detection.json")
}

fn catalog_entry<'a>(entries: &'a [Value], id: &str) -> &'a Value {
    entries
        .iter()
        .find(|entry| entry["id"] == id)
        .unwrap_or_else(|| panic!("catalog entry {id} exists"))
}

#[tokio::test]
async fn list_and_status_never_trigger_detection() {
    let directory = tempfile::tempdir().expect("tempdir");
    let app = test_app(directory.path());

    let (status, entries) = call(&app, "GET", "/tool-catalog", None).await;
    assert_eq!(status, StatusCode::OK);
    let entries = entries.as_array().expect("entries array");
    assert!(
        entries.len() >= MIN_CATALOG_TOOLS,
        "metadata always present: {}",
        entries.len()
    );
    // 无快照：所有未配置条目如实 Unknown（list 没有偷偷探测）。
    assert!(
        entries
            .iter()
            .filter(|entry| entry["detection"]["availability"] != "configured")
            .all(|entry| entry["detection"]["availability"] == "unknown"),
        "list must not perform PATH detection"
    );

    let (status, body) = call(&app, "GET", "/tool-catalog/status", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["stale"], true);
    assert_eq!(body["state"], "unknown");
    assert_eq!(body["detected_at"], Value::Null);

    // 创建快照文件 == 发生了探测或写盘：list/status 后文件必须不存在。
    assert!(
        !snapshot_file(directory.path()).exists(),
        "list/status must never write the detection snapshot"
    );
}

#[tokio::test]
async fn refresh_runs_detection_and_persists_snapshot() {
    let directory = tempfile::tempdir().expect("tempdir");
    let app = test_app(directory.path());

    let (status, body) = call(&app, "POST", "/tool-catalog/refresh", None).await;
    assert_eq!(status, StatusCode::OK, "refresh responds: {body}");
    assert!(body["detected_at"].is_string(), "detected_at recorded");
    assert_eq!(body["state"], "ready");
    assert!(
        body["detection_count"].as_u64().expect("count") >= MIN_CATALOG_TOOLS as u64,
        "refresh covers the whole catalog"
    );
    assert!(
        snapshot_file(directory.path()).exists(),
        "snapshot persisted"
    );

    let (status, body) = call(&app, "GET", "/tool-catalog/status", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["stale"], false);
    assert_eq!(body["state"], "ready");
    assert!(body["detected_at"].is_string());

    // list 复用快照：configured 之外，availability 分布与快照一致
    //（PATH 上碰巧存在的工具以 path 出现是真实探测结果，不断言具
    // 体值；这里锁定「list 之后快照 mtime 不变」）。
    let mtime_before = std::fs::metadata(snapshot_file(directory.path()))
        .and_then(|meta| meta.modified())
        .expect("mtime");
    let (status, _) = call(&app, "GET", "/tool-catalog", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, "GET", "/tool-catalog", None).await;
    assert_eq!(status, StatusCode::OK);
    let mtime_after = std::fs::metadata(snapshot_file(directory.path()))
        .and_then(|meta| meta.modified())
        .expect("mtime");
    assert_eq!(
        mtime_before, mtime_after,
        "cached state survives normal page reads (no re-detection on list)"
    );
}

#[tokio::test]
async fn configure_invalidates_detection_immediately() {
    let directory = tempfile::tempdir().expect("tempdir");
    let app = test_app(directory.path());

    let (status, entry) = call(
        &app,
        "POST",
        "/tool-catalog/semgrep/configure",
        Some(json!({"executable_path": "C:/fake/semgrep.exe", "enabled": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "configure responds: {entry}");
    assert_eq!(entry["detection"]["availability"], "configured");

    // list 立即可见（live configured detection，无需 refresh）。
    let (status, entries) = call(&app, "GET", "/tool-catalog", None).await;
    assert_eq!(status, StatusCode::OK);
    let entries = entries.as_array().expect("entries");
    let semgrep = catalog_entry(entries, "semgrep");
    assert_eq!(semgrep["detection"]["availability"], "configured");
    assert_eq!(
        semgrep["detection"]["executable_path"],
        "C:/fake/semgrep.exe"
    );

    // 快照同步包含该条（configure 事件驱动失效已落盘）。
    let snapshot: Value = serde_json::from_str(
        &std::fs::read_to_string(snapshot_file(directory.path())).expect("snapshot exists"),
    )
    .expect("snapshot parses");
    let record = snapshot["detections"]
        .as_array()
        .expect("detections")
        .iter()
        .find(|record| record["tool_id"] == "semgrep")
        .expect("semgrep in snapshot");
    assert_eq!(record["detection"]["availability"], "configured");
}
