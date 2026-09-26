//! Provider 配置与模型调用审计模型 —— `server/core/models/provider.py` 的移植。
//!
//! core 只拥有结构化的配置与审计形状，不持有任何具体 OpenAI/Anthropic/
//! Gemini HTTP 客户端；具体实现属于 engines 层（`engines`），
//! 依赖边界与 Python 侧一致。
//!
//! 秘密处理红线：`encrypted_api_key` 与 `api_key_ref` 永不出现在公开
//! API/MCP 响应中（`has_secret` 布尔与掩码视图由上层 API schema 提供）；
//! 错误与摘要文本在落审计前必须经 engines 层的脱敏器处理。

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use thiserror::Error;

use crate::common::StrMap;
use crate::common::Timestamp;
use crate::common::new_id;
use crate::common::utcnow;
use crate::ids::ModelCapabilityId;
use crate::ids::ModelInvocationId;
use crate::ids::ProviderId;
use crate::ids::ProviderRouteId;

/// 支持的 LLM / agent provider 家族（Python `ProviderType`，str-Enum）。
///
/// 具体 SDK/HTTP 客户端不在 core；本枚举只为配置分类，供 engine/app
/// 层解析到正确的 runtime。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    /// `OpenAI` 官方。
    Openai,
    /// Anthropic（Claude）。
    Anthropic,
    /// Google Gemini。
    Gemini,
    /// `OpenAI` `Chat Completions` 兼容代理（GLM / `DeepSeek` 等）。
    OpenaiCompatible,
    /// 本地 Ollama。
    Ollama,
    /// vLLM 服务。
    Vllm,
    /// LM Studio 本地服务。
    LmStudio,
    /// llama.cpp 服务。
    LlamaCpp,
    /// 其他本地运行时。
    Local,
    /// Codex CLI 运行时（暂不支持 HTTP 调用）。
    CodexCli,
    /// Claude Code 运行时（暂不支持 HTTP 调用）。
    ClaudeCode,
    /// 远端 MCP provider（暂不支持 HTTP 调用）。
    McpRemote,
    /// 自定义。
    Custom,
}

impl ProviderType {
    /// wire 值（Python `provider_type.value` 的镜像）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ProviderType::Openai => "openai",
            ProviderType::Anthropic => "anthropic",
            ProviderType::Gemini => "gemini",
            ProviderType::OpenaiCompatible => "openai_compatible",
            ProviderType::Ollama => "ollama",
            ProviderType::Vllm => "vllm",
            ProviderType::LmStudio => "lm_studio",
            ProviderType::LlamaCpp => "llama_cpp",
            ProviderType::Local => "local",
            ProviderType::CodexCli => "codex_cli",
            ProviderType::ClaudeCode => "claude_code",
            ProviderType::McpRemote => "mcp_remote",
            ProviderType::Custom => "custom",
        }
    }
}

/// 单次 provider/model 调用的结局（Python `ModelInvocationStatus`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelInvocationStatus {
    /// 调用成功。
    Ok,
    /// 调用失败（默认错误类别）。
    Error,
    /// 请求超时。
    Timeout,
    /// 认证/授权被拒。
    Denied,
}

impl ModelInvocationStatus {
    /// wire 值（Python `status.value` 的镜像）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ModelInvocationStatus::Ok => "ok",
            ModelInvocationStatus::Error => "error",
            ModelInvocationStatus::Timeout => "timeout",
            ModelInvocationStatus::Denied => "denied",
        }
    }
}

/// `ProviderConfig` 字段约束校验错误（Python pydantic `ValidationError` 的镜像）。
///
/// 不派生 `Eq`：`TemperatureOutOfRange(f64)` 的 f64 只有 `PartialEq`。
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ProviderConfigError {
    /// `name` 为空（Python `min_length=1`）。
    #[error("provider name must not be blank")]
    BlankName,
    /// `timeout_seconds` 超出 `[1, 3600]`。
    #[error("timeout_seconds must be within 1..=3600, got {0}")]
    TimeoutOutOfRange(u64),
    /// `max_tokens` 小于 1。
    #[error("max_tokens must be >= 1, got {0}")]
    MaxTokensBelowOne(u64),
    /// `temperature` 超出 `[0.0, 2.0]`。
    #[error("temperature must be within 0.0..=2.0, got {0}")]
    TemperatureOutOfRange(f64),
}

