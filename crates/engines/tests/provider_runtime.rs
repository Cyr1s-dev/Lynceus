//! LLM 网关 HTTP 集成测试 —— `server/engines/tests/test_provider_runtime.py`
//! 14 个测试语义的移植，外加 M4 新增的 SSE 流式集成测试（共 15 个）。
//!
//! Python 侧用 `httpx.MockTransport` 注入假传输层；Rust 侧 reqwest 无
//! 对应物，本文件提供一个本地 mock HTTP server（`127.0.0.1` 随机端口，
//! 脚本化响应 + 请求全文记录），provider 的 `base_url` 指向该 server——
//! 全程不触外网（M4 验收红线）。仓储用 `SqliteRepository` + 临时文件
//! （Python 侧 `InMemoryRepository` 的等价物）。
//!
//! 与 Python 的**有意**差异：Python 测试在 handler 内部断言请求形状
//! （path/头/体）；本文件把请求记录下来事后断言——断言在测试任务里
//! 失败而非 mock server 任务里 panic，失败信息可读。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use agents::llm::LlmMessage;
use agents::llm::ProviderRuntime;
use agents::llm::StructuredGenerationRequest;
use agents::llm::TextGenerationRequest;
use engines::model_providers::GatewayError;
use engines::model_providers::GatewayStreamItem;
use engines::model_providers::OpenAiCompatibleProviderRuntime;
use engines::model_providers::ProviderRouterRuntime;
use engines::model_providers::StreamOutcome;
use models::ids::ProjectId;
use models::ids::ProviderId;
use models::provider::ModelInvocation;
use models::provider::ModelInvocationStatus;
use models::provider::ProviderConfig;
use models::provider::ProviderRouteBinding;
use models::provider::ProviderType;
use serde_json::Value;
use serde_json::json;
use storage::Repository;
use storage::SqliteRepository;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio_stream::StreamExt;

// ---------------------------------------------------------------------------
// 本地 mock HTTP server
// ---------------------------------------------------------------------------

/// 一条收到的 HTTP 请求（测试断言用）。
#[derive(Debug, Clone)]
struct RecordedHttpRequest {
    /// 请求方法（`POST` / `GET`）。
    method: String,
    /// URL 路径（不含查询串）。
    path: String,
    /// 查询参数。
    query: HashMap<String, String>,
    /// 请求头（名字已小写归一）。
    headers: HashMap<String, String>,
    /// 解析后的 JSON 请求体；非 JSON 体为 `None`。
    body: Option<Value>,
}

impl RecordedHttpRequest {
    /// JSON 请求体断言便捷入口。
    fn body_value(&self) -> &Value {
        self.body.as_ref().expect("请求体必须是 JSON")
    }

    /// 请求头断言便捷入口（缺失返回空串，断言随之失败）。
    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .map(String::as_str)
            .unwrap_or_default()
    }
}

/// 脚本化响应。
struct MockHttpResponse {
    status: u16,
    content_type: String,
    body: Vec<u8>,
    /// 逐块写出的块大小（模拟流式分帧）；`None` = 一次性写完。
    piece_size: Option<usize>,
}

impl MockHttpResponse {
    fn json(status: u16, body: &Value) -> Self {
        Self {
            status,
            content_type: "application/json".to_string(),
            body: serde_json::to_vec(body).expect("测试响应序列化不会失败"),
            piece_size: None,
        }
    }

    fn text(status: u16, content_type: &str, body: &str) -> Self {
        Self {
            status,
            content_type: content_type.to_string(),
            body: body.as_bytes().to_vec(),
            piece_size: None,
        }
    }

    /// SSE 流：小块逐帧写出，真实驱动 reqwest 流式路径的增量解析。
    fn sse(body: &'static str) -> Self {
        Self {
            status: 200,
            content_type: "text/event-stream".to_string(),
            body: body.as_bytes().to_vec(),
            piece_size: Some(16),
        }
    }
}

/// 本地 mock HTTP server：每连接处理一个请求（`connection: close`），
/// 响应来自测试提供的 responder，请求全文记录供事后断言。
struct MockServer {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<RecordedHttpRequest>>>,
    handle: tokio::task::JoinHandle<()>,
}

impl MockServer {
    /// provider `base_url` 应指向的根地址。
    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// 已收到的请求（按到达序）。
    fn requests(&self) -> Vec<RecordedHttpRequest> {
        self.requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// 启动 mock server；`responder` 对每个请求返回脚本化响应。
///
/// # Errors
/// 监听端口绑定失败。
async fn spawn_mock_server<F, Fut>(responder: F) -> io::Result<MockServer>
where
    F: Fn(RecordedHttpRequest) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = MockHttpResponse> + Send,
{
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let requests: Arc<Mutex<Vec<RecordedHttpRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let Some(request) = read_request(&mut stream).await.ok() else {
                continue;
            };
            recorded
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(request.clone());
            let response = responder(request).await;
            write_response(&mut stream, &response).await.ok();
        }
    });
    Ok(MockServer {
        addr,
        requests,
        handle,
    })
}

async fn read_request(stream: &mut TcpStream) -> io::Result<RecordedHttpRequest> {
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "客户端在请求头发送完成前断开",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
    };

    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();

    let mut headers: HashMap<String, String> = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_lowercase(), value.trim().to_string());
        }
    }
    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);

    while buffer.len() < head_end + 4 + content_length {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "客户端在请求体发送完成前断开",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let body_bytes = buffer[head_end + 4..head_end + 4 + content_length].to_vec();
    let body_text = String::from_utf8_lossy(&body_bytes).into_owned();
    let body = serde_json::from_str(&body_text).ok();

    let (path, query) = match target.split_once('?') {
        Some((path, query_string)) => (path.to_string(), parse_query(query_string)),
        None => (target, HashMap::new()),
    };
    Ok(RecordedHttpRequest {
        method,
        path,
        query,
        headers,
        body,
    })
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| {
            let (name, value) = pair.split_once('=')?;
            Some((name.to_string(), value.to_string()))
        })
        .collect()
}

