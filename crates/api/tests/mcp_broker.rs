//! lynceus-mcp 契约冒烟：tools/list 只暴露 9 个稳定入口；tool_execute
//! 守卫链 fail-closed（保留键 / 未知会话 / 未声明 invocation 的工具）。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use engines::broker::ExecutionScope;
use engines::broker::mcp::{WorkerGrantCredentials, WorkerGrantSpec};
use models::ids::{MissionId, ProjectId, RunId, TaskId};
use models::retrieval::ArtifactRecord;
use models::{AuditRun, BlackboardEntryKind, Evidence, EvidenceKind, ToolInvocation};
use serde_json::{Value, json};
use storage::SqliteRepository;
use tower::ServiceExt;

struct TestApp {
    router: Router,
    manager: Arc<runtime::AuditManager>,
    mcp: Arc<engines::broker::mcp::McpServerState>,
    _directory: tempfile::TempDir,
}

fn app() -> TestApp {
    let directory = tempfile::tempdir().expect("temp dir");
    let repository =
        Arc::new(SqliteRepository::open(directory.path().join("mcp.sqlite3")).expect("repository"));
    let manager = api::build_production_manager(repository).expect("composition root");
    let state = api::ApiState::new(manager).expect("api state");
    let manager = state.manager.clone();
    let mcp = state.mcp.clone();
    TestApp {
        router: api::router(state),
        manager,
        mcp,
        _directory: directory,
    }
}