fn default_provider_id() -> ProviderId {
    ProviderId::new(new_id("provider"))
}

fn default_provider_type() -> ProviderType {
    ProviderType::OpenaiCompatible
}

fn default_timeout_seconds() -> u64 {
    60
}

/// 单个 LLM / agent provider 的存储配置（Python `ProviderConfig`）。
///
/// MVP 中 `encrypted_api_key` 可保存本地开发密钥；生产存储应替换为真实
/// 加密 secret store 而不改变仓储或 runtime 契约。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// Provider 标识符。
    #[serde(default = "default_provider_id")]
    pub id: ProviderId,
    /// 展示名（非空）。
    pub name: String,
    /// Provider 家族。
    #[serde(default = "default_provider_type")]
    pub provider_type: ProviderType,
    /// API 基址（`None` = 家族默认值）。
    #[serde(default)]
    pub base_url: Option<String>,
    /// 模型 ID（OpenAI 兼容 / Anthropic 路径必填）。
    #[serde(default)]
    pub model: Option<String>,
    /// 密钥引用（支持 `env:NAME` 形式）。
    #[serde(default)]
    pub api_key_ref: Option<String>,
    /// 存储的密钥值（永不进入公开 API/MCP 响应）。
    #[serde(default)]
    pub encrypted_api_key: Option<String>,
    /// 逐请求附加头。
    #[serde(default)]
    pub default_headers: StrMap,
    /// 请求超时秒数（`1..=3600`）。
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    /// 最大生成 token 数。
    #[serde(default)]
    pub max_tokens: Option<u64>,
    /// 采样温度（`0.0..=2.0`）。
    #[serde(default)]
    pub temperature: Option<f64>,
    /// 是否默认 provider。
    #[serde(default)]
    pub is_default: bool,
    /// 是否启用。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 创建时间。
    #[serde(default = "utcnow")]
    pub created_at: Timestamp,
    /// 更新时间。
    #[serde(default = "utcnow")]
    pub updated_at: Timestamp,
}

fn default_true() -> bool {
    true
}

impl ProviderConfig {
    /// 以 Python 默认值构造（`ProviderConfig(name=..., provider_type=...)`）。
    #[must_use]
    pub fn new(name: String, provider_type: ProviderType) -> Self {
        Self {
            id: default_provider_id(),
            name,
            provider_type,
            base_url: None,
            model: None,
            api_key_ref: None,
            encrypted_api_key: None,
            default_headers: StrMap::new(),
            timeout_seconds: default_timeout_seconds(),
            max_tokens: None,
            temperature: None,
            is_default: false,
            enabled: true,
            created_at: utcnow(),
            updated_at: utcnow(),
        }
    }

    /// 本配置能否从任一来源解析出 API 密钥（Python `has_secret` 属性）。
    #[must_use]
    pub fn has_secret(&self) -> bool {
        self.api_key_ref.is_some() || self.encrypted_api_key.is_some()
    }

    /// 校验字段约束，返回规范化后的配置（pydantic 构造期校验的镜像）。
    ///
    /// # Errors
    /// 任一约束不满足时返回 [`ProviderConfigError`]。
    pub fn validated(self) -> Result<Self, ProviderConfigError> {
        if self.name.trim().is_empty() {
            return Err(ProviderConfigError::BlankName);
        }
        if !(1..=3600).contains(&self.timeout_seconds) {
            return Err(ProviderConfigError::TimeoutOutOfRange(self.timeout_seconds));
        }
        if let Some(max_tokens) = self.max_tokens
            && max_tokens < 1
        {
            return Err(ProviderConfigError::MaxTokensBelowOne(max_tokens));
        }
        if let Some(temperature) = self.temperature
            && !(0.0..=2.0).contains(&temperature)
        {
            return Err(ProviderConfigError::TemperatureOutOfRange(temperature));
        }
        Ok(self)
    }
}

