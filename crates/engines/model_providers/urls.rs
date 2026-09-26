//! Base URL 归一化 —— Python `_normalize_base_url` 的移植。
//!
//! Anthropic 特例：Claude Code 风格配置用服务源（`https://x`）作
//! `ANTHROPIC_BASE_URL`，客户端自行追加 `/v1/messages`；Lynceus 自己
//! 拼端点，此处接受同源根形式并补 `/v1`。

use models::provider::ProviderType;

/// 归一化 provider base URL（尾斜杠剥离 + Anthropic 根路径补 `/v1` +
/// 各家族默认端点）。
#[must_use]
pub fn normalize_base_url(base_url: Option<&str>, provider_type: ProviderType) -> String {
    if let Some(base_url) = base_url {
        let normalized = base_url.trim_end_matches('/');
        if provider_type == ProviderType::Anthropic
            && let Ok(parsed) = url::Url::parse(normalized)
        {
            let path = parsed.path();
            if path.is_empty() || path == "/" {
                let authority = match parsed.port() {
                    Some(port) => format!("{}:{port}", parsed.host_str().unwrap_or_default()),
                    None => parsed.host_str().unwrap_or_default().to_string(),
                };
                return format!("{}://{authority}/v1", parsed.scheme());
            }
        }
        return normalized.to_string();
    }
    match provider_type {
        ProviderType::Anthropic => "https://api.anthropic.com/v1",
        ProviderType::Gemini => "https://generativelanguage.googleapis.com/v1beta",
        ProviderType::Ollama => "http://127.0.0.1:11434/v1",
        ProviderType::LmStudio => "http://127.0.0.1:1234/v1",
        _ => "https://api.openai.com/v1",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_trailing_slash() {
        assert_eq!(
            normalize_base_url(
                Some("https://example.invalid/v1/"),
                ProviderType::OpenaiCompatible
            ),
            "https://example.invalid/v1"
        );
    }

    #[test]
    fn anthropic_root_path_gets_v1_appended() {
        assert_eq!(
            normalize_base_url(Some("https://gateway.example"), ProviderType::Anthropic),
            "https://gateway.example/v1"
        );
        assert_eq!(
            normalize_base_url(Some("https://gateway.example/"), ProviderType::Anthropic),
            "https://gateway.example/v1"
        );
        assert_eq!(
            normalize_base_url(Some("http://127.0.0.1:8080"), ProviderType::Anthropic),
            "http://127.0.0.1:8080/v1"
        );
    }

    #[test]
    fn anthropic_subpath_is_kept_as_is() {
        assert_eq!(
            normalize_base_url(Some("https://gw.example/sub"), ProviderType::Anthropic),
            "https://gw.example/sub"
        );
    }

    #[test]
    fn family_defaults_match_python() {
        assert_eq!(
            normalize_base_url(None, ProviderType::Anthropic),
            "https://api.anthropic.com/v1"
        );
        assert_eq!(
            normalize_base_url(None, ProviderType::Gemini),
            "https://generativelanguage.googleapis.com/v1beta"
        );
        assert_eq!(
            normalize_base_url(None, ProviderType::Ollama),
            "http://127.0.0.1:11434/v1"
        );
        assert_eq!(
            normalize_base_url(None, ProviderType::LmStudio),
            "http://127.0.0.1:1234/v1"
        );
        assert_eq!(
            normalize_base_url(None, ProviderType::Openai),
            "https://api.openai.com/v1"
        );
        assert_eq!(
            normalize_base_url(None, ProviderType::OpenaiCompatible),
            "https://api.openai.com/v1"
        );
    }
}