async fn post_mcp(
    app: &Router,
    headers: &[(&str, &str)],
    body: Value,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder().method("POST").uri("/mcp");
    for (key, value) in headers {
        builder = builder.header(*key, *value);
    }
    builder = builder.header("content-type", "application/json");
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_string())).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let response_headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (
        status,
        response_headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn grant(app: &TestApp) -> WorkerGrantCredentials {
    app.mcp
        .issue_worker_grant(WorkerGrantSpec::new(
            ExecutionScope {
                project_id: Some(ProjectId::new("project_test".to_string())),
                mission_id: Some(MissionId::new("mission_test".to_string())),
                run_id: Some(RunId::new("run_test".to_string())),
                task_id: Some(TaskId::new("task_test".to_string())),
                worker_id: Some("worker_test".to_string()),
                worker_run_id: Some("worker_run_test".to_string()),
                artifact_dir: Some(app._directory.path().join("artifacts")),
                ..ExecutionScope::default()
            },
            None,
        ))
        .expect("valid worker grant")
}

fn grant_for(
    app: &TestApp,
    mission_id: &str,
    run_id: &str,
    task_id: &str,
    worker_id: &str,
    worker_run_id: &str,
) -> WorkerGrantCredentials {
    app.mcp
        .issue_worker_grant(WorkerGrantSpec::new(
            ExecutionScope {
                project_id: Some(ProjectId::new("project_test".to_string())),
                mission_id: Some(MissionId::new(mission_id.to_string())),
                run_id: Some(RunId::new(run_id.to_string())),
                task_id: Some(TaskId::new(task_id.to_string())),
                worker_id: Some(worker_id.to_string()),
                worker_run_id: Some(worker_run_id.to_string()),
                artifact_dir: Some(app._directory.path().join("artifacts")),
                ..ExecutionScope::default()
            },
            None,
        ))
        .expect("valid worker grant")
}

async fn call_tool(
    app: &TestApp,
    credentials: &WorkerGrantCredentials,
    session_id: &str,
    name: &str,
    arguments: Value,
) -> Value {
    let authorization = format!("Bearer {}", credentials.bearer_token);
    let (_, _, body) = post_mcp(
        &app.router,
        &[
            ("mcp-session-id", session_id),
            ("authorization", authorization.as_str()),
        ],
        json!({
            "jsonrpc": "2.0",
            "id": 100,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }),
    )
    .await;
    body
}

async fn append_from_fake_worker(
    router: Router,
    token: String,
    session_id: String,
    index: usize,
) -> Value {
    let authorization = format!("Bearer {token}");
    let (_, _, body) = post_mcp(
        &router,
        &[
            ("mcp-session-id", session_id.as_str()),
            ("authorization", authorization.as_str()),
        ],
        json!({
            "jsonrpc": "2.0",
            "id": index,
            "method": "tools/call",
            "params": {"name": "blackboard_append", "arguments": {
                "kind": "observation",
                "content": format!("parallel observation {index}"),
                "idempotency_key": format!("parallel-{index}")
            }}
        }),
    )
    .await;
    body
}

fn tool_payload(body: &Value) -> Value {
    serde_json::from_str(
        body["result"]["content"][0]["text"]
            .as_str()
            .expect("tool response text"),
    )
    .expect("tool response payload")
}

fn create_claim_task(app: &TestApp, run_id: &str, mission_id: &str, task_id: &str) {
    let mut run = AuditRun::new(ProjectId::new("project_test".to_string()));
    run.id = RunId::new(run_id.to_string());
    run.mission_id = Some(MissionId::new(mission_id.to_string()));
    app.manager.repository().create_run(&run).expect("run");
    let mut task = models::AgentTask::new(
        ProjectId::new("project_test".to_string()),
        run.id,
        "fake_worker".to_string(),
    );
    task.id = TaskId::new(task_id.to_string());
    task.mission_id = Some(MissionId::new(mission_id.to_string()));
    app.manager.repository().create_task(&task).expect("task");
}

async fn initialize(app: &TestApp, credentials: &WorkerGrantCredentials) -> String {
    let authorization = format!("Bearer {}", credentials.bearer_token);
    let (status, headers, body) = post_mcp(
        &app.router,
        &[("authorization", authorization.as_str())],
        json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {"protocolVersion": "2025-06-18"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"]["protocolVersion"], json!("2025-06-18"));
    headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .expect("initialize returns Mcp-Session-Id")
        .to_string()
}

#[tokio::test]
async fn tools_list_exposes_only_stable_entries() {
    let app = app();
    let credentials = grant(&app);
    let session_id = initialize(&app, &credentials).await;
    let authorization = format!("Bearer {}", credentials.bearer_token);
    let (status, _, body) = post_mcp(
        &app.router,
        &[
            ("mcp-session-id", session_id.as_str()),
            ("authorization", authorization.as_str()),
        ],
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = body["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(
        names,
        vec![
            "tool_list",
            "tool_search",
            "tool_describe",
            "tool_execute",
            "knowledge_search",
            "knowledge_get",
            "evidence_propose",
            "blackboard_read",
            "blackboard_append",
            "blackboard_claim",
            "skill_list",
            "skill_load",
            "traffic_search",
            "traffic_get"
        ]
    );
}

#[tokio::test]
async fn tool_execute_guard_chain_fails_closed() {
    let app = app();
    let credentials = grant(&app);
    let session_id = initialize(&app, &credentials).await;
    let authorization = format!("Bearer {}", credentials.bearer_token);
    let (status, _, body) = post_mcp(
        &app.router,
        &[
            ("mcp-session-id", session_id.as_str()),
            ("authorization", authorization.as_str()),
        ],
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "tool_execute",
                "arguments": {"tool_id": "nuclei", "arguments": {"command": "echo pwned"}}
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // 认证 session 已建立；工具不可用/参数不安全只能作为工具错误返回，
    // 不能绕过 JSON-RPC 或创建未审计的执行路径。
    assert_eq!(body["result"]["isError"], json!(true), "{body}");
}

#[tokio::test]
async fn blackboard_append_rejection_lists_valid_kinds_for_self_healing() {
    // worker 首次 append 很自然会写 finding / fact / result——被拒时错误必须
    // 列出合法 kind，worker 才能立刻改用 observation / summary 重试；否则它
    // 连撞几次后就放弃并报告"白板不可用"（实测 codex / claude 都因此丢结论）。
    let app = app();
    let credentials = grant(&app);
    let session_id = initialize(&app, &credentials).await;

    let rejected = call_tool(
        &app,
        &credentials,
        &session_id,
        "blackboard_append",
        json!({
            "kind": "finding",
            "content": "a real observed result",
            "idempotency_key": "self-heal-reject"
        }),
    )
    .await;
    assert_eq!(rejected["result"]["isError"], json!(true), "{rejected}");
    let error_text = rejected["result"]["content"][0]["text"]
        .as_str()
        .expect("error text");
    assert!(
        error_text.contains("valid kinds"),
        "rejection must teach the vocabulary: {error_text}"
    );
    for kind in ["observation", "summary", "hypothesis", "decision"] {
        assert!(
            error_text.contains(kind),
            "rejection must list valid kind '{kind}': {error_text}"
        );
    }

    // 自愈闭环：凭错误里给出的合法 kind 重试即成功落库。
    let accepted = call_tool(
        &app,
        &credentials,
        &session_id,
        "blackboard_append",
        json!({
            "kind": "observation",
            "content": "a real observed result",
            "idempotency_key": "self-heal-accept"
        }),
    )
    .await;
    assert_ne!(accepted["result"]["isError"], json!(true), "{accepted}");
}

#[tokio::test]
async fn evidence_propose_only_accepts_a_scoped_persisted_reference() {
    let app = app();
    let credentials = grant(&app);
    let session_id = initialize(&app, &credentials).await;

    let mut invocation = ToolInvocation::new("test-tool".to_string(), "target".to_string());
    invocation.project_id = Some(ProjectId::new("project_test".to_string()));
    invocation.mission_id = Some(MissionId::new("mission_test".to_string()));
    invocation.run_id = Some(RunId::new("run_test".to_string()));
    invocation.task_id = Some(TaskId::new("task_test".to_string()));
    invocation
        .metadata
        .insert("worker_run_id".to_string(), json!("worker_run_test"));
    let mut artifact = ArtifactRecord::new("artifact://test-output".to_string());
    artifact.project_id = invocation.project_id.clone();
    artifact.run_id = invocation.run_id.clone();
    artifact.task_id = invocation.task_id.clone();
    artifact.tool_invocation_id = Some(invocation.id.clone());
    app.manager
        .persist_mcp_audit(invocation.clone(), vec![artifact.clone()])
        .expect("AuditManager persists the invocation and artifact");

    let authorization = format!("Bearer {}", credentials.bearer_token);
    let (status, _, body) = post_mcp(
        &app.router,
        &[
            ("mcp-session-id", session_id.as_str()),
            ("authorization", authorization.as_str()),
        ],
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "evidence_propose",
                "arguments": {
                    "invocation_id": invocation.id,
                    "artifact_id": artifact.id,
                    "locator": "artifact://test-output",
                    "summary": "bounded reference"
                }
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let candidate: Value = serde_json::from_str(
        body["result"]["content"][0]["text"]
            .as_str()
            .expect("candidate text"),
    )
    .expect("candidate JSON");
    assert_eq!(candidate["kind"], json!("evidence_candidate"));
    assert!(candidate.get("content").is_none());
}

#[tokio::test]
async fn evidence_propose_confirms_flag_bound_to_sealed_artifact() {
    // 观察→发现的桥：worker 的 tool_execute 把输出字节精确封存成工件并记
    // SHA-256；evidence_propose 绑定 flag 后，只有逐字节出现在工件里的 flag
    // 才过 flag_capture 门成为 confirmed Finding——伪造的 flag 必须被拒。
    let app = app();
    let credentials = grant(&app);
    let session_id = initialize(&app, &credentials).await;

    let dir = tempfile::tempdir().expect("temp dir");
    let artifact_path = dir.path().join("resp.out");
    let flag = "flag{lynceus_bridge_ok}";
    let body = format!("HTTP/1.1 200 OK\r\n\r\ncongrats, your flag is {flag}\n");
    std::fs::write(&artifact_path, body.as_bytes()).expect("write sealed artifact");
    let sha256 = evidence::Sha256Fingerprint::compute(body.as_bytes())
        .as_hex()
        .to_string();
    let locator = artifact_path.to_string_lossy().into_owned();

    let mut invocation = ToolInvocation::new("curl".to_string(), "http://target/".to_string());
    invocation.project_id = Some(ProjectId::new("project_test".to_string()));
    invocation.mission_id = Some(MissionId::new("mission_test".to_string()));
    invocation.run_id = Some(RunId::new("run_test".to_string()));
    invocation.task_id = Some(TaskId::new("task_test".to_string()));
    invocation.status = models::lifecycle::ToolStatus::Ok;
    invocation.artifact_paths.push(locator.clone());
    invocation
        .metadata
        .insert("worker_run_id".to_string(), json!("worker_run_test"));
    invocation
        .metadata
        .insert("sha256".to_string(), json!(sha256));

    let mut artifact = ArtifactRecord::new(locator.clone());
    artifact.project_id = invocation.project_id.clone();
    artifact.run_id = invocation.run_id.clone();
    artifact.task_id = invocation.task_id.clone();
    artifact.tool_invocation_id = Some(invocation.id.clone());
    artifact.sha256 = Some(sha256);
    app.manager
        .persist_mcp_audit(invocation.clone(), vec![artifact.clone()])
        .expect("persist invocation + sealed artifact");

    let confirmed = call_tool(
        &app,
        &credentials,
        &session_id,
        "evidence_propose",
        json!({
            "invocation_id": invocation.id,
            "artifact_id": artifact.id,
            "locator": locator,
            "summary": "flag recovered verbatim from target response",
            "flag": flag
        }),
    )
    .await;
    assert_ne!(confirmed["result"]["isError"], json!(true), "{confirmed}");
    let payload = tool_payload(&confirmed);
    assert_eq!(payload["status"], json!("confirmed"), "{payload}");
    let finding_id = payload["finding_id"].as_str().expect("finding_id");
    let findings = app
        .manager
        .repository()
        .list_findings("project_test")
        .expect("list findings");
    assert!(
        findings.iter().any(|finding| finding.id.as_str() == finding_id),
        "confirmed finding must be persisted: {findings:?}"
    );

    // 伪造的 flag（不在工件字节里）必须被 flag_capture 门拒绝。
    let forged = call_tool(
        &app,
        &credentials,
        &session_id,
        "evidence_propose",
        json!({
            "invocation_id": invocation.id,
            "artifact_id": artifact.id,
            "locator": locator,
            "summary": "fabricated flag",
            "flag": "flag{not_in_this_artifact}"
        }),
    )
    .await;
    assert_ne!(forged["result"]["isError"], json!(true), "{forged}");
    let forged_payload = tool_payload(&forged);
    assert_eq!(forged_payload["status"], json!("rejected"), "{forged_payload}");
    assert!(
        !forged_payload["reasons"].as_array().is_none_or(|reasons| reasons.is_empty()),
        "rejection must carry reasons: {forged_payload}"
    );
}

#[tokio::test]
async fn concurrent_calls_keep_the_same_session_bound() {
    let app = app();
    let credentials = grant(&app);
    let session_id = initialize(&app, &credentials).await;
    let authorization = format!("Bearer {}", credentials.bearer_token);
    let headers = [
        ("mcp-session-id", session_id.as_str()),
        ("authorization", authorization.as_str()),
    ];
    let first = post_mcp(
        &app.router,
        &headers,
        json!({"jsonrpc": "2.0", "id": 5, "method": "ping"}),
    );
    let second = post_mcp(
        &app.router,
        &headers,
        json!({"jsonrpc": "2.0", "id": 6, "method": "ping"}),
    );
    let ((first_status, _, first_body), (second_status, _, second_body)) =
        tokio::join!(first, second);
    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(second_status, StatusCode::OK);
    assert!(
        first_body["error"]["message"]
            .as_str()
            .is_none_or(|message| !message.contains("unknown session"))
    );
    assert!(
        second_body["error"]["message"]
            .as_str()
            .is_none_or(|message| !message.contains("unknown session"))
    );
}

#[tokio::test]
async fn concurrent_blackboard_appends_have_unique_monotonic_sequences() {
    let app = app();
    let credentials = grant(&app);
    let session_id = initialize(&app, &credentials).await;
    let bodies = futures_util::future::join_all((0..8).map(|index| {
        append_from_fake_worker(
            app.router.clone(),
            credentials.bearer_token.clone(),
            session_id.clone(),
            index,
        )
    }))
    .await;
    let mut sequences = bodies
        .iter()
        .map(|body| {
            assert_ne!(body["result"]["isError"], json!(true), "{body}");
            tool_payload(body)["sequence"].as_i64().expect("sequence")
        })
        .collect::<Vec<_>>();
    sequences.sort_unstable();
    sequences.dedup();
    assert_eq!(sequences.len(), 8);
    assert!(sequences.windows(2).all(|window| window[0] < window[1]));
    let read = call_tool(
        &app,
        &credentials,
        &session_id,
        "blackboard_read",
        json!({"cursor": 0, "limit": 100}),
    )
    .await;
    assert_eq!(
        tool_payload(&read)["entries"]
            .as_array()
            .expect("entries")
            .len(),
        8
    );
}

#[tokio::test]
async fn unknown_method_is_jsonrpc_error() {
    let app = app();
    let (status, _, body) = post_mcp(
        &app.router,
        &[],
        json!({"jsonrpc": "2.0", "id": 3, "method": "definitely/not/a/method"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["error"]["code"], json!(-32601));
}

#[tokio::test]
async fn blackboard_is_mission_scoped_cursor_visible_and_idempotent() {
    let app = app();
    let first = grant_for(
        &app,
        "mission_shared",
        "run_shared",
        "task_a",
        "worker_a",
        "run_a",
    );
    let second = grant_for(
        &app,
        "mission_shared",
        "run_shared",
        "task_b",
        "worker_b",
        "run_b",
    );
    let other = grant_for(
        &app,
        "mission_other",
        "run_shared",
        "task_c",
        "worker_c",
        "run_c",
    );
    let first_session = initialize(&app, &first).await;
    let second_session = initialize(&app, &second).await;
    let other_session = initialize(&app, &other).await;

    let first_body = call_tool(
        &app,
        &first,
        &first_session,
        "blackboard_append",
        json!({
            "kind": "observation",
            "content": "worker A saw a redirect",
            "idempotency_key": "obs-a"
        }),
    )
    .await;
    assert_ne!(first_body["result"]["isError"], json!(true));
    let first_entry = tool_payload(&first_body);
    assert_eq!(first_entry["author_worker_run_id"], json!("run_a"));
    let first_sequence = first_entry["sequence"].as_i64().expect("sequence");

    let second_read = call_tool(
        &app,
        &second,
        &second_session,
        "blackboard_read",
        json!({"cursor": 0}),
    )
    .await;
    let second_payload = tool_payload(&second_read);
    assert_eq!(
        second_payload["entries"][0]["content"],
        json!("worker A saw a redirect")
    );
    assert_eq!(
        second_payload["entries"][0]["author_worker_run_id"],
        json!("run_a")
    );

    let second_body = call_tool(
        &app,
        &second,
        &second_session,
        "blackboard_append",
        json!({
            "kind": "decision",
            "content": "follow the redirect",
            "idempotency_key": "decision-b"
        }),
    )
    .await;
    let second_entry = tool_payload(&second_body);
    let next_read = call_tool(
        &app,
        &first,
        &first_session,
        "blackboard_read",
        json!({"cursor": first_sequence}),
    )
    .await;
    assert_eq!(
        tool_payload(&next_read)["entries"][0]["entry_id"],
        second_entry["entry_id"]
    );

    let other_read = call_tool(
        &app,
        &other,
        &other_session,
        "blackboard_read",
        json!({"cursor": 0}),
    )
    .await;
    assert!(
        tool_payload(&other_read)["entries"]
            .as_array()
            .expect("entries")
            .is_empty()
    );

    let retry = call_tool(
        &app,
        &first,
        &first_session,
        "blackboard_append",
        json!({
            "kind": "observation",
            "content": "worker A saw a redirect",
            "idempotency_key": "obs-a"
        }),
    )
    .await;
    assert_eq!(tool_payload(&retry)["entry_id"], first_entry["entry_id"]);
    let mismatch = call_tool(
        &app,
        &first,
        &first_session,
        "blackboard_append",
        json!({
            "kind": "observation",
            "content": "different text",
            "idempotency_key": "obs-a"
        }),
    )
    .await;
    assert_eq!(mismatch["result"]["isError"], json!(true));
}

#[tokio::test]
async fn blackboard_rejects_forged_scope_oversized_and_cross_mission_refs() {
    let app = app();
    let credentials = grant(&app);
    let session_id = initialize(&app, &credentials).await;
    let forged = call_tool(
        &app,
        &credentials,
        &session_id,
        "blackboard_append",
        json!({
            "kind": "observation",
            "content": "not trusted",
            "idempotency_key": "forged",
            "author_worker_run_id": "evil"
        }),
    )
    .await;
    assert_eq!(forged["result"]["isError"], json!(true));
    let oversized = call_tool(
        &app,
        &credentials,
        &session_id,
        "blackboard_append",
        json!({
            "kind": "observation",
            "content": "x".repeat(16_385),
            "idempotency_key": "oversized"
        }),
    )
    .await;
    assert_eq!(oversized["result"]["isError"], json!(true));

    let mut invocation = ToolInvocation::new("cross-scope".to_string(), "input".to_string());
    invocation.project_id = Some(ProjectId::new("project_test".to_string()));
    invocation.mission_id = Some(MissionId::new("mission_other".to_string()));
    invocation.run_id = Some(RunId::new("run_test".to_string()));
    invocation.task_id = Some(TaskId::new("task_test".to_string()));
    invocation
        .metadata
        .insert("worker_run_id".to_string(), json!("other"));
    let mut artifact = ArtifactRecord::new("artifact://other".to_string());
    artifact.project_id = invocation.project_id.clone();
    artifact.run_id = invocation.run_id.clone();
    artifact.task_id = invocation.task_id.clone();
    artifact.tool_invocation_id = Some(invocation.id.clone());
    app.manager
        .persist_mcp_audit(invocation, vec![artifact.clone()])
        .expect("cross mission artifact");
    let cross_artifact = call_tool(
        &app,
        &credentials,
        &session_id,
        "blackboard_append",
        json!({
            "kind": "artifact_ref",
            "artifact_id": artifact.id,
            "locator": "artifact://other",
            "idempotency_key": "cross-artifact"
        }),
    )
    .await;
    assert_eq!(cross_artifact["result"]["isError"], json!(true));

    let mut evidence = Evidence::new(
        ProjectId::new("project_test".to_string()),
        EvidenceKind::SourceSnippet,
        "other mission evidence".to_string(),
    );
    evidence.mission_id = Some(MissionId::new("mission_other".to_string()));
    evidence.run_id = Some(RunId::new("run_test".to_string()));
    evidence.produced_by_task_id = Some(TaskId::new("task_test".to_string()));
    app.manager
        .repository()
        .add_evidence(&evidence)
        .expect("evidence");
    let cross_evidence = call_tool(
        &app,
        &credentials,
        &session_id,
        "blackboard_append",
        json!({
            "kind": "evidence_ref",
            "evidence_id": evidence.id,
            "idempotency_key": "cross-evidence"
        }),
    )
    .await;
    assert_eq!(cross_evidence["result"]["isError"], json!(true));
}

#[tokio::test]
async fn two_fake_workers_claim_append_read_and_complete_without_unknown_session() {
    let app = app();
    create_claim_task(&app, "run_claim", "mission_claim", "task_claim");
    let first = grant_for(
        &app,
        "mission_claim",
        "run_claim",
        "task_claim",
        "worker_a",
        "worker-run-a",
    );
    let second = grant_for(
        &app,
        "mission_claim",
        "run_claim",
        "task_claim",
        "worker_b",
        "worker-run-b",
    );
    let first_session = initialize(&app, &first).await;
    let second_session = initialize(&app, &second).await;
    let first_router = app.router.clone();
    let second_router = app.router.clone();
    let first_token = first.bearer_token.clone();
    let second_token = second.bearer_token.clone();
    let first_sid = first_session.clone();
    let second_sid = second_session.clone();
    let (first_claim, second_claim) = tokio::join!(
        async move {
            let auth = format!("Bearer {first_token}");
            post_mcp(
                &first_router,
                &[
                    ("mcp-session-id", first_sid.as_str()),
                    ("authorization", auth.as_str()),
                ],
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
                    "name":"blackboard_claim","arguments":{"task_id":"task_claim","lease_seconds":60}
                }}),
            )
            .await
        },
        async move {
            let auth = format!("Bearer {second_token}");
            post_mcp(
                &second_router,
                &[
                    ("mcp-session-id", second_sid.as_str()),
                    ("authorization", auth.as_str()),
                ],
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"blackboard_claim","arguments":{"task_id":"task_claim","lease_seconds":60}
                }}),
            )
            .await
        },
    );
    let claims = [first_claim, second_claim];
    assert_eq!(
        claims
            .iter()
            .filter(|(_, _, body)| body["result"]["isError"] != json!(true))
            .count(),
        1
    );
    let winner = claims
        .into_iter()
        .find(|(_, _, body)| body["result"]["isError"] != json!(true))
        .map(|(_, _, body)| tool_payload(&body))
        .expect("one fake worker wins");
    let winner_credentials = if winner["worker_run_id"] == json!("worker-run-a") {
        &first
    } else {
        &second
    };
    let winner_session = if winner["worker_run_id"] == json!("worker-run-a") {
        &first_session
    } else {
        &second_session
    };
    let appended = call_tool(
        &app,
        winner_credentials,
        winner_session,
        "blackboard_append",
        json!({
            "kind": BlackboardEntryKind::Observation.as_str(),
            "content": "fake worker completed its observation",
            "idempotency_key": "fake-e2e-observation"
        }),
    )
    .await;
    assert_ne!(appended["result"]["isError"], json!(true));
    let read = call_tool(
        &app,
        if winner["worker_run_id"] == json!("worker-run-a") {
            &second
        } else {
            &first
        },
        if winner["worker_run_id"] == json!("worker-run-a") {
            &second_session
        } else {
            &first_session
        },
        "blackboard_read",
        json!({"cursor": 0}),
    )
    .await;
    assert_eq!(
        tool_payload(&read)["entries"]
            .as_array()
            .expect("entries")
            .len(),
        1
    );

    let owner = winner["worker_run_id"].as_str().expect("owner");
    let lease_id = winner["id"].as_str().expect("lease id");
    let revision = winner["revision"].as_i64().expect("revision");
    let completed = app
        .manager
        .repository()
        .complete_worker_lease(lease_id, owner, revision)
        .expect("complete")
        .expect("lease completion");
    assert_eq!(completed.status, models::WorkerLeaseStatus::Completed);
    assert_eq!(
        app.manager
            .repository()
            .get_task("task_claim")
            .expect("task")
            .expect("task exists")
            .status,
        models::TaskStatus::Succeeded
    );
}


/// WP6 skill 工具链冒烟：目录 + 双工具 + fail-closed 解锁 + 台账。
///
/// 注意：SkillManager 是 `McpServerState::new` 时从 `LYNCEUS_SKILLS_DIR`
/// 构造的（进程级 env），本测试在构造 app 前设置 env 并持有 TempDir；
/// 其余并行测试不调用 skill 工具，不受影响。
#[tokio::test]
async fn skill_list_load_unlocks_modules_and_ledgers_usage() {
    let directory = tempfile::tempdir().expect("temp dir");

    // 准备 skill：modules 声明 nuclei（fail-closed 解锁面）。
    let manager = engines::skills::SkillManager::new(directory.path().join("skills"));
    manager
        .create(
            "api-recon",
            "收集网站API接口",
            &["nuclei".to_string()],
            None,
            None,
            "# API Recon 手册\n按步骤枚举接口……",
        )
        .expect("seed skill");

    let app = app();
    // 注入 skill 根目录（McpServerState 构造时用 env 默认；测试直接替换）。
    app.mcp
        .skills
        .write()
        .expect("skills lock")
        .set_root(directory.path().join("skills"));
    let credentials = grant(&app);
    let session_id = initialize(&app, &credentials).await;

    // 收紧策略：空 allowlist（= 什么都不允许）。
    let restricted = app
        .mcp
        .issue_worker_grant(WorkerGrantSpec::new(
            ExecutionScope {
                project_id: Some(ProjectId::new("project_test".to_string())),
                mission_id: Some(MissionId::new("mission_test".to_string())),
                run_id: Some(RunId::new("run_test".to_string())),
                task_id: Some(TaskId::new("task_test".to_string())),
                worker_id: Some("worker_test".to_string()),
                worker_run_id: Some("worker_run_test".to_string()),
                artifact_dir: Some(directory.path().join("artifacts")),
                ..ExecutionScope::default()
            },
            Some(Vec::new()),
        ))
        .expect("restricted grant");
    let restricted_session = initialize(&app, &restricted).await;

    // 解锁前：tool_list 不含 nuclei。
    let before = call_tool(
        &app,
        &restricted,
        &restricted_session,
        "tool_list",
        json!({}),
    )
    .await;
    let before_text = tool_payload(&before).to_string();
    assert!(!before_text.contains("\"nuclei\""), "解锁前 nuclei 不可见");

    // skill_list 可见（无预设 → 全部可见）。
    let listing = call_tool(&app, &credentials, &session_id, "skill_list", json!({})).await;
    let listing_payload = tool_payload(&listing);
    assert_eq!(listing_payload["skills"][0]["name"], json!("api-recon"));
    assert_eq!(listing_payload["skills"][0]["modules"], json!(["nuclei"]));

    // skill_load：返回手册 + 解锁 nuclei。
    let loaded = call_tool(
        &app,
        &restricted,
        &restricted_session,
        "skill_load",
        json!({"name": "api-recon"}),
    )
    .await;
    let loaded_payload = tool_payload(&loaded);
    assert_eq!(loaded_payload["name"], json!("api-recon"));
    assert_eq!(loaded_payload["modules_unlocked"], json!(["nuclei"]));
    assert!(loaded_payload["manual"]
        .as_str()
        .expect("manual")
        .contains("API Recon 手册"));

    // 解锁后：tool_list 出现 nuclei（同一 session 的策略面被扩展）。
    let after = call_tool(
        &app,
        &restricted,
        &restricted_session,
        "tool_list",
        json!({}),
    )
    .await;
    assert!(
        tool_payload(&after).to_string().contains("\"nuclei\""),
        "skill_load 后 nuclei 必须在允许集内"
    );

    // 台账：一行 found=true。
    let usage = app
        .manager
        .repository()
        .list_skill_usage(None, 10)
        .expect("ledger");
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].skill, "api-recon");
    assert!(usage[0].found);
    assert_eq!(usage[0].mission_id.as_deref(), Some("mission_test"));

    // miss 记账：不存在的 skill 报错且 found=false。
    let miss = call_tool(
        &app,
        &credentials,
        &session_id,
        "skill_load",
        json!({"name": "ghost-skill"}),
    )
    .await;
    assert_eq!(miss["result"]["isError"], json!(true));
    let missing = app
        .manager
        .repository()
        .skill_missing_report()
        .expect("missing report");
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].skill, "ghost-skill");
    assert_eq!(missing[0].misses, 1);

}

