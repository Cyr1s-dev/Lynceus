//! 确定性秘密脱敏 —— `server/core/tools/redaction.py` 的移植。
//!
//! 工具与 worker 审计面共用：journal 落盘前对 payload 做递归脱敏，
//! 敏感键下的值整体替换为 `********`，字符串内的常见凭据形态就地打码。
//! 三张正则与 Python 逐字一致。

use std::sync::OnceLock;

use regex::Captures;
use regex::Regex;
use serde_json::Map;
use serde_json::Value;

/// Python `_SECRET_KEY_RE`：`key: value` / `key=value` 形态。
fn secret_key_re() -> &'static Regex {
    SECRET_KEY_RE.get_or_init(|| {
        Regex::new(
            r"(?i)(api[_-]?key|authorization|cookie|password|secret|token)\s*[:=]\s*([^\s,;]+)",
        )
        .unwrap_or_else(|error| panic!("内置正则必须可编译（静态字面量）: {error}"))
    })
}

/// Python `_BEARER_RE`：`Bearer xxx` 形态。
fn bearer_re() -> &'static Regex {
    BEARER_RE.get_or_init(|| {
        Regex::new(r"(?i)bearer\s+[a-z0-9._~+/=-]+")
            .unwrap_or_else(|error| panic!("内置正则必须可编译（静态字面量）: {error}"))
    })
}

static SECRET_KEY_RE: OnceLock<Regex> = OnceLock::new();
static BEARER_RE: OnceLock<Regex> = OnceLock::new();

/// Python `_SENSITIVE_KEYS`：键名（小写、`-`→`_` 归一后）包含任一标记即视为敏感。
const SENSITIVE_KEYS: [&str; 7] = [
    "api_key",
    "apikey",
    "authorization",
    "cookie",
    "password",
    "secret",
    "token",
];

/// 就地打码字符串里的常见凭据形态，其余文本不变（Python `redact_text`）。
#[must_use]
pub fn redact_text(value: &str) -> String {
    let debearer = bearer_re().replace_all(value, "Bearer ********");
    secret_key_re()
        .replace_all(&debearer, |captures: &Captures<'_>| {
            format!("{}=********", &captures[1])
        })
        .into_owned()
}

/// 递归脱敏 JSON 形数据中敏感键下的值（Python `redact_value`）。
///
/// Python 对 `None` 输入返回 `None`、对字符串返回 `redact_text(value) or ""`；
/// Rust 侧 [`Value`] 的 `Null` 原样保留（等价 None→None），空字符串脱敏后
/// 仍为空串，语义一致。
#[must_use]
pub fn redact_value(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(redact_text(text)),
        Value::Array(items) => Value::Array(items.iter().map(redact_value).collect()),
        Value::Object(map) => Value::Object(redact_map(map)),
        other => other.clone(),
    }
}

fn redact_map(map: &Map<String, Value>) -> Map<String, Value> {
    let mut redacted = Map::new();
    for (key, item) in map {
        let normalized = key.to_lowercase().replace('-', "_");
        if SENSITIVE_KEYS
            .iter()
            .any(|marker| normalized.contains(marker))
        {
            redacted.insert(key.clone(), Value::String("********".to_string()));
        } else {
            redacted.insert(key.clone(), redact_value(item));
        }
    }
    redacted
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redact_text_masks_bearer_and_keyed_secrets() {
        // 两遍替换顺序与 Python 一致：Bearer 先整体打码，随后
        // `Authorization: Bearer` 的键值形态再被打码（值捕获组吃掉 Bearer）。
        assert_eq!(
            redact_text("Authorization: Bearer abc123.def"),
            "Authorization=******** ********"
        );
        assert_eq!(
            redact_text("password=hunter2, other"),
            "password=********, other"
        );
        assert_eq!(redact_text("clean text"), "clean text");
    }

    #[test]
    fn redact_value_replaces_sensitive_keys_recursively() {
        let input = json!({
            "api_key": "sk-123",
            "nested": {"X-API-Key": "v", "note": "keep"},
            "list": ["token: abc", "plain"],
            "count": 7
        });
        let redacted = redact_value(&input);
        assert_eq!(redacted["api_key"], json!("********"));
        assert_eq!(redacted["nested"]["X-API-Key"], json!("********"));
        assert_eq!(redacted["nested"]["note"], json!("keep"));
        assert_eq!(redacted["list"][0], json!("token=********"));
        assert_eq!(redacted["list"][1], json!("plain"));
        assert_eq!(redacted["count"], json!(7));
    }
}
