//! LLM provider 抽象 —— `server/core/llm.py` 的完整移植。
//!
//! core 只定义契约（Protocol），具体 HTTP/SDK/CLI/MCP 实现属于 engines
//! 层（`engines::model_providers`），依赖边界与 Python 侧一致：
//! 本 crate 不依赖 reqwest / 厂商 SDK / shell 执行。
//!
//! trait 家族：
//! - [`ProviderRuntime`]：`server/core/llm.py` `ProviderRuntime` 的镜像——
//!   provider 配置读取、健康检查、文本/结构化生成；
//! - [`ProviderModelDiscoveryRuntime`]：可选扩展，列举远端可用模型。

use models::ids::ModelInvocationId;
use models::ids::ProjectId;
use models::ids::RunId;
use models::ids::TaskId;
use models::provider::ProviderConfig;
use models::provider::ProviderHealthResult;
use models::provider::ProviderModelDiscoveryResult;
use serde_json::Map;
use serde_json::Value;

/// 一条 chat 风格消息（Python `LLMMessage(role, content)` 的镜像）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmMessage {
    /// 角色（`system` / `user` / `assistant`）。
    pub role: String,
    /// 消息内容。
    pub content: String,
}

impl LlmMessage {
    /// 构造一条消息。
    #[must_use]
    pub fn new(role: &str, content: String) -> Self {
        Self {
            role: role.to_string(),
            content,
        }
    }
}

/// provider 客户端归一化后的响应（Python `LLMResponse`）。
#[derive(Debug, Clone, PartialEq)]
pub struct LlmResponse {
    /// 归一化文本（三家 wire 格式各自抽取）。
    pub text: String,
    /// 响应报告的模型 ID（缺失回退配置模型）。
    pub model: Option<String>,
    /// 产生的审计记录 ID。
    pub model_invocation_id: Option<ModelInvocationId>,
    /// 原始响应（网关成功路径恒为空对象，Python 同）。
    pub raw: Map<String, Value>,
    /// wire 层的结束原因原值（OpenAI `finish_reason` / Anthropic
    /// `stop_reason` / Gemini `finishReason`），缺失为 `None`。
    pub finish_reason: Option<String>,
    /// 模型的**思考过程原文**（reasoning / thinking / thought）。
    ///
    /// 与 [`Self::text`] 分开：text 是给用户的答案，reasoning 是模型
    /// 得出答案前的内部推理。三家 wire 格式各有自己的字段名
    /// （OpenAI 兼容 `message.reasoning_content` / Anthropic
    /// `content[].type=="thinking"` / Gemini `parts[].thought==true`），
    /// 由 provider 层抽取后归一化到此处。
    ///
    /// `None` = 该 provider / 该次调用没有返回思考（多数非推理模型、
    /// 以及未显式开启 extended thinking 的 Anthropic 调用都属于此类）。
    /// 空串一律归一成 `None`。
    pub reasoning: Option<String>,
}

impl LlmResponse {
    /// 构造响应（`model` / `model_invocation_id` / `raw` / `finish_reason`
    /// 取 Python 默认值）。
    #[must_use]
    pub fn new(text: String) -> Self {
        Self {
            text,
            model: None,
            model_invocation_id: None,
            raw: Map::new(),
            finish_reason: None,
            reasoning: None,
        }
    }
}

/// 一次结构化生成的请求（Python `generate_structured` 的 keyword-only
/// 参数镜像，借用传递避免拷贝消息列表）。
#[derive(Debug)]
pub struct StructuredGenerationRequest<'a> {
    /// Provider 标识（空串 = 使用默认 provider）。
    pub provider_id: &'a str,
    /// 消息序列。
    pub messages: &'a [LlmMessage],
    /// 调用目的标签（审计字段，如 `metacognition_divergence`）。
    pub purpose: &'a str,
    /// 关联 Project。
    pub project_id: Option<&'a ProjectId>,
    /// 关联 Run。
    pub run_id: Option<&'a RunId>,
    /// 关联 Task。
    pub task_id: Option<&'a TaskId>,
}

