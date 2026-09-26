//! 脱敏 / 摘要 / 哈希辅助 —— Python `runtime.py` 模块级辅助函数的移植。
//!
//! 三张正则表与 Python 逐字一致（`_SECRET_KEY_RE` / `_BEARER_RE` /
//! `_COMMON_SECRET_VALUE_RE`），`OnceLock` 编译一次；摘要与哈希供
//! `ModelInvocation` 审计字段使用——长正文不落库，秘密绝不入库。

use std::sync::OnceLock;

use agents::llm::LlmMessage;
use regex::Captures;
use regex::Regex;
use sha2::Digest;
use sha2::Sha256;

static SECRET_KEY_RE: OnceLock<Regex> = OnceLock::new();
static BEARER_RE: OnceLock<Regex> = OnceLock::new();
static COMMON_SECRET_VALUE_RE: OnceLock<Regex> = OnceLock::new();

fn secret_key_re() -> &'static Regex {
    SECRET_KEY_RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(authorization|x-api-key|api-key|api_key|apikey|anthropic-api-key|openai-api-key|gemini-api-key|secret|token|password)\b\s*[:=]\s*([^\s,;]+)",
        )
        .unwrap_or_else(|error| panic!("内置正则必须可编译（静态字面量）: {error}"))
    })
}

fn bearer_re() -> &'static Regex {
    BEARER_RE.get_or_init(|| {
        Regex::new(r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]+")
            .unwrap_or_else(|error| panic!("内置正则必须可编译（静态字面量）: {error}"))
    })
}

fn common_secret_value_re() -> &'static Regex {
    COMMON_SECRET_VALUE_RE.get_or_init(|| {
        Regex::new(r"\b(?:sk|sk-proj|sk-ant|AIza)[A-Za-z0-9._-]{8,}\b")
            .unwrap_or_else(|error| panic!("内置正则必须可编译（静态字面量）: {error}"))
    })
}

/// 三级脱敏（Python `_redact_secrets`）：
/// `key: value` / `key=value` 形态 → `key=********`；`Bearer xxx` →
/// `Bearer ********`；`sk-…` / `AIza…` 常见密钥值 → `********`。
#[must_use]
pub fn redact_secrets(text: &str) -> String {
    let keyed = secret_key_re().replace_all(text, |captures: &Captures<'_>| {
        format!("{}=********", &captures[1])
    });
    let debearer = bearer_re().replace_all(&keyed, "Bearer ********");
    common_secret_value_re()
        .replace_all(&debearer, "********")
        .into_owned()
}

/// 脱敏 + 折叠空白 + 500 字符截断（Python `_summarize_text`）。
#[must_use]
pub fn summarize_text(text: &str) -> String {
    const LIMIT: usize = 500;
    let compact = redact_secrets(text)
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ");
    if compact.chars().count() <= LIMIT {
        return compact;
    }
    let truncated: String = compact.chars().take(LIMIT).collect();
    format!("{truncated}...")
}

/// 消息序列摘要（Python `_summarize_messages`）：
/// `"role: content"` 逐行拼接后走 [`summarize_text`]。
#[must_use]
pub fn summarize_messages(messages: &[LlmMessage]) -> String {
    let joined = messages
        .iter()
        .map(|m| format!("{}: {}", m.role, m.content))
        .collect::<Vec<String>>()
        .join("\n");
    summarize_text(&joined)
}