/// Provider/模型能力元数据，供路由选择（Python `ModelCapability`）。
///
/// 4 个 `supports_*` 布尔是冻结 wire 契约的字段形状，不可折并为位标志。
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCapability {
    /// 能力记录标识符。
    #[serde(default = "default_model_capability_id")]
    pub id: ModelCapabilityId,
    /// 所属 provider。
    pub provider_id: ProviderId,
    /// 模型 ID。
    pub model: String,
    /// 上下文窗口大小。
    #[serde(default)]
    pub context_window: Option<i64>,
    /// 最大输出 token 数。
    #[serde(default)]
    pub max_output_tokens: Option<i64>,
    /// 是否支持 JSON 输出。
    #[serde(default)]
    pub supports_json: bool,
    /// 是否支持工具调用。
    #[serde(default)]
    pub supports_tools: bool,
    /// 是否支持视觉输入。
    #[serde(default)]
    pub supports_vision: bool,
    /// 是否支持 embedding。
    #[serde(default)]
    pub supports_embeddings: bool,
    /// Provider 原生工具名列表。
    #[serde(default)]
    pub provider_native_tools: Vec<String>,
    /// 附加元数据。
    #[serde(default)]
    pub metadata: Map<String, Value>,
    /// 创建时间。
    #[serde(default = "utcnow")]
    pub created_at: Timestamp,
    /// 更新时间。
    #[serde(default = "utcnow")]
    pub updated_at: Timestamp,
}

fn default_model_capability_id() -> ModelCapabilityId {
    ModelCapabilityId::new(new_id("modelcap"))
}

/// `ProviderRouteBinding` 字段约束校验错误。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProviderRouteError {
    /// `purpose` / `fallback_group` 归一化后为空。
    #[error("provider route labels must not be blank")]
    BlankLabel,
    /// `priority` 小于 0。
    #[error("priority must be >= 0, got {0}")]
    PriorityNegative(i64),
    /// `weight` 小于 0。
    #[error("weight must be >= 0, got {0}")]
    WeightNegative(i64),
    /// `max_failures` 小于 1。
    #[error("max_failures must be >= 1, got {0}")]
    MaxFailuresBelowOne(i64),
    /// `cooldown_seconds` 小于 1。
    #[error("cooldown_seconds must be >= 1, got {0}")]
    CooldownSecondsBelowOne(i64),
    /// `failure_count` 小于 0。
    #[error("failure_count must be >= 0, got {0}")]
    FailureCountNegative(i64),
}

fn default_route_id() -> ProviderRouteId {
    ProviderRouteId::new(new_id("route"))
}

fn default_priority() -> i64 {
    100
}

fn default_weight() -> i64 {
    100
}

fn default_fallback_group() -> String {
    "default".to_string()
}

fn default_max_failures() -> i64 {
    3
}

fn default_cooldown_seconds() -> i64 {
    60
}

