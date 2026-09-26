//! Intake analyze 回归：provider 输出不合 [`IntakePlan`] 契约时必须降级
//! 确定性回退（HTTP 200 + `used_fallback=true`），绝不让 serde
//! `ValidationError` 直达客户端。
//!
//! 畸形输出形状取自真实事故（2026-09，`z-ai/glm-5.3-flash` via Cline）：
//! 模型把 `response_contract` 里的 `"root": "IntakePlan"` 与输入键照抄进
//! 输出，不含任何 `IntakePlan` 字段。修复前该形状让 `analyze` 以 4xx 失败，
//! 用户在 Dashboard 建任务只看到一条 serde 报错。
//!
//! 全程 hermetic：mock gateway 是 `127.0.0.1` 随机端口的脚本化 TCP
//! server，不触外网。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use engines::tool_catalog::ToolInstallCoordinator;
use runtime::{AuditManager, ExecutionControlPlane};
use serde_json::{Value, json};
use storage::{Repository, SqliteRepository};
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tower::ServiceExt;

/// 启动脚本化 mock gateway，对每个连接返回同一固定 HTTP 响应。
async fn spawn_mock_gateway(status: u16, body: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("mock gateway must bind");
    let address = listener.local_addr().expect("local addr");
    let body = body.to_string();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let body = body.clone();
            tokio::spawn(async move {
                // 必须读完整个请求再应答：只读一次就 `connection: close`
                // 会让仍在发送请求体的客户端撞 RST（transport error）。
                read_full_request(&mut socket).await;
                let reason = if status == 200 { "OK" } else { "Error" };
                let wire = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(wire.as_bytes()).await;
            });
        }
    });
    format!("http://{address}")
}