/// WP6 preset 可见性 fail-closed：`skills` 白名单非空时，白名单外的
/// skill 不可 list、不可 load。
#[tokio::test]
async fn preset_skill_visibility_fails_closed() {
    let directory = tempfile::tempdir().expect("temp dir");
    let manager = engines::skills::SkillManager::new(directory.path().join("skills"));
    manager
        .create("api-recon", "收集网站API接口", &[], None, None, "# 手册")
        .expect("seed skill");

    let app = app();
    app.mcp
        .skills
        .write()
        .expect("skills lock")
        .set_root(directory.path().join("skills"));
    // 给这个 worker 挂一个 Agent 预设（skills 白名单 = other-skill）。
    let now = models::common::utcnow();
    let mut preset = models::AgentPreset::new_v1(
        "worker_visible",
        "Visibility preset",
        None,
        "worker body",
        now,
    );
    preset.skills = vec!["other-skill".to_string()];
    app.manager
        .repository()
        .upsert_agent_preset(&preset)
        .expect("seed preset");

    let mut worker_run = models::WorkerRun::new(
        ProjectId::new("project_test".to_string()),
        models::WorkerRuntimeType::ClaudeCode,
        "skill visibility worker",
    );
    worker_run.agent_preset_id = Some("worker_visible".to_string());
    app.manager
        .repository()
        .upsert_worker_run(&worker_run)
        .expect("seed worker run");

    let credentials = grant_for(
        &app,
        "mission_vis",
        "run_vis",
        "task_vis",
        "worker_vis",
        worker_run.id.as_str(),
    );
    let session_id = initialize(&app, &credentials).await;

    // skill_list 被白名单过滤：api-recon 不可见。
    let listing = call_tool(&app, &credentials, &session_id, "skill_list", json!({})).await;
    let listing_payload = tool_payload(&listing);
    assert_eq!(
        listing_payload["skills"]
            .as_array()
            .expect("skills array")
            .len(),
        0,
        "白名单外的 skill 不得出现在 skill_list"
    );

    // skill_load 白名单外 → 显式拒绝。
    let denied = call_tool(
        &app,
        &credentials,
        &session_id,
        "skill_load",
        json!({"name": "api-recon"}),
    )
    .await;
    assert_eq!(denied["result"]["isError"], json!(true));
    assert!(
        denied["result"]["content"][0]["text"]
            .as_str()
            .expect("error text")
            .contains("not visible")
    );

    // 台账记了 load 意图（found=true 但被拒），agent_preset 关联正确。
    let usage = app
        .manager
        .repository()
        .list_skill_usage(Some("api-recon"), 10)
        .expect("ledger");
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].agent_preset.as_deref(), Some("worker_visible"));

    // 放开白名单后可加载。
    preset.skills = Vec::new();
    app.manager
        .repository()
        .upsert_agent_preset(&preset)
        .expect("update preset");
    let allowed = call_tool(
        &app,
        &credentials,
        &session_id,
        "skill_load",
        json!({"name": "api-recon"}),
    )
    .await;
    assert_eq!(tool_payload(&allowed)["name"], json!("api-recon"));

}

