//! Provider 密钥解析 —— `engines/model_providers/runtime.py` 的
//! `SecretStore` / `EnvironmentSecretStore` / `PlaintextSecretStore` 移植。
//!
//! 密钥只在请求构造瞬间出现于内存，永不进入审计记录（落库前统一
//! `redact_secrets` 脱敏）与公开 API/MCP 响应。

use models::provider::ProviderConfig;

/// 从存储配置解析请求密钥，不向 API 响应暴露（Python `SecretStore`）。
pub trait SecretStore: Send + Sync {
    /// 解析 provider 的密钥；`None` = 未配置（匿名/本地 provider 合法）。
    fn resolve(&self, provider: &ProviderConfig) -> Option<String>;
}

/// 支持 `api_key_ref='env:NAME'` 引用的 secret store。
#[derive(Debug, Default)]
pub struct EnvironmentSecretStore;

impl SecretStore for EnvironmentSecretStore {
    fn resolve(&self, provider: &ProviderConfig) -> Option<String> {
        if let Some(api_key_ref) = provider.api_key_ref.as_ref()
            && api_key_ref.starts_with("env:")
        {
            return std::env::var(&api_key_ref[4..]).ok();
        }
        provider.encrypted_api_key.clone()
    }
}

/// MVP 本地开发 store（Python `PlaintextSecretStore`）。
///
/// `encrypted_api_key` 被当作已存储的密钥值直接解析；公开 API schema
/// 永不返回它，真实加密 secret store 接入后替换本实现而不改仓储/runtime
/// 契约。
#[derive(Debug, Default)]
pub struct PlaintextSecretStore(EnvironmentSecretStore);

impl SecretStore for PlaintextSecretStore {
    fn resolve(&self, provider: &ProviderConfig) -> Option<String> {
        self.0.resolve(provider)
    }
}
