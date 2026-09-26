//! 请求 payload 构建 / 响应抽取 —— Python `runtime.py` 私有辅助函数的移植。
//!
//! 三家 wire 方言（OpenAI Chat Completions / Anthropic Messages / Gemini
//! generateContent）各自的 payload 键序与 Python 侧构造序一致
//! （`serde_json` `preserve_order` 保持插入序）；抽取函数对畸形响应的
//! 错误消息逐字镜像 Python。

use std::sync::OnceLock;

use agents::llm::LlmMessage;
use models::provider::ProviderConfig;
use models::provider::ProviderType;
use regex::Regex;
use serde_json::Map;
use serde_json::Value;

use super::error::GatewayError;
use super::error::response_error_detail_value;

/// 按 provider 家族构建请求 payload（Python `_build_payload`）。
#[must_use]
pub fn build_payload(provider: &ProviderConfig, messages: &[LlmMessage]) -> Value {
    match provider.provider_type {
        ProviderType::Anthropic => Value::Object(build_anthropic_payload(provider, messages)),
        ProviderType::Gemini => Value::Object(build_gemini_payload(provider, messages)),
        _ => Value::Object(build_openai_compatible_payload(provider, messages)),
    }
}

fn message_object(role: &str, content: &str) -> Value {
    // Python `m.model_dump()`：字段序 role → content。
    let mut object = Map::new();
    object.insert("role".to_string(), Value::String(role.to_string()));
    object.insert("content".to_string(), Value::String(content.to_string()));
    Value::Object(object)
}

fn build_openai_compatible_payload(
    provider: &ProviderConfig,
    messages: &[LlmMessage],
) -> Map<String, Value> {
    // 键序：model, messages, [max_tokens], [temperature]（Python 构造序）。
    let mut payload = Map::new();
    payload.insert(
        "model".to_string(),
        Value::String(provider.model.clone().unwrap_or_default()),
    );
    payload.insert(
        "messages".to_string(),
        Value::Array(
            messages
                .iter()
                .map(|m| message_object(&m.role, &m.content))
                .collect(),
        ),
    );
    if let Some(max_tokens) = provider.max_tokens {
        payload.insert("max_tokens".to_string(), Value::from(max_tokens));
    }
    if let Some(temperature) = provider.temperature {
        payload.insert("temperature".to_string(), Value::from(temperature));
    }
    payload
}

fn build_anthropic_payload(
    provider: &ProviderConfig,
    messages: &[LlmMessage],
) -> Map<String, Value> {
    let mut system_parts: Vec<&str> = Vec::new();
    let mut anthropic_messages: Vec<Value> = Vec::new();
    for message in messages {
        if message.role == "system" {
            system_parts.push(&message.content);
            continue;
        }
        let role = if message.role == "assistant" {
            "assistant"
        } else {
            "user"
        };
        anthropic_messages.push(message_object(role, &message.content));
    }
    if anthropic_messages.is_empty() {
        anthropic_messages.push(message_object("user", ""));
    }

    // 键序：model, max_tokens, messages, [system], [temperature]。
    let mut payload = Map::new();
    payload.insert(
        "model".to_string(),
        Value::String(provider.model.clone().unwrap_or_default()),
    );
    payload.insert(
        "max_tokens".to_string(),
        Value::from(provider.max_tokens.unwrap_or(1024)),
    );
    payload.insert("messages".to_string(), Value::Array(anthropic_messages));
    if !system_parts.is_empty() {
        payload.insert(
            "system".to_string(),
            Value::String(system_parts.join("\n\n")),
        );
    }
    if let Some(temperature) = provider.temperature {
        payload.insert("temperature".to_string(), Value::from(temperature));
    }
    payload
}

