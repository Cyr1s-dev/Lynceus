//! 测试用内存 provider runtime —— Python 侧无对应物，为 M4 网关测试与
//! M5+ 编排引擎测试提供不触网的 [`ProviderRuntime`] 实现。
//!
//! 语义镜像真实网关的契约面：provider 未知/禁用报错；结构化生成 =
//! 文本响应 + JSON 解析（复用真实网关的围栏解析器）；响应队列 FIFO
//! 耗尽即报错（显式失败优于静默重复）。

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::MutexGuard;

use agents::llm::LlmMessage;
use agents::llm::LlmResponse;
use agents::llm::ProviderCallError;
use agents::llm::ProviderModelDiscoveryRuntime;
use agents::llm::ProviderRuntime;
use agents::llm::StructuredGenerationRequest;
use agents::llm::TextGenerationRequest;
use models::provider::ModelInvocationStatus;
use models::provider::ProviderConfig;
use models::provider::ProviderHealthResult;
use models::provider::ProviderModelDiscoveryResult;
use serde_json::Map;
use serde_json::Value;

use super::payload::parse_structured_response;

/// 记录到 mock 的一次生成请求（测试断言用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    /// 请求的 provider 标识。
    pub provider_id: String,
    /// 调用目的标签。
    pub purpose: String,
    /// 请求消息序列。
    pub messages: Vec<LlmMessage>,
}

enum QueuedResponse {
    Text(String),
    Error(String),
}

#[derive(Default)]
struct MockState {
    providers: Vec<ProviderConfig>,
    responses: VecDeque<QueuedResponse>,
    requests: Vec<RecordedRequest>,
}

/// 内存 provider runtime：预置响应序列 + 请求记录。
pub struct MockProviderRuntime {
    state: Mutex<MockState>,
}

impl MockProviderRuntime {
    /// 空 mock（无 provider、无响应）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(MockState::default()),
        }
    }

    /// 注册 provider 配置（`list_providers` / `get_provider` /
    /// `resolve_default_provider` / 生成调用的存在性校验可见）。
    pub fn register_provider(&mut self, provider: ProviderConfig) {
        self.lock_mut().providers.push(provider);
    }

    /// 预置一条文本响应（FIFO 队尾追加）。
    pub fn push_text(&mut self, text: &str) {
        self.lock_mut()
            .responses
            .push_back(QueuedResponse::Text(text.to_string()));
    }

    /// 预置一条失败响应（FIFO 队尾追加）。
    pub fn push_error(&mut self, message: &str) {
        self.lock_mut()
            .responses
            .push_back(QueuedResponse::Error(message.to_string()));
    }

    /// 已记录的生成请求（按到达序）。
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.lock().requests.clone()
    }

    fn lock(&self) -> MutexGuard<'_, MockState> {
        // 仅在持锁期间 panic 才会毒化；本类型的临界区不 panic。
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 独占可变访问（`&mut self` 下 `get_mut` 免锁，毒化语义与 [`Self::lock`] 一致）。
    fn lock_mut(&mut self) -> &mut MockState {
        self.state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 共享核心：存在性/启用校验 + 请求记录 + 响应出队，返回
    /// `(配置模型, 文本响应)`。
    fn take_response(
        &self,
        provider_id: &str,
        purpose: &str,
        messages: &[LlmMessage],
    ) -> Result<(Option<String>, String), ProviderCallError> {
        let mut state = self.lock();
        let (enabled, model) = {
            let provider = state
                .providers
                .iter()
                .find(|provider| provider.id.as_str() == provider_id)
                .ok_or_else(|| {
                    ProviderCallError::new(&format!("unknown provider: {provider_id}"))
                })?;
            (provider.enabled, provider.model.clone())
        };
        if !enabled {
            return Err(ProviderCallError::new(&format!(
                "provider disabled: {provider_id}"
            )));
        }
        state.requests.push(RecordedRequest {
            provider_id: provider_id.to_string(),
            purpose: purpose.to_string(),
            messages: messages.to_vec(),
        });
        match state.responses.pop_front() {
            Some(QueuedResponse::Text(text)) => Ok((model, text)),
            Some(QueuedResponse::Error(message)) => Err(ProviderCallError {
                message,
                truncated: false,
            }),
            None => Err(ProviderCallError::new(
                "mock provider has no queued response",
            )),
        }
    }
}

impl Default for MockProviderRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ProviderRuntime for MockProviderRuntime {
    async fn list_providers(&self) -> Result<Vec<ProviderConfig>, ProviderCallError> {
        Ok(self.lock().providers.clone())
    }

    async fn get_provider(
        &self,
        provider_id: &str,
    ) -> Result<Option<ProviderConfig>, ProviderCallError> {
        Ok(self
            .lock()
            .providers
            .iter()
            .find(|provider| provider.id.as_str() == provider_id)
            .cloned())
    }

    async fn resolve_default_provider(&self) -> Result<Option<ProviderConfig>, ProviderCallError> {
        Ok(self
            .lock()
            .providers
            .iter()
            .find(|provider| provider.enabled && provider.is_default)
            .cloned())
    }