/// 把一个模型调用 purpose 路由到有序 provider/model 候选
/// （Python `ProviderRouteBinding`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderRouteBinding {
    /// 路由标识符。
    #[serde(default = "default_route_id")]
    pub id: ProviderRouteId,
    /// 调用目的标签（归一化为 strip + 小写）。
    pub purpose: String,
    /// 候选 provider。
    pub provider_id: ProviderId,
    /// 模型覆盖。
    #[serde(default)]
    pub model_override: Option<String>,
    /// 优先级（越大越先尝试）。
    #[serde(default = "default_priority")]
    pub priority: i64,
    /// 权重。
    #[serde(default = "default_weight")]
    pub weight: i64,
    /// 是否启用。
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 回退组。
    #[serde(default = "default_fallback_group")]
    pub fallback_group: String,
    /// 必备能力（键为能力名）。
    #[serde(default)]
    pub required_capabilities: Map<String, Value>,
    /// 触发熔断的连续失败次数。
    #[serde(default = "default_max_failures")]
    pub max_failures: i64,
    /// 熔断冷却秒数。
    #[serde(default = "default_cooldown_seconds")]
    pub cooldown_seconds: i64,
    /// 当前连续失败计数。
    #[serde(default)]
    pub failure_count: i64,
    /// 熔断打开的截止时刻（`None` = 未熔断）。
    #[serde(default)]
    pub circuit_open_until: Option<Timestamp>,
    /// 附加元数据。
    #[serde(default)]
    pub metadata: Map<String, Value>,
    /// 创建时间。
    #[serde(default = "utcnow")]
    pub created_at: Timestamp,
    /// 更新时间。
    #[serde(default = "utcnow")]
    pub updated_at: Timestamp,
}

impl ProviderRouteBinding {
    /// 以 Python 默认值构造（`ProviderRouteBinding(purpose=..., provider_id=...)`）。
    ///
    /// 标签按 Python `_normalize_labels` 校验器语义归一化：strip + 小写；
    /// 归一化后为空即拒绝。
    ///
    /// # Errors
    /// 标签为空或数值约束不满足时返回 [`ProviderRouteError`]。
    pub fn new(purpose: &str, provider_id: ProviderId) -> Result<Self, ProviderRouteError> {
        let route = Self {
            id: default_route_id(),
            purpose: purpose.to_string(),
            provider_id,
            model_override: None,
            priority: default_priority(),
            weight: default_weight(),
            enabled: true,
            fallback_group: default_fallback_group(),
            required_capabilities: Map::new(),
            max_failures: default_max_failures(),
            cooldown_seconds: default_cooldown_seconds(),
            failure_count: 0,
            circuit_open_until: None,
            metadata: Map::new(),
            created_at: utcnow(),
            updated_at: utcnow(),
        };
        let route = route.validated()?;
        Ok(route)
    }

    /// 校验字段约束并归一化标签（pydantic 构造期行为的镜像）。
    ///
    /// # Errors
    /// 任一约束不满足时返回 [`ProviderRouteError`]。
    pub fn validated(mut self) -> Result<Self, ProviderRouteError> {
        self.purpose = normalize_label(&self.purpose)?;
        self.fallback_group = normalize_label(&self.fallback_group)?;
        if self.priority < 0 {
            return Err(ProviderRouteError::PriorityNegative(self.priority));
        }
        if self.weight < 0 {
            return Err(ProviderRouteError::WeightNegative(self.weight));
        }
        if self.max_failures < 1 {
            return Err(ProviderRouteError::MaxFailuresBelowOne(self.max_failures));
        }
        if self.cooldown_seconds < 1 {
            return Err(ProviderRouteError::CooldownSecondsBelowOne(
                self.cooldown_seconds,
            ));
        }
        if self.failure_count < 0 {
            return Err(ProviderRouteError::FailureCountNegative(self.failure_count));
        }
        Ok(self)
    }
}

fn normalize_label(value: &str) -> Result<String, ProviderRouteError> {
    let normalized = value.trim().to_lowercase();
    if normalized.is_empty() {
        return Err(ProviderRouteError::BlankLabel);
    }
    Ok(normalized)
}

fn default_invocation_id() -> ModelInvocationId {
    ModelInvocationId::new(new_id("model"))
}

