//! Canonicalization：不同 source 对同一资产的表示必须收敛到同一
//! 归一值，否则 dedup 无从谈起（2.8）。
//!
//! 规则全部确定性、可单测：域名小写去尾点、IP 走 `IpAddr` 解析、
//! URL 小写 host、去默认端口、去 fragment。

use std::net::IpAddr;

/// 域名归一：trim → 去 `*.` 前缀 → 小写 → 去尾点 → 校验标签。
/// 非法输入返回 `None`（调用方丢弃，不产生实体）。
#[must_use]
pub fn normalize_domain(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_start_matches("*.");
    let lowered = trimmed.trim_end_matches('.').to_lowercase();
    if lowered.is_empty() || lowered.len() > 253 || lowered.contains(['/', ' ', '@', ':']) {
        return None;
    }
    let labels: Vec<&str> = lowered.split('.').collect();
    if labels.len() < 2 {
        return None;
    }
    for label in &labels {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return None;
        }
    }
    Some(lowered)
}

/// IP 归一：`IpAddr` 解析后的规范文本（v4 点分 / v6 压缩）。
#[must_use]
pub fn normalize_ip(raw: &str) -> Option<String> {
    raw.trim().parse::<IpAddr>().ok().map(|ip| ip.to_string())
}

/// URL 归一：`scheme://host[:port][/path][?query]`——scheme/host 小写、
/// 去默认端口、去 fragment、去末尾空 path 的 `/`。
#[must_use]
pub fn normalize_url(raw: &str) -> Option<String> {
    let parsed = url::Url::parse(raw.trim()).ok()?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return None;
    }
    let host = parsed.host_str()?.to_lowercase();
    let mut normalized = format!("{}://{host}", parsed.scheme());
    if let Some(port) = parsed.port() {
        normalized.push(':');
        normalized.push_str(&port.to_string());
    }
    let path = parsed.path();
    if path != "/" {
        normalized.push_str(path);
    }
    if let Some(query) = parsed.query() {
        normalized.push('?');
        normalized.push_str(query);
    }
    Some(normalized)
}

/// 从 URL 提取归一化 host（域名或 IP）。
#[must_use]
pub fn url_host(raw: &str) -> Option<String> {
    let parsed = url::Url::parse(raw.trim()).ok()?;
    let host = parsed.host_str()?;
    normalize_domain(host).or_else(|| normalize_ip(host))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_lowercases_and_strips_wildcard_and_trailing_dot() {
        assert_eq!(
            normalize_domain("*.Example.COM."),
            Some("example.com".into())
        );
        assert_eq!(
            normalize_domain("Sub.Example.com"),
            Some("sub.example.com".into())
        );
    }

    #[test]
    fn domain_rejects_garbage() {
        assert_eq!(normalize_domain(""), None);
        assert_eq!(normalize_domain("not-a-domain"), None);
        assert_eq!(normalize_domain("http://example.com/x"), None);
        assert_eq!(normalize_domain("bad label.com"), None);
        assert_eq!(normalize_domain("-bad.com"), None);
    }

    #[test]
    fn ip_normalizes_both_families() {
        assert_eq!(normalize_ip(" 1.2.3.4 "), Some("1.2.3.4".into()));
        assert_eq!(
            normalize_ip("2001:0db8:0000:0000:0000:ff00:0042:8329"),
            Some("2001:db8::ff00:42:8329".into())
        );
        assert_eq!(normalize_ip("999.1.1.1"), None);
    }

    #[test]
    fn url_normalizes_host_port_fragment() {
        assert_eq!(
            normalize_url("HTTP://Example.COM:80/a/b?x=1#frag"),
            Some("http://example.com/a/b?x=1".into())
        );
        assert_eq!(
            normalize_url("https://example.com:443/"),
            Some("https://example.com".into())
        );
        assert_eq!(normalize_url("ftp://example.com/x"), None);
        assert_eq!(normalize_url("not a url"), None);
    }

    #[test]
    fn url_host_extracts_domain_or_ip() {
        assert_eq!(
            url_host("https://Sub.Example.com:8443/x"),
            Some("sub.example.com".into())
        );
        assert_eq!(url_host("http://1.2.3.4:8080/"), Some("1.2.3.4".into()));
    }
}