async fn write_response(stream: &mut TcpStream, response: &MockHttpResponse) -> io::Result<()> {
    let head = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        response.status,
        reason_phrase(response.status),
        response.content_type,
        response.body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    if let Some(size) = response.piece_size {
        for piece in response.body.chunks(size) {
            stream.write_all(piece).await?;
            stream.flush().await?;
            // 让 TCP 分段送达，驱动 reqwest 流式路径的增量解析。
            tokio::time::sleep(Duration::from_millis(3)).await;
        }
    } else {
        stream.write_all(&response.body).await?;
    }
    stream.flush().await?;
    stream.shutdown().await
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Status",
    }
}

// ---------------------------------------------------------------------------
// 测试辅助
// ---------------------------------------------------------------------------

/// 每测试独立 SQLite 临时库（Python `InMemoryRepository` 的等价物）。
/// 返回的 `TempDir` 必须保持存活到测试结束。
fn test_repository() -> (tempfile::TempDir, Arc<dyn Repository>) {
    let dir = tempfile::tempdir().expect("系统临时目录应可创建");
    let database = dir.path().join("provider_runtime.sqlite3");
    let repository = SqliteRepository::open(&database).expect("测试数据库必须可打开");
    (dir, Arc::new(repository))
}

/// 带 `base_url` / model / 快速超时的 provider 配置（失败测试不挂 60 秒）。
fn provider_config(
    name: &str,
    provider_type: ProviderType,
    base_url: String,
    model: &str,
) -> ProviderConfig {
    let mut provider = ProviderConfig::new(name.to_string(), provider_type);
    provider.base_url = Some(base_url);
    provider.model = Some(model.to_string());
    provider.timeout_seconds = 5;
    provider
}

fn openai_compatible_response(content: &str) -> MockHttpResponse {
    MockHttpResponse::json(
        200,
        &json!({
            "model": "gpt-test",
            "choices": [{"message": {"content": content}}],
        }),
    )
}

/// Cline 风格包装 envelope：完整 Chat Completions 响应嵌套在
/// `success/data` 里，model / usage / `finish_reason` 一并嵌套。
fn cline_envelope_response(content: &str) -> MockHttpResponse {
    MockHttpResponse::json(
        200,
        &json!({
            "success": true,
            "data": {
                "model": "z-ai/glm-5.3-flash",
                "choices": [{
                    "message": {"role": "assistant", "content": content},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 4, "completion_tokens": 2},
            },
        }),
    )
}

fn text_request<'a>(
    provider_id: &'a str,
    messages: &'a [LlmMessage],
    project_id: Option<&'a ProjectId>,
) -> TextGenerationRequest<'a> {
    TextGenerationRequest {
        provider_id,
        messages,
        purpose: "unit_test",
        project_id,
        run_id: None,
        task_id: None,
        model_override: None,
    }
}

// ---------------------------------------------------------------------------
// 移植测试（1/15 —— 14/15 对应 test_provider_runtime.py，15/15 为 M4 新增）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn openai_compatible_generate_text_records_invocation() {
    let (_dir, repo) = test_repository();
    let server = spawn_mock_server(|_| async move {
        MockHttpResponse::json(
            200,
            &json!({
                "model": "gpt-test",
                "choices": [{"message": {"content": "ok"}}],
                "usage": {"prompt_tokens": 3, "completion_tokens": 1},
            }),
        )
    })
    .await
    .expect("本地 mock server 必须可启动");

    let mut config = provider_config(
        "p",
        ProviderType::OpenaiCompatible,
        format!("{}/v1", server.base_url()),
        "gpt-test",
    );
    config.encrypted_api_key = Some("secret".to_string());
    let provider = repo.create_provider(&config).expect("provider 必须可创建");

    let runtime = OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造");
    let messages = [LlmMessage::new("user", "hello secret body".to_string())];
    let project_id = ProjectId::new("proj_x".to_string());
    let response = runtime
        .generate_text(text_request(
            provider.id.as_str(),
            &messages,
            Some(&project_id),
        ))
        .await
        .expect("生成必须成功");

    assert_eq!(response.text, "ok");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/v1/chat/completions");
    assert_eq!(requests[0].header("authorization"), "Bearer secret");

    let invocations = repo
        .list_model_invocations(Some("proj_x"))
        .expect("审计记录必须可读取");
    assert_eq!(invocations.len(), 1);
    let invocation = &invocations[0];
    assert_eq!(invocation.status, ModelInvocationStatus::Ok);
    assert_eq!(invocation.input_tokens, Some(3));
    assert_eq!(invocation.output_tokens, Some(1));
    assert!(
        invocation
            .prompt_hash
            .as_deref()
            .is_some_and(|hash| !hash.is_empty())
    );
    assert!(invocation.prompt_summary.contains("secret body"));
    assert_eq!(invocation.response_summary, "ok");
}

/// 思考过程必须从 wire 一路带到 `LlmResponse.reasoning`，并落到
/// `ModelInvocation.reasoning`。
///
/// 这是"会话里看不到思考过程"的端到端回归防线：历史上三层同时断——
/// wire 字段被丢、`LlmResponse` 没有位置放、落库没有列。任一层退化，
/// 这个测试都会红。
#[tokio::test]
async fn generate_text_surfaces_provider_reasoning_end_to_end() {
    let (_dir, repo) = test_repository();
    let server = spawn_mock_server(|_| async move {
        MockHttpResponse::json(
            200,
            &json!({
                "model": "gpt-test",
                "choices": [{"message": {
                    "content": "这是答案",
                    "reasoning_content": "先想一步，再想一步"
                }}],
                "usage": {"prompt_tokens": 3, "completion_tokens": 1},
            }),
        )
    })
    .await
    .expect("本地 mock server 必须可启动");

    let mut config = provider_config(
        "p",
        ProviderType::OpenaiCompatible,
        format!("{}/v1", server.base_url()),
        "gpt-test",
    );
    config.encrypted_api_key = Some("secret".to_string());
    let provider = repo.create_provider(&config).expect("provider 必须可创建");

    let runtime = OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造");
    let messages = [LlmMessage::new("user", "hi".to_string())];
    let project_id = ProjectId::new("proj_x".to_string());
    let response = runtime
        .generate_text(text_request(
            provider.id.as_str(),
            &messages,
            Some(&project_id),
        ))
        .await
        .expect("生成必须成功");

    // 第一层：归一化响应带上了思考。
    assert_eq!(response.text, "这是答案");
    assert_eq!(
        response.reasoning.as_deref(),
        Some("先想一步，再想一步"),
        "思考过程必须从 wire 进 LlmResponse"
    );

    // 第二层：审计记录也带上了思考（前端会话视图从这条读）。
    let invocations = repo
        .list_model_invocations(Some("proj_x"))
        .expect("审计记录必须可读取");
    assert_eq!(invocations.len(), 1);
    assert_eq!(
        invocations[0].reasoning.as_deref(),
        Some("先想一步，再想一步"),
        "思考过程必须落库到 ModelInvocation.reasoning"
    );
    // 思考与答案摘要互不污染。
    assert_eq!(invocations[0].response_summary, "这是答案");
    assert!(!invocations[0].response_summary.contains("先想一步"));
}