/// 单次 model/provider 调用的审计记录（Python `ModelInvocation`）。
///
/// 长提示/响应正文有意只做摘要与哈希而非全文落库：审计日志保持可用，
/// 同时不泄漏密钥、不放大上下文体积。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelInvocation {
    /// 审计记录标识符。
    #[serde(default = "default_invocation_id")]
    pub id: ModelInvocationId,
    /// 关联 Project。
    #[serde(default)]
    pub project_id: Option<crate::ids::ProjectId>,
    /// 关联 Run。
    #[serde(default)]
    pub run_id: Option<crate::ids::RunId>,
    /// 关联 Task。
    #[serde(default)]
    pub task_id: Option<crate::ids::TaskId>,
    /// Provider 标识符。
    pub provider_id: ProviderId,
    /// Provider 家族。
    pub provider_type: ProviderType,
    /// 模型 ID。
    #[serde(default)]
    pub model: Option<String>,
    /// 调用目的标签。
    pub purpose: String,
    /// 提示词摘要（脱敏 + 500 字符截断）。
    #[serde(default)]
    pub prompt_summary: String,
    /// 响应摘要（脱敏 + 500 字符截断）。
    #[serde(default)]
    pub response_summary: String,
    /// 提示词 SHA-256。
    #[serde(default)]
    pub prompt_hash: Option<String>,
    /// 响应 SHA-256。
    #[serde(default)]
    pub response_hash: Option<String>,
    /// 输入 token 数。
    #[serde(default)]
    pub input_tokens: Option<i64>,
    /// 输出 token 数。
    #[serde(default)]
    pub output_tokens: Option<i64>,
    /// 调用结局。
    #[serde(default = "default_invocation_status")]
    pub status: ModelInvocationStatus,
    /// 错误描述（脱敏后）。
    #[serde(default)]
    pub error: Option<String>,
    /// 耗时毫秒。
    #[serde(default)]
    pub duration_ms: Option<i64>,
    /// 关联工件路径。
    #[serde(default)]
    pub artifact_paths: Vec<String>,
    /// 模型的**思考过程原文**（reasoning / thinking / thought）。
    ///
    /// 与 [`Self::response_summary`] 分开：`response_summary` 是脱敏 +
    /// 500 字符截断的答案摘要（审计卫生），`reasoning` 是模型得出答案前
    /// 的内部推理原文。后者长度常是前者的数倍，因此单独成列，不与答案
    /// 摘要争同一个截断预算。
    ///
    /// 存储策略（对齐参考实现）：**全量原样保留，不截断、不只留最近 N
    /// 条**。思考原文是排查"模型为什么这么判"的唯一依据，截断它会直接
    /// 毁掉可审计性；体积代价由 SQLite 文本列承担，随记录生命周期
    /// （project 删除级联 / 手动清理）一起回收。
    ///
    /// `None` = 该次调用没有思考（非推理模型 / 未开 extended thinking）。
    #[serde(default)]
    pub reasoning: Option<String>,
    /// 开始时刻。
    #[serde(default = "utcnow")]
    pub started_at: Timestamp,
    /// 结束时刻。
    #[serde(default)]
    pub finished_at: Option<Timestamp>,
}

fn default_invocation_status() -> ModelInvocationStatus {
    ModelInvocationStatus::Ok
}

impl ModelInvocation {
    /// 以 Python 默认值构造新的审计记录骨架。
    ///
    /// `status` 默认 `OK`（Python 侧同）；运行时在调用结束后覆盖为最终
    /// 结局（成功路径显式置 `OK`，失败路径置 `ERROR`）。
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(provider_id: ProviderId, provider_type: ProviderType, purpose: String) -> Self {
        Self {
            id: default_invocation_id(),
            project_id: None,
            run_id: None,
            task_id: None,
            provider_id,
            provider_type,
            model: None,
            purpose,
            prompt_summary: String::new(),
            response_summary: String::new(),
            prompt_hash: None,
            response_hash: None,
            input_tokens: None,
            output_tokens: None,
            status: default_invocation_status(),
            error: None,
            duration_ms: None,
            artifact_paths: Vec::new(),
            reasoning: None,
            started_at: utcnow(),
            finished_at: None,
        }
    }
}

