//! 流式文本生成 —— M4 新增的 Rust 侧能力（Python 网关无流式路径）。
//!
//! 仅支持 `OpenAI` Chat Completions 兼容家族的 SSE 流（`GLM` / `DeepSeek` /
//! Ollama / vLLM / LM Studio 均走该方言）；Anthropic / Gemini 的流式方言
//! 留待后续里程碑移植。后台任务持有全部可变状态，审计落库在任务收尾
//! 单点完成；消费端丢弃通道后任务以已收到的部分文本收尾并退出。
//!
//! 超时语义镜像 httpx 分相超时：连接/响应头阶段整体限时
//! `timeout_seconds`，流式读取阶段每次读操作独立限时（总时长不设限，
//! 与 reqwest 的请求级总超时语义相反——总超时会掐断合法的慢流）。

use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use agents::llm::TextGenerationRequest;
use models::common::utcnow;
use models::ids::ModelInvocationId;
use models::provider::ModelInvocation;
use models::provider::ModelInvocationStatus;
use models::provider::ProviderType;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use super::error::GatewayError;
use super::error::response_error_detail;
use super::payload::build_payload;
use super::redact::hash_text;
use super::redact::python_json_dumps_messages;
use super::redact::redact_secrets;
use super::redact::summarize_messages;
use super::redact::summarize_text;
use super::runtime::OpenAiCompatibleProviderRuntime;
use super::runtime::apply_model_override;
use super::runtime::chat_completions_url;
use super::runtime::is_supported;
use super::sse::SseEvent;
use super::sse::SseParser;
use super::sse::extract_stream_delta;
use super::sse::extract_stream_model;
use super::sse::extract_stream_usage;

/// 通道缓冲上限：流分块产量小（远小于 token 产量），32 足以让消费端
/// 短暂停顿时后台任务继续读网络。
const CHANNEL_CAPACITY: usize = 32;

/// 流式生成吐出的类型化事件。
#[derive(Debug)]
pub enum GatewayStreamItem {
    /// 一段增量文本。
    Delta(String),
    /// 流正常结束（审计记录已落库）。
    Completed(StreamOutcome),
}

/// 流式生成的收尾元数据。
#[derive(Debug, Clone)]
pub struct StreamOutcome {
    /// 拼接后的完整文本。
    pub text: String,
    /// 流分块报告的模型 ID。
    pub model: Option<String>,
    /// 审计记录 ID。
    pub model_invocation_id: ModelInvocationId,
    /// 输入 token 用量（末块 `usage`，缺失为 `None`）。
    pub input_tokens: Option<i64>,
    /// 输出 token 用量。
    pub output_tokens: Option<i64>,
}

impl OpenAiCompatibleProviderRuntime {
    /// 流式生成文本（OpenAI Chat Completions 家族 SSE）。
    ///
    /// 预检失败（provider 未知/禁用/不支持/缺 model）立即返回 `Err` 且
    /// **不落审计**——与一次性路径的预检语义一致；流中途失败先落 ERROR
    /// 审计（响应摘要留空，镜像一次性路径的失败审计形状）再经通道吐
    /// `Err`。
    ///
    /// # Errors
    /// 预检失败；HTTP 客户端初始化异常。
    pub fn generate_text_stream(
        self: &Arc<Self>,
        request: &TextGenerationRequest<'_>,
    ) -> Result<ReceiverStream<Result<GatewayStreamItem, GatewayError>>, GatewayError> {
        let provider = self.require_enabled_provider(request.provider_id)?;
        let provider = apply_model_override(provider, request.model_override);
        if !is_supported(provider.provider_type) {
            return Err(GatewayError::config(&format!(
                "provider type {} is not supported",
                provider.provider_type.as_str()
            )));
        }
        if matches!(
            provider.provider_type,
            ProviderType::Anthropic | ProviderType::Gemini
        ) {
            return Err(GatewayError::config(&format!(
                "streaming is not supported for provider type {}",
                provider.provider_type.as_str()
            )));
        }
        if provider.model.as_deref().is_none_or(str::is_empty) {
            return Err(GatewayError::config("provider.model is required"));
        }

        let mut invocation = ModelInvocation::new(
            provider.id.clone(),
            provider.provider_type,
            request.purpose.to_string(),
        );
        invocation.project_id = request.project_id.cloned();
        invocation.run_id = request.run_id.cloned();
        invocation.task_id = request.task_id.cloned();
        invocation.model.clone_from(&provider.model);
        invocation.prompt_summary = summarize_messages(request.messages);
        invocation.prompt_hash = Some(hash_text(&python_json_dumps_messages(request.messages)));

        let mut payload = build_payload(&provider, request.messages);
        if let Value::Object(payload_map) = &mut payload {
            payload_map.insert("stream".to_string(), Value::Bool(true));
        }

        let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            run_stream_task(runtime, provider, payload, invocation, sender).await;
        });
        Ok(ReceiverStream::new(receiver))
    }
}

