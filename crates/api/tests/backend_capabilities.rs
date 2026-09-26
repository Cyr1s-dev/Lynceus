//!  后端能力移植的端到端 HTTP 契约测试。
//!
//! 这些能力此前在本仓都是「前端降级实现」：发现状态只读、工具调用的 Worker
//! 列拿 module_id 冒充、覆盖图在前端现算、报告在前端现拼 Markdown、复测存
//! localStorage。现在它们都在 Rust 后端，但这个测试的目的不是重复单元测试——
//! 而是**通过真实 axum 路由**证明：路由挂对了、序列化对得上、状态码符合契约、
//! 边界条件真的被拦住。
//!
//! 覆盖：
//! 1. `PATCH /projects/{pid}/findings/{fid}` —— 发现状态从只读变成可写，
//!    指针语义 + 422/404 边界 + 状态机不变量（`confirmed` 无证据必须被拒）；
//! 2. `GET /missions/{mid}/coverage-graph` —— 覆盖图由后端推导；
//! 3. `GET /reports/{report_id}` —— 报告后端生成，`not_ready` 信封、下载头、
//!    以及未实现的格式被 422 拒绝（不对外宣称支持 sarif/html）；
//! 4. `POST /missions/{mid}/findings/{fid}/retests` —— 复测落库、空上下文
//!    fail-closed、无 worker runtime 时也 fail-closed，以及记录列表/未收口列表。
//!
//! 复测的正常收口路径需要一个真实 worker runtime，那部分由
//! `crates/runtime/finding_retests.rs` 的状态机单测覆盖。

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

/// 发一次请求并返回 `(status, body)`；body 解析不了时退化成字符串。
async fn raw_call(
    app: &Router,
    method: &str,
    path: &str,
    json: Option<Json>,
) -> (StatusCode, Json) {
    let body = json
        .as_ref()
        .map(|value| value.to_string())
        .unwrap_or_default();
    let mut builder = Request::builder().method(method).uri(path);
    if json.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let request = builder
        .body(Body::from(body))
        .unwrap_or_else(|error| panic!("request `{method} {path}` must build: {error}"));
    let response = app
        .clone()
        .oneshot(request)
        .await
        .unwrap_or_else(|error| panic!("`{method} {path}` must respond: {error}"));
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap_or_else(|error| panic!("`{method} {path}` body must read: {error}"));
    let text = String::from_utf8_lossy(&bytes).to_string();
    let body = serde_json::from_str::<Json>(&text).unwrap_or(Json::String(text));
    (status, body)
}

struct Fixture {
    app: Router,
    manager: Arc<AuditManager>,
}

fn test_app(directory: &std::path::Path) -> Fixture {
    let repository: Arc<dyn storage::Repository> = Arc::new(
        SqliteRepository::open(":memory:")
            .unwrap_or_else(|error| panic!("in-memory repository must open: {error}")),
    );
    let manager = Arc::new(AuditManager::new(
        Arc::clone(&repository),
        default_solver_registry(),
        Arc::new(InMemoryTaskBackend::default()),
    ));
    let execution_control = Arc::new(
        ExecutionControlPlane::safe_local(Arc::clone(&manager.repository()))
            .unwrap_or_else(|error| panic!("execution control must initialize: {error}")),
    );
    let tool_installs = Arc::new(
        ToolInstallCoordinator::new(directory.join("local-tools.json"))
            .unwrap_or_else(|error| panic!("tool coordinator must initialize: {error}")),
    );
    let app = api::router(api::ApiState::with_services(
        Arc::clone(&manager),
        execution_control,
        tool_installs,
        directory.join("local-tools.json"),
        directory.join("uploads"),
        directory.join("missions"),
    ));
    Fixture { app, manager }
}