/// UTF-8 SHA-256 十六进制（Python `_hash_text`）。
#[must_use]
pub fn hash_text(text: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(text.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push(HEX[usize::from(byte >> 4)] as char);
        hex.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    hex
}

/// Python `json.dumps([m.model_dump() for m in messages], sort_keys=True)`
/// 的逐字节镜像——`prompt_hash` 的输入。
///
/// 与 `serde_json` 默认序列化的差异点全部按 `CPython` `json.encoder` 复刻：
/// - 分隔符 `", "` 与 `": "`（默认 separators）；
/// - 键按字母序（`sort_keys`）：`content` 在 `role` 前；
/// - `ensure_ascii=True`：非 ASCII 转义为 `\uXXXX`（astral 走代理对）；
/// - 控制字符转义 `\b \f \n \r \t`，其余 < 0x20 为 `\u00xx` 小写十六进制。
#[must_use]
pub fn python_json_dumps_messages(messages: &[LlmMessage]) -> String {
    let mut out = String::with_capacity(messages.len() * 32 + 2);
    out.push('[');
    for (index, message) in messages.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push_str("{\"content\": ");
        python_json_escape_into(&mut out, &message.content);
        out.push_str(", \"role\": ");
        python_json_escape_into(&mut out, &message.role);
        out.push('}');
    }
    out.push(']');
    out
}

/// Python `json.dumps` 的字符串编码（`ensure_ascii=True`）。
fn python_json_escape_into(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => push_unicode_escape(out, c as u32),
            c if (c as u32) > 0x7f => {
                let code = c as u32;
                if code <= 0xffff {
                    push_unicode_escape(out, code);
                } else {
                    // astral 平面 → UTF-16 代理对（CPython ensure_ascii 行为）。
                    let offset = code - 0x1_0000;
                    let high = 0xd800 + (offset >> 10);
                    let low = 0xdc00 + (offset & 0x3ff);
                    push_unicode_escape(out, high);
                    push_unicode_escape(out, low);
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `\uXXXX` 小写十六进制转义（CPython `json.encoder` 的 `py_encode_basestring_ascii`）。
fn push_unicode_escape(out: &mut String, code: u32) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push_str("\\u");
    for shift in [12, 8, 4, 0] {
        let nibble = ((code >> shift) & 0x0f) as usize;
        out.push(HEX[nibble] as char);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_key_colon_and_equals_forms() {
        // group(2) 吃掉整个密钥值，替换后不再残留可匹配的 sk- 值。
        assert_eq!(
            redact_secrets("api-key: sk-abcdef12345678"),
            "api-key=********"
        );
        assert_eq!(redact_secrets("token=abc123"), "token=********");
        assert_eq!(redact_secrets("password: hunter2"), "password=********");
    }

    #[test]
    fn redacts_bearer_and_common_secret_values() {
        assert_eq!(
            redact_secrets("Authorization: Bearer abc.def.ghi"),
            "Authorization=******** abc.def.ghi"
        );
        assert_eq!(
            redact_secrets("use sk-proj-abcdefgh1234 now"),
            "use ******** now"
        );
        assert_eq!(redact_secrets("key AIzaSyA0123456789"), "key ********");
    }

    #[test]
    fn redaction_replacement_uses_original_key_case() {
        assert_eq!(redact_secrets("API_KEY=zzz"), "API_KEY=********");
    }

    #[test]
    fn summarize_collapses_whitespace_and_truncates() {
        assert_eq!(summarize_text("a\n\n  b\t c"), "a b c");
        let long = "x".repeat(600);
        let summarized = summarize_text(&long);
        assert_eq!(summarized.chars().count(), 503);
        assert!(summarized.ends_with("..."));
    }

    #[test]
    fn hash_text_matches_sha256_hex() {
        assert_eq!(
            hash_text("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn python_json_dumps_matches_cpython_formatting() {
        let messages = vec![
            LlmMessage::new("system", "note".to_string()),
            LlmMessage::new("user", "hello \"world\"\nsecond".to_string()),
        ];
        assert_eq!(
            python_json_dumps_messages(&messages),
            "[{\"content\": \"note\", \"role\": \"system\"}, \
             {\"content\": \"hello \\\"world\\\"\\nsecond\", \"role\": \"user\"}]"
        );
    }

    #[test]
    fn python_json_dumps_escapes_non_ascii_as_ensure_ascii() {
        let messages = vec![LlmMessage::new("user", "héllo 🎉".to_string())];
        assert_eq!(
            python_json_dumps_messages(&messages),
            "[{\"content\": \"h\\u00e9llo \\ud83c\\udf89\", \"role\": \"user\"}]"
        );
    }
}