/// 一次文本生成的请求（Python `generate_text` 的 keyword-only 参数镜像）。
#[derive(Debug)]
pub struct TextGenerationRequest<'a> {
    /// Provider 标识。
    pub provider_id: &'a str,
    /// 消息序列。
    pub messages: &'a [LlmMessage],
    /// 调用目的标签（审计字段）。
    pub purpose: &'a str,
    /// 关联 Project。
    pub project_id: Option<&'a ProjectId>,
    /// 关联 Run。
    pub run_id: Option<&'a RunId>,
    /// 关联 Task。
    pub task_id: Option<&'a TaskId>,
    /// 模型覆盖（空串 = 用配置模型，Python falsy 语义）。
    pub model_override: Option<&'a str>,
}

/// Provider 运行时调用失败（Python 侧为开放异常集合——`RuntimeError`、
/// 网络错误、`ValidationError` 等均由调用方按"整体降级"处理）。
#[derive(Debug, thiserror::Error)]
#[error("provider runtime call failed: {message}")]
pub struct ProviderCallError {
    /// 面向日志的失败描述。
    pub message: String,
    /// 响应被输出上限截断（pi 的 truncation fail-closed 语义）：调用方
    /// 应视为可重试的预算现象，不计入模型/轨迹的连续错误。
    pub truncated: bool,
}

impl ProviderCallError {
    /// 以固定消息构造（mock 与网关实现用）。
    #[must_use]
    pub fn new(message: &str) -> Self {
        Self {
            message: message.to_string(),
            truncated: false,
        }
    }

    /// 构造截断标记的调用失败（截断守卫路径专用）。
    #[must_use]
    pub fn new_truncated(message: &str) -> Self {
        Self {
            message: message.to_string(),
            truncated: true,
        }
    }
}

/// 完整 provider 运行时契约（Python `ProviderRuntime` Protocol 的镜像），
/// 供 solver / observer 等后续阶段消费。
///
/// `generate_structured` 返回 JSON 对象（Python `dict[str, object]`）。
#[async_trait::async_trait]
pub trait ProviderRuntime: Send + Sync {
    /// 列全部 Provider 配置。
    ///
    /// # Errors
    /// 存储读取失败。
    async fn list_providers(&self) -> Result<Vec<ProviderConfig>, ProviderCallError>;

    /// 按 id 取 Provider 配置，不存在返回 `None`。
    ///
    /// # Errors
    /// 存储读取失败。
    async fn get_provider(
        &self,
        provider_id: &str,
    ) -> Result<Option<ProviderConfig>, ProviderCallError>;

    /// 解析默认 provider（首个 enabled 且 `is_default`）。
    ///
    /// # Errors
    /// 存储读取失败。
    async fn resolve_default_provider(&self) -> Result<Option<ProviderConfig>, ProviderCallError>;

    /// 对指定 provider 做健康检查（文本 + 结构化能力探测）。
    ///
    /// # Errors
    /// provider 未知或已禁用（Python `KeyError` / `ValueError` 路径）；
    /// 探测失败本身以 [`ProviderHealthResult`] 结构化返回，不进 `Err`。
    async fn health_check(
        &self,
        provider_id: &str,
    ) -> Result<ProviderHealthResult, ProviderCallError>;

    /// 生成文本响应。
    ///
    /// # Errors
    /// provider 未知/禁用/不支持、网络失败、响应抽取失败等一切失败场景。
    async fn generate_text(
        &self,
        request: TextGenerationRequest<'_>,
    ) -> Result<LlmResponse, ProviderCallError>;

    /// 生成结构化（JSON 对象）响应。
    ///
    /// # Errors
    /// 同 [`ProviderRuntime::generate_text`]，外加响应不是合法 JSON 对象。
    async fn generate_structured(
        &self,
        request: StructuredGenerationRequest<'_>,
    ) -> Result<Map<String, Value>, ProviderCallError>;
}

/// 可选扩展：列举远端可用模型（Python `ProviderModelDiscoveryRuntime`）。
#[async_trait::async_trait]
pub trait ProviderModelDiscoveryRuntime: Send + Sync {
    /// 拉取并归一化 provider 广播的模型标识。
    ///
    /// # Errors
    /// 端点失败本身以 [`ProviderModelDiscoveryResult`] 结构化返回（诊断
    /// 信息脱敏），不进 `Err`；`Err` 仅保留给不可恢复的调用方错误。
    async fn discover_models(
        &self,
        provider: &ProviderConfig,
    ) -> Result<ProviderModelDiscoveryResult, ProviderCallError>;
}
