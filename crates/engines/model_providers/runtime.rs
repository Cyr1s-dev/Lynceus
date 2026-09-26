//! `OpenAI` 兼容 provider 网关 —— Python `OpenAICompatibleProviderRuntime`
//! 的移植。
//!
//! 真实 HTTP 调用属 engines 层（Python 同）；core 只见 trait 与审计模型。
//! 与 Python 的**有意**差异（记录于 `docs/REFACTOR_PROGRESS.md`）：
//!
//! 1. 错误以 [`GatewayError`] 穷尽枚举承载（Python 是开放异常集合），
//!    `python_error_string()` 复刻 `{type(exc).__name__}: {exc}` 审计文本
//!    形状，但 httpx 异常的英文长消息无法逐字节复刻——差分对拍不覆盖
//!    网关审计的 `error` 字段（无真实 API 的测试无法产生同源异常）；
//! 2. `client` 注入位保留（reqwest 客户端），重定向默认**禁用**
//!    （httpx 默认不跟随重定向，reqwest 默认跟随——网关必须把 3xx 当
//!    错误暴露给配置方）；
//! 3. `_record_model_operation`（Mission 操作日志）依赖 `core.workers`
//!    的 `SwarmOperationJournal`，该模块尚未移植——此处整体延后，
//!    仓储与 runtime 契约不受影响。

use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use agents::llm::LlmMessage;
use agents::llm::LlmResponse;
use agents::llm::ProviderCallError;
use agents::llm::ProviderModelDiscoveryRuntime;
use agents::llm::ProviderRuntime;
use agents::llm::StructuredGenerationRequest;
use agents::llm::TextGenerationRequest;
use models::common::utcnow;
use models::provider::ModelInvocation;
use models::provider::ModelInvocationStatus;
use models::provider::ProviderConfig;
use models::provider::ProviderHealthResult;
use models::provider::ProviderModelDiscoveryResult;
use models::provider::ProviderType;
use serde_json::Map;
use serde_json::Value;
use storage::Repository;
use tokio::sync::OwnedSemaphorePermit;
use tokio::sync::Semaphore;

use super::error::GatewayError;
use super::error::format_model_discovery_error;
use super::error::format_provider_error;
use super::error::provider_error_status;
use super::error::response_error_detail;
use super::payload::build_payload;
use super::payload::extract_discovered_model_ids;
use super::payload::extract_finish_reason;
use super::payload::extract_model;
use super::payload::extract_provider_reasoning;
use super::payload::extract_provider_text;
use super::payload::extract_usage;
use super::payload::finish_reason_is_truncated;
use super::payload::normalize_chat_completions_envelope;
use super::payload::parse_structured_response;
use super::redact::hash_text;
use super::redact::python_json_dumps_messages;
use super::redact::redact_secrets;

const DEFAULT_PROVIDER_MAX_CONCURRENCY: usize = 3;
use super::redact::summarize_messages;
use super::redact::summarize_text;
use super::secrets::PlaintextSecretStore;
use super::secrets::SecretStore;
use super::urls::normalize_base_url;

/// `SUPPORTED_TYPES` 的镜像（Python 集合成员的补集）：排除
/// `codex_cli` / `claude_code` / `mcp_remote` 三类 CLI/MCP 运行时。
pub(crate) const fn is_supported(provider_type: ProviderType) -> bool {
    !matches!(
        provider_type,
        ProviderType::CodexCli | ProviderType::ClaudeCode | ProviderType::McpRemote
    )
}

/// `model_override` 应用（Python `model_copy(update={"model": ...})`）：
/// 空串视为未覆盖（Python falsy 语义）。
pub(crate) fn apply_model_override(
    mut provider: ProviderConfig,
    model_override: Option<&str>,
) -> ProviderConfig {
    if let Some(model_override) = model_override.filter(|value| !value.is_empty()) {
        provider.model = Some(model_override.to_string());
    }
    provider
}

/// Chat Completions 端点 URL（`post_chat_completions` 与流式路径共享）。
pub(crate) fn chat_completions_url(provider: &ProviderConfig) -> String {
    let base_url = normalize_base_url(provider.base_url.as_deref(), provider.provider_type);
    format!("{base_url}/chat/completions")
}