fn build_gemini_payload(provider: &ProviderConfig, messages: &[LlmMessage]) -> Map<String, Value> {
    let mut system_parts: Vec<&str> = Vec::new();
    let mut contents: Vec<Value> = Vec::new();
    for message in messages {
        if message.role == "system" {
            system_parts.push(&message.content);
            continue;
        }
        let role = if message.role == "assistant" {
            "model"
        } else {
            "user"
        };
        let mut content = Map::new();
        content.insert("role".to_string(), Value::String(role.to_string()));
        let mut part = Map::new();
        part.insert("text".to_string(), Value::String(message.content.clone()));
        content.insert("parts".to_string(), Value::Array(vec![Value::Object(part)]));
        contents.push(Value::Object(content));
    }
    if contents.is_empty() {
        let mut content = Map::new();
        content.insert("role".to_string(), Value::String("user".to_string()));
        let mut part = Map::new();
        part.insert("text".to_string(), Value::String(String::new()));
        content.insert("parts".to_string(), Value::Array(vec![Value::Object(part)]));
        contents.push(Value::Object(content));
    }

    // 键序：contents, [systemInstruction], [generationConfig]。
    let mut payload = Map::new();
    payload.insert("contents".to_string(), Value::Array(contents));
    if !system_parts.is_empty() {
        let mut part = Map::new();
        part.insert("text".to_string(), Value::String(system_parts.join("\n\n")));
        let mut instruction = Map::new();
        instruction.insert("parts".to_string(), Value::Array(vec![Value::Object(part)]));
        payload.insert("systemInstruction".to_string(), Value::Object(instruction));
    }
    let mut generation_config = Map::new();
    if let Some(max_tokens) = provider.max_tokens {
        generation_config.insert("maxOutputTokens".to_string(), Value::from(max_tokens));
    }
    if let Some(temperature) = provider.temperature {
        generation_config.insert("temperature".to_string(), Value::from(temperature));
    }
    if !generation_config.is_empty() {
        payload.insert(
            "generationConfig".to_string(),
            Value::Object(generation_config),
        );
    }
    payload
}

/// 抽取归一化文本（Python `_extract_provider_text`）。
///
/// # Errors
/// 响应缺 choices / message / content 等畸形结构时返回
/// [`GatewayError::Protocol`]，消息逐字镜像 Python `ValueError`。
pub fn extract_provider_text(
    provider_type: ProviderType,
    data: &Value,
) -> Result<String, GatewayError> {
    match provider_type {
        ProviderType::Anthropic => extract_anthropic_text(data),
        ProviderType::Gemini => extract_gemini_text(data),
        _ => extract_openai_compatible_text(data),
    }
}

/// 抽取模型的**思考过程原文**（reasoning / thinking / thought）。
///
/// 与 [`extract_provider_text`] 的关键差别：**缺思考不是错误**。非推理
/// 模型、未显式开启 extended thinking 的 Anthropic 调用、以及一切把思考
/// 关掉的 provider，都理所当然返回 `Ok(None)`。调用方不得因为拿不到思考
/// 就 fail closed——那会把"这个模型不吐思考"误判成"响应畸形"。
///
/// 三家 wire 字段（都在 `text` 之外，此前被整个丢弃）：
///
/// | provider | 思考在哪 |
/// |---|---|
/// | OpenAI 兼容（含 o 系列 / DeepSeek-R1） | `choices[0].message.reasoning_content`，部分网关用 `reasoning` |
/// | Anthropic（extended thinking） | `content[]` 里 `type=="thinking"` 的块，正文在 `thinking` |
/// | Gemini（2.5+ 思考） | `candidates[0].content.parts[]` 里 `thought==true` 的 part |
///
/// # Errors
/// 结构与 [`extract_provider_text`] 同样畸形时返回同一协议错误（保证
/// "文本抽得出/抽不出"与"思考抽得出/抽不出"的失败语义一致，不会出现
/// 文本失败但思考成功的分裂状态）。
pub fn extract_provider_reasoning(
    provider_type: ProviderType,
    data: &Value,
) -> Result<Option<String>, GatewayError> {
    let reasoning = match provider_type {
        ProviderType::Anthropic => extract_anthropic_reasoning(data)?,
        ProviderType::Gemini => extract_gemini_reasoning(data)?,
        _ => extract_openai_compatible_reasoning(data)?,
    };
    // 空串 / 纯空白一律归一成 None：调用方只该区分"有思考"和"没思考"。
    Ok(reasoning.filter(|text| !text.trim().is_empty()))
}