/// Provider 运行时健康检查结果（Python `ProviderHealthResult`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderHealthResult {
    /// Provider 标识符。
    pub provider_id: ProviderId,
    /// 检查结论。
    #[serde(default = "default_invocation_status")]
    pub status: ModelInvocationStatus,
    /// 人类可读说明。
    pub message: String,
    /// 检查时的模型 ID。
    #[serde(default)]
    pub model: Option<String>,
    /// 产生本结果的审计记录。
    #[serde(default)]
    pub model_invocation_id: Option<ModelInvocationId>,
    /// 能力矩阵（键序 = Python dict 插入序，经 `serde_json::Map` 保序）。
    #[serde(default)]
    pub capabilities: Map<String, Value>,
}

impl ProviderHealthResult {
    /// 构造健康结果（Python 字段默认值的镜像）。
    #[must_use]
    pub fn new(provider_id: ProviderId, status: ModelInvocationStatus, message: String) -> Self {
        Self {
            provider_id,
            status,
            message,
            model: None,
            model_invocation_id: None,
            capabilities: Map::new(),
        }
    }
}

/// Provider 模型列表端点的归一化结果（Python `ProviderModelDiscoveryResult`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderModelDiscoveryResult {
    /// Provider 标识符。
    #[serde(default)]
    pub provider_id: Option<ProviderId>,
    /// 发现结局。
    #[serde(default = "default_invocation_status")]
    pub status: ModelInvocationStatus,
    /// 人类可读说明。
    pub message: String,
    /// 请求的端点 URL。
    pub endpoint: String,
    /// 发现的模型 ID（casefold 排序）。
    #[serde(default)]
    pub models: Vec<String>,
    /// 配置的模型 ID。
    #[serde(default)]
    pub configured_model: Option<String>,
    /// 配置模型是否在发现列表中（`None` = 未配置模型）。
    #[serde(default)]
    pub configured_model_available: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_type_wire_values_match_python() {
        assert_eq!(ProviderType::OpenaiCompatible.as_str(), "openai_compatible");
        assert_eq!(ProviderType::CodexCli.as_str(), "codex_cli");
        assert_eq!(ProviderType::ClaudeCode.as_str(), "claude_code");
        assert_eq!(ProviderType::McpRemote.as_str(), "mcp_remote");
        assert_eq!(ProviderType::LmStudio.as_str(), "lm_studio");
        assert_eq!(ProviderType::LlamaCpp.as_str(), "llama_cpp");
        let wire = serde_json::to_string(&ProviderType::OpenaiCompatible)
            .expect("枚举序列化为字符串不会失败");
        assert_eq!(wire, "\"openai_compatible\"");
    }

    #[test]
    fn model_invocation_status_wire_values_match_python() {
        assert_eq!(ModelInvocationStatus::Timeout.as_str(), "timeout");
        assert_eq!(ModelInvocationStatus::Denied.as_str(), "denied");
        let wire =
            serde_json::to_string(&ModelInvocationStatus::Ok).expect("枚举序列化为字符串不会失败");
        assert_eq!(wire, "\"ok\"");
    }

    #[test]
    fn provider_config_defaults_match_python() {
        let config = ProviderConfig::new("p".to_string(), ProviderType::Ollama);
        assert_eq!(config.provider_type, ProviderType::Ollama);
        assert_eq!(config.timeout_seconds, 60);
        assert!(config.enabled);
        assert!(!config.is_default);
        assert!(!config.has_secret());
        assert!(config.default_headers.is_empty());
    }

    #[test]
    fn provider_config_has_secret_covers_both_sources() {
        let mut config = ProviderConfig::new("p".to_string(), ProviderType::Openai);
        assert!(!config.has_secret());
        config.api_key_ref = Some("env:OPENAI_API_KEY".to_string());
        assert!(config.has_secret());
        config.api_key_ref = None;
        assert!(!config.has_secret());
        config.encrypted_api_key = Some("sk-x".to_string());
        assert!(config.has_secret());
    }

    #[test]
    fn provider_config_validated_rejects_out_of_range_fields() {
        let mut config = ProviderConfig::new("p".to_string(), ProviderType::Openai);
        assert!(config.clone().validated().is_ok());

        config.name = "  ".to_string();
        assert_eq!(
            config.clone().validated(),
            Err(ProviderConfigError::BlankName)
        );
        config.name = "p".to_string();

        config.timeout_seconds = 0;
        assert_eq!(
            config.clone().validated(),
            Err(ProviderConfigError::TimeoutOutOfRange(0))
        );
        config.timeout_seconds = 3601;
        assert_eq!(
            config.clone().validated(),
            Err(ProviderConfigError::TimeoutOutOfRange(3601))
        );
        config.timeout_seconds = 60;

        config.max_tokens = Some(0);
        assert_eq!(
            config.clone().validated(),
            Err(ProviderConfigError::MaxTokensBelowOne(0))
        );
        config.max_tokens = None;

        config.temperature = Some(2.5);
        assert!(matches!(
            config.clone().validated(),
            Err(ProviderConfigError::TemperatureOutOfRange(t)) if (t - 2.5).abs() < f64::EPSILON
        ));
        config.temperature = Some(0.0);
        assert!(config.validated().is_ok());
    }

    #[test]
    fn provider_config_deserializes_with_python_defaults() {
        let config: ProviderConfig =
            serde_json::from_str(r#"{"name":"p","id":"provider_abc","provider_type":"openai"}"#)
                .expect("缺省字段按 Python 默认值填充");
        assert_eq!(config.timeout_seconds, 60);
        assert!(config.enabled);
        assert_eq!(config.provider_type, ProviderType::Openai);
        assert!(config.default_headers.is_empty());
    }

    #[test]
    fn provider_config_rejects_unknown_fields() {
        let result: Result<ProviderConfig, _> =
            serde_json::from_str(r#"{"name":"p","id":"provider_abc","extra":1}"#);
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }

    #[test]
    fn route_binding_new_normalizes_labels() {
        let provider = ProviderId::new("provider_abc".to_string());
        let route = ProviderRouteBinding::new("  Advisor ", provider).expect("标签归一化后合法");
        assert_eq!(route.purpose, "advisor");
        assert_eq!(route.fallback_group, "default");
        assert_eq!(route.priority, 100);
        assert_eq!(route.max_failures, 3);
        assert_eq!(route.cooldown_seconds, 60);
        assert_eq!(route.failure_count, 0);
        assert!(route.circuit_open_until.is_none());
    }

    #[test]
    fn route_binding_rejects_blank_and_out_of_range() {
        let provider = ProviderId::new("provider_abc".to_string());
        assert_eq!(
            ProviderRouteBinding::new("   ", provider.clone()),
            Err(ProviderRouteError::BlankLabel)
        );
        let mut route = ProviderRouteBinding::new("advisor", provider).expect("合法路由");
        route.max_failures = 0;
        assert_eq!(
            route.clone().validated(),
            Err(ProviderRouteError::MaxFailuresBelowOne(0))
        );
        route.max_failures = 1;
        route.failure_count = -1;
        assert_eq!(
            route.validated(),
            Err(ProviderRouteError::FailureCountNegative(-1))
        );
    }

    #[test]
    fn model_invocation_defaults_match_python() {
        let invocation = ModelInvocation::new(
            ProviderId::new("provider_abc".to_string()),
            ProviderType::OpenaiCompatible,
            "unit_test".to_string(),
        );
        assert_eq!(invocation.status, ModelInvocationStatus::Ok);
        assert!(invocation.artifact_paths.is_empty());
        assert!(invocation.prompt_summary.is_empty());
        assert!(invocation.finished_at.is_none());
        assert!(invocation.input_tokens.is_none());
    }

    #[test]
    fn health_result_new_takes_python_defaults() {
        let result = ProviderHealthResult::new(
            ProviderId::new("provider_abc".to_string()),
            ModelInvocationStatus::Denied,
            "not supported yet".to_string(),
        );
        assert_eq!(result.status, ModelInvocationStatus::Denied);
        assert!(result.model.is_none());
        assert!(result.model_invocation_id.is_none());
        assert!(result.capabilities.is_empty());
    }
}