/// WP5 preset 工具授权：`tools` 非空时，worker 会话只允许列出的目录条目
/// （None grant → Some(白名单)；已收紧 grant → 交集）。
#[tokio::test]
async fn preset_tool_authorization_intersects_session_allowlist() {
    let app = app();
    let now = models::common::utcnow();
    let mut preset =
        models::AgentPreset::new_v1("scoped_worker", "Scoped", None, "worker body", now);
    preset.tools = vec!["nuclei".to_string()];
    app.manager
        .repository()
        .upsert_agent_preset(&preset)
        .expect("seed preset");

    let mut worker_run = models::WorkerRun::new(
        ProjectId::new("project_test".to_string()),
        models::WorkerRuntimeType::ClaudeCode,
        "scoped worker",
    );
    worker_run.agent_preset_id = Some("scoped_worker".to_string());
    app.manager
        .repository()
        .upsert_worker_run(&worker_run)
        .expect("seed worker run");

    let credentials = grant_for(
        &app,
        "mission_tools",
        "run_tools",
        "task_tools",
        "worker_tools",
        worker_run.id.as_str(),
    );
    let session_id = initialize(&app, &credentials).await;

    // tool_list 只剩 nuclei（其它 catalog 工具被预设白名单滤掉）。
    let listing = call_tool(&app, &credentials, &session_id, "tool_list", json!({})).await;
    let payload_text = tool_payload(&listing).to_string();
    assert!(payload_text.contains("\"nuclei\""), "nuclei 在白名单内必须可见");
    assert!(
        !payload_text.contains("\"subfinder\""),
        "白名单外的 subfinder 不得可见"
    );
}