/// Chat Completions 响应 envelope 归一化（Rust 侧扩展，无 Python 对应）。
///
/// 部分 `OpenAI` 兼容网关在 HTTP 200 下返回非标准 envelope：完整响应嵌套
/// 在 `{"success": true, "data": {...}}` 里（如 Cline），或以
/// `{"success": false, "error": ...}` 报告逻辑失败。归一化在字段抽取
/// （text / model / usage / `finish_reason`）之前只执行一次：
///
/// 1. 顶层 `choices` 存在 → 原对象原样放行（空 choices / 缺 message 等
///    畸形仍由抽取函数按既有协议错误 fail closed）；
/// 2. `success=true` 且 `data.choices` 存在 → 解包 `data` 对象；
/// 3. `success=false` → 提取 `detail` / `message` / `error` 文本（复用
///    [`response_error_detail_value`] 的回退链，含脱敏与截断）→
///    结构化协议错误；
/// 4. 其余形状 → fail closed（与缺 `choices` 同一错误）。
pub(crate) fn normalize_chat_completions_envelope(data: Value) -> Result<Value, GatewayError> {
    if data.get("choices").is_some() {
        return Ok(data);
    }
    let success = data.get("success").and_then(Value::as_bool);
    if success == Some(true)
        && let Value::Object(map) = &data
        && let Some(inner) = map
            .get("data")
            .filter(|inner| inner.get("choices").is_some())
    {
        return Ok(inner.clone());
    }
    if success == Some(false) {
        return Err(match response_error_detail_value(&data) {
            Some(detail) => {
                GatewayError::protocol(&format!("provider response reported failure: {detail}"))
            }
            None => {
                GatewayError::protocol("provider response reported failure without an error detail")
            }
        });
    }
    Err(GatewayError::protocol("provider response missing choices"))
}

fn extract_openai_compatible_text(data: &Value) -> Result<String, GatewayError> {
    let choices = data
        .get("choices")
        .and_then(Value::as_array)
        .filter(|choices| !choices.is_empty())
        .ok_or_else(|| GatewayError::protocol("provider response missing choices"))?;
    let first = choices
        .first()
        .filter(|first| first.is_object())
        .ok_or_else(|| GatewayError::protocol("provider response choice is invalid"))?;
    let message = first
        .get("message")
        .filter(|message| message.is_object())
        .ok_or_else(|| GatewayError::protocol("provider response missing message"))?;
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| GatewayError::protocol("provider response message content is not text"))?;
    Ok(content.to_string())
}

/// OpenAI 兼容 wire 的思考抽取。
///
/// 字段名歧义是真实存在的：OpenAI o 系列与 DeepSeek-R1 用
/// `reasoning_content`，部分中转/网关（以及较新的 OpenAI 字段）用
/// `reasoning`。两个都试，`reasoning_content` 优先——它是事实标准，
/// `reasoning` 只在它缺失时兜底，避免某些网关把别的东西放在 `reasoning`
/// 键上时抢错。
fn extract_openai_compatible_reasoning(data: &Value) -> Result<Option<String>, GatewayError> {
    // 复用 text 的同一串结构校验：choices/message 畸形必须与 text 路径
    // 报同一个错，不能出现"文本抽失败、思考却抽成功"的分裂状态。
    let choices = data
        .get("choices")
        .and_then(Value::as_array)
        .filter(|choices| !choices.is_empty())
        .ok_or_else(|| GatewayError::protocol("provider response missing choices"))?;
    let first = choices
        .first()
        .filter(|first| first.is_object())
        .ok_or_else(|| GatewayError::protocol("provider response choice is invalid"))?;
    let message = first
        .get("message")
        .filter(|message| message.is_object())
        .ok_or_else(|| GatewayError::protocol("provider response missing message"))?;
    Ok(message
        .get("reasoning_content")
        .and_then(Value::as_str)
        .or_else(|| message.get("reasoning").and_then(Value::as_str))
        .map(str::to_string))
}

fn extract_anthropic_text(data: &Value) -> Result<String, GatewayError> {
    let content = data
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| GatewayError::protocol("provider response missing content"))?;
    let parts: Vec<&str> = content
        .iter()
        .filter_map(|item| {
            // Python：isinstance(item, dict) 且 type == "text" 且 text 为 str。
            if item.get("type").and_then(Value::as_str) == Some("text") {
                item.get("text").and_then(Value::as_str)
            } else {
                None
            }
        })
        .collect();
    if parts.is_empty() {
        return Err(GatewayError::protocol(
            "provider response content has no text",
        ));
    }
    Ok(parts.join("\n"))
}