/// 没有思考的调用（非推理模型）不得被误判成错误：`reasoning` 为 `None`，
/// 调用照常成功。
#[tokio::test]
async fn generate_text_without_reasoning_is_none_and_still_succeeds() {
    let (_dir, repo) = test_repository();
    let server = spawn_mock_server(|_| async move {
        MockHttpResponse::json(
            200,
            &json!({
                "model": "gpt-test",
                "choices": [{"message": {"content": "ok"}}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1},
            }),
        )
    })
    .await
    .expect("本地 mock server 必须可启动");

    let mut config = provider_config(
        "p",
        ProviderType::OpenaiCompatible,
        format!("{}/v1", server.base_url()),
        "gpt-test",
    );
    config.encrypted_api_key = Some("secret".to_string());
    let provider = repo.create_provider(&config).expect("provider 必须可创建");

    let runtime = OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造");
    let messages = [LlmMessage::new("user", "hi".to_string())];
    let response = runtime
        .generate_text(text_request(provider.id.as_str(), &messages, None))
        .await
        .expect("缺思考不得让调用失败");

    assert_eq!(response.text, "ok");
    assert_eq!(
        response.reasoning, None,
        "没有思考字段时必须归一成 None"
    );
}

/// 思考要能在 JSON 序列化里存活：会话面板经 HTTP 拿到的就是这层。
#[test]
fn model_invocation_reasoning_survives_json_round_trip() {
    let mut invocation = ModelInvocation::new(
        ProviderId::new("p".to_string()),
        ProviderType::OpenaiCompatible,
        "natural_language_intake".to_string(),
    );
    assert_eq!(invocation.reasoning, None, "默认无思考");

    invocation.reasoning = Some("内心独白".to_string());
    let json = serde_json::to_value(&invocation).expect("必须可序列化");
    assert_eq!(json["reasoning"], serde_json::json!("内心独白"));

    let restored: ModelInvocation = serde_json::from_value(json).expect("必须可反序列化");
    assert_eq!(restored.reasoning.as_deref(), Some("内心独白"));

    // 老记录（没有 reasoning 键）必须能反序列化——后端 blob 存储里
    // 历史行没有这一列，反序列化不能因此炸。
    let legacy = serde_json::to_value(ModelInvocation::new(
        ProviderId::new("p".to_string()),
        ProviderType::OpenaiCompatible,
        "natural_language_intake".to_string(),
    ))
    .expect("必须可序列化");
    let mut legacy_obj = legacy.as_object().expect("顶层必须是对象").clone();
    legacy_obj.remove("reasoning");
    let restored: ModelInvocation =
        serde_json::from_value(serde_json::Value::Object(legacy_obj))
            .expect("缺 reasoning 键的老记录必须可反序列化");
    assert_eq!(restored.reasoning, None);
}

#[tokio::test]
async fn openai_compatible_failure_records_invocation_without_api_key() {
    let (_dir, repo) = test_repository();
    let server =
        spawn_mock_server(
            |_| async move { MockHttpResponse::json(500, &json!({"error": "boom"})) },
        )
        .await
        .expect("本地 mock server 必须可启动");

    let mut config = provider_config(
        "p",
        ProviderType::OpenaiCompatible,
        format!("{}/v1", server.base_url()),
        "gpt-test",
    );
    config.encrypted_api_key = Some("sk-never-log".to_string());
    let provider = repo.create_provider(&config).expect("provider 必须可创建");

    let runtime = OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造");
    let messages = [LlmMessage::new("user", "hello".to_string())];
    let project_id = ProjectId::new("proj_x".to_string());
    let error = runtime
        .generate_text(text_request(
            provider.id.as_str(),
            &messages,
            Some(&project_id),
        ))
        .await
        .expect_err("HTTP 500 必须报错");
    assert!(matches!(error, GatewayError::Status { status: 500, .. }));

    let invocations = repo
        .list_model_invocations(Some("proj_x"))
        .expect("审计记录必须可读取");
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].status, ModelInvocationStatus::Error);
    assert!(
        !invocations[0]
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("sk-never-log")
    );
}

