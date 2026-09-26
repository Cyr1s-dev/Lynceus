//! 网关错误类型与错误格式化 —— Python `runtime.py` 异常处理辅助的移植。
//!
//! Python 侧异常是开放集合（`KeyError` / `ValueError` / `httpx.*`），全部
//! 汇入"审计后重抛"；Rust 用穷尽枚举承载同一信息结构（状态码 + URL +
//! 脱敏 detail），`Display` 文本进审计 `error` 字段前统一脱敏。

use agents::llm::ProviderCallError;
use models::provider::ModelInvocationStatus;
use models::provider::ProviderConfig;
use serde_json::Value;

use super::redact::redact_secrets;

/// provider 调用全程的失败分类。
///
/// `Display` 为手写实现（`Status.detail` 是 `Option`，thiserror 的字段
/// 插值不支持"缺失时省略"的形状）；`audit_display` 才是进审计文本的
/// 消息主体（剥离 `transport error:` 等类别前缀，对齐 Python `str(exc)`）。
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    /// HTTP 非 2xx 响应（Python `httpx.HTTPStatusError` 路径）。
    Status {
        /// HTTP 状态码。
        status: u16,
        /// 请求 URL（不含查询串——Gemini 的 `?key=` 绝不进错误文本）。
        url: String,
        /// 脱敏后的响应 detail（`None` 时格式化为空）。
        detail: Option<String>,
    },
    /// 请求超时（Python `httpx.TimeoutException`）。
    Timeout,
    /// 网络/传输层失败。
    Transport(String),
    /// 响应抽取/解析等协议违约（Python `ValueError` 家族）。
    Protocol(String),
    /// provider 未知（Python `KeyError`，审计文本里消息带单引号 repr）。
    UnknownProvider(String),
    /// provider 禁用/不支持、缺 model 等配置问题（Python `ValueError`）。
    Config(String),
    /// 响应被输出 token 上限截断且结构化解析失败（截断守卫：调用方
    /// 视为可重试的预算现象，不计入模型/轨迹错误）。
    Truncated,
    /// 存储层失败。
    Storage(String),
}

impl std::fmt::Display for GatewayError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GatewayError::Status {
                status,
                url,
                detail,
            } => {
                write!(formatter, "HTTP {status} from {url}")?;
                if let Some(detail) = detail {
                    write!(formatter, ": {detail}")?;
                }
                Ok(())
            }
            GatewayError::Timeout => write!(formatter, "request timed out"),
            GatewayError::Transport(message) => {
                write!(formatter, "transport error: {message}")
            }
            GatewayError::Protocol(message) => {
                write!(formatter, "protocol error: {message}")
            }
            GatewayError::UnknownProvider(message) | GatewayError::Config(message) => {
                write!(formatter, "{message}")
            }
            GatewayError::Truncated => {
                write!(formatter, "response truncated by output token limit")
            }
            GatewayError::Storage(message) => write!(formatter, "storage error: {message}"),
        }
    }
}

impl GatewayError {
    /// 构造协议错误（提取函数的便捷入口）。
    #[must_use]
    pub fn protocol(message: &str) -> Self {
        GatewayError::Protocol(message.to_string())
    }

    /// 构造配置错误（Python `ValueError` 家族）。
    #[must_use]
    pub fn config(message: &str) -> Self {
        GatewayError::Config(message.to_string())
    }

    /// 构造未知 provider 错误（Python `KeyError`）。
    #[must_use]
    pub fn unknown_provider(provider_id: &str) -> Self {
        GatewayError::UnknownProvider(format!("unknown provider: {provider_id}"))
    }

    /// 构造禁用 provider 错误（Python `ValueError`）。
    #[must_use]
    pub fn disabled_provider(provider_id: &str) -> Self {
        GatewayError::Config(format!("provider disabled: {provider_id}"))
    }

