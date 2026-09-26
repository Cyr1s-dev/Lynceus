//! Provider 配置写入入口。
//!
//! 密钥只接受后端输入并保存到配置的 secret 字段；读取端由 API 生成
//! 脱敏视图。把校验与默认 provider 语义集中在 Manager，避免控制面直接
//! 调仓储绕过配置不变量。

#![allow(clippy::doc_markdown)]

use models::{ProviderConfig, ProviderRouteBinding};

use crate::errors::EngineError;
use crate::manager::AuditManager;

impl AuditManager {
    /// 写入 Provider 模型能力元数据。
    ///
    /// # Errors
    /// Provider 不存在、模型名为空或仓储写入失败。
    pub fn upsert_model_capability(
        &self,
        mut capability: models::ModelCapability,
    ) -> Result<models::ModelCapability, EngineError> {
        self.repository()
            .get_provider(capability.provider_id.as_str())?
            .ok_or_else(|| {
                EngineError::ProviderNotFound(capability.provider_id.as_str().to_string())
            })?;
        if capability.model.trim().is_empty() {
            return Err(EngineError::Value(
                "model capability model must not be blank".to_string(),
            ));
        }
        capability.model = capability.model.trim().to_string();
        capability.updated_at = models::utcnow();
        Ok(self.repository().upsert_model_capability(&capability)?)
    }

    /// 列出模型能力元数据，可按 Provider 过滤。
    ///
    /// # Errors
    /// 仓储读取失败。
    pub fn list_model_capabilities(
        &self,
        provider_id: Option<&str>,
    ) -> Result<Vec<models::ModelCapability>, EngineError> {
        Ok(self.repository().list_model_capabilities(provider_id)?)
    }

    /// 创建并校验 Provider 配置。
    ///
    /// # Errors
    /// Provider 字段不满足模型约束，或仓储写入失败。
    pub fn create_provider(&self, provider: ProviderConfig) -> Result<ProviderConfig, EngineError> {
        let provider = provider
            .validated()
            .map_err(|error| EngineError::ProviderConfigError(error.to_string()))?;
        Ok(self.repository().create_provider(&provider)?)
    }

    /// 更新并校验 Provider 配置。
    ///
    /// # Errors
    /// Provider 字段不满足模型约束，或仓储写入失败。
    pub fn update_provider(&self, provider: ProviderConfig) -> Result<ProviderConfig, EngineError> {
        let provider = provider
            .validated()
            .map_err(|error| EngineError::ProviderConfigError(error.to_string()))?;
        Ok(self.repository().update_provider(&provider)?)
    }

    /// 删除 Provider 配置；历史 Run 中已落盘的 provider id 不受影响。
    ///
    /// # Errors
    /// Provider 不存在或仓储写入失败。
    pub fn delete_provider(&self, provider_id: &str) -> Result<(), EngineError> {
        self.repository()
            .get_provider(provider_id)?
            .ok_or_else(|| EngineError::ProviderNotFound(provider_id.to_string()))?;
        self.repository().delete_provider(provider_id)?;
        Ok(())
    }

    /// 列出按 purpose 过滤的 Provider 路由绑定。
    ///
    /// # Errors
    /// 仓储读取失败。
    pub fn list_provider_routes(
        &self,
        purpose: Option<&str>,
    ) -> Result<Vec<ProviderRouteBinding>, EngineError> {
        Ok(self.repository().list_provider_routes(purpose)?)
    }

    /// 校验并新增 Provider 路由绑定。
    ///
    /// # Errors
    /// Provider 不存在、路由约束非法或仓储写入失败。
    pub fn create_provider_route(
        &self,
        route: ProviderRouteBinding,
    ) -> Result<ProviderRouteBinding, EngineError> {
        self.repository()
            .get_provider(route.provider_id.as_str())?
            .ok_or_else(|| EngineError::ProviderNotFound(route.provider_id.as_str().to_string()))?;
        let route = route
            .validated()
            .map_err(|error| EngineError::ProviderConfigError(error.to_string()))?;
        Ok(self.repository().upsert_provider_route(&route)?)
    }

    /// 校验并更新 Provider 路由绑定。
    ///
    /// # Errors
    /// Provider 不存在、路由约束非法或仓储写入失败。
    pub fn update_provider_route(
        &self,
        route: ProviderRouteBinding,
    ) -> Result<ProviderRouteBinding, EngineError> {
        self.create_provider_route(route)
    }

    /// 删除 Provider 路由绑定。
    ///
    /// # Errors
    /// 路由不存在或仓储删除失败。
    pub fn delete_provider_route(&self, route_id: &str) -> Result<(), EngineError> {
        self.repository()
            .get_provider_route(route_id)?
            .ok_or_else(|| EngineError::Value(format!("provider route not found: {route_id}")))?;
        self.repository().delete_provider_route(route_id)?;
        Ok(())
    }
}