#[tokio::test]
async fn prompt_and_error_redaction_covers_common_secret_headers() {
    let (_dir, repo) = test_repository();
    // Python 用 handler 抛出携带密钥的 httpx.HTTPError 复现错误文本；
    // Rust 侧等价物：HTTP 500 的 detail 文本携带密钥——两者都汇入审计
    // `error` 字段，脱敏语义一致。
    let server = spawn_mock_server(|_| async move {
        MockHttpResponse::json(
            500,
            &json!({"detail": "Authorization: Bearer sk-proj-requestsecret api-key=sk-response-secret"}),
        )
    })
    .await
    .expect("本地 mock server 必须可启动");

    let mut config = provider_config(
        "p",
        ProviderType::OpenaiCompatible,
        format!("{}/v1", server.base_url()),
        "gpt-test",
    );
    config
        .default_headers
        .insert("api-key".to_string(), "sk-header-secret".to_string());
    let provider = repo.create_provider(&config).expect("provider 必须可创建");

    let runtime = OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造");
    let messages = [LlmMessage::new(
        "user",
        "api-key=sk-prompt-secret Authorization: Bearer sk-prompt-bearer".to_string(),
    )];
    let project_id = ProjectId::new("proj_x".to_string());
    runtime
        .generate_text(text_request(
            provider.id.as_str(),
            &messages,
            Some(&project_id),
        ))
        .await
        .expect_err("HTTP 500 必须报错");

    let invocations = repo
        .list_model_invocations(Some("proj_x"))
        .expect("审计记录必须可读取");
    assert_eq!(invocations.len(), 1);
    let serialized = serde_json::to_string(&invocations[0]).expect("审计记录必须可序列化");
    assert!(!serialized.contains("sk-prompt-secret"));
    assert!(!serialized.contains("sk-prompt-bearer"));
    assert!(!serialized.contains("sk-response-secret"));
    assert!(!serialized.contains("sk-header-secret"));
    assert!(serialized.contains("********"));
}

#[tokio::test]
async fn health_check_unsupported_type_is_denied() {
    let (_dir, repo) = test_repository();
    let provider = repo
        .create_provider(&ProviderConfig::new(
            "claude".to_string(),
            ProviderType::ClaudeCode,
        ))
        .expect("provider 必须可创建");

    let runtime = OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造");
    let result = runtime
        .health_check(provider.id.as_str())
        .await
        .expect("健康检查必须返回结构化结果");

    assert_eq!(result.status, ModelInvocationStatus::Denied);
    assert!(result.message.contains("not supported"));
    let mut expected = serde_json::Map::new();
    expected.insert("text_generation".to_string(), Value::Bool(false));
    expected.insert("structured_output".to_string(), Value::Bool(false));
    assert_eq!(result.capabilities, expected);
}

#[tokio::test]
async fn health_check_requires_expected_text_and_reports_structured_failure() {
    let (_dir, repo) = test_repository();
    let server = {
        let calls = Arc::new(AtomicUsize::new(0));
        spawn_mock_server(move |_| {
            let calls = Arc::clone(&calls);
            async move {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    openai_compatible_response("ok")
                } else {
                    openai_compatible_response("not-json")
                }
            }
        })
        .await
        .expect("本地 mock server 必须可启动")
    };

    let provider = repo
        .create_provider(&provider_config(
            "p",
            ProviderType::OpenaiCompatible,
            format!("{}/v1", server.base_url()),
            "gpt-test",
        ))
        .expect("provider 必须可创建");

    let runtime = OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造");
    let result = runtime
        .health_check(provider.id.as_str())
        .await
        .expect("健康检查必须返回结构化结果");

    assert_eq!(result.status, ModelInvocationStatus::Ok);
    let mut expected = serde_json::Map::new();
    expected.insert("text_generation".to_string(), Value::Bool(true));
    expected.insert("structured_output".to_string(), Value::Bool(false));
    assert_eq!(result.capabilities, expected);
    assert_eq!(
        repo.list_model_invocations(None)
            .expect("审计记录必须可读取")
            .len(),
        2
    );
}

#[tokio::test]
async fn generate_structured_accepts_markdown_json_fence() {
    let (_dir, repo) = test_repository();
    let server = spawn_mock_server(|_| async move {
        MockHttpResponse::json(
            200,
            &json!({
                "model": "claude-test",
                "content": [
                    {
                        "type": "text",
                        "text": "Here is the result:\n```json\n{\"ok\": true}\n```\nDone."
                    }
                ],
            }),
        )
    })
    .await
    .expect("本地 mock server 必须可启动");

    let provider = repo
        .create_provider(&provider_config(
            "claude-proxy",
            ProviderType::Anthropic,
            server.base_url(),
            "claude-test",
        ))
        .expect("provider 必须可创建");

    let runtime = OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造");
    let messages = [LlmMessage::new("user", "return JSON".to_string())];
    let result = runtime
        .generate_structured(StructuredGenerationRequest {
            provider_id: provider.id.as_str(),
            messages: &messages,
            purpose: "unit_test",
            project_id: None,
            run_id: None,
            task_id: None,
        })
        .await
        .expect("围栏 JSON 必须可解析");
    assert_eq!(result.get("ok"), Some(&Value::Bool(true)));
}

#[tokio::test]
async fn health_check_wrong_content_is_error() {
    let (_dir, repo) = test_repository();
    let server = spawn_mock_server(|_| async move { openai_compatible_response("pong") })
        .await
        .expect("本地 mock server 必须可启动");

    let provider = repo
        .create_provider(&provider_config(
            "p",
            ProviderType::OpenaiCompatible,
            format!("{}/v1", server.base_url()),
            "gpt-test",
        ))
        .expect("provider 必须可创建");

    let runtime = OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造");
    let result = runtime
        .health_check(provider.id.as_str())
        .await
        .expect("健康检查必须返回结构化结果");

    assert_eq!(result.status, ModelInvocationStatus::Error);
    assert!(result.message.contains("expected token"));
    let mut expected = serde_json::Map::new();
    expected.insert("text_generation".to_string(), Value::Bool(false));
    expected.insert("structured_output".to_string(), Value::Bool(false));
    assert_eq!(result.capabilities, expected);
}

#[tokio::test]
async fn discover_models_normalizes_ids_and_reports_case_mismatch() {
    let (_dir, repo) = test_repository();
    let server = spawn_mock_server(|_| async move {
        MockHttpResponse::json(
            200,
            &json!({
                "object": "list",
                "data": [
                    {"id": "qwen3", "owned_by": "proxy"},
                    {"id": "glm-5.2", "owned_by": "zhipu"},
                ],
            }),
        )
    })
    .await
    .expect("本地 mock server 必须可启动");

    // Python：provider 不入库，直接传给 discover_models。
    // base_url 尾斜杠同时锻炼归一化剥离。
    let mut provider = provider_config(
        "glm proxy",
        ProviderType::OpenaiCompatible,
        format!("{}/v1/", server.base_url()),
        "GLM-5.2",
    );
    provider.encrypted_api_key = Some("secret".to_string());

    let runtime = OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造");
    let result = runtime
        .discover_models(&provider)
        .await
        .expect("模型发现必须返回结构化结果");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/v1/models");
    assert_eq!(requests[0].header("authorization"), "Bearer secret");

    assert_eq!(result.status, ModelInvocationStatus::Ok);
    assert_eq!(result.endpoint, format!("{}/v1/models", server.base_url()));
    assert_eq!(
        result.models,
        vec!["glm-5.2".to_string(), "qwen3".to_string()]
    );
    assert_eq!(result.configured_model_available, Some(false));
    assert!(result.message.contains("different casing"));
    assert!(result.message.contains("glm-5.2"));
}