/// Anthropic extended thinking 的思考抽取。
///
/// 思考是 `content[]` 里 `type=="thinking"` 的块，正文在 `thinking` 键
/// （不是 `text`）。多块按序拼接。
///
/// 注意：**未开启 extended thinking 时这类块根本不存在**，返回
/// `Ok(None)` 是正常路径而非异常。
fn extract_anthropic_reasoning(data: &Value) -> Result<Option<String>, GatewayError> {
    let content = data
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| GatewayError::protocol("provider response missing content"))?;
    let parts: Vec<&str> = content
        .iter()
        .filter_map(|item| {
            if item.get("type").and_then(Value::as_str) == Some("thinking") {
                item.get("thinking").and_then(Value::as_str)
            } else {
                None
            }
        })
        .collect();
    Ok((!parts.is_empty()).then(|| parts.join("\n")))
}

fn extract_gemini_text(data: &Value) -> Result<String, GatewayError> {
    let candidates = data
        .get("candidates")
        .and_then(Value::as_array)
        .filter(|candidates| !candidates.is_empty())
        .ok_or_else(|| GatewayError::protocol("provider response missing candidates"))?;
    let first = candidates
        .first()
        .filter(|first| first.is_object())
        .ok_or_else(|| GatewayError::protocol("provider response candidate is invalid"))?;
    let content = first
        .get("content")
        .filter(|content| content.is_object())
        .ok_or_else(|| GatewayError::protocol("provider response missing content"))?;
    let raw_parts = content
        .get("parts")
        .and_then(Value::as_array)
        .ok_or_else(|| GatewayError::protocol("provider response content missing parts"))?;
    let text_parts: Vec<&str> = raw_parts
        .iter()
        .filter(|part| part.is_object())
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect();
    if text_parts.is_empty() {
        return Err(GatewayError::protocol(
            "provider response content has no text",
        ));
    }
    Ok(text_parts.join("\n"))
}

/// Gemini 思考抽取。
///
/// 思考是 `candidates[0].content.parts[]` 里 `thought==true` 的 part，
/// 正文同样在 `text` 键。`extract_gemini_text` 把**所有**带 text 的 part
/// 都拼进答案——思考 part 因此一直被混进正文里（Gemini 2.5 思考模型的
/// 实际行为），这里按 `thought` 标记把它们认出来。
///
/// 未标注 `thought` 的 part 不属于思考，返回 `Ok(None)`。
fn extract_gemini_reasoning(data: &Value) -> Result<Option<String>, GatewayError> {
    let candidates = data
        .get("candidates")
        .and_then(Value::as_array)
        .filter(|candidates| !candidates.is_empty())
        .ok_or_else(|| GatewayError::protocol("provider response missing candidates"))?;
    let first = candidates
        .first()
        .filter(|first| first.is_object())
        .ok_or_else(|| GatewayError::protocol("provider response candidate is invalid"))?;
    let content = first
        .get("content")
        .filter(|content| content.is_object())
        .ok_or_else(|| GatewayError::protocol("provider response missing content"))?;
    let raw_parts = content
        .get("parts")
        .and_then(Value::as_array)
        .ok_or_else(|| GatewayError::protocol("provider response content missing parts"))?;
    let thought_parts: Vec<&str> = raw_parts
        .iter()
        .filter(|part| part.is_object())
        .filter(|part| part.get("thought").and_then(Value::as_bool) == Some(true))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect();
    Ok((!thought_parts.is_empty()).then(|| thought_parts.join("\n")))
}

/// 抽取 token 用量（Python `_extract_usage`，非 int / bool 一律 `None`）。
#[must_use]
pub fn extract_usage(provider_type: ProviderType, data: &Value) -> (Option<i64>, Option<i64>) {
    let usage = match provider_type {
        ProviderType::Gemini => data.get("usageMetadata"),
        _ => data.get("usage"),
    }
    .and_then(Value::as_object);
    let Some(usage) = usage else {
        return (None, None);
    };
    match provider_type {
        ProviderType::Anthropic => (
            int_or_none(usage.get("input_tokens")),
            int_or_none(usage.get("output_tokens")),
        ),
        ProviderType::Gemini => (
            int_or_none(usage.get("promptTokenCount")),
            int_or_none(usage.get("candidatesTokenCount")),
        ),
        _ => (
            int_or_none(usage.get("prompt_tokens")),
            int_or_none(usage.get("completion_tokens")),
        ),
    }
}

