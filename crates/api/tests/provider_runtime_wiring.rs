//! Provider runtime 注入回归测试（spec PART 3/7）：
//!
//! 历史 bug：`AuditManager` 的 provider runtime 是 `Option` 且全仓无人
//! 注入，`POST /providers/discover-models` 永远 503
//! `provider runtime is not configured`。修复把注入收口到
//! [`api::attach_provider_runtimes`]，本文件锁定四条语义：
//!
//! 1. production 同款注入后两个 runtime 槽位均非空；
//! 2. preview discover（未保存 provider，body 直接带
//!    `base_url`/`api_key`）经真实网关打到本地 mock server 并成功解析；
//! 3. 未注入时 503 回归（防止未来 composition root 再丢注入）；
//! 4. 网关错误语义：401 → `denied`、超时 → `timeout`（均为 200 +
//!    结构化状态，密钥已被脱敏）。
//!
//! 全程 hermetic：mock gateway 是 `127.0.0.1` 随机端口的脚本化 TCP
//! server，不触外网。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use engines::default_solver_registry;
use engines::tool_catalog::ToolInstallCoordinator;
use runtime::{AuditManager, ExecutionControlPlane, InMemoryTaskBackend};
use serde_json::{Value, json};
use storage::{Repository, SqliteRepository};
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// 本地 mock gateway（脚本化单响应；`None` = 挂起不响应，用于超时测试）
// ---------------------------------------------------------------------------