    /// 对应的 Python 异常类名（审计 `error` 字段 `{type(exc).__name__}` 的镜像）。
    #[must_use]
    pub fn python_exception_name(&self) -> &'static str {
        match self {
            GatewayError::Status { .. } => "HTTPStatusError",
            GatewayError::Timeout => "TimeoutException",
            GatewayError::Transport(_) => "HTTPError",
            GatewayError::Protocol(_) | GatewayError::Config(_) | GatewayError::Truncated => {
                "ValueError"
            }
            GatewayError::UnknownProvider(_) => "KeyError",
            GatewayError::Storage(_) => "RuntimeError",
        }
    }

    /// Python `str(exc)` 的近似体——Rust `Display` 文本剥离类别前缀后的
    /// 消息主体（`KeyError` 的 `str()` 是消息的 repr，带单引号）。
    #[must_use]
    pub fn python_error_string(&self) -> String {
        match self {
            GatewayError::UnknownProvider(message) => format!("KeyError: '{message}'"),
            _ => format!("{}: {}", self.python_exception_name(), self.audit_display()),
        }
    }

    fn audit_display(&self) -> String {
        match self {
            GatewayError::Status {
                status,
                url,
                detail,
            } => {
                let mut text = format!("HTTP {status} from {url}");
                if let Some(detail) = detail {
                    text.push_str(": ");
                    text.push_str(detail);
                }
                text
            }
            GatewayError::Timeout => "request timed out".to_string(),
            GatewayError::Transport(message)
            | GatewayError::Protocol(message)
            | GatewayError::Config(message)
            | GatewayError::UnknownProvider(message)
            | GatewayError::Storage(message) => message.clone(),
            GatewayError::Truncated => "response truncated by output token limit".to_string(),
        }
    }
}

impl From<GatewayError> for ProviderCallError {
    fn from(error: GatewayError) -> Self {
        let truncated = matches!(error, GatewayError::Truncated);
        let message = error.to_string();
        if truncated {
            ProviderCallError::new_truncated(&message)
        } else {
            ProviderCallError::new(&message)
        }
    }
}

impl From<reqwest::Error> for GatewayError {
    /// 从 reqwest 传输错误归类（Python `httpx.TimeoutException` 判定）。
    fn from(error: reqwest::Error) -> Self {
        if error.is_timeout() {
            GatewayError::Timeout
        } else {
            GatewayError::Transport(error.to_string())
        }
    }
}

impl From<storage::StorageError> for GatewayError {
    fn from(error: storage::StorageError) -> Self {
        GatewayError::Storage(error.to_string())
    }
}

/// 判定错误的审计结局（Python `_provider_error_status`）：
/// 超时 → `TIMEOUT`；401/403 → `DENIED`；其余 → `ERROR`。
#[must_use]
pub fn provider_error_status(error: &GatewayError) -> ModelInvocationStatus {
    match error {
        GatewayError::Timeout => ModelInvocationStatus::Timeout,
        GatewayError::Status { status: 401, .. } | GatewayError::Status { status: 403, .. } => {
            ModelInvocationStatus::Denied
        }
        _ => ModelInvocationStatus::Error,
    }
}

/// 健康检查失败信息（Python `_format_provider_error`）：404 + Chat
/// Completions 端点追加模型 ID/协议提示；401/403 追加密钥提示。
#[must_use]
pub fn format_provider_error(error: &GatewayError, provider: &ProviderConfig) -> String {
    if let GatewayError::Status {
        status,
        url,
        detail,
    } = error
    {
        let mut prefix = format!("HTTP {status} from {url}");
        if let Some(detail) = detail {
            prefix.push_str(": ");
            prefix.push_str(detail);
        }
        if *status == 404 && url.ends_with("/chat/completions") {
            // Python `provider.model!r`：None 的 repr 是字面 "None"。
            let model = provider.model.clone().unwrap_or_else(|| "None".to_string());
            return redact_secrets(&format!(
                "{prefix}. Verify the exact case-sensitive model ID '{model}'; \
                 then verify that the Base URL supports the OpenAI Chat Completions protocol."
            ));
        }
        if *status == 401 || *status == 403 {
            return redact_secrets(&format!(
                "{prefix}. Verify the API key and authentication headers."
            ));
        }
        return redact_secrets(&prefix);
    }
    if matches!(error, GatewayError::Timeout) {
        return "Provider request timed out. Increase the timeout or verify endpoint reachability."
            .to_string();
    }
    redact_secrets(&error.python_error_string())
}