fn int_or_none(value: Option<&Value>) -> Option<i64> {
    // Python `isinstance(value, bool)` 先排除：serde_json 的 as_i64 对
    // Bool / Float 返回 None，语义天然对齐。
    value.and_then(Value::as_i64)
}

/// 抽取 wire 层的结束原因原值（无 Python 对应——截断守卫的 Rust 侧扩展）：
/// `OpenAI` `choices[0].finish_reason` / Anthropic 顶层 `stop_reason` /
/// Gemini `candidates[0].finishReason`。
#[must_use]
pub fn extract_finish_reason(provider_type: ProviderType, data: &Value) -> Option<String> {
    let reason = match provider_type {
        ProviderType::Anthropic => data.get("stop_reason"),
        ProviderType::Gemini => data
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.first())
            .and_then(|first| first.get("finishReason")),
        _ => data
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|first| first.get("finish_reason")),
    };
    reason
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// 判定结束原因是否代表输出被 token 上限截断（三家方言归一）：
/// `OpenAI` `length`、Anthropic/Gemini `max_tokens`（Gemini wire 值
/// `MAX_TOKENS` 大小写不敏感比较）。
#[must_use]
pub fn finish_reason_is_truncated(reason: &str) -> bool {
    matches!(reason.to_lowercase().as_str(), "length" | "max_tokens")
}

/// 抽取响应报告的模型 ID（Python `_extract_model`，缺失回退配置模型）。
#[must_use]
pub fn extract_model(provider: &ProviderConfig, data: &Value) -> String {
    if let Some(model) = data
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
    {
        return model.to_string();
    }
    provider.model.clone().unwrap_or_default()
}

/// 抽取并归一化 `/models` 端点广播的模型 ID
/// （Python `_extract_discovered_model_ids`）。
///
/// 去重后按小写键排序（Python `sorted(key=str.casefold)`；模型 ID 为
/// ASCII，两种归一在有效域内等价）。
#[must_use]
pub fn extract_discovered_model_ids(provider: &ProviderConfig, data: &Value) -> Vec<String> {
    let raw_models = data
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| data.get("models").and_then(Value::as_array));
    let Some(raw_models) = raw_models else {
        return Vec::new();
    };

    let mut models: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in raw_models {
        // Python `item.get("id") or item.get("name") or item.get("model")`：
        // 空串 falsy 滑到下一键。
        let model_id: Option<&str> = match item {
            Value::String(text) => Some(text.as_str()),
            Value::Object(object) => ["id", "name", "model"]
                .iter()
                .find_map(|key| object.get(*key).and_then(Value::as_str))
                .filter(|value| !value.is_empty()),
            _ => None,
        };
        let Some(model_id) = model_id else {
            continue;
        };
        let mut normalized = model_id.trim().to_string();
        if provider.provider_type == ProviderType::Gemini
            && let Some(stripped) = normalized.strip_prefix("models/")
        {
            normalized = stripped.to_string();
        }
        if !normalized.is_empty() {
            models.insert(normalized);
        }
    }
    let mut list: Vec<String> = models.into_iter().collect();
    list.sort_unstable_by_key(|model| model.to_lowercase());
    list
}

/// 解析结构化响应（Python `_parse_structured_response`）：接受 markdown
/// JSON 围栏（三反引号 + `json` 语言标注）；只接受 JSON 对象。
///
/// # Errors
/// 非 JSON 文本或非对象结构返回 [`GatewayError::Protocol`]。
///
/// # Panics
/// 内置围栏正则为静态字面量，仅在其编译失败（静态字符串下不可达）时
/// panic。
pub fn parse_structured_response(text: &str) -> Result<Map<String, Value>, GatewayError> {
    static FENCE_RE: OnceLock<Regex> = OnceLock::new();
    let fence_re = FENCE_RE.get_or_init(|| {
        Regex::new(r"(?is)```(?:json)?\s*(.*?)\s*```")
            .unwrap_or_else(|error| panic!("内置正则必须可编译（静态字面量）: {error}"))
    });

    let trimmed = text.trim().trim_start_matches('\u{feff}');
    let candidate: String = match fence_re.captures(trimmed) {
        Some(captures) => captures[1].trim().to_string(),
        None => trimmed.to_string(),
    };
    let parsed: Value = serde_json::from_str(&candidate)
        .map_err(|_| GatewayError::protocol("provider response was not valid JSON"))?;
    let Some(object) = parsed.as_object() else {
        return Err(GatewayError::protocol(
            "provider response JSON must be an object",
        ));
    };
    Ok(object.clone())
}