/// 启动脚本化 mock server，返回其 base URL。
///
/// `response` 为 `Some((status, body))` 时对每个连接回一个固定 HTTP
/// 响应；为 `None` 时读入请求后挂起（客户端超时后连接被丢弃）。
async fn spawn_mock_gateway(response: Option<(u16, String)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("mock gateway must bind");
    let address = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let response = response.clone();
            tokio::spawn(async move {
                // 必须读完整个请求再应答/挂起：只读一次就让客户端在
                // 继续发送请求体时撞 RST（transport error）。
                read_full_request(&mut socket).await;
                let Some((status, body)) = response else {
                    // 挂起：永不响应（客户端超时取消 future 后此任务随
                    // socket drop 结束）。
                    std::future::pending::<()>().await;
                    return;
                };
                let reason = match status {
                    200 => "OK",
                    401 => "Unauthorized",
                    _ => "Error",
                };
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

async fn spawn_disconnect_gateway() -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("disconnect gateway must bind");
    let address = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            read_full_request(&mut socket).await;
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

// ---------------------------------------------------------------------------
// 应用构造（与 contract_sequence.rs 同构，可选注入 provider runtime）
// ---------------------------------------------------------------------------

fn test_app(directory: &std::path::Path, attach_runtimes: bool) -> (Router, Arc<AuditManager>) {
    let repository: Arc<dyn Repository> =
        Arc::new(SqliteRepository::open(":memory:").expect("in-memory repository"));
    let manager = if attach_runtimes {
        api::build_production_manager(repository).expect("production manager must compose")
    } else {
        Arc::new(AuditManager::new(
            repository,
            default_solver_registry(),
            Arc::new(InMemoryTaskBackend::default()),
        ))
    };
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

fn preview_payload(base_url: &str) -> Value {
    json!({
        "provider_id": null,
        "provider_type": "openai_compatible",
        "base_url": base_url,
        "api_key": "sk-test-super-secret-key",
        "timeout_seconds": 1
    })
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[test]
fn production_composition_attaches_real_runtimes() {
    let repository: Arc<dyn Repository> =
        Arc::new(SqliteRepository::open(":memory:").expect("repository"));
    let manager = api::build_production_manager(repository).expect("composition succeeds");
    assert!(
        manager.provider_runtime().is_some(),
        "production composition must provide the provider runtime"
    );
    assert!(
        manager.provider_discovery_runtime().is_some(),
        "production composition must provide the discovery runtime"
    );
}

#[tokio::test]
async fn saved_provider_health_check_reaches_runtime() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (app, manager) = test_app(directory.path(), true);
    let gateway = spawn_mock_gateway(Some((
        200,
        r#"{"choices":[{"message":{"content":"ok"}}],"model":"fixture-model"}"#.to_string(),
    )))
    .await;
    let mut provider = models::ProviderConfig::new(
        "saved fixture".to_string(),
        models::ProviderType::OpenaiCompatible,
    );
    provider.base_url = Some(gateway);
    provider.model = Some("fixture-model".to_string());
    provider.encrypted_api_key = Some("saved-super-secret".to_string());
    let provider = manager
        .create_provider(provider)
        .expect("provider persists");

    let (status, body) = post(
        &app,
        &format!("/providers/{}/test", provider.id.as_str()),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "health responds: {body}");
    assert_eq!(body["status"], "ok", "health reaches gateway: {body}");
    assert!(!body.to_string().contains("saved-super-secret"));
}

/// Cline 风格 `success/data` 包装 envelope 的端到端回归：归一化在网关
/// 边界生效后，文本探测通过；结构化探测拿到非 JSON 的 "ok" 内容，必须
/// 如实报告 `structured_output=false` 而非整体失败。
#[tokio::test]
async fn saved_provider_health_check_accepts_success_data_envelope() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (app, manager) = test_app(directory.path(), true);
    let gateway = spawn_mock_gateway(Some((
        200,
        r#"{"success":true,"data":{"model":"fixture-model","choices":[{"message":{"content":"ok"}}],"usage":{"prompt_tokens":1,"completion_tokens":1}}}"#.to_string(),
    )))
    .await;
    let mut provider = models::ProviderConfig::new(
        "cline fixture".to_string(),
        models::ProviderType::OpenaiCompatible,
    );
    provider.base_url = Some(gateway);
    provider.model = Some("fixture-model".to_string());
    provider.encrypted_api_key = Some("saved-super-secret".to_string());
    let provider = manager
        .create_provider(provider)
        .expect("provider persists");

    let (status, body) = post(
        &app,
        &format!("/providers/{}/test", provider.id.as_str()),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "health responds: {body}");
    assert_eq!(body["status"], "ok", "envelope must normalize: {body}");
    assert_eq!(
        body["capabilities"]["text_generation"], true,
        "text probe must pass through the wrapper: {body}"
    );
    assert_eq!(
        body["capabilities"]["structured_output"], false,
        "non-JSON probe content must be reported truthfully: {body}"
    );
    assert!(
        !body.to_string().contains("saved-super-secret"),
        "response must not echo the API key: {body}"
    );
}

#[tokio::test]
async fn preview_discover_models_against_mock_gateway() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (app, _manager) = test_app(directory.path(), true);
    let gateway = spawn_mock_gateway(Some((
        200,
        r#"{"data":[{"id":"gpt-4o"},{"id":"gpt-4o-mini"}]}"#.to_string(),
    )))
    .await;

    let (status, body) = post(
        &app,
        "/providers/discover-models",
        preview_payload(&gateway),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "discover must respond 200: {body}");
    assert_eq!(body["status"], "ok", "discovery must succeed: {body}");
    let models: Vec<&str> = body["models"]
        .as_array()
        .expect("models array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(models.contains(&"gpt-4o"), "models parsed: {models:?}");
    // 密钥红线：响应任何字段不得回显 API key。
    assert!(
        !body.to_string().contains("sk-test-super-secret-key"),
        "response must not echo the API key: {body}"
    );
}

#[tokio::test]
async fn discover_models_without_runtime_returns_503() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (app, _manager) = test_app(directory.path(), false);
    let (status, body) = post(
        &app,
        "/providers/discover-models",
        preview_payload("http://127.0.0.1:1"),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        body.to_string()
            .contains("provider runtime is not configured"),
        "503 detail keeps the fail-closed wording: {body}"
    );
}

#[tokio::test]
async fn discover_models_401_maps_to_denied_with_redacted_message() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (app, _manager) = test_app(directory.path(), true);
    let gateway = spawn_mock_gateway(Some((
        401,
        r#"{"error":{"message":"invalid key"}}"#.to_string(),
    )))
    .await;

    let (status, body) = post(
        &app,
        "/providers/discover-models",
        preview_payload(&gateway),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "denied", "401 maps to denied: {body}");
    let message = body["message"].as_str().expect("message");
    assert!(message.contains("HTTP 401"), "status surfaced: {message}");
    assert!(
        !message.contains("sk-test-super-secret-key"),
        "message must be secret-redacted: {message}"
    );
}

#[tokio::test]
async fn discover_models_403_maps_to_denied() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (app, _manager) = test_app(directory.path(), true);
    let gateway = spawn_mock_gateway(Some((403, r#"{"error":"forbidden"}"#.to_string()))).await;

    let (status, body) = post(
        &app,
        "/providers/discover-models",
        preview_payload(&gateway),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "denied");
    assert!(!body.to_string().contains("sk-test-super-secret-key"));
}

#[tokio::test]
async fn discovery_classifies_network_unsupported_and_invalid_responses() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (app, _manager) = test_app(directory.path(), true);

    let unreachable = spawn_disconnect_gateway().await;
    let (status, network) = post(
        &app,
        "/providers/discover-models",
        preview_payload(&unreachable),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(network["status"], "error");
    assert!(
        network["message"]
            .as_str()
            .is_some_and(|message| message.contains("HTTPError"))
    );

    let unsupported = spawn_mock_gateway(Some((404, "{}".to_string()))).await;
    let (_, unsupported_body) = post(
        &app,
        "/providers/discover-models",
        preview_payload(&unsupported),
    )
    .await;
    assert!(
        unsupported_body["message"]
            .as_str()
            .is_some_and(|message| message.contains("standard /models endpoint"))
    );

    let invalid = spawn_mock_gateway(Some((200, "[]".to_string()))).await;
    let (_, invalid_body) = post(
        &app,
        "/providers/discover-models",
        preview_payload(&invalid),
    )
    .await;
    assert_eq!(invalid_body["status"], "error");
    assert!(
        invalid_body["message"]
            .as_str()
            .is_some_and(|message| message.contains("non-object"))
    );
}

#[tokio::test]
async fn discover_models_timeout_maps_to_timeout() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (app, _manager) = test_app(directory.path(), true);
    // 永不响应的 gateway；客户端 timeout_seconds=2。
    let gateway = spawn_mock_gateway(None).await;

    let (status, body) = post(
        &app,
        "/providers/discover-models",
        preview_payload(&gateway),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "timeout", "hang maps to timeout: {body}");
}
