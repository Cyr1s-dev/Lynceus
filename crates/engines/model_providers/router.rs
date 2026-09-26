//! 目的路由 provider 网关 —— Python `ProviderRouterRuntime` 的移植。
//!
//! 按 purpose 取路由（priority/weight 降序由仓储保证），逐条 fallback；
//! 连续失败达到 `max_failures` 的路由熔断 `cooldown_seconds` 秒（熔断期
//! 内的候选在查询阶段就被过滤）。路由选择留在 engine/app 层，core 只
//! 持久化路由记录——依赖边界与 Python 一致。

use std::sync::Arc;

use agents::llm::LlmResponse;
use agents::llm::ProviderCallError;
use agents::llm::ProviderModelDiscoveryRuntime;
use agents::llm::ProviderRuntime;
use agents::llm::StructuredGenerationRequest;
use agents::llm::TextGenerationRequest;
use models::common::utcnow;
use models::ids::ProjectId;
use models::ids::RunId;
use models::ids::TaskId;
use models::provider::ProviderConfig;
use models::provider::ProviderHealthResult;
use models::provider::ProviderModelDiscoveryResult;
use models::provider::ProviderRouteBinding;
use serde_json::Map;
use serde_json::Value;
use storage::Repository;

use super::error::GatewayError;
use super::redact::redact_secrets;
use super::runtime::OpenAiCompatibleProviderRuntime;

/// 带熔断的目的路由网关（Python `ProviderRouterRuntime`）。
pub struct ProviderRouterRuntime {
    inner: Arc<OpenAiCompatibleProviderRuntime>,
}

impl ProviderRouterRuntime {
    /// 包装具体网关实现。
    #[must_use]
    pub fn new(inner: Arc<OpenAiCompatibleProviderRuntime>) -> Self {
        Self { inner }
    }

    /// 被包装的网关实现。
    #[must_use]
    pub fn inner(&self) -> &Arc<OpenAiCompatibleProviderRuntime> {
        &self.inner
    }

    /// 按 purpose 生成文本（Python `generate_text_for_purpose`）：无可用
    /// 路由时回退默认 provider；逐条尝试路由，成功即返回并复位失败计数，
    /// 全部失败则汇总错误。
    ///
    /// # Errors
    /// 无路由且无默认 provider；全部路由失败（错误消息含各路由失败原因，
    /// 已脱敏）。
    pub async fn generate_text_for_purpose(
        &self,
        purpose: &str,
        messages: &[agents::llm::LlmMessage],
        project_id: Option<&ProjectId>,
        run_id: Option<&RunId>,
        task_id: Option<&TaskId>,
    ) -> Result<LlmResponse, GatewayError> {
        let repository = self.inner.repository();
        let routes: Vec<ProviderRouteBinding> = repository
            .list_provider_routes(Some(purpose))
            .map_err(GatewayError::from)?
            .into_iter()
            .filter(|route| route.enabled && !route_circuit_open(route))
            .collect();

        if routes.is_empty() {
            let default_provider = self.inner.resolve_default_provider()?;
            let Some(default_provider) = default_provider else {
                return Err(GatewayError::config(&format!(
                    "no enabled provider route or default provider for purpose '{purpose}'"
                )));
            };
            return self
                .inner
                .generate_text(TextGenerationRequest {
                    provider_id: default_provider.id.as_str(),
                    messages,
                    purpose,
                    project_id,
                    run_id,
                    task_id,
                    model_override: None,
                })
                .await;
        }

        let mut errors: Vec<String> = Vec::new();
        for route in routes {
            let provider = repository
                .get_provider(route.provider_id.as_str())
                .map_err(GatewayError::from)?;
            let provider = match provider {
                Some(provider) if provider.enabled => provider,
                _ => {
                    errors.push(format!("{}: provider unavailable", route.id.as_str()));
                    continue;
                }
            };
            let attempt = self
                .inner
                .generate_text(TextGenerationRequest {
                    provider_id: provider.id.as_str(),
                    messages,
                    purpose,
                    project_id,
                    run_id,
                    task_id,
                    model_override: route.model_override.as_deref(),
                })
                .await;
            match attempt {
                Ok(response) => {
                    if route.failure_count != 0 {
                        let mut reset = route;
                        reset.failure_count = 0;
                        reset.circuit_open_until = None;
                        reset.updated_at = utcnow();
                        repository
                            .upsert_provider_route(&reset)
                            .map_err(GatewayError::from)?;
                    }
                    return Ok(response);
                }
                Err(error) => {
                    errors.push(format!(
                        "{}: {}",
                        route.id.as_str(),
                        redact_secrets(&error.python_error_string())
                    ));
                    record_route_failure(repository, route)?;
                }
            }
        }

        Err(GatewayError::config(&format!(
            "all provider routes failed for purpose '{purpose}': {}",
            errors.join("; ")
        )))
    }
}