/// SSE 事件累积器：拼接文本、捕获用量与模型 ID、转发增量。
struct StreamPipeline {
    text: String,
    model: Option<String>,
    usage: (Option<i64>, Option<i64>),
    sender: mpsc::Sender<Result<GatewayStreamItem, GatewayError>>,
    consumer_alive: bool,
}

impl StreamPipeline {
    fn new(sender: mpsc::Sender<Result<GatewayStreamItem, GatewayError>>) -> Self {
        Self {
            text: String::new(),
            model: None,
            usage: (None, None),
            sender,
            consumer_alive: true,
        }
    }

    async fn process(&mut self, event: SseEvent) {
        if let Some(delta) = extract_stream_delta(&event.data)
            && !delta.is_empty()
        {
            self.text.push_str(&delta);
            if self.consumer_alive {
                // 消费端丢弃通道后不再发送，但继续读完流以完成审计。
                self.consumer_alive = self
                    .sender
                    .send(Ok(GatewayStreamItem::Delta(delta)))
                    .await
                    .is_ok();
            }
        }
        if let Some(usage) = extract_stream_usage(&event.data) {
            self.usage = usage;
        }
        if self.model.is_none() {
            self.model = extract_stream_model(&event.data);
        }
    }
}

async fn run_stream_task(
    runtime: Arc<OpenAiCompatibleProviderRuntime>,
    provider: models::provider::ProviderConfig,
    payload: Value,
    mut invocation: ModelInvocation,
    sender: mpsc::Sender<Result<GatewayStreamItem, GatewayError>>,
) {
    let t0 = Instant::now();
    let mut pipeline = StreamPipeline::new(sender.clone());

    let result: Result<(), GatewayError> = async {
        let url = chat_completions_url(&provider);
        let headers = runtime.chat_completions_headers(&provider)?;
        // 连接 + 响应头阶段整体限时（httpx 连接超时的镜像）。
        let send = tokio::time::timeout(
            Duration::from_secs(provider.timeout_seconds),
            runtime
                .client()
                .post(&url)
                .json(&payload)
                .headers(headers)
                .send(),
        )
        .await
        .map_err(|_| GatewayError::Timeout)?
        .map_err(GatewayError::from)?;

        let status = send.status();
        if !status.is_success() {
            let body = send.text().await.unwrap_or_default();
            return Err(GatewayError::Status {
                status: status.as_u16(),
                url,
                detail: response_error_detail(&body),
            });
        }

        // 流式读取不做总超时（会掐断合法慢流）；每次读独立限时。
        let read_timeout = Duration::from_secs(provider.timeout_seconds);
        let mut stream = send.bytes_stream();
        let mut parser = SseParser::default();
        loop {
            let Ok(chunk) = tokio::time::timeout(read_timeout, stream.next()).await else {
                return Err(GatewayError::Timeout);
            };
            let Some(chunk) = chunk else { break };
            let chunk = chunk.map_err(GatewayError::from)?;
            for event in parser.feed(&chunk) {
                pipeline.process(event).await;
            }
        }
        for event in parser.finish() {
            pipeline.process(event).await;
        }
        Ok(())
    }
    .await;

    invocation.duration_ms = Some(i64::try_from(t0.elapsed().as_millis()).unwrap_or(i64::MAX));
    invocation.finished_at = Some(utcnow());

    let terminal: Result<GatewayStreamItem, GatewayError> = match result {
        Ok(()) => {
            invocation.status = ModelInvocationStatus::Ok;
            invocation.response_summary = summarize_text(&pipeline.text);
            invocation.response_hash = Some(hash_text(&pipeline.text));
            invocation.input_tokens = pipeline.usage.0;
            invocation.output_tokens = pipeline.usage.1;
            let outcome = StreamOutcome {
                text: pipeline.text,
                model: pipeline.model,
                model_invocation_id: invocation.id.clone(),
                input_tokens: pipeline.usage.0,
                output_tokens: pipeline.usage.1,
            };
            runtime
                .repository()
                .add_model_invocation(&invocation)
                .map(|_| GatewayStreamItem::Completed(outcome))
                .map_err(GatewayError::from)
        }
        Err(error) => {
            invocation.status = ModelInvocationStatus::Error;
            invocation.error = Some(redact_secrets(&error.python_error_string()));
            match runtime.repository().add_model_invocation(&invocation) {
                // Python 语义：审计落库失败覆盖原始错误后重抛。
                Err(storage_error) => Err(GatewayError::from(storage_error)),
                Ok(_) => Err(error),
            }
        }
    };

    // 终态事件发送失败 = 消费端已退出，无法送达（审计已尽力落库）。
    let _ = sender.send(terminal).await;
}