/// 读完一个 HTTP 请求（headers + `Content-Length` 声明的 body）。
///
/// 单段 `read` 在请求超过缓冲区或被拆段时只拿到前缀；提前关连接会让
/// 客户端发送侧失败。带 1 MiB 上限防止异常客户端撑爆内存。
async fn read_full_request(socket: &mut tokio::net::TcpStream) {
    const MAX_REQUEST_BYTES: usize = 1024 * 1024;
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let Ok(read) = socket.read(&mut chunk).await else {
            return;
        };
        if read == 0 {
            return;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(headers_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&buffer[..headers_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if buffer.len() >= headers_end + 4 + content_length {
                return;
            }
        }
        if buffer.len() >= MAX_REQUEST_BYTES {
            return;
        }
    }
}

/// production 同款注入的应用（`provider_runtime_wiring.rs` 同构）。
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

async fn post(app: &Router, path: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request builds");
    let response = app.clone().oneshot(request).await.expect("response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, parsed)
}

/// 把"模型输出文本"包装成 Chat Completions 响应（mock gateway 对
/// `/chat/completions` 的应答形状）。
fn chat_completions_body(model_content: &Value) -> String {
    json!({
        "model": "fixture-model",
        "choices": [{"message": {"role": "assistant", "content": model_content.to_string()}}],
    })
    .to_string()
}

#[tokio::test]
async fn analyze_degrades_to_deterministic_fallback_on_malformed_model_plan() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (app, manager) = test_app(directory.path());
    let model_output = json!({
        "root": "IntakePlan",
        "raw_user_query": "hi",
        "raw_user_query_preserved": true,
        "intent_assessment": {
            "classification": "non_actionable_greeting",
            "confidence": 0.98,
        },
    });
    let gateway = spawn_mock_gateway(200, &chat_completions_body(&model_output)).await;
    let mut provider = models::ProviderConfig::new(
        "intake fixture".to_string(),
        models::ProviderType::OpenaiCompatible,
    );
    provider.base_url = Some(gateway);
    provider.model = Some("fixture-model".to_string());
    provider.is_default = true;
    manager
        .create_provider(provider)
        .expect("provider persists");

    let (status, body) = post(
        &app,
        "/intake/analyze",
        json!({"prompt": "Audit https://app.example.test/login thoroughly", "artifact_record_ids": []}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "analyze must not fail on malformed model output: {body}"
    );
    assert_eq!(body["used_fallback"], true, "must degrade: {body}");
    let reason = body["fallback_reason"]
        .as_str()
        .expect("fallback reason is present");
    assert!(reason.contains("ValidationError"), "actual: {reason}");
    assert!(reason.contains("unknown field `root`"), "actual: {reason}");
    // 分类机制已删：回退计划只把原文带下去（composite + 澄清问题），不再
    // 由正则"猜"出 web_dast。
    assert_eq!(
        body["plan"]["project"]["audit_domain"], "composite",
        "fallback plan carries the raw query without classification: {body}"
    );
}

/// 平凡输入（裸问候语）也必须走模型分析并落正式 Mission——任何输入都建
/// 任务派发 Agent，绝不静默不答。
#[tokio::test]
async fn analyze_routes_trivial_input_through_model_analysis() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (app, manager) = test_app(directory.path());
    let model_output = json!({
        "project": {
            "name": "Greeting audit",
            "audit_domain": "composite",
            "description": "Analyze the greeting",
        },
        "pipeline": {"profile": "web_full"},
        "goal_contract": {"outcome_type": "confirmed_finding", "confidence": 0.9},
        "recommended_intents": ["Clarify the greeting"],
        "confidence": 0.95,
        "rationale": "Greeting detected",
    });
    let gateway = spawn_mock_gateway(200, &chat_completions_body(&model_output)).await;
    let mut provider = models::ProviderConfig::new(
        "intake fixture".to_string(),
        models::ProviderType::OpenaiCompatible,
    );
    provider.base_url = Some(gateway);
    provider.model = Some("fixture-model".to_string());
    provider.is_default = true;
    manager
        .create_provider(provider)
        .expect("provider persists");

    let (status, body) = post(
        &app,
        "/intake/analyze",
        json!({"prompt": "hi", "artifact_record_ids": []}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "trivial input must analyze: {body}");
    assert_eq!(
        body["used_fallback"], false,
        "model plan must be consumed, not the deterministic fallback: {body}"
    );
    let invocation = body["model_invocation_id"]
        .as_str()
        .expect("model call must happen for trivial input now");
    assert!(!invocation.is_empty(), "model invocation audited: {body}");
}

#[tokio::test]
async fn analyze_accepts_wellformed_model_plan_without_fallback() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (app, manager) = test_app(directory.path());
    let model_output = json!({
        "project": {
            "name": "Login surface audit",
            "audit_domain": "web_dast",
            "description": "Audit https://app.example.test/login",
        },
        "pipeline": {"profile": "web_full", "audit_domains": ["web_recon", "web_dast"]},
        "goal_contract": {"outcome_type": "confirmed_finding", "confidence": 0.9},
        "recommended_intents": ["Run template-based web validation"],
        "confidence": 0.82,
        "rationale": "URL target detected",
    });
    let gateway = spawn_mock_gateway(200, &chat_completions_body(&model_output)).await;
    let mut provider = models::ProviderConfig::new(
        "intake fixture".to_string(),
        models::ProviderType::OpenaiCompatible,
    );
    provider.base_url = Some(gateway);
    provider.model = Some("fixture-model".to_string());
    provider.is_default = true;
    manager
        .create_provider(provider)
        .expect("provider persists");

    let (status, body) = post(
        &app,
        "/intake/analyze",
        json!({"prompt": "Audit https://app.example.test/login thoroughly", "artifact_record_ids": []}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "well-formed plan must pass: {body}");
    assert_eq!(body["used_fallback"], false, "model plan accepted: {body}");
    assert_eq!(
        body["plan"]["project"]["audit_domain"], "web_dast",
        "model classification must survive: {body}"
    );
    assert_eq!(body["fallback_reason"], Value::Null);
}

/// 未钉定 provider 的 analyze 必须按 `natural_language_intake` 用途路由：
/// 路由绑定指向非默认 provider 时，模型调用落在路由侧（计划内容由路由
/// 侧 fixture 证明），而不是默认 provider。
#[tokio::test]
async fn analyze_routes_by_purpose_when_provider_not_pinned() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (app, manager) = test_app(directory.path());
    let default_gateway = spawn_mock_gateway(
        200,
        r#"{"choices":[{"message":{"content":"{\"project\":{\"name\":\"DEFAULT-SIDE\",\"audit_domain\":\"composite\"},\"pipeline\":{}}"}}]}"#,
    )
    .await;
    let routed_gateway = spawn_mock_gateway(
        200,
        r#"{"choices":[{"message":{"content":"{\"project\":{\"name\":\"ROUTED-SIDE\",\"audit_domain\":\"composite\"},\"pipeline\":{}}"}}]}"#,
    )
    .await;

    let mut default_provider = models::ProviderConfig::new(
        "default".to_string(),
        models::ProviderType::OpenaiCompatible,
    );
    default_provider.base_url = Some(default_gateway);
    default_provider.model = Some("fixture-model".to_string());
    default_provider.is_default = true;
    let default_provider = manager
        .create_provider(default_provider)
        .expect("provider persists");
    let routed_provider = models::ProviderConfig::new(
        "profiler".to_string(),
        models::ProviderType::OpenaiCompatible,
    );
    let mut routed_provider = routed_provider;
    routed_provider.base_url = Some(routed_gateway);
    routed_provider.model = Some("fixture-model".to_string());
    let routed_provider = manager
        .create_provider(routed_provider)
        .expect("provider persists");

    let binding =
        models::ProviderRouteBinding::new("natural_language_intake", routed_provider.id.clone())
            .expect("route binding");
    manager
        .create_provider_route(binding)
        .expect("route persists");

    let (status, body) = post(
        &app,
        "/intake/analyze",
        json!({"prompt": "Audit https://app.example.test", "artifact_record_ids": []}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "analyze must succeed: {body}");
    assert_eq!(
        body["plan"]["project"]["name"], "ROUTED-SIDE",
        "unpinned intake must route by purpose: {body}"
    );
    assert_eq!(body["used_fallback"], false);
    let _ = default_provider;
}