/// 熔断判定（Python `_route_circuit_open`）。
fn route_circuit_open(route: &ProviderRouteBinding) -> bool {
    route
        .circuit_open_until
        .as_ref()
        .is_some_and(|until| *until > utcnow())
}

/// 记录路由失败并按需熔断（Python `_record_route_failure`）。
fn record_route_failure(
    repository: &Arc<dyn Repository>,
    mut route: ProviderRouteBinding,
) -> Result<(), GatewayError> {
    route.failure_count += 1;
    route.updated_at = utcnow();
    if route.failure_count >= route.max_failures {
        route.circuit_open_until =
            Some(utcnow() + chrono::Duration::seconds(route.cooldown_seconds));
    }
    repository
        .upsert_provider_route(&route)
        .map_err(GatewayError::from)?;
    Ok(())
}

#[async_trait::async_trait]
impl ProviderRuntime for ProviderRouterRuntime {
    async fn list_providers(&self) -> Result<Vec<ProviderConfig>, ProviderCallError> {
        self.inner.list_providers().map_err(ProviderCallError::from)
    }

    async fn get_provider(
        &self,
        provider_id: &str,
    ) -> Result<Option<ProviderConfig>, ProviderCallError> {
        self.inner
            .get_provider(provider_id)
            .map_err(ProviderCallError::from)
    }

    async fn resolve_default_provider(&self) -> Result<Option<ProviderConfig>, ProviderCallError> {
        self.inner
            .resolve_default_provider()
            .map_err(ProviderCallError::from)
    }

    async fn health_check(
        &self,
        provider_id: &str,
    ) -> Result<ProviderHealthResult, ProviderCallError> {
        self.inner
            .health_check(provider_id)
            .await
            .map_err(ProviderCallError::from)
    }

    async fn generate_text(
        &self,
        request: TextGenerationRequest<'_>,
    ) -> Result<LlmResponse, ProviderCallError> {
        // 空 provider_id = 未钉定：按用途走路由表（无路由回落默认
        // provider）；非空 = 显式钉定，直连指定 provider。钉定请求的
        // `model_override` 照常透传；路由路径的模型覆盖由路由绑定提供。
        if request.provider_id.is_empty() {
            return self
                .generate_text_for_purpose(
                    request.purpose,
                    request.messages,
                    request.project_id,
                    request.run_id,
                    request.task_id,
                )
                .await
                .map_err(ProviderCallError::from);
        }
        self.inner
            .generate_text(request)
            .await
            .map_err(ProviderCallError::from)
    }

    async fn generate_structured(
        &self,
        request: StructuredGenerationRequest<'_>,
    ) -> Result<Map<String, Value>, ProviderCallError> {
        // 未钉定：先按用途路由文本生成，再走与 inner 相同的结构化解析与
        // 截断守卫（`parse_structured_response` + finish_reason 判定），
        // 路由的 model_override 由文本路径应用。
        if request.provider_id.is_empty() {
            let response = self
                .generate_text_for_purpose(
                    request.purpose,
                    request.messages,
                    request.project_id,
                    request.run_id,
                    request.task_id,
                )
                .await?;
            let parsed = super::payload::parse_structured_response(&response.text);
            if parsed.is_err()
                && response
                    .finish_reason
                    .as_deref()
                    .is_some_and(super::payload::finish_reason_is_truncated)
            {
                return Err(GatewayError::Truncated.into());
            }
            return parsed.map_err(ProviderCallError::from);
        }
        self.inner
            .generate_structured(request)
            .await
            .map_err(ProviderCallError::from)
    }
}

#[async_trait::async_trait]
impl ProviderModelDiscoveryRuntime for ProviderRouterRuntime {
    async fn discover_models(
        &self,
        provider: &ProviderConfig,
    ) -> Result<ProviderModelDiscoveryResult, ProviderCallError> {
        self.inner
            .discover_models(provider)
            .await
            .map_err(ProviderCallError::from)
    }
}