#[tokio::test]
async fn discover_models_returns_safe_auth_diagnostic() {
    let (_dir, repo) = test_repository();
    let server = spawn_mock_server(|_| async move {
        MockHttpResponse::json(
            401,
            &json!({"detail": "API key sk-never-return is invalid"}),
        )
    })
    .await
    .expect("本地 mock server 必须可启动");

    let mut provider = provider_config(
        "p",
        ProviderType::OpenaiCompatible,
        format!("{}/v1", server.base_url()),
        "gpt-test",
    );
    provider.encrypted_api_key = Some("sk-never-return".to_string());

    let runtime = OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造");
    let result = runtime
        .discover_models(&provider)
        .await
        .expect("模型发现必须返回结构化结果");

    assert_eq!(result.status, ModelInvocationStatus::Denied);
    assert!(result.message.contains("model-list endpoint exists"));
    let serialized = serde_json::to_string(&result).expect("结果必须可序列化");
    assert!(!serialized.contains("sk-never-return"));
}

#[tokio::test]
async fn health_check_404_explains_model_and_protocol() {
    let (_dir, repo) = test_repository();
    let server = spawn_mock_server(|_| async move {
        MockHttpResponse::json(404, &json!({"detail": "model not found"}))
    })
    .await
    .expect("本地 mock server 必须可启动");

    let provider = repo
        .create_provider(&provider_config(
            "p",
            ProviderType::OpenaiCompatible,
            format!("{}/v1", server.base_url()),
            "GLM-5.2",
        ))
        .expect("provider 必须可创建");

    let runtime = OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造");
    let result = runtime
        .health_check(provider.id.as_str())
        .await
        .expect("健康检查必须返回结构化结果");

    assert_eq!(result.status, ModelInvocationStatus::Error);
    assert!(result.message.contains("case-sensitive model ID 'GLM-5.2'"));
    assert!(result.message.contains("Chat Completions protocol"));
    assert!(!result.message.contains("developer.mozilla.org"));
}

#[tokio::test]
async fn anthropic_generate_text_records_invocation() {
    let (_dir, repo) = test_repository();
    let server = spawn_mock_server(|_| async move {
        MockHttpResponse::json(
            200,
            &json!({
                "model": "claude-test",
                "content": [{"type": "text", "text": "ok"}],
                "usage": {"input_tokens": 5, "output_tokens": 2},
            }),
        )
    })
    .await
    .expect("本地 mock server 必须可启动");

    // Anthropic 根路径 base_url：归一化补 /v1（Python 同款场景）。
    let mut config = provider_config(
        "claude",
        ProviderType::Anthropic,
        server.base_url(),
        "claude-test",
    );
    config.encrypted_api_key = Some("anthropic-secret".to_string());
    let provider = repo.create_provider(&config).expect("provider 必须可创建");

    let runtime = OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造");
    let messages = [
        LlmMessage::new("system", "system note".to_string()),
        LlmMessage::new("user", "hello".to_string()),
    ];
    let project_id = ProjectId::new("proj_anthropic".to_string());
    let response = runtime
        .generate_text(text_request(
            provider.id.as_str(),
            &messages,
            Some(&project_id),
        ))
        .await
        .expect("生成必须成功");

    assert_eq!(response.text, "ok");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v1/messages");
    assert_eq!(requests[0].header("x-api-key"), "anthropic-secret");
    assert_eq!(requests[0].header("anthropic-version"), "2023-06-01");
    let body = requests[0].body_value();
    assert_eq!(
        body.get("model"),
        Some(&Value::String("claude-test".to_string()))
    );
    assert_eq!(
        body.get("system"),
        Some(&Value::String("system note".to_string()))
    );
    assert_eq!(
        body.get("messages"),
        Some(&json!([{"role": "user", "content": "hello"}]))
    );

    let invocations = repo
        .list_model_invocations(Some("proj_anthropic"))
        .expect("审计记录必须可读取");
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].input_tokens, Some(5));
    assert_eq!(invocations[0].output_tokens, Some(2));
}

#[tokio::test]
async fn anthropic_non_json_response_explains_base_url_mismatch() {
    let (_dir, repo) = test_repository();
    let server = spawn_mock_server(|_| async move {
        MockHttpResponse::text(
            200,
            "text/html",
            "<html><body>gateway home page</body></html>",
        )
    })
    .await
    .expect("本地 mock server 必须可启动");

    let provider = repo
        .create_provider(&provider_config(
            "claude",
            ProviderType::Anthropic,
            server.base_url(),
            "claude-test",
        ))
        .expect("provider 必须可创建");

    let runtime = OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造");
    let messages = [LlmMessage::new("user", "hello".to_string())];
    let error = runtime
        .generate_text(text_request(provider.id.as_str(), &messages, None))
        .await
        .expect_err("非 JSON 响应必须报错");
    let GatewayError::Protocol(message) = &error else {
        panic!("非 JSON 响应必须是协议错误，实际: {error:?}");
    };
    assert!(message.contains("/v1/messages API rather than its website"));
}