/// 模型发现失败信息（Python `_format_model_discovery_error`）：
/// 401/403 解释端点存在但鉴权被拒；404 解释非标准 `/models` 端点。
#[must_use]
pub fn format_model_discovery_error(error: &GatewayError, endpoint: &str) -> String {
    if let GatewayError::Status { status, detail, .. } = error {
        let mut prefix = format!("HTTP {status} from {endpoint}");
        if let Some(detail) = detail {
            prefix.push_str(": ");
            prefix.push_str(detail);
        }
        if *status == 401 || *status == 403 {
            return redact_secrets(&format!(
                "{prefix}. The model-list endpoint exists, but the API key or auth headers \
                 were not accepted."
            ));
        }
        if *status == 404 {
            return redact_secrets(&format!(
                "{prefix}. This provider may not expose the standard /models endpoint; \
                 manual model entry is still available."
            ));
        }
        return redact_secrets(&prefix);
    }
    if matches!(error, GatewayError::Timeout) {
        return format!("Timed out while fetching {endpoint}.");
    }
    redact_secrets(&error.python_error_string())
}

/// Python truthiness 镜像（`or` 链的判定基础）。
fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|float| float != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// 解析错误响应体的 detail（Python `_response_error_detail`）：
/// `detail` → `message` → `error`(str / `{message|code}`) 三级回退，
/// 脱敏 + 折叠空白 + 截断 300 字符。
#[must_use]
pub fn response_error_detail(body: &str) -> Option<String> {
    let data: Value = serde_json::from_str(body).ok()?;
    response_error_detail_value(&data)
}