/// 建 Project + Mission，返回两个 id。
async fn seed_project_mission(app: &Router) -> (String, String) {
    let (status, project) = raw_call(
        app,
        "POST",
        "/projects",
        Some(serde_json::json!({
            "name": "cap-proj",
            "audit_domain": "web_recon",
            "target": {"domain": "example.test"},
            "goal": "Exercise the ported capabilities"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "POST /projects");
    let project_id = project["id"].as_str().expect("project id").to_string();

    let (status, mission) = raw_call(
        app,
        "POST",
        "/missions",
        Some(serde_json::json!({
            "project_id": project_id,
            "user_goal": "Audit https://app.example.test/login",
            "target": {"url": "https://app.example.test/login"},
            "target_type": "url"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "POST /missions");
    let mission_id = mission["id"].as_str().expect("mission id").to_string();

    (project_id, mission_id)
}

/// 建一条**带证据**的 Finding（`confirmed` 状态要求有证据，这是
/// `Finding::validated()` 的不变量）。
async fn seed_finding_with_evidence(app: &Router, project_id: &str) -> (String, String) {
    let (status, evidence) = raw_call(
        app,
        "POST",
        &format!("/projects/{project_id}/evidence"),
        Some(serde_json::json!({
            "kind": "taint_path",
            "summary": "request.body reaches eval()",
            "content": {"snippet": "eval(request.body['q'])"}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "POST evidence");
    let evidence_id = evidence["id"].as_str().expect("evidence id").to_string();

    let (status, finding) = raw_call(
        app,
        "POST",
        &format!("/projects/{project_id}/findings"),
        Some(serde_json::json!({
            "title": "Eval injection in /login",
            "description": "request.body reaches eval()",
            "severity": "high",
            "rule_id": "web_sast.eval_injection",
            "source_label": "request.body",
            "sink_label": "eval()",
            "evidence_ids": [evidence_id]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "POST findings");
    let finding_id = finding["id"].as_str().expect("finding id").to_string();

    (finding_id, evidence_id)
}

/// 直接从仓储挂一条**属于该 Mission** 的 Finding。
///
/// `POST /projects/{pid}/findings` 建的是项目级手工 Finding，不带
/// `mission_id`；而复测端点按 Mission 收窄（同规则），所以这里走仓储
/// 补上归属。这不是绕过 API——只是把「任务跑出来的 Finding」这件事在测试里
/// 摆出来。
async fn seed_mission_finding(
    fixture: &Fixture,
    project_id: &str,
    mission_id: &str,
) -> (String, String) {
    let (finding_id, evidence_id) =
        seed_finding_with_evidence(&fixture.app, project_id).await;
    let mut finding = fixture
        .manager
        .repository()
        .get_finding(&finding_id)
        .expect("read must work")
        .expect("finding must exist");
    finding.mission_id = Some(models::MissionId::new(mission_id.to_string()));
    let stored = fixture
        .manager
        .repository()
        .update_finding(&finding)
        .expect("mission binding must persist");
    (stored.id.as_str().to_string(), evidence_id)
}

/// 直接从仓储挂一条**属于该 Mission 的裸 Finding**（无任何证据/事实/分支/
/// 工具调用），用来验证空上下文下的 fail-closed 行为。
async fn seed_bare_mission_finding(
    manager: &AuditManager,
    project_id: &str,
    mission_id: &str,
) -> String {
    let mut finding = models::Finding::new(
        models::ProjectId::new(project_id.to_string()),
        "Bare finding with no evidence".to_string(),
    );
    finding.severity = models::Severity::Info;
    finding.mission_id = Some(models::MissionId::new(mission_id.to_string()));
    let stored = manager
        .repository()
        .add_finding(&finding)
        .expect("bare finding must persist");
    stored.id.as_str().to_string()
}

/* ─────────────── 1. 发现状态可写 ─────────────── */

#[tokio::test]
async fn finding_status_is_writable_through_the_api() {
    let directory = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir: {error}"));
    let fixture = test_app(directory.path());
    let app = &fixture.app;
    let (project_id, _mission_id) = seed_project_mission(app).await;
    let (finding_id, _evidence_id) = seed_finding_with_evidence(app, &project_id).await;

    let patch = |body: Json| {
        let path = format!("/projects/{project_id}/findings/{finding_id}");
        let app = app.clone();
        async move { raw_call(&app, "PATCH", &path, Some(body)).await }
    };

    // 只改 status：指针语义，severity 不动。
    let (status, patched) = patch(serde_json::json!({ "status": "fixed" })).await;
    assert_eq!(status, StatusCode::OK, "PATCH finding status");
    assert_eq!(patched["status"], "fixed", "状态必须真的写进去");
    assert_eq!(patched["severity"], "high", "没传的字段不能被抹掉");

    // 只改 severity：反向指针语义。
    let (status, patched) = patch(serde_json::json!({ "severity": "critical" })).await;
    assert_eq!(status, StatusCode::OK, "PATCH finding severity");
    assert_eq!(patched["severity"], "critical");
    assert_eq!(patched["status"], "fixed", "没传的字段不能被抹掉");

    // 两个都改。
    let (status, patched) =
        patch(serde_json::json!({ "status": "confirmed", "severity": "medium" })).await;
    assert_eq!(status, StatusCode::OK, "PATCH status+severity");
    assert_eq!(patched["status"], "confirmed");
    assert_eq!(patched["severity"], "medium");

    // 写路径真的落了库：重新读回来，不是只在响应里好看。
    let (status, list) = raw_call(
        app,
        "GET",
        &format!("/projects/{project_id}/findings"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "GET findings");
    let reread = list
        .as_array()
        .expect("findings must be an array")
        .iter()
        .find(|item| item["id"] == finding_id.as_str())
        .unwrap_or_else(|| panic!("finding {finding_id} must be listed"));
    assert_eq!(reread["status"], "confirmed", "状态必须已落库");
    assert_eq!(reread["severity"], "medium", "严重度必须已落库");

    // 空 body：至少一个字段是硬要求。
    let (status, _empty) = patch(serde_json::json!({})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "空 PATCH 必须 422");

    // 未知 status 值：值校验，不能静默接受。
    let (status, _bad) = patch(serde_json::json!({ "status": "not-a-status" })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "非法 status 必须 422");

    // 状态机不变量：`confirmed` 要求有证据。这是 Finding::validated() 的规则，
    // 不是橡皮图章——无证据的 finding 不许被人工推到 confirmed。
    let (status, evidence_less) = raw_call(
        app,
        "POST",
        &format!("/projects/{project_id}/findings"),
        Some(serde_json::json!({ "title": "No evidence yet", "severity": "low" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let bare_id = evidence_less["id"].as_str().expect("finding id");
    let (status, rejected) = raw_call(
        app,
        "PATCH",
        &format!("/projects/{project_id}/findings/{bare_id}"),
        Some(serde_json::json!({ "status": "confirmed" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "无证据的 finding 不许进 confirmed：{rejected}"
    );

    // 不存在的 finding：404。
    let (status, _missing) = raw_call(
        app,
        "PATCH",
        &format!("/projects/{project_id}/findings/find_missing"),
        Some(serde_json::json!({ "status": "fixed" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "未知 finding 必须 404");

    // finding 不属于该 project：404（不能跨项目改）。
    let (other_project_id, _other_mission) = seed_project_mission(app).await;
    let (status, _cross) = raw_call(
        app,
        "PATCH",
        &format!("/projects/{other_project_id}/findings/{finding_id}"),
        Some(serde_json::json!({ "status": "fixed" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "跨项目 triage 必须 404");
}

/* ─────────────── 2. 覆盖图走后端 ─────────────── */

#[tokio::test]
async fn coverage_graph_is_served_by_the_backend() {
    let directory = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir: {error}"));
    let fixture = test_app(directory.path());
    let app = &fixture.app;
    let (project_id, mission_id) = seed_project_mission(app).await;
    let (_finding_id, _evidence_id) = seed_finding_with_evidence(app, &project_id).await;

    let (status, graph) = raw_call(
        app,
        "GET",
        &format!("/missions/{mission_id}/coverage-graph"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "GET coverage-graph");

    // 结构：Mission 根节点 + 后端推导的边 + 汇总统计。
    assert_eq!(graph["mission_id"], mission_id);
    let nodes = graph["nodes"].as_array().expect("nodes must be an array");
    assert!(!nodes.is_empty(), "至少要有一个 Mission 根节点");
    assert_eq!(
        nodes[0]["kind"], "mission",
        "首节点必须是 Mission 根：{}",
        nodes[0]
    );
    assert!(graph["edges"].is_array(), "edges must be an array");
    assert!(graph["stats"].is_object(), "stats must be an object");
    // stats.total 刻意不含 Mission 根。
    assert_eq!(
        graph["stats"]["total"].as_u64().expect("total"),
        nodes.len() as u64 - 1,
        "stats.total 必须等于非根节点数"
    );
    // 没有资产时一切都是未测——覆盖图宁可少算不可虚报。
    assert_eq!(graph["stats"]["tested"].as_u64().expect("tested"), 0);
    assert_eq!(nodes[0]["tested"], false, "Mission 根永远不是已测节点");

    // 未知 Mission：404。
    let (status, _missing) = raw_call(
        app,
        "GET",
        "/missions/mission_missing/coverage-graph",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/* ─────────────── 3. 报告后端生成 ─────────────── */

#[tokio::test]
async fn report_is_generated_by_the_backend() {
    let directory = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir: {error}"));
    let fixture = test_app(directory.path());
    let app = &fixture.app;
    let (project_id, mission_id) = seed_project_mission(app).await;
    // 任务级报告只收属于该 Mission 的 Finding，所以这里挂上归属。
    let (finding_id, _evidence_id) =
        seed_mission_finding(&fixture, &project_id, &mission_id).await;

    // 默认只收 confirmed；这条 finding 还是 candidate，所以先是 0。
    let (status, before) = raw_call(&app, "GET", &format!("/reports/{mission_id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        before["payload"]["finding_total"], 0,
        "默认只收 confirmed，candidate 不算"
    );

    // 用 triage 端点把它推到 confirmed（有证据，不变量过得去）。
    let (status, triaged) = raw_call(
        app,
        "PATCH",
        &format!("/projects/{project_id}/findings/{finding_id}"),
        Some(serde_json::json!({ "status": "confirmed" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "PATCH to confirmed");
    assert_eq!(triaged["status"], "confirmed");

    // Markdown：报告由后端生成，不是前端现拼的。
    let (status, report) = raw_call(
        app,
        "GET",
        &format!("/reports/{mission_id}?format=markdown"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "GET report markdown");
    assert_eq!(report["status"], "ready");
    assert_eq!(report["id"], mission_id, "report_id 必须回显");
    assert_eq!(
        report["available_formats"],
        serde_json::json!(["json", "markdown"]),
        "只能声明真的实现了的格式"
    );
    let payload = &report["payload"];
    assert_eq!(payload["scope"], "mission");
    assert_eq!(payload["id"], mission_id);
    assert_eq!(
        payload["finding_total"], 1,
        "报告必须汇入真实落库的发现"
    );
    assert_eq!(payload["findings"][0]["id"], finding_id);
    assert_eq!(payload["findings"][0]["title"], "Eval injection in /login");
    let markdown = report["markdown"].as_str().expect("markdown body");
    assert!(markdown.starts_with("# "), "Markdown 必须以标题开头");
    assert!(
        markdown.contains("Eval injection in /login"),
        "Markdown 必须含发现标题"
    );
    assert!(markdown.contains("| 严重度"), "Markdown 必须含严重度表");

    // JSON：同一份 payload，只是表示形式不同。
    let (status, json_report) =
        raw_call(app, "GET", &format!("/reports/{mission_id}?format=json"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_report["payload"]["finding_total"], 1);

    // include_statuses=all：口径可调，但不悄悄改默认。
    let (status, all) = raw_call(
        app,
        "GET",
        &format!("/reports/{mission_id}?format=json&include_statuses=all"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(all["payload"]["finding_total"], 1);

    // download=true：裸 Markdown + Content-Disposition。
    let request = Request::builder()
        .method("GET")
        .uri(format!(
            "/reports/{mission_id}?format=markdown&download=true"
        ))
        .body(Body::empty())
        .expect("download request must build");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("download must respond");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get("content-disposition")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("attachment")),
        "download=true 必须带 Content-Disposition"
    );
    let bytes = to_bytes(response.into_body(), usize::MAX).await.expect("body");
    assert!(
        String::from_utf8_lossy(&bytes).starts_with("# "),
        "下载体必须是裸 Markdown"
    );

    // 未实现的格式：422，不对外宣称支持 sarif/html。
    let (status, rejected) =
        raw_call(app, "GET", &format!("/reports/{mission_id}?format=sarif"), None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "sarif 未实现必须 422");
    assert!(
        rejected["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("unsupported format: sarif")
                && detail.contains("supported: json, markdown")),
        "422 必须说明原因并列出的确支持的形式：{rejected}"
    );

    // report_id 解析不出实体：永不 404，返回 not_ready 信封。
    let (status, not_ready) = raw_call(app, "GET", "/reports/report_bogus", None).await;
    assert_eq!(status, StatusCode::OK, "契约要求这个端点永不 404");
    assert_eq!(not_ready["status"], "not_ready");
    assert!(
        not_ready["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "not_ready 必须带原因"
    );
    assert!(
        not_ready["payload"].is_null(),
        "not_ready 时不得伪造 payload"
    );

    // 按 Project 解析也能出报告。
    let (status, project_report) =
        raw_call(app, "GET", &format!("/reports/{project_id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(project_report["payload"]["scope"], "project");
    assert_eq!(project_report["payload"]["id"], project_id);
}

/* ─────────────── 5. 工具调用的 Worker 维度 ─────────────── */

#[tokio::test]
async fn tool_invocation_carries_worker_id_as_a_first_class_field() {
    // 这一项在单元层已经锁定（ToolInvocation.worker_id 的 wire parity 测试 +
    // broker 从 ExecutionScope 透传的测试），这里再从**契约**角度确认一次：
    // 前端 Worker 列读的是 `worker_id` 字段，不是 `metadata.worker_id`，
    // 所以契约里必须真的有这个字段，而且必须在 required 里。
    let contract: Json = serde_json::from_str(include_str!("../../../contracts/openapi.json"))
        .expect("contract must parse");
    let invocation = &contract["components"]["schemas"]["ToolInvocation"];
    assert!(
        invocation["properties"]["worker_id"].is_object(),
        "ToolInvocation 必须有 worker_id 字段：{invocation}"
    );
    assert!(
        invocation["required"]
            .as_array()
            .is_some_and(|required| required.iter().any(|item| item == "worker_id")),
        "worker_id 必须在 required 里——它不是可选装饰"
    );
    // module_id 仍然在（"哪个模块提供工具"和"谁点的按钮"是两件事）。
    assert!(
        invocation["properties"]["module_id"].is_object(),
        "module_id 不能因为加了 worker_id 就消失"
    );
}

/* ─────────────── 4. 复测走后端 ─────────────── */

#[tokio::test]
async fn retest_without_a_worker_runtime_fails_closed_but_persists() {
    let directory = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir: {error}"));
    let fixture = test_app(directory.path());
    let app = &fixture.app;
    let (project_id, mission_id) = seed_project_mission(app).await;
    let (finding_id, evidence_id) = seed_mission_finding(&fixture, &project_id, &mission_id).await;

    // 这条 finding 有证据，所以只读上下文非空；但测试 app 没装 worker runtime，
    // 复测必须 fail-closed（failed + 原因），而不是挂起或假装成功。
    let (status, record) = raw_call(
        app,
        "POST",
        &format!("/missions/{mission_id}/findings/{finding_id}/retests"),
        Some(serde_json::json!({ "notes": "验证一下修复是否生效" })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "POST retest");
    assert_eq!(record["status"], "failed", "没有 worker runtime 必须判失败");
    assert_eq!(record["finding_id"], finding_id);
    assert_eq!(record["notes"], "验证一下修复是否生效", "补充说明必须落库");
    assert_eq!(
        record["context_source"], "inline_snapshot_only",
        "没拿到只读 grant 时必须如实记录上下文来源"
    );
    assert!(
        record["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()),
        "失败原因必须说清楚：{record}"
    );
    assert!(
        record["verdict"].as_str().is_some_and(str::is_empty),
        "失败的复测不得带 verdict"
    );
    let retest_id = record["id"].as_str().expect("retest id").to_string();

    // 失败的复测绝不改写漏洞状态。
    let (status, finding) = raw_call(
        app,
        "GET",
        &format!("/projects/{project_id}/findings"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let stored = finding
        .as_array()
        .expect("findings must be an array")
        .iter()
        .find(|item| item["id"] == finding_id.as_str())
        .expect("finding must be listed");
    assert_eq!(stored["status"], "candidate", "失败的复测不能改漏洞状态");

    // 收口后可以再发起一条（"同时只能一条"只管未收口的）。
    let (status, second) = raw_call(
        app,
        "POST",
        &format!("/missions/{mission_id}/findings/{finding_id}/retests"),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_ne!(
        record["id"], second["id"],
        "上一条已收口，必须能开新的一条"
    );

    // 记录列表：新的在前，且只含这条 finding 的。
    let (status, records) = raw_call(
        app,
        "GET",
        &format!("/missions/{mission_id}/findings/{finding_id}/retests"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "GET retests");
    let records = records.as_array().expect("records must be an array");
    assert_eq!(records.len(), 2, "两次 POST 都必须落库");
    assert_eq!(records[0]["id"], second["id"], "新的在前");
    assert!(
        records
            .iter()
            .all(|record| record["finding_id"] == finding_id.as_str())
    );
    assert!(
        records
            .iter()
            .any(|record| record["id"] == retest_id.as_str()),
        "第一条记录必须可查"
    );

    // 未收口列表：两条都收口了，应为空。
    let (status, active) = raw_call(
        app,
        "GET",
        &format!("/missions/{mission_id}/retests/active"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "GET active retests");
    assert_eq!(
        active.as_array().map(Vec::len).unwrap_or(0),
        0,
        "没有未收口复测时 active 必须为空数组"
    );

    // notes 超长：422。
    let (status, _too_long) = raw_call(
        app,
        "POST",
        &format!("/missions/{mission_id}/findings/{finding_id}/retests"),
        Some(serde_json::json!({ "notes": "x".repeat(4001) })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "notes 超长必须 422");

    let _ = evidence_id;
}

#[tokio::test]
async fn retest_of_a_bare_finding_is_refused_before_any_worker_runs() {
    let directory = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir: {error}"));
    let fixture = test_app(directory.path());
    let app = &fixture.app;
    let (project_id, mission_id) = seed_project_mission(app).await;
    let bare_id = seed_bare_mission_finding(&fixture.manager, &project_id, &mission_id).await;

    let (status, record) = raw_call(
        app,
        "POST",
        &format!("/missions/{mission_id}/findings/{bare_id}/retests"),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "POST retest");
    assert_eq!(record["status"], "failed", "空证据必须判失败");
    assert_eq!(record["context_source"], "none");
    assert!(
        record["error"]
            .as_str()
            .is_some_and(|error| error.contains("没有任何已登记证据")),
        "失败原因必须点名是空证据：{record}"
    );

    // 漏洞状态没被顺带改动。
    let (status, list) = raw_call(
        app,
        "GET",
        &format!("/projects/{project_id}/findings"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let stored = list
        .as_array()
        .expect("findings must be an array")
        .iter()
        .find(|item| item["id"] == bare_id.as_str())
        .expect("finding must be listed");
    assert_eq!(stored["status"], "candidate");
}

#[tokio::test]
async fn retest_rejects_a_finding_from_another_mission() {
    let directory = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir: {error}"));
    let fixture = test_app(directory.path());
    let app = &fixture.app;
    let (project_id, mission_id) = seed_project_mission(app).await;
    let (finding_id, _evidence_id) =
        seed_mission_finding(&fixture, &project_id, &mission_id).await;

    // 同一个 project 下的另一个 mission：不能给别的任务的漏洞复测。
    let (status, other_mission) = raw_call(
        app,
        "POST",
        "/missions",
        Some(serde_json::json!({
            "project_id": project_id,
            "user_goal": "Audit https://other.example.test",
            "target": {"url": "https://other.example.test"},
            "target_type": "url"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let other_id = other_mission["id"].as_str().expect("mission id").to_string();

    let (status, _foreign) = raw_call(
        app,
        "POST",
        &format!("/missions/{other_id}/findings/{finding_id}/retests"),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "跨 mission 复测必须 404");

    // 未知 mission 同样 404。
    let (status, _unknown) = raw_call(
        app,
        "POST",
        &format!("/missions/mission_missing/findings/{finding_id}/retests"),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