#[cfg(test)]
mod tests {
    #![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

    use serde_json::json;

    use super::*;

    #[test]
    fn finish_reason_extracts_three_dialects() {
        let openai = json!({"choices": [{"finish_reason": "length"}]});
        assert_eq!(
            extract_finish_reason(ProviderType::OpenaiCompatible, &openai),
            Some("length".to_string())
        );
        let anthropic = json!({"stop_reason": "max_tokens"});
        assert_eq!(
            extract_finish_reason(ProviderType::Anthropic, &anthropic),
            Some("max_tokens".to_string())
        );
        let gemini = json!({"candidates": [{"finishReason": "MAX_TOKENS"}]});
        assert_eq!(
            extract_finish_reason(ProviderType::Gemini, &gemini),
            Some("MAX_TOKENS".to_string())
        );
        // 缺失/空串/非字符串一律 None。
        assert_eq!(
            extract_finish_reason(ProviderType::OpenaiCompatible, &json!({})),
            None
        );
        assert_eq!(
            extract_finish_reason(ProviderType::Anthropic, &json!({"stop_reason": ""})),
            None
        );
        assert_eq!(
            extract_finish_reason(ProviderType::Anthropic, &json!({"stop_reason": 7})),
            None
        );
    }

    #[test]
    fn finish_reason_truncation_dialects() {
        assert!(finish_reason_is_truncated("length"));
        assert!(finish_reason_is_truncated("max_tokens"));
        assert!(finish_reason_is_truncated("MAX_TOKENS"));
        assert!(!finish_reason_is_truncated("stop"));
        assert!(!finish_reason_is_truncated("end_turn"));
        assert!(!finish_reason_is_truncated("STOP"));
    }