#[tokio::test]
async fn gemini_generate_text_records_invocation() {
    let (_dir, repo) = test_repository();
    let server = spawn_mock_server(|_| async move {
        MockHttpResponse::json(
            200,
            &json!({
                "candidates": [
                    {"content": {"parts": [{"text": "ok"}], "role": "model"}}
                ],
                "usageMetadata": {"promptTokenCount": 7, "candidatesTokenCount": 3},
            }),
        )
    })
    .await
    .expect("本地 mock server 必须可启动");

    let mut config = provider_config(
        "gemini",
        ProviderType::Gemini,
        format!("{}/v1beta", server.base_url()),
        "gemini-test",
    );
    config.encrypted_api_key = Some("gemini-secret".to_string());
    let provider = repo.create_provider(&config).expect("provider 必须可创建");

    let runtime = OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造");
    let messages = [
        LlmMessage::new("system", "system note".to_string()),
        LlmMessage::new("user", "hello".to_string()),
    ];
    let project_id = ProjectId::new("proj_gemini".to_string());
    let response = runtime
        .generate_text(text_request(
            provider.id.as_str(),
            &messages,
            Some(&project_id),
        ))
        .await
        .expect("生成必须成功");

    assert_eq!(response.text, "ok");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].path,
        "/v1beta/models/gemini-test:generateContent"
    );
    assert_eq!(
        requests[0].query.get("key").map(String::as_str),
        Some("gemini-secret")
    );
    let body = requests[0].body_value();
    assert_eq!(
        body.get("systemInstruction"),
        Some(&json!({"parts": [{"text": "system note"}]}))
    );
    assert_eq!(
        body.get("contents"),
        Some(&json!([{"role": "user", "parts": [{"text": "hello"}]}]))
    );

    let invocations = repo
        .list_model_invocations(Some("proj_gemini"))
        .expect("审计记录必须可读取");
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].input_tokens, Some(7));
    assert_eq!(invocations[0].output_tokens, Some(3));
}

#[tokio::test]
async fn provider_router_falls_back_and_opens_failed_route_circuit() {
    let (_dir, repo) = test_repository();
    let server = spawn_mock_server(move |request| async move {
        let model = request
            .body
            .as_ref()
            .and_then(|body| body.get("model"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if model == "first-model" {
            MockHttpResponse::json(500, &json!({"error": "boom"}))
        } else {
            MockHttpResponse::json(
                200,
                &json!({
                    "model": "second-model",
                    "choices": [{"message": {"content": "ok fallback"}}],
                }),
            )
        }
    })
    .await
    .expect("本地 mock server 必须可启动");

    let first = repo
        .create_provider(&provider_config(
            "first",
            ProviderType::OpenaiCompatible,
            format!("{}/v1", server.base_url()),
            "first-model",
        ))
        .expect("provider 必须可创建");
    let second = repo
        .create_provider(&provider_config(
            "second",
            ProviderType::OpenaiCompatible,
            format!("{}/v1", server.base_url()),
            "second-model",
        ))
        .expect("provider 必须可创建");

    let mut failing = ProviderRouteBinding::new("advisor", first.id.clone()).expect("路由必须合法");
    failing.priority = 100;
    failing.max_failures = 1;
    let failing = repo
        .upsert_provider_route(&failing)
        .expect("路由必须可写入");
    let mut fallback =
        ProviderRouteBinding::new("advisor", second.id.clone()).expect("路由必须合法");
    fallback.priority = 10;
    repo.upsert_provider_route(&fallback)
        .expect("路由必须可写入");

    let runtime =
        Arc::new(OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造"));
    let router = ProviderRouterRuntime::new(Arc::clone(&runtime));
    let messages = [LlmMessage::new("user", "review".to_string())];
    let project_id = ProjectId::new("proj_router".to_string());
    let response = router
        .generate_text_for_purpose("advisor", &messages, Some(&project_id), None, None)
        .await
        .expect("fallback 必须成功");

    assert_eq!(response.text, "ok fallback");

    let updated = repo
        .get_provider_route(failing.id.as_str())
        .expect("路由必须可读取")
        .expect("失败路由必须存在");
    assert_eq!(updated.failure_count, 1);
    assert!(updated.circuit_open_until.is_some());

    let invocations = repo
        .list_model_invocations(Some("proj_router"))
        .expect("审计记录必须可读取");
    let models: Vec<&str> = invocations
        .iter()
        .map(|invocation| invocation.model.as_deref().unwrap_or_default())
        .collect();
    assert_eq!(models, vec!["first-model", "second-model"]);
}

#[tokio::test]
async fn generate_text_stream_delivers_deltas_and_records_invocation() {
    let (_dir, repo) = test_repository();
    let sse_body = concat!(
        "data: {\"model\":\"glm-test\",\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{}}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":2}}\n\n",
        "data: [DONE]\n\n",
    );
    let server = spawn_mock_server(move |_| async move { MockHttpResponse::sse(sse_body) })
        .await
        .expect("本地 mock server 必须可启动");

    let provider = repo
        .create_provider(&provider_config(
            "glm",
            ProviderType::OpenaiCompatible,
            format!("{}/v1", server.base_url()),
            "glm-test",
        ))
        .expect("provider 必须可创建");

    let runtime =
        Arc::new(OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造"));
    let messages = [LlmMessage::new("user", "stream hello".to_string())];
    let mut stream = runtime
        .generate_text_stream(&TextGenerationRequest {
            provider_id: provider.id.as_str(),
            messages: &messages,
            purpose: "unit_test",
            project_id: None,
            run_id: None,
            task_id: None,
            model_override: None,
        })
        .expect("流式预检必须通过");

    let mut deltas: Vec<String> = Vec::new();
    let mut outcome: Option<StreamOutcome> = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(GatewayStreamItem::Delta(delta)) => deltas.push(delta),
            Ok(GatewayStreamItem::Completed(completed)) => outcome = Some(completed),
            Err(error) => panic!("流式生成不应失败: {error}"),
        }
    }

    assert_eq!(deltas, vec!["Hel".to_string(), "lo".to_string()]);
    let outcome = outcome.expect("流必须以 Completed 收尾");
    assert_eq!(outcome.text, "Hello");
    assert_eq!(outcome.model.as_deref(), Some("glm-test"));
    assert_eq!(outcome.input_tokens, Some(2));
    assert_eq!(outcome.output_tokens, Some(2));

    let invocations = repo
        .list_model_invocations(None)
        .expect("审计记录必须可读取");
    assert_eq!(invocations.len(), 1);
    let invocation = &invocations[0];
    assert_eq!(invocation.status, ModelInvocationStatus::Ok);
    assert_eq!(invocation.response_summary, "Hello");
    assert_eq!(invocation.input_tokens, Some(2));
    assert_eq!(invocation.output_tokens, Some(2));
    assert_eq!(invocation.id, outcome.model_invocation_id);

    // 流式请求体必须携带 stream: true。
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].body_value().get("stream"),
        Some(&Value::Bool(true))
    );
}