/// [`response_error_detail`] 的结构化层：错误 envelope 已经是 JSON
/// `Value` 时免去序列化/再解析往返（回退链与脱敏语义一致）。
#[must_use]
pub(crate) fn response_error_detail_value(data: &Value) -> Option<String> {
    let object = data.as_object()?;
    let mut detail: Option<&Value> = object
        .get("detail")
        .filter(|value| python_truthy(value))
        .or_else(|| object.get("message").filter(|value| python_truthy(value)));
    if detail.is_none()
        && let Some(error) = object.get("error")
    {
        if error.is_string() {
            detail = Some(error);
        } else if let Some(error_object) = error.as_object() {
            detail = error_object
                .get("message")
                .filter(|value| python_truthy(value))
                .or_else(|| {
                    error_object
                        .get("code")
                        .filter(|value| python_truthy(value))
                });
        }
    }
    let text = detail.and_then(Value::as_str)?;
    if text.trim().is_empty() {
        return None;
    }
    let collapsed: String = text.split_whitespace().collect::<Vec<&str>>().join(" ");
    let redacted = redact_secrets(&collapsed);
    Some(redacted.chars().take(300).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncated_error_is_retryable_budget_phenomenon() {
        let error = GatewayError::Truncated;
        assert_eq!(
            error.to_string(),
            "response truncated by output token limit"
        );
        // 截断是预算现象而非 provider 故障：审计结局仍是 ERROR，但
        // 转换成 ProviderCallError 后带 truncated 标记供 runner 区分。
        assert_eq!(provider_error_status(&error), ModelInvocationStatus::Error);
        let converted: ProviderCallError = error.into();
        assert!(converted.truncated);
        assert!(
            converted
                .message
                .contains("response truncated by output token limit")
        );
        let plain: ProviderCallError = GatewayError::protocol("bad json").into();
        assert!(!plain.truncated);
    }

    #[test]
    fn status_error_display_includes_detail_when_present() {
        let error = GatewayError::Status {
            status: 500,
            url: "http://x/v1/chat/completions".to_string(),
            detail: Some("boom".to_string()),
        };
        assert_eq!(
            error.to_string(),
            "HTTP 500 from http://x/v1/chat/completions: boom"
        );
        let no_detail = GatewayError::Status {
            status: 404,
            url: "http://x".to_string(),
            detail: None,
        };
        assert_eq!(no_detail.to_string(), "HTTP 404 from http://x");
    }

    #[test]
    fn provider_error_status_maps_timeout_and_denied() {
        assert_eq!(
            provider_error_status(&GatewayError::Timeout),
            ModelInvocationStatus::Timeout
        );
        assert_eq!(
            provider_error_status(&GatewayError::Status {
                status: 401,
                url: "http://x".to_string(),
                detail: None
            }),
            ModelInvocationStatus::Denied
        );
        assert_eq!(
            provider_error_status(&GatewayError::Status {
                status: 500,
                url: "http://x".to_string(),
                detail: None
            }),
            ModelInvocationStatus::Error
        );
        assert_eq!(
            provider_error_status(&GatewayError::protocol("bad")),
            ModelInvocationStatus::Error
        );
    }

    #[test]
    fn response_error_detail_falls_back_through_detail_message_error() {
        assert_eq!(
            response_error_detail(r#"{"detail": "model not found"}"#),
            Some("model not found".to_string())
        );
        assert_eq!(
            response_error_detail(r#"{"message": "oops"}"#),
            Some("oops".to_string())
        );
        assert_eq!(
            response_error_detail(r#"{"error": "kaput"}"#),
            Some("kaput".to_string())
        );
        assert_eq!(
            response_error_detail(r#"{"error": {"message": "inner"}}"#),
            Some("inner".to_string())
        );
        // Python：code 为 int 时 detail 非 str，整体返回 None。
        assert_eq!(response_error_detail(r#"{"error": {"code": 42}}"#), None);
        assert_eq!(response_error_detail("not json"), None);
        assert_eq!(response_error_detail(r#"{"detail": ""}"#), None);
        assert_eq!(response_error_detail(r#"{"detail": 5}"#), None);
    }

    #[test]
    fn format_provider_error_404_explains_model_and_protocol() {
        let mut provider = ProviderConfig::new(
            "p".to_string(),
            models::provider::ProviderType::OpenaiCompatible,
        );
        provider.model = Some("GLM-5.2".to_string());
        let error = GatewayError::Status {
            status: 404,
            url: "http://x/v1/chat/completions".to_string(),
            detail: Some("model not found".to_string()),
        };
        let message = format_provider_error(&error, &provider);
        assert!(message.contains("case-sensitive model ID 'GLM-5.2'"));
        assert!(message.contains("Chat Completions protocol"));
        assert!(!message.contains("developer.mozilla.org"));
    }

    #[test]
    fn format_provider_error_404_without_model_uses_python_none_repr() {
        let provider = ProviderConfig::new(
            "p".to_string(),
            models::provider::ProviderType::OpenaiCompatible,
        );
        let error = GatewayError::Status {
            status: 404,
            url: "http://x/v1/chat/completions".to_string(),
            detail: None,
        };
        let message = format_provider_error(&error, &provider);
        assert!(message.contains("case-sensitive model ID 'None'"));
    }

    #[test]
    fn format_model_discovery_error_401_explains_endpoint() {
        let error = GatewayError::Status {
            status: 401,
            url: "http://x/v1/models".to_string(),
            detail: None,
        };
        let message = format_model_discovery_error(&error, "http://x/v1/models");
        assert!(message.contains("model-list endpoint exists"));
        assert!(message.contains("API key or auth headers"));
    }
}