fn elapsed_ms(started: Instant) -> i64 {
    i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)
}

/// 一次 provider 调用的归一化产物。
///
/// 取代原先的 `(String, String, Option<String>)` 三元组：加上 reasoning
/// 之后四元组里有两个 `Option<String>`，位置极易搞错。命名字段让"答案 /
/// 模型 / 结束原因 / 思考"各归其位。
struct ProviderCallOutcome {
    /// 归一化答案文本。
    text: String,
    /// 响应报告的模型 ID。
    model: String,
    /// wire 层结束原因原值。
    finish_reason: Option<String>,
    /// 模型思考过程原文（无思考为 `None`）。
    reasoning: Option<String>,
}

/// HTTP 模型 provider 网关（Python `OpenAICompatibleProviderRuntime`）。
pub struct OpenAiCompatibleProviderRuntime {
    repository: Arc<dyn Repository>,
    secret_store: Box<dyn SecretStore>,
    client: reqwest::Client,
    request_limiter: Arc<Semaphore>,
}

impl OpenAiCompatibleProviderRuntime {
    /// 以默认 secret store（[`PlaintextSecretStore`]）与默认 HTTP 客户端
    /// 构造。
    ///
    /// # Errors
    /// HTTP 客户端初始化失败（rustls 后端初始化异常，实际不可达）。
    pub fn new(repository: Arc<dyn Repository>) -> Result<Self, GatewayError> {
        // httpx 默认不跟随重定向；reqwest 默认跟随——网关把 3xx 当错误。
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                GatewayError::Transport(format!("failed to initialize HTTP client: {error}"))
            })?;
        Ok(Self {
            repository,
            secret_store: Box::<PlaintextSecretStore>::default(),
            client,
            request_limiter: Arc::new(Semaphore::new(DEFAULT_PROVIDER_MAX_CONCURRENCY)),
        })
    }

    /// 注入 secret store（builder 风格）。
    #[must_use]
    pub fn with_secret_store(mut self, secret_store: Box<dyn SecretStore>) -> Self {
        self.secret_store = secret_store;
        self
    }

    /// 注入 HTTP 客户端（builder 风格）。
    #[must_use]
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// Limit concurrent outbound provider HTTP requests made through this runtime.
    #[must_use]
    pub fn with_max_concurrent_requests(mut self, limit: usize) -> Self {
        self.request_limiter = Arc::new(Semaphore::new(limit.max(1)));
        self
    }

    /// 仓储句柄（router 与上层编排共享同一仓储）。
    #[must_use]
    pub fn repository(&self) -> &Arc<dyn Repository> {
        &self.repository
    }

    /// HTTP 客户端句柄（流式路径共享连接池）。
    pub(crate) fn client(&self) -> &reqwest::Client {
        &self.client
    }

    async fn request_permit(&self) -> Result<OwnedSemaphorePermit, GatewayError> {
        self.request_limiter
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| GatewayError::Transport("provider concurrency limiter closed".to_string()))
    }

    /// 列全部 Provider 配置（Python `list_providers`）。
    ///
    /// # Errors
    /// 存储读取失败。
    pub fn list_providers(&self) -> Result<Vec<ProviderConfig>, GatewayError> {
        self.repository.list_providers().map_err(GatewayError::from)
    }

    /// 按 id 取 Provider 配置（Python `get_provider`）。
    ///
    /// # Errors
    /// 存储读取失败。
    pub fn get_provider(&self, provider_id: &str) -> Result<Option<ProviderConfig>, GatewayError> {
        self.repository
            .get_provider(provider_id)
            .map_err(GatewayError::from)
    }

    /// 解析默认 provider：首个 `enabled` 且 `is_default`
    /// （Python `resolve_default_provider`）。
    ///
    /// # Errors
    /// 存储读取失败。
    pub fn resolve_default_provider(&self) -> Result<Option<ProviderConfig>, GatewayError> {
        let providers = self
            .repository
            .list_providers()
            .map_err(GatewayError::from)?;
        Ok(providers
            .into_iter()
            .find(|provider| provider.enabled && provider.is_default))
    }

    /// 取 provider 并校验启用状态（Python `_require_enabled_provider`）。
    pub(crate) fn require_enabled_provider(
        &self,
        provider_id: &str,
    ) -> Result<ProviderConfig, GatewayError> {
        let provider = self
            .repository
            .get_provider(provider_id)
            .map_err(GatewayError::from)?
            .ok_or_else(|| GatewayError::unknown_provider(provider_id))?;
        if !provider.enabled {
            return Err(GatewayError::disabled_provider(provider_id));
        }
        Ok(provider)
    }

    /// 生成文本（Python `generate_text`）：审计记录先建骨架，成功/失败
    /// 都落库（预检失败——未知/禁用/不支持/缺 model——不落库，Python 同）。
    ///
    /// # Errors
    /// 网络/协议/存储等一切失败场景；审计记录已落库后再返回错误。
    pub async fn generate_text(
        &self,
        request: TextGenerationRequest<'_>,
    ) -> Result<LlmResponse, GatewayError> {
        let provider = self.require_enabled_provider(request.provider_id)?;
        let provider = apply_model_override(provider, request.model_override);
        if !is_supported(provider.provider_type) {
            return Err(GatewayError::config(&format!(
                "provider type {} is not supported",
                provider.provider_type.as_str()
            )));
        }
        if provider.model.as_deref().is_none_or(str::is_empty) {
            return Err(GatewayError::config("provider.model is required"));
        }
        let _permit = self.request_permit().await?;

        let started = utcnow();
        let t0 = Instant::now();
        let mut invocation = ModelInvocation::new(
            provider.id.clone(),
            provider.provider_type,
            request.purpose.to_string(),
        );
        invocation.project_id = request.project_id.cloned();
        invocation.run_id = request.run_id.cloned();
        invocation.task_id = request.task_id.cloned();
        invocation.model = provider.model.clone();
        invocation.prompt_summary = summarize_messages(request.messages);
        invocation.prompt_hash = Some(hash_text(&python_json_dumps_messages(request.messages)));
        invocation.started_at = started;

        match self
            .call_provider(&mut invocation, &provider, request.messages)
            .await
        {
            Ok(outcome) => {
                invocation.status = ModelInvocationStatus::Ok;
                invocation.duration_ms = Some(elapsed_ms(t0));
                invocation.finished_at = Some(utcnow());
                self.repository.add_model_invocation(&invocation)?;
                Ok(LlmResponse {
                    text: outcome.text,
                    model: Some(outcome.model),
                    model_invocation_id: Some(invocation.id.clone()),
                    raw: Map::new(),
                    finish_reason: outcome.finish_reason,
                    reasoning: outcome.reasoning,
                })
            }
            Err(error) => {
                invocation.status = ModelInvocationStatus::Error;
                invocation.error = Some(redact_secrets(&error.python_error_string()));
                invocation.duration_ms = Some(elapsed_ms(t0));
                invocation.finished_at = Some(utcnow());
                self.repository.add_model_invocation(&invocation)?;
                Err(error)
            }
        }
    }

    /// 生成结构化响应（Python `generate_structured`）：文本生成 + 围栏
    /// JSON 解析。解析失败发生在审计落库**之后**（成功记录已入库）。
    ///
    /// 截断守卫：解析失败且 wire 结束原因为输出上限（`length` /
    /// `max_tokens`）时返回 [`GatewayError::Truncated`]——被截断的 JSON
    /// 不受信，调用方按可重试的预算现象处理；JSON 完整时截断只砍掉了
    /// 尾部额外输出，照常返回。
    ///
    /// # Errors
    /// 同 [`Self::generate_text`]；响应不是合法 JSON 对象（截断时为
    /// [`GatewayError::Truncated`]）。
    pub async fn generate_structured(
        &self,
        request: StructuredGenerationRequest<'_>,
    ) -> Result<Map<String, Value>, GatewayError> {
        let response = self
            .generate_text(TextGenerationRequest {
                provider_id: request.provider_id,
                messages: request.messages,
                purpose: request.purpose,
                project_id: request.project_id,
                run_id: request.run_id,
                task_id: request.task_id,
                model_override: None,
            })
            .await?;
        let parsed = parse_structured_response(&response.text);
        if parsed.is_err()
            && response
                .finish_reason
                .as_deref()
                .is_some_and(finish_reason_is_truncated)
        {
            return Err(GatewayError::Truncated);
        }
        parsed
    }

    /// 健康检查（Python `health_check`）：文本探测要求响应含独立 token
    /// `ok`，结构化探测接受 `{"ok": true}`。探测失败以结构化结果返回，
    /// 不进 `Err`。
    ///
    /// # Errors
    /// provider 未知或已禁用（Python `KeyError` / `ValueError` 路径）。
    pub async fn health_check(
        &self,
        provider_id: &str,
    ) -> Result<ProviderHealthResult, GatewayError> {
        let provider = self.require_enabled_provider(provider_id)?;
        if !is_supported(provider.provider_type) {
            let mut result = ProviderHealthResult::new(
                provider.id.clone(),
                ModelInvocationStatus::Denied,
                format!(
                    "provider type {} is not supported yet",
                    provider.provider_type.as_str()
                ),
            );
            result.model.clone_from(&provider.model);
            result.capabilities = false_capabilities();
            return Ok(result);
        }

        let mut capabilities = false_capabilities();

        let probe = vec![LlmMessage::new("user", "Return the word ok.".to_string())];
        let response = match self
            .generate_text(TextGenerationRequest {
                provider_id: provider.id.as_str(),
                messages: &probe,
                purpose: "provider_health_check",
                project_id: None,
                run_id: None,
                task_id: None,
                model_override: None,
            })
            .await
        {
            Ok(response) => response,
            Err(error) => {
                let mut result = ProviderHealthResult::new(
                    provider.id.clone(),
                    provider_error_status(&error),
                    format_provider_error(&error, &provider),
                );
                result.model.clone_from(&provider.model);
                result.capabilities = capabilities;
                return Ok(result);
            }
        };
        if !response
            .text
            .trim()
            .to_lowercase()
            .split_whitespace()
            .any(|token| token == "ok")
        {
            let error = GatewayError::protocol(
                "provider health response did not contain expected token 'ok'",
            );
            let mut result = ProviderHealthResult::new(
                provider.id.clone(),
                provider_error_status(&error),
                format_provider_error(&error, &provider),
            );
            result.model.clone_from(&provider.model);
            result.capabilities = capabilities;
            return Ok(result);
        }
        capabilities.insert("text_generation".to_string(), Value::Bool(true));

        let structured_probe = vec![LlmMessage::new(
            "user",
            r#"Return exactly {"ok": true}."#.to_string(),
        )];
        if let Ok(structured) = self
            .generate_structured(StructuredGenerationRequest {
                provider_id: provider.id.as_str(),
                messages: &structured_probe,
                purpose: "provider_health_check_structured",
                project_id: None,
                run_id: None,
                task_id: None,
            })
            .await
            && structured.get("ok") == Some(&Value::Bool(true))
        {
            capabilities.insert("structured_output".to_string(), Value::Bool(true));
        }

        let mut result = ProviderHealthResult::new(
            provider.id.clone(),
            ModelInvocationStatus::Ok,
            "ok".to_string(),
        );
        result.model = response.model;
        result.model_invocation_id = response.model_invocation_id;
        result.capabilities = capabilities;
        Ok(result)
    }

    /// 拉取并归一化 provider 广播的模型标识（Python `discover_models`）。
    /// 端点失败以结构化诊断返回（脱敏），不进 `Err`、不落审计。
    ///
    /// # Errors
    /// 仅保留给不可恢复的调用方错误。
    pub async fn discover_models(
        &self,
        provider: &ProviderConfig,
    ) -> Result<ProviderModelDiscoveryResult, GatewayError> {
        let base_url = normalize_base_url(provider.base_url.as_deref(), provider.provider_type);
        let endpoint = format!("{base_url}/models");
        let request = self.models_request(provider, &endpoint)?;
        let _permit = self.request_permit().await?;

        let outcome: Result<Vec<String>, GatewayError> = async {
            let response = request.send().await.map_err(GatewayError::from)?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(GatewayError::Status {
                    status: status.as_u16(),
                    url: endpoint.clone(),
                    detail: response_error_detail(&body),
                });
            }
            let text = response.text().await.map_err(GatewayError::from)?;
            let data: Value = serde_json::from_str(&text).map_err(|_| {
                GatewayError::protocol("provider model endpoint returned invalid JSON")
            })?;
            if !data.is_object() {
                return Err(GatewayError::protocol(
                    "provider model endpoint returned non-object JSON",
                ));
            }
            let models = extract_discovered_model_ids(provider, &data);
            if models.is_empty() {
                return Err(GatewayError::protocol(
                    "provider model endpoint returned no recognizable model IDs",
                ));
            }
            Ok(models)
        }
        .await;

        let models = match outcome {
            Ok(models) => models,
            Err(error) => {
                return Ok(ProviderModelDiscoveryResult {
                    provider_id: Some(provider.id.clone()),
                    status: provider_error_status(&error),
                    message: format_model_discovery_error(&error, &endpoint),
                    endpoint,
                    models: Vec::new(),
                    configured_model: provider.model.clone(),
                    configured_model_available: None,
                });
            }
        };

        let configured_available = provider.model.as_ref().map(|model| models.contains(model));
        let mut message = format!("Fetched {} model(s).", models.len());
        if provider.model.is_some()
            && configured_available == Some(false)
            && let Some(model) = provider.model.as_ref()
        {
            let case_match = models
                .iter()
                .find(|candidate| candidate.to_lowercase() == model.to_lowercase());
            let suffix = match case_match {
                Some(case_match) => format!(
                    " Configured model '{model}' has different casing; use exact ID '{case_match}'."
                ),
                None => format!(" Configured model '{model}' was not advertised."),
            };
            message.push_str(&suffix);
        }
        Ok(ProviderModelDiscoveryResult {
            provider_id: Some(provider.id.clone()),
            status: ModelInvocationStatus::Ok,
            message,
            endpoint,
            models,
            configured_model: provider.model.clone(),
            configured_model_available: configured_available,
        })
    }

    /// `/models` 探测请求构造（Python `discover_models` 的请求段）：按
    /// provider 家族挂密钥（Anthropic 头 / Gemini 查询参数 / 其余 Bearer）。
    fn models_request(
        &self,
        provider: &ProviderConfig,
        endpoint: &str,
    ) -> Result<reqwest::RequestBuilder, GatewayError> {
        let mut headers = Self::header_map(provider)?;
        let secret = self.secret_store.resolve(provider);
        let mut request = self
            .client
            .get(endpoint)
            .timeout(Duration::from_secs(provider.timeout_seconds));
        match provider.provider_type {
            ProviderType::Anthropic => {
                if let Some(secret) = secret {
                    Self::insert_default_header(&mut headers, "x-api-key", &secret)?;
                }
                Self::insert_default_header(&mut headers, "anthropic-version", "2023-06-01")?;
            }
            ProviderType::Gemini => {
                if let Some(secret) = secret {
                    request = request.query(&[("key", secret)]);
                }
            }
            _ => {
                if let Some(secret) = secret {
                    Self::insert_default_header(
                        &mut headers,
                        "Authorization",
                        &format!("Bearer {secret}"),
                    )?;
                }
            }
        }
        Ok(request.headers(headers))
    }

    /// 请求执行 + 审计字段填充（Python `generate_text` try 块主体）。
    async fn call_provider(
        &self,
        invocation: &mut ModelInvocation,
        provider: &ProviderConfig,
        messages: &[LlmMessage],
    ) -> Result<ProviderCallOutcome, GatewayError> {
        let payload = build_payload(provider, messages);
        let data = self.post_provider(provider, &payload).await?;
        let text = extract_provider_text(provider.provider_type, &data)?;
        // 思考过程与答案文本分开抽取、分开落库。缺思考不是错误——非推理
        // 模型与未开 extended thinking 的调用都理所当然返回 None。
        let reasoning = extract_provider_reasoning(provider.provider_type, &data)?;
        let usage = extract_usage(provider.provider_type, &data);
        invocation.response_summary = summarize_text(&text);
        invocation.response_hash = Some(hash_text(&text));
        invocation.reasoning = reasoning.clone();
        invocation.input_tokens = usage.0;
        invocation.output_tokens = usage.1;
        Ok(ProviderCallOutcome {
            text,
            model: extract_model(provider, &data),
            finish_reason: extract_finish_reason(provider.provider_type, &data),
            reasoning,
        })
    }

    /// 按 provider 家族分发请求（Python `_post_provider`）。
    async fn post_provider(
        &self,
        provider: &ProviderConfig,
        payload: &Value,
    ) -> Result<Value, GatewayError> {
        match provider.provider_type {
            ProviderType::Anthropic => self.post_anthropic_messages(provider, payload).await,
            ProviderType::Gemini => self.post_gemini_generate_content(provider, payload).await,
            _ => self.post_chat_completions(provider, payload).await,
        }
    }

    /// Chat Completions 端点（Python `_post_chat_completions`）。响应在
    /// 进入字段抽取前先做 envelope 归一化（顶层 `choices` /
    /// `success`+`data` 包装 / 显式失败 / fail closed）。
    async fn post_chat_completions(
        &self,
        provider: &ProviderConfig,
        payload: &Value,
    ) -> Result<Value, GatewayError> {
        let url = chat_completions_url(provider);
        let headers = self.chat_completions_headers(provider)?;
        let response = self
            .client
            .post(&url)
            .json(payload)
            .headers(headers)
            .timeout(Duration::from_secs(provider.timeout_seconds))
            .send()
            .await
            .map_err(GatewayError::from)?;
        Self::decode_json_object(response, &url)
            .await
            .and_then(normalize_chat_completions_envelope)
    }

    /// Chat Completions 请求头：默认头 + Bearer 密钥
    /// （Python `headers.setdefault("Authorization", ...)`）。
    pub(crate) fn chat_completions_headers(
        &self,
        provider: &ProviderConfig,
    ) -> Result<reqwest::header::HeaderMap, GatewayError> {
        let mut headers = Self::header_map(provider)?;
        if let Some(secret) = self.secret_store.resolve(provider) {
            Self::insert_default_header(
                &mut headers,
                "Authorization",
                &format!("Bearer {secret}"),
            )?;
        }
        Ok(headers)
    }

    /// Anthropic Messages 端点（Python `_post_anthropic_messages`）：非 JSON
    /// 响应触发可解释的 Base URL 配错错误。
    async fn post_anthropic_messages(
        &self,
        provider: &ProviderConfig,
        payload: &Value,
    ) -> Result<Value, GatewayError> {
        let base_url = normalize_base_url(provider.base_url.as_deref(), provider.provider_type);
        let url = format!("{base_url}/messages");
        let mut headers = Self::header_map(provider)?;
        if let Some(secret) = self.secret_store.resolve(provider) {
            Self::insert_default_header(&mut headers, "x-api-key", &secret)?;
        }
        Self::insert_default_header(&mut headers, "anthropic-version", "2023-06-01")?;

        let response = self
            .client
            .post(&url)
            .json(payload)
            .headers(headers)
            .timeout(Duration::from_secs(provider.timeout_seconds))
            .send()
            .await
            .map_err(GatewayError::from)?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(GatewayError::Status {
                status: status.as_u16(),
                url,
                detail: response_error_detail(&body),
            });
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("unknown")
            .to_string();
        let text = response.text().await.map_err(GatewayError::from)?;
        let data: Value = serde_json::from_str(&text).map_err(|_| {
            GatewayError::protocol(&format!(
                "Anthropic Messages endpoint returned a non-JSON response \
                 (content-type: {content_type}). Verify that the Base URL resolves \
                 to the provider's /v1/messages API rather than its website."
            ))
        })?;
        if !data.is_object() {
            return Err(GatewayError::protocol("provider returned non-object JSON"));
        }
        Ok(data)
    }

    /// Gemini generateContent 端点（Python `_post_gemini_generate_content`）：
    /// 密钥走查询参数。
    async fn post_gemini_generate_content(
        &self,
        provider: &ProviderConfig,
        payload: &Value,
    ) -> Result<Value, GatewayError> {
        let base_url = normalize_base_url(provider.base_url.as_deref(), provider.provider_type);
        let model = provider.model.clone().unwrap_or_default();
        let url = format!("{base_url}/models/{model}:generateContent");
        let headers = Self::header_map(provider)?;
        let mut request = self
            .client
            .post(&url)
            .json(payload)
            .headers(headers)
            .timeout(Duration::from_secs(provider.timeout_seconds));
        if let Some(secret) = self.secret_store.resolve(provider) {
            request = request.query(&[("key", secret)]);
        }
        let response = request.send().await.map_err(GatewayError::from)?;
        Self::decode_json_object(response, &url).await
    }

    /// 默认头构建（Python `dict(provider.default_headers)`）。
    fn header_map(provider: &ProviderConfig) -> Result<reqwest::header::HeaderMap, GatewayError> {
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in provider.default_headers.iter() {
            let header_name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| GatewayError::config(&format!("invalid header name: {name}")))?;
            let header_value = reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| GatewayError::config(&format!("invalid header value for {name}")))?;
            headers.insert(header_name, header_value);
        }
        Ok(headers)
    }

    /// `setdefault` 语义的头插入（键不存在时才写入；HeaderMap 键比较
    /// 大小写不敏感，与 httpx 发送侧合并语义一致）。
    fn insert_default_header(
        headers: &mut reqwest::header::HeaderMap,
        name: &str,
        value: &str,
    ) -> Result<(), GatewayError> {
        let header_name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| GatewayError::config(&format!("invalid header name: {name}")))?;
        let header_value = reqwest::header::HeaderValue::from_str(value)
            .map_err(|_| GatewayError::config(&format!("invalid header value for {name}")))?;
        headers.entry(header_name).or_insert(header_value);
        Ok(())
    }

    /// 2xx + JSON 对象校验（Python `raise_for_status` + `response.json()` +
    /// `isinstance(data, dict)`）。
    async fn decode_json_object(
        response: reqwest::Response,
        url: &str,
    ) -> Result<Value, GatewayError> {
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(GatewayError::Status {
                status: status.as_u16(),
                url: url.to_string(),
                detail: response_error_detail(&body),
            });
        }
        let text = response.text().await.map_err(GatewayError::from)?;
        let data: Value = serde_json::from_str(&text)
            .map_err(|_| GatewayError::protocol("provider response was not valid JSON"))?;
        if !data.is_object() {
            return Err(GatewayError::protocol("provider returned non-object JSON"));
        }
        Ok(data)
    }
}

