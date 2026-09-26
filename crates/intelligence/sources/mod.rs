//! 真实实现的数据源（本轮 2 个参考实现）：
//!
//! - [`CrtShSource`]：crt.sh Certificate Transparency 查询（域名 →
//!   证书 SAN/CN 包含的域名 + 签发组织）。
//! - [`WaybackSource`]：Internet Archive Wayback CDX（域名 → 历史
//!   URL）。
//!
//! 二者都无需凭据、有公开稳定接口，且都支持 `base_url` 注入（测试用
//! 本地 mock server，不打外网）。
//!
//! 未实现的 source（FOFA / Hunter / Quake / Shodan / Censys / pDNS /
//! GitHub code search / CVE 源）不会进入 active registry，UI 因此不会
//! 宣称它们已可用。

mod crtsh;
mod wayback;

pub use crtsh::CrtShSource;
pub use wayback::WaybackSource;

use std::sync::Arc;

use crate::source::IntelligenceSource;

/// 生产默认 source 集（真实实现的全部注册）。
#[must_use]
pub fn default_sources() -> Vec<Arc<dyn IntelligenceSource>> {
    let crtsh_base_url = source_base_url("LYNCEUS_CRTSH_BASE_URL", "https://crt.sh");
    let wayback_base_url = source_base_url("LYNCEUS_WAYBACK_BASE_URL", "https://web.archive.org");
    vec![
        Arc::new(CrtShSource::with_base_url(&crtsh_base_url)),
        Arc::new(WaybackSource::with_base_url(&wayback_base_url)),
    ]
}

fn source_base_url(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}