    async fn health_check(
        &self,
        provider_id: &str,
    ) -> Result<ProviderHealthResult, ProviderCallError> {
        let state = self.lock();
        let provider = state
            .providers
            .iter()
            .find(|provider| provider.id.as_str() == provider_id)
            .ok_or_else(|| ProviderCallError::new(&format!("unknown provider: {provider_id}")))?;
        let mut capabilities = Map::new();
        capabilities.insert("text_generation".to_string(), Value::Bool(true));
        capabilities.insert("structured_output".to_string(), Value::Bool(true));
        let mut result = ProviderHealthResult::new(
            provider.id.clone(),
            ModelInvocationStatus::Ok,
            "mock provider".to_string(),
        );
        result.model.clone_from(&provider.model);
        result.capabilities = capabilities;
        Ok(result)
    }

    async fn generate_text(
        &self,
        request: TextGenerationRequest<'_>,
    ) -> Result<LlmResponse, ProviderCallError> {
        let (model, text) =
            self.take_response(request.provider_id, request.purpose, request.messages)?;
        Ok(LlmResponse {
            text,
            model,
            model_invocation_id: None,
            raw: Map::new(),
            finish_reason: None,
            reasoning: None,
        })
    }

    async fn generate_structured(
        &self,
        request: StructuredGenerationRequest<'_>,
    ) -> Result<Map<String, Value>, ProviderCallError> {
        let (_, text) =
            self.take_response(request.provider_id, request.purpose, request.messages)?;
        parse_structured_response(&text).map_err(|error| ProviderCallError::new(&error.to_string()))
    }
}

#[async_trait::async_trait]
impl ProviderModelDiscoveryRuntime for MockProviderRuntime {
    async fn discover_models(
        &self,
        provider: &ProviderConfig,
    ) -> Result<ProviderModelDiscoveryResult, ProviderCallError> {
        Ok(ProviderModelDiscoveryResult {
            provider_id: Some(provider.id.clone()),
            status: ModelInvocationStatus::Ok,
            message: "mock discovery".to_string(),
            endpoint: "mock://models".to_string(),
            models: Vec::new(),
            configured_model: provider.model.clone(),
            configured_model_available: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agents::llm::ProviderRuntime;
    use models::provider::ProviderType;

    fn provider(name: &str) -> ProviderConfig {
        let mut config = ProviderConfig::new(name.to_string(), ProviderType::OpenaiCompatible);
        config.model = Some("mock-model".to_string());
        config
    }

    fn text_request<'a>(
        provider_id: &'a str,
        messages: &'a [LlmMessage],
    ) -> TextGenerationRequest<'a> {
        TextGenerationRequest {
            provider_id,
            messages,
            purpose: "unit_test",
            project_id: None,
            run_id: None,
            task_id: None,
            model_override: None,
        }
    }

    fn structured_request<'a>(
        provider_id: &'a str,
        messages: &'a [LlmMessage],
    ) -> StructuredGenerationRequest<'a> {
        StructuredGenerationRequest {
            provider_id,
            messages,
            purpose: "unit_test",
            project_id: None,
            run_id: None,
            task_id: None,
        }
    }

    #[tokio::test]
    async fn unknown_provider_is_rejected() {
        let runtime = MockProviderRuntime::new();
        let messages = [LlmMessage::new("user", "hello".to_string())];
        let error = runtime
            .generate_text(text_request("missing", &messages))
            .await
            .expect_err("未注册 provider 必须报错");
        assert!(error.message.contains("unknown provider: missing"));
    }

    #[tokio::test]
    async fn responses_are_consumed_in_fifo_order_and_recorded() {
        let mut runtime = MockProviderRuntime::new();
        let provider = provider("p");
        let provider_id = provider.id.to_string();
        runtime.register_provider(provider);
        runtime.push_text("first");
        runtime.push_error("boom");
        runtime.push_text(r#"{"ok": true}"#);

        let messages = [LlmMessage::new("user", "hello".to_string())];
        let first = runtime
            .generate_text(text_request(&provider_id, &messages))
            .await
            .expect("首条响应必须成功");
        assert_eq!(first.text, "first");
        assert_eq!(first.model.as_deref(), Some("mock-model"));

        let error = runtime
            .generate_text(text_request(&provider_id, &messages))
            .await
            .expect_err("第二条预置错误必须生效");
        assert_eq!(error.message, "boom");

        let structured = ProviderRuntime::generate_structured(
            &runtime,
            structured_request(&provider_id, &messages),
        )
        .await
        .expect("结构化响应必须解析 JSON 对象");
        assert_eq!(structured.get("ok"), Some(&Value::Bool(true)));

        let requests = runtime.requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].purpose, "unit_test");
        assert_eq!(requests[2].messages[0].content, "hello");

        let exhausted = runtime
            .generate_text(text_request(&provider_id, &messages))
            .await
            .expect_err("队列耗尽必须显式报错");
        assert!(exhausted.message.contains("no queued response"));
    }

    #[tokio::test]
    async fn provider_runtime_parses_queued_json() {
        let mut runtime = MockProviderRuntime::new();
        let provider = provider("p");
        let provider_id = provider.id.to_string();
        runtime.register_provider(provider);
        runtime.push_text("```json\n{\"answer\": 42}\n```");

        let messages = [LlmMessage::new("user", "q".to_string())];
        let value = ProviderRuntime::generate_structured(
            &runtime,
            structured_request(&provider_id, &messages),
        )
        .await
        .expect("围栏 JSON 必须可解析");
        assert_eq!(value.get("answer"), Some(&Value::from(42)));
    }
}