// ---------------------------------------------------------------------------
// envelope 归一化（success/data 包装、显式失败、fail closed）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chat_completions_success_data_envelope_generates_text() {
    let (_dir, repo) = test_repository();
    let server = spawn_mock_server(|_| async move { cline_envelope_response("wrapped ok") })
        .await
        .expect("本地 mock server 必须可启动");

    let provider = repo
        .create_provider(&provider_config(
            "cline",
            ProviderType::OpenaiCompatible,
            format!("{}/api/v1", server.base_url()),
            "z-ai/glm-5.3-flash",
        ))
        .expect("provider 必须可创建");

    let runtime = OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造");
    let messages = [LlmMessage::new("user", "hello".to_string())];
    let response = runtime
        .generate_text(text_request(provider.id.as_str(), &messages, None))
        .await
        .expect("包装 envelope 必须可生成");

    // 嵌套层的 model / usage / finish_reason 必须原样抽取。
    assert_eq!(response.text, "wrapped ok");
    assert_eq!(response.model.as_deref(), Some("z-ai/glm-5.3-flash"));
    assert_eq!(response.finish_reason.as_deref(), Some("stop"));

    let invocations = repo
        .list_model_invocations(None)
        .expect("审计记录必须可读取");
    assert_eq!(invocations.len(), 1);
    assert_eq!(invocations[0].input_tokens, Some(4));
    assert_eq!(invocations[0].output_tokens, Some(2));
    assert_eq!(invocations[0].response_summary, "wrapped ok");
}

#[tokio::test]
async fn health_check_passes_on_cline_envelope() {
    let (_dir, repo) = test_repository();
    // health_check 顺序固定：先文本探测（"Return the word ok."），后结构化
    // 探测（"Return exactly {"ok": true}."）。两次响应都是 Cline 包装
    // envelope——归一化必须让两个探测都走到抽取层。
    let server = spawn_mock_server(|request| async move {
        let wants_structured = request
            .body
            .as_ref()
            .and_then(|body| body.get("messages"))
            .and_then(Value::as_array)
            .and_then(|messages| messages.first())
            .and_then(|first| first.get("content"))
            .and_then(Value::as_str)
            .is_some_and(|content| content.contains("Return exactly"));
        if wants_structured {
            cline_envelope_response(r#"{"ok": true}"#)
        } else {
            cline_envelope_response("ok")
        }
    })
    .await
    .expect("本地 mock server 必须可启动");

    let provider = repo
        .create_provider(&provider_config(
            "cline",
            ProviderType::OpenaiCompatible,
            format!("{}/api/v1", server.base_url()),
            "z-ai/glm-5.3-flash",
        ))
        .expect("provider 必须可创建");

    let runtime = OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造");
    let result = runtime
        .health_check(provider.id.as_str())
        .await
        .expect("健康检查必须返回结构化结果");

    assert_eq!(result.status, ModelInvocationStatus::Ok);
    assert_eq!(result.model.as_deref(), Some("z-ai/glm-5.3-flash"));
    assert_eq!(
        result.capabilities.get("text_generation"),
        Some(&Value::Bool(true))
    );
    assert_eq!(
        result.capabilities.get("structured_output"),
        Some(&Value::Bool(true))
    );
}

#[tokio::test]
async fn chat_completions_success_false_envelope_is_redacted_protocol_error() {
    let (_dir, repo) = test_repository();
    let server = spawn_mock_server(|_| async move {
        MockHttpResponse::json(
            200,
            &json!({
                "success": false,
                "error": "upstream quota exhausted (api-key=sk-envelope-leak)",
            }),
        )
    })
    .await
    .expect("本地 mock server 必须可启动");

    let mut config = provider_config(
        "cline",
        ProviderType::OpenaiCompatible,
        format!("{}/api/v1", server.base_url()),
        "z-ai/glm-5.3-flash",
    );
    config.encrypted_api_key = Some("sk-request-side-secret".to_string());
    let provider = repo.create_provider(&config).expect("provider 必须可创建");

    let runtime = OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造");
    let messages = [LlmMessage::new("user", "hello".to_string())];
    let error = runtime
        .generate_text(text_request(provider.id.as_str(), &messages, None))
        .await
        .expect_err("success=false 必须报错");
    let GatewayError::Protocol(message) = &error else {
        panic!("失败 envelope 必须是协议错误，实际: {error:?}");
    };
    assert!(message.contains("provider response reported failure"));
    assert!(message.contains("quota exhausted"));
    assert!(!message.contains("sk-envelope-leak"));

    // 审计记录不得携带响应 detail 里的密钥，也不得携带请求侧密钥。
    let invocations = repo
        .list_model_invocations(None)
        .expect("审计记录必须可读取");
    assert_eq!(invocations.len(), 1);
    let serialized = serde_json::to_string(&invocations[0]).expect("审计记录必须可序列化");
    assert!(!serialized.contains("sk-envelope-leak"));
    assert!(!serialized.contains("sk-request-side-secret"));
    assert!(serialized.contains("********"));
}

// ---------------------------------------------------------------------------
// 目的路由（空 provider_id = 未钉定 → 按 purpose 路由；非空 = 钉定直连）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn router_routes_unpinned_request_by_purpose() {
    let (_dir, repo) = test_repository();
    // 两个 mock gateway：default 命中 A，路由绑定命中 B。B 记录请求体供
    // 断言 model_override 是否生效。
    let (default_server, routed_server) = tokio::join!(
        spawn_mock_server(|_| async move { openai_compatible_response("default side") }),
        spawn_mock_server(|request| async move {
            MockHttpResponse::json(
                200,
                &json!({
                    "model": request.body_value().get("model").cloned().unwrap_or_default(),
                    "choices": [{"message": {"content": "{\"ok\": true}"}}],
                }),
            )
        }),
    );
    let default_server = default_server.expect("默认 gateway 必须可启动");
    let routed_server = routed_server.expect("路由 gateway 必须可启动");

    let mut default_provider = provider_config(
        "default",
        ProviderType::OpenaiCompatible,
        format!("{}/v1", default_server.base_url()),
        "default-model",
    );
    default_provider.is_default = true;
    repo.create_provider(&default_provider)
        .expect("provider 必须可创建");
    let routed = repo
        .create_provider(&provider_config(
            "routed",
            ProviderType::OpenaiCompatible,
            format!("{}/v1", routed_server.base_url()),
            "routed-model",
        ))
        .expect("provider 必须可创建");

    let mut binding =
        ProviderRouteBinding::new("toolset_select", routed.id.clone()).expect("路由必须合法");
    binding.model_override = Some("override-model".to_string());
    repo.upsert_provider_route(&binding)
        .expect("路由必须可写入");

    let runtime =
        Arc::new(OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造"));
    let gateway = ProviderRouterRuntime::new(Arc::clone(&runtime));
    let messages = [LlmMessage::new("user", "profile this".to_string())];
    let result = gateway
        .generate_structured(StructuredGenerationRequest {
            // 空 provider_id = 未钉定 → 按 purpose 路由。
            provider_id: "",
            messages: &messages,
            purpose: "toolset_select",
            project_id: None,
            run_id: None,
            task_id: None,
        })
        .await
        .expect("路由调用必须成功");
    assert_eq!(result.get("ok"), Some(&Value::Bool(true)));

    // 默认 gateway 零命中；路由 gateway 承接调用且 model_override 生效。
    assert!(
        default_server.requests().is_empty(),
        "未钉定请求不得落回默认 provider"
    );
    let requests = routed_server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].body_value().get("model"),
        Some(&Value::String("override-model".to_string()))
    );
}