    #[test]
    fn envelope_top_level_choices_passes_through() {
        let standard = json!({
            "model": "gpt-test",
            "choices": [{"message": {"content": "ok"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1},
        });
        let normalized = normalize_chat_completions_envelope(standard.clone())
            .expect("顶层 choices 必须原样放行");
        assert_eq!(normalized, standard);

        // 顶层 choices 优先于包装键：即便 success=false 也不解包、不报
        // 错——畸形 choices 的报错语义仍归抽取函数。
        let hybrid = json!({
            "success": false,
            "error": "boom",
            "choices": [{"message": {"content": "ok"}}],
        });
        let normalized =
            normalize_chat_completions_envelope(hybrid.clone()).expect("hybrid 必须原样放行");
        assert_eq!(normalized, hybrid);
    }

    #[test]
    fn envelope_unwraps_success_data_wrapper() {
        let wrapped = json!({
            "success": true,
            "data": {
                "model": "z-ai/glm-5.3-flash",
                "choices": [{
                    "message": {"role": "assistant", "content": "wrapped ok"},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 4, "completion_tokens": 2},
            },
        });
        let normalized =
            normalize_chat_completions_envelope(wrapped).expect("包装 envelope 必须解包");
        assert_eq!(normalized["model"], "z-ai/glm-5.3-flash");
        assert_eq!(normalized["choices"][0]["finish_reason"], "stop");
        assert_eq!(normalized["usage"]["completion_tokens"], 2);
        // 解包后各字段抽取在 data 对象上照常工作。
        assert_eq!(
            extract_openai_compatible_text(&normalized).expect("文本必须可抽取"),
            "wrapped ok"
        );
        assert_eq!(
            extract_usage(ProviderType::OpenaiCompatible, &normalized),
            (Some(4), Some(2))
        );
        assert_eq!(
            extract_finish_reason(ProviderType::OpenaiCompatible, &normalized),
            Some("stop".to_string())
        );
        let provider = ProviderConfig::new("p".to_string(), ProviderType::OpenaiCompatible);
        assert_eq!(extract_model(&provider, &normalized), "z-ai/glm-5.3-flash");
    }

    #[test]
    fn envelope_failure_is_structured_and_redacted() {
        let failure = json!({
            "success": false,
            "error": "upstream quota exhausted (api-key=sk-envelope-leak)",
        });
        let error =
            normalize_chat_completions_envelope(failure).expect_err("success=false 必须报错");
        let GatewayError::Protocol(message) = &error else {
            panic!("失败 envelope 必须是协议错误，实际: {error:?}");
        };
        assert!(message.contains("provider response reported failure"));
        assert!(message.contains("quota exhausted"));
        assert!(!message.contains("sk-envelope-leak"));
        assert!(message.contains("********"));

        // 无可提取 detail 的失败 envelope 同样 fail closed。
        let error = normalize_chat_completions_envelope(json!({"success": false}))
            .expect_err("无 detail 失败也必须报错");
        assert!(error.to_string().contains("without an error detail"));
    }

    #[test]
    fn envelope_other_shapes_fail_closed() {
        // 无 choices、无 success——与抽取函数的既有错误一致。
        let error =
            normalize_chat_completions_envelope(json!({})).expect_err("空对象必须 fail closed");
        assert!(
            error
                .to_string()
                .contains("provider response missing choices")
        );
        // success=true 但 data 缺 choices：不解包，fail closed。
        let error = normalize_chat_completions_envelope(json!({"success": true, "data": {}}))
            .expect_err("data 缺 choices 必须报错");
        assert!(
            error
                .to_string()
                .contains("provider response missing choices")
        );
        // data 非 JSON 对象同样 fail closed。
        let error = normalize_chat_completions_envelope(json!({"success": true, "data": "text"}))
            .expect_err("data 非对象必须报错");
        assert!(
            error
                .to_string()
                .contains("provider response missing choices")
        );
    }

    // ---------- 思考过程抽取 ----------
    //
    // 回归防线：三家 wire 的思考字段此前被整个丢弃。这三组锁住字段名、
    // 归一化（空串 → None）与"缺思考不是错误"的语义。

    #[test]
    fn reasoning_extracts_three_dialects() {
        // OpenAI 兼容：reasoning_content（o 系列 / DeepSeek-R1 的事实标准）。
        let openai = json!({"choices": [{"message": {
            "content": "答案",
            "reasoning_content": "先想一步，再想一步"
        }}]});
        assert_eq!(
            extract_provider_reasoning(ProviderType::OpenaiCompatible, &openai)
                .expect("合法响应不得报错"),
            Some("先想一步，再想一步".to_string())
        );

        // Anthropic extended thinking：type=="thinking" 的块，正文在 thinking 键。
        let anthropic = json!({"content": [
            {"type": "thinking", "thinking": "内心独白", "signature": "sig"},
            {"type": "text", "text": "答案"}
        ]});
        assert_eq!(
            extract_provider_reasoning(ProviderType::Anthropic, &anthropic)
                .expect("合法响应不得报错"),
            Some("内心独白".to_string())
        );

        // Gemini：thought==true 的 part。
        let gemini = json!({"candidates": [{"content": {"parts": [
            {"text": "Gemini 的思考", "thought": true},
            {"text": "答案"}
        ]}}]});
        assert_eq!(
            extract_provider_reasoning(ProviderType::Gemini, &gemini).expect("合法响应不得报错"),
            Some("Gemini 的思考".to_string())
        );
    }

    #[test]
    fn reasoning_absent_is_none_not_error() {
        // 非推理模型 / 未开 extended thinking：缺思考是正常路径，绝不报错。
        let openai = json!({"choices": [{"message": {"content": "答案"}}]});
        assert_eq!(
            extract_provider_reasoning(ProviderType::OpenaiCompatible, &openai)
                .expect("缺思考不得报错"),
            None
        );
        let anthropic = json!({"content": [{"type": "text", "text": "答案"}]});
        assert_eq!(
            extract_provider_reasoning(ProviderType::Anthropic, &anthropic)
                .expect("缺思考不得报错"),
            None
        );
        let gemini = json!({"candidates": [{"content": {"parts": [{"text": "答案"}]}}]});
        assert_eq!(
            extract_provider_reasoning(ProviderType::Gemini, &gemini).expect("缺思考不得报错"),
            None
        );
    }

    #[test]
    fn reasoning_blank_and_non_string_normalize_to_none() {
        // 空串 / 纯空白 / 非字符串一律 None：调用方只该区分"有/没有"。
        let blank = json!({"choices": [{"message": {"content": "a", "reasoning_content": "   "}}]});
        assert_eq!(
            extract_provider_reasoning(ProviderType::OpenaiCompatible, &blank)
                .expect("空思考不得报错"),
            None
        );
        let non_string =
            json!({"choices": [{"message": {"content": "a", "reasoning_content": 7}}]});
        assert_eq!(
            extract_provider_reasoning(ProviderType::OpenaiCompatible, &non_string)
                .expect("非字符串思考不得报错"),
            None
        );
    }

    #[test]
    fn reasoning_prefers_reasoning_content_over_reasoning_key() {
        // 两个键都在时取 reasoning_content（事实标准）；某些网关会在
        // reasoning 键上放别的东西，不能让它抢错。
        let both = json!({"choices": [{"message": {
            "content": "a",
            "reasoning_content": "正统思考",
            "reasoning": "别的东西"
        }}]});
        assert_eq!(
            extract_provider_reasoning(ProviderType::OpenaiCompatible, &both)
                .expect("合法响应不得报错"),
            Some("正统思考".to_string())
        );
        // 只有 reasoning 键时它兜底。
        let only_reasoning =
            json!({"choices": [{"message": {"content": "a", "reasoning": "兜底思考"}}]});
        assert_eq!(
            extract_provider_reasoning(ProviderType::OpenaiCompatible, &only_reasoning)
                .expect("合法响应不得报错"),
            Some("兜底思考".to_string())
        );
    }

    #[test]
    fn reasoning_multi_block_joins_in_order() {
        // Anthropic 多 thinking 块 / Gemini 多 thought part 按序拼接。
        let anthropic = json!({"content": [
            {"type": "thinking", "thinking": "第一段"},
            {"type": "thinking", "thinking": "第二段"},
            {"type": "text", "text": "答案"}
        ]});
        assert_eq!(
            extract_provider_reasoning(ProviderType::Anthropic, &anthropic)
                .expect("合法响应不得报错"),
            Some("第一段\n第二段".to_string())
        );
    }

    #[test]
    fn reasoning_malformed_shape_fails_like_text() {
        // 畸形结构的失败语义必须与 text 路径一致——不能出现"文本抽失败、
        // 思考却抽成功"的分裂状态。
        let error = extract_provider_reasoning(ProviderType::OpenaiCompatible, &json!({}))
            .expect_err("缺 choices 必须报错");
        assert!(
            error.to_string().contains("provider response missing choices"),
            "实际: {error}"
        );
        let error = extract_provider_reasoning(ProviderType::Anthropic, &json!({}))
            .expect_err("缺 content 必须报错");
        assert!(
            error.to_string().contains("provider response missing content"),
            "实际: {error}"
        );
        let error = extract_provider_reasoning(ProviderType::Gemini, &json!({}))
            .expect_err("缺 candidates 必须报错");
        assert!(
            error
                .to_string()
                .contains("provider response missing candidates"),
            "实际: {error}"
        );
    }

    #[test]
    fn reasoning_does_not_leak_answer_text() {
        // 关键不变量：思考与答案必须互不污染。text 里不该出现思考内容，
        // reasoning 里也不该混进答案。
        let openai = json!({"choices": [{"message": {
            "content": "这是答案",
            "reasoning_content": "这是思考"
        }}]});
        let text = extract_provider_text(ProviderType::OpenaiCompatible, &openai)
            .expect("合法响应不得报错");
        let reasoning = extract_provider_reasoning(ProviderType::OpenaiCompatible, &openai)
            .expect("合法响应不得报错");
        assert_eq!(text, "这是答案");
        assert_eq!(reasoning, Some("这是思考".to_string()));
        assert!(!text.contains("这是思考"));
        assert!(!reasoning.unwrap_or_default().contains("这是答案"));
    }
}