/// Python `capabilities = {"text_generation": False, "structured_output": False}`
/// 的键序镜像。
fn false_capabilities() -> Map<String, Value> {
    let mut capabilities = Map::new();
    capabilities.insert("text_generation".to_string(), Value::Bool(false));
    capabilities.insert("structured_output".to_string(), Value::Bool(false));
    capabilities
}

#[async_trait::async_trait]
impl ProviderRuntime for OpenAiCompatibleProviderRuntime {
    async fn list_providers(&self) -> Result<Vec<ProviderConfig>, ProviderCallError> {
        self.list_providers().map_err(ProviderCallError::from)
    }

    async fn get_provider(
        &self,
        provider_id: &str,
    ) -> Result<Option<ProviderConfig>, ProviderCallError> {
        self.get_provider(provider_id)
            .map_err(ProviderCallError::from)
    }

    async fn resolve_default_provider(&self) -> Result<Option<ProviderConfig>, ProviderCallError> {
        self.resolve_default_provider()
            .map_err(ProviderCallError::from)
    }

    async fn health_check(
        &self,
        provider_id: &str,
    ) -> Result<ProviderHealthResult, ProviderCallError> {
        self.health_check(provider_id)
            .await
            .map_err(ProviderCallError::from)
    }

    async fn generate_text(
        &self,
        request: TextGenerationRequest<'_>,
    ) -> Result<LlmResponse, ProviderCallError> {
        self.generate_text(request)
            .await
            .map_err(ProviderCallError::from)
    }

    async fn generate_structured(
        &self,
        request: StructuredGenerationRequest<'_>,
    ) -> Result<Map<String, Value>, ProviderCallError> {
        self.generate_structured(request)
            .await
            .map_err(ProviderCallError::from)
    }
}

#[async_trait::async_trait]
impl ProviderModelDiscoveryRuntime for OpenAiCompatibleProviderRuntime {
    async fn discover_models(
        &self,
        provider: &ProviderConfig,
    ) -> Result<ProviderModelDiscoveryResult, ProviderCallError> {
        self.discover_models(provider)
            .await
            .map_err(ProviderCallError::from)
    }
}