#[tokio::test]
async fn router_explicit_provider_pins_even_when_route_exists() {
    let (_dir, repo) = test_repository();
    let (pinned_server, routed_server) = tokio::join!(
        spawn_mock_server(|_| async move { openai_compatible_response("pinned side") }),
        spawn_mock_server(|_| async move { openai_compatible_response("routed side") }),
    );
    let pinned_server = pinned_server.expect("钉定 gateway 必须可启动");
    let routed_server = routed_server.expect("路由 gateway 必须可启动");

    let pinned = repo
        .create_provider(&provider_config(
            "pinned",
            ProviderType::OpenaiCompatible,
            format!("{}/v1", pinned_server.base_url()),
            "pinned-model",
        ))
        .expect("provider 必须可创建");
    let routed = repo
        .create_provider(&provider_config(
            "routed",
            ProviderType::OpenaiCompatible,
            format!("{}/v1", routed_server.base_url()),
            "routed-model",
        ))
        .expect("provider 必须可创建");
    let binding =
        ProviderRouteBinding::new("agent_tool_harness", routed.id.clone()).expect("路由必须合法");
    repo.upsert_provider_route(&binding)
        .expect("路由必须可写入");

    let runtime =
        Arc::new(OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造"));
    let gateway = ProviderRouterRuntime::new(runtime);
    let messages = [LlmMessage::new("user", "solve".to_string())];
    let response = gateway
        .generate_text(TextGenerationRequest {
            provider_id: pinned.id.as_str(),
            messages: &messages,
            purpose: "agent_tool_harness",
            project_id: None,
            run_id: None,
            task_id: None,
            model_override: None,
        })
        .await
        .expect("钉定调用必须成功");
    assert_eq!(response.text, "pinned side");

    assert!(
        routed_server.requests().is_empty(),
        "显式钉定不得被路由表劫持"
    );
    assert_eq!(pinned_server.requests().len(), 1);
}

#[tokio::test]
async fn router_unpinned_request_falls_back_to_default_provider() {
    let (_dir, repo) = test_repository();
    let server = spawn_mock_server(|_| async move { openai_compatible_response("default ok") })
        .await
        .expect("本地 mock server 必须可启动");
    let mut default_provider = provider_config(
        "default",
        ProviderType::OpenaiCompatible,
        format!("{}/v1", server.base_url()),
        "default-model",
    );
    default_provider.is_default = true;
    repo.create_provider(&default_provider)
        .expect("provider 必须可创建");

    let runtime =
        Arc::new(OpenAiCompatibleProviderRuntime::new(Arc::clone(&repo)).expect("网关必须可构造"));
    let gateway = ProviderRouterRuntime::new(runtime);
    let messages = [LlmMessage::new("user", "hello".to_string())];
    // 无任何路由：未钉定请求回落默认 provider。
    let response = gateway
        .generate_text(TextGenerationRequest {
            provider_id: "",
            messages: &messages,
            purpose: "advisor",
            project_id: None,
            run_id: None,
            task_id: None,
            model_override: None,
        })
        .await
        .expect("无路由必须回落默认 provider");
    assert_eq!(response.text, "default ok");
    assert_eq!(server.requests().len(), 1);

    // 无路由且无默认 provider：报可解释错误（含 purpose）。
    let empty_repo_runtime = {
        let (_dir, empty_repo) = test_repository();
        Arc::new(OpenAiCompatibleProviderRuntime::new(empty_repo).expect("网关必须可构造"))
    };
    let gateway = ProviderRouterRuntime::new(empty_repo_runtime);
    let error = gateway
        .generate_text(TextGenerationRequest {
            provider_id: "",
            messages: &messages,
            purpose: "reflector",
            project_id: None,
            run_id: None,
            task_id: None,
            model_override: None,
        })
        .await
        .expect_err("无路由无默认必须报错");
    let message = error.message;
    assert!(message.contains("purpose 'reflector'"), "actual: {message}");
}
