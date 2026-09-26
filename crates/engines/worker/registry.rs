//! Worker Runtime 注册表：探测缓存、Connection 解析与运行时选择。
//!
//! **认证红线**：Agent CLI 子进程只使用本注册表解析出的 Lynceus
//! Connection（复用 [`crate::model_providers::SecretStore`] 体系）；
//! 未绑定有效 Connection 的 runtime 状态是 NotReady（Configuration
//! Required），绝不回退用户本机的 CLI 登录 / OAuth / shell 环境密钥。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use agents::worker::WorkerConnectionResolver;
use agents::worker::WorkerRuntime;
use agents::worker::WorkerRuntimeError;
use agents::worker::WorkerRuntimeErrorKind;
use agents::worker::WorkerRuntimeSelector;
use async_trait::async_trait;
use models::worker::ResolvedWorkerConnection;
use models::worker::WorkerAvailability;
use models::worker::WorkerProbe;
use models::worker::WorkerRun;
use models::worker::WorkerRuntimeProfile;
use models::worker::WorkerRuntimeType;
use storage::Repository;
use tracing;
use tokio::sync::Semaphore;

use crate::model_providers::PlaintextSecretStore;
use crate::model_providers::SecretStore;

use super::adapters::adapter_supports_protocol;
use super::adapters::ClaudeCodeWorker;
use super::adapters::PiWorker;
use super::adapters::CodexWorker;

use super::adapters::DeepSeekHarnessWorker;

/// 探测快照的 TTL（`--version` 探测是真实进程 spawn，不宜每次调用都跑）。
const PROBE_CACHE_TTL: Duration = Duration::from_secs(30);

/// 组合根装配的 worker 注册表：`WorkerRuntimeSelector` +
/// `WorkerConnectionResolver` 的生产实现。
pub struct WorkerRegistry {
    adapters: Vec<Arc<dyn WorkerRuntime>>,
    resolver: Arc<ConnectionResolver>,
    probe_cache: Mutex<Option<(Instant, Vec<WorkerProbe>)>>,
    limiters: Mutex<HashMap<(WorkerRuntimeType, String, u32), Arc<Semaphore>>>,
    /// 无偏好派发时的轮转游标：多个可用 runtime（claude_code/codex/…）
    /// 交替接管任务，保证多 Agent 并行而不是全部压给第一个。
    dispatch_cursor: std::sync::atomic::AtomicUsize,
    /// 中断注入表：`worker_run_id` → 用户消息。派发层在 `cancelled`
    /// 结果上取走（一次性）。
    interrupts: Mutex<HashMap<String, String>>,
}

impl WorkerRegistry {
    /// 以仓储句柄装配全部显式适配器（无注册项则视为编程错误）。
    #[must_use]
    pub fn new(repository: Arc<dyn Repository>) -> Self {
        let resolver = Arc::new(ConnectionResolver {
            repository,
            secret_store: PlaintextSecretStore::default(),
        });
        // 稳定优先级：探测通过的靠前者先被 select 选中。
        let resolver_trait = Arc::clone(&resolver) as Arc<dyn WorkerConnectionResolver>;
        let adapters: Vec<Arc<dyn WorkerRuntime>> = vec![
            Arc::new(ClaudeCodeWorker::new(Arc::clone(&resolver_trait))),
            Arc::new(CodexWorker::new(Arc::clone(&resolver_trait))),
            Arc::new(PiWorker::new(Arc::clone(&resolver_trait))),
            Arc::new(DeepSeekHarnessWorker::new(Arc::clone(&resolver_trait))),

        ];
        Self {
            adapters,
            resolver,
            probe_cache: Mutex::new(None),
            limiters: Mutex::new(HashMap::new()),
            dispatch_cursor: std::sync::atomic::AtomicUsize::new(0),
            interrupts: Mutex::new(HashMap::new()),
        }
    }

    /// 强制重新探测（忽略缓存）。
    pub async fn refresh_probes(&self) -> Vec<WorkerProbe> {
        let probes = self.probe_all_uncached().await;
        if let Ok(mut cache) = self.probe_cache.lock() {
            *cache = Some((Instant::now(), probes.clone()));
        }
        probes
    }

    async fn probe_all_uncached(&self) -> Vec<WorkerProbe> {
        let mut probes = Vec::new();
        for adapter in &self.adapters {
            let probe = adapter.probe().await;
            probes.push(self.merge_connection_readiness(probe).await);
        }
        probes
    }

    /// 把 Connection 就绪状态并入探测结论：二进制可用但未绑定有效
    /// Connection / 协议不兼容时降级为 NotReady / Unsupported。
    async fn merge_connection_readiness(&self, probe: WorkerProbe) -> WorkerProbe {
        if probe.availability != WorkerAvailability::Available {
            return probe;
        }
        match self.ready_connection(probe.runtime).await {
            Ok((_, connection)) => {
                if adapter_supports_protocol(probe.runtime, connection.protocol) {
                    probe
                } else {
                    // 绑定的 Connection 协议与该 worker 不兼容：探测期显式
                    // 呈现 Unsupported，绝不等到派发执行才失败。
                    let mut merged = probe;
                    merged.availability = WorkerAvailability::Unsupported;
                    merged.detail = Some(format!(
                        "bound connection '{}' has protocol '{}' which worker \
                         runtime '{}' does not support; rebind to a compatible \
                         connection",
                        connection.connection_id,
                        connection.protocol.as_str(),
                        merged.runtime.as_str()
                    ));
                    merged
                }
            }
            Err(error) => {
                let mut merged = probe;
                merged.availability = match error.kind {
                    WorkerRuntimeErrorKind::NotReady => WorkerAvailability::NotReady,
                    WorkerRuntimeErrorKind::Unsupported => WorkerAvailability::Unsupported,
                    WorkerRuntimeErrorKind::NotInstalled => WorkerAvailability::NotReady,
                    _ => WorkerAvailability::Error,
                };
                merged.detail = Some(error.message);
                merged
            }
        }
    }

    /// 解析某 runtime 的就绪 Connection（Profile → Connection → 完整性）。
    ///
    /// # Errors
    /// 未绑定 Profile、Connection 缺失或配置不完整（均为显式错误分类）。
    pub async fn ready_connection(
        &self,
        runtime_type: WorkerRuntimeType,
    ) -> Result<(WorkerRuntimeProfile, ResolvedWorkerConnection), WorkerRuntimeError> {
        let Some(profile) = self.resolver.profile(runtime_type).await? else {
            return Err(WorkerRuntimeError::new(
                WorkerRuntimeErrorKind::NotReady,
                format!(
                    "worker runtime '{}' has no bound profile: configuration required \
                     (bind a connection in Worker Runtimes settings)",
                    runtime_type.as_str()
                ),
            ));
        };
        if !profile.enabled {
            return Err(WorkerRuntimeError::new(
                WorkerRuntimeErrorKind::NotReady,
                format!(
                    "worker runtime '{}' profile is disabled: configuration required",
                    runtime_type.as_str()
                ),
            ));
        }
        let mut connection = self
            .resolver
            .validate_connection(profile.connection_id.as_str())
            .await?;
        // LiteLLM Gateway 启用且该 runtime 有别名绑定时，上游改指网关：
        // CLI 侧看到的是网关端口 + 绑定别名（满足 CLI 本地模型名校验），
        // 模型名改写与协议转换由网关完成。未启用/无绑定 → 保持直连。
        if let Some(gateway) = super::gateway::global_gateway()
            && gateway.rewrite_connection(
                runtime_type,
                &mut connection.base_url,
                &mut connection.default_model,
            )
        {
            // 改写生效时 CLI→网关段协议即 CLI 原生协议（探测可用性随之
            // 与上游 provider 协议解耦）。
            if let Some(native) = super::gateway::GatewayManager::native_protocol(runtime_type) {
                connection.protocol = native;
            }
            tracing::debug!(
                runtime = runtime_type.as_str(),
                base_url = connection.base_url.as_deref().unwrap_or(""),
                model = connection.default_model.as_deref().unwrap_or(""),
                "worker connection rewritten through LiteLLM gateway"
            );
        }
        Ok((profile, connection))
    }
}

#[async_trait]
impl WorkerRuntimeSelector for WorkerRegistry {
    /// Profile 绑定的 Agent 预设 key（`runtime_options.agent_preset`，
    /// Worker Runtimes 页配置；WP4 解析优先级第二层）。
    async fn profile_agent_preset_key(&self, runtime_type: WorkerRuntimeType) -> Option<String> {
        let profile = self.resolver.profile(runtime_type).await.ok()??;
        profile
            .runtime_options
            .get("agent_preset")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }

    /// 选择 runtime：显式偏好优先，否则按稳定优先级取第一个探测可用项。
    /// **没有任何可用项时返回显式错误**（区分 not installed / not ready），
    /// 编排层据此 fail-closed，绝不回退内部执行。
    async fn select(
        &self,
        preferred: Option<&str>,
    ) -> Result<Arc<dyn WorkerRuntime>, WorkerRuntimeError> {
        let probes = self.current_probes().await;
        let candidates: Vec<(&WorkerProbe, Arc<dyn WorkerRuntime>)> = probes
            .iter()
            .filter_map(|probe| self.runtime(probe.runtime).map(|adapter| (probe, adapter)))
            .collect();
        let pick = |wanted: WorkerRuntimeType| {
            candidates
                .iter()
                .find(|(probe, _)| probe.runtime == wanted)
                .map(|(probe, adapter)| (*probe, Arc::clone(adapter)))
        };
        if let Some(preferred) = preferred.map(str::trim).filter(|value| !value.is_empty()) {
            let wanted = preferred
                .parse::<PreferredRuntime>()
                .map_err(|_| {
                    WorkerRuntimeError::new(
                        WorkerRuntimeErrorKind::Unsupported,
                        format!("unknown worker runtime type '{preferred}'"),
                    )
                })?
                .0;
            let Some((probe, adapter)) = pick(wanted) else {
                return Err(WorkerRuntimeError::new(
                    WorkerRuntimeErrorKind::NotInstalled,
                    format!("worker runtime '{preferred}' is not registered"),
                ));
            };
            if probe.availability != WorkerAvailability::Available {
                return Err(WorkerRuntimeError::new(
                    WorkerRuntimeErrorKind::Unavailable,
                    format!(
                        "worker runtime '{}' is {}: {}",
                        preferred,
                        probe.availability.as_str(),
                        probe.detail.as_deref().unwrap_or("no detail")
                    ),
                ));
            }
            return self.limited_runtime(wanted, adapter).await;
        }
        // 多 Agent 轮转：收集全部可用 runtime，按游标偏移取本次的执行者
        // （claude_code ↔ codex ↔ … 交替，而不是全部压给第一个）。
        let available: Vec<(&WorkerProbe, Arc<dyn WorkerRuntime>)> = WorkerRuntimeType::all()
            .iter()
            .filter_map(|wanted| pick(*wanted))
            .filter(|(probe, _)| probe.availability == WorkerAvailability::Available)
            .collect();
        if !available.is_empty() {
            let cursor = self
                .dispatch_cursor
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let (probe, adapter) = &available[cursor % available.len()];
            return self.limited_runtime(probe.runtime, Arc::clone(adapter)).await;
        }
        let summary = probes
            .iter()
            .map(|probe| format!("{}={}", probe.runtime.as_str(), probe.availability.as_str()))
            .collect::<Vec<_>>()
            .join(", ");
        Err(WorkerRuntimeError::new(
            WorkerRuntimeErrorKind::NotReady,
            format!("no external worker runtime is available: {summary}"),
        ))
    }

    async fn probes(&self) -> Vec<WorkerProbe> {
        self.current_probes().await
    }

    fn runtime(&self, runtime_type: WorkerRuntimeType) -> Option<Arc<dyn WorkerRuntime>> {
        self.adapters
            .iter()
            .find(|adapter| adapter.runtime_type() == runtime_type)
            .cloned()
    }

    async fn begin_dispatch(
        &self,
        attribution: &agents::worker::DispatchAttribution,
    ) -> Option<String> {
        // 落一条"正在跑"的骨架：runtime 已由 select 定下，归属（project /
        // mission / branch / run / task）由编排层给出。adapter 之后会用同一
        // id upsert 覆盖终态，不会留下重复行。
        let mut record = WorkerRun::new(
            attribution.project_id.clone(),
            attribution.runtime_type,
            attribution.instruction.clone(),
        );
        // MCP grant 已按 preferred_id 发了 scope；复用同一 id 既避免重复行，
        // 也让 broker 的 insert_session 能反查到预设收紧授权。
        if let Some(preferred) = attribution
            .preferred_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            record.id = preferred.to_string();
        }
        record.mission_id = attribution.mission_id.clone();
        record.branch_id = attribution.branch_id.clone();
        record.run_id = Some(attribution.run_id.clone());
        record.task_id = Some(attribution.task_id.clone());
        record.agent_preset_id = attribution.agent_preset_id.clone();
        record.mark_started();
        let id = record.id.clone();
        if let Err(error) = self.resolver.repository.upsert_worker_run(&record) {
            // best-effort：落库失败只警告，绝不因此阻止派发。
            tracing::warn!(
                worker_run_id = %id,
                runtime = attribution.runtime_type.as_str(),
                error = %error,
                "worker dispatch 骨架落库失败；执行继续，前端将延迟获知 harness 归属"
            );
            return None;
        }
        Some(id)
    }

    async fn interrupt_worker(&self, worker_run_id: &str, message: &str) -> bool {
        let Some(runtime) = self.runtime_of_inflight(worker_run_id).await else {
            return false;
        };
        // 消息必须**先于**取消信号落表：取消信号一发，被杀进程的
        // `select!` 立即醒来走完「execute 返回 → 派发层取消息」整条链，
        // 次序反了会把注入消息丢掉（任务卡在 running 无人续跑）。
        if let Ok(mut interrupts) = self.interrupts.lock() {
            interrupts.insert(worker_run_id.to_string(), message.to_string());
        }
        let cancelled = runtime.cancel(worker_run_id).await.unwrap_or(false);
        if !cancelled {
            // 没杀成（inflight 已消失）：回收消息，绝不永留表中。
            if let Ok(mut interrupts) = self.interrupts.lock() {
                interrupts.remove(worker_run_id);
            }
        }
        cancelled
    }

    async fn take_interrupt_message(&self, worker_run_id: &str) -> Option<String> {
        self.interrupts.lock().ok()?.remove(worker_run_id)
    }

    async fn worker_session_ref(&self, worker_run_id: &str) -> Option<String> {
        // 会话引用的单一数据源是 adapter 侧共享 sink（流式解析写入）。
        super::adapters::session_sink().lock().ok()?.get(worker_run_id).cloned()
    }

    async fn inflight_worker_run_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = super::adapters::session_sink()
            .lock()
            .map(|sink| sink.keys().cloned().collect())
            .unwrap_or_default();
        ids.sort();
        ids
    }

    /// 终结清理：会话 sink 与中断注入表一起清，防止簿记泄漏。
    /// 派发层在消费完结终态（成功/失败/超时/取消）后调用。
    async fn forget_worker(&self, worker_run_id: &str) {
        if let Ok(mut sink) = super::adapters::session_sink().lock() {
            sink.remove(worker_run_id);
        }
        if let Ok(mut interrupts) = self.interrupts.lock() {
            interrupts.remove(worker_run_id);
        }
    }
}

impl WorkerRegistry {
    /// 按运行中的 worker_run_id 找它当时的 runtime 适配器。
    ///
    /// 骨架行（`begin_dispatch` 落的）记着 runtime 类型；中断只对
    /// **本进程内** inflight 的 worker 有意义（跨重启进程已死，inflight
    /// 表空，cancel 天然返回 false）。
    async fn runtime_of_inflight(&self, worker_run_id: &str) -> Option<Arc<dyn WorkerRuntime>> {
        let runtime_type = self.inflight_runtime_type(worker_run_id).await?;
        self.runtime(runtime_type)
    }

    /// 运行簿记里没有类型信息时回落骨架行（库里 status=Running 的
    /// WorkerRun 带 runtime 类型）。
    async fn inflight_runtime_type(&self, worker_run_id: &str) -> Option<WorkerRuntimeType> {
        let record = self
            .resolver
            .repository
            .get_worker_run(worker_run_id)
            .ok()
            .flatten()?;
        (record.status == models::worker::WorkerRunStatus::Running).then_some(record.runtime)
    }
}

impl WorkerRegistry {
    async fn limited_runtime(
        &self,
        runtime_type: WorkerRuntimeType,
        adapter: Arc<dyn WorkerRuntime>,
    ) -> Result<Arc<dyn WorkerRuntime>, WorkerRuntimeError> {
        let Some(profile) = self.resolver.profile(runtime_type).await? else {
            return Err(WorkerRuntimeError::new(
                WorkerRuntimeErrorKind::NotReady,
                format!(
                    "worker runtime '{}' has no bound profile: configuration required",
                    runtime_type.as_str()
                ),
            ));
        };
        if !profile.enabled {
            return Err(WorkerRuntimeError::new(
                WorkerRuntimeErrorKind::NotReady,
                "worker profile is disabled: configuration required",
            ));
        }
        let key = (runtime_type, profile.id.clone(), profile.max_concurrency);
        let semaphore = {
            let mut limiters = self.limiters.lock().map_err(|_| {
                WorkerRuntimeError::new(
                    WorkerRuntimeErrorKind::Internal,
                    "worker concurrency limiter is poisoned",
                )
            })?;
            Arc::clone(
                limiters
                    .entry(key)
                    .or_insert_with(|| Arc::new(Semaphore::new(profile.max_concurrency as usize))),
            )
        };
        Ok(Arc::new(LimitedWorkerRuntime {
            inner: adapter,
            semaphore,
            runtime_type,
        }))
    }

    async fn current_probes(&self) -> Vec<WorkerProbe> {
        if let Ok(cache) = self.probe_cache.lock()
            && let Some((at, probes)) = cache.as_ref()
            && at.elapsed() < PROBE_CACHE_TTL
        {
            return probes.clone();
        }
        self.refresh_probes().await
    }
}

struct LimitedWorkerRuntime {
    inner: Arc<dyn WorkerRuntime>,
    semaphore: Arc<Semaphore>,
    runtime_type: WorkerRuntimeType,
}

#[async_trait]
impl WorkerRuntime for LimitedWorkerRuntime {
    fn runtime_type(&self) -> WorkerRuntimeType {
        self.runtime_type
    }

    async fn probe(&self) -> WorkerProbe {
        self.inner.probe().await
    }

    fn capabilities(&self) -> Vec<String> {
        self.inner.capabilities()
    }

    async fn start(
        &self,
        request: agents::worker::WorkerExecutionRequest,
    ) -> Result<agents::worker::WorkerExecutionOutcome, WorkerRuntimeError> {
        let permit = self.semaphore.clone().acquire_owned().await.map_err(|_| {
            WorkerRuntimeError::new(
                WorkerRuntimeErrorKind::Internal,
                "worker concurrency limiter is closed",
            )
        })?;
        let result = self.inner.start(request).await;
        drop(permit);
        result
    }

    async fn resume(
        &self,
        request: agents::worker::WorkerExecutionRequest,
    ) -> Result<agents::worker::WorkerExecutionOutcome, WorkerRuntimeError> {
        let permit = self.semaphore.clone().acquire_owned().await.map_err(|_| {
            WorkerRuntimeError::new(
                WorkerRuntimeErrorKind::Internal,
                "worker concurrency limiter is closed",
            )
        })?;
        let result = self.inner.resume(request).await;
        drop(permit);
        result
    }

    async fn events(
        &self,
        session_ref: &str,
    ) -> Result<Vec<models::worker::WorkerEvent>, WorkerRuntimeError> {
        self.inner.events(session_ref).await
    }

    async fn cancel(&self, session_ref: &str) -> Result<bool, WorkerRuntimeError> {
        self.inner.cancel(session_ref).await
    }
}

/// `PreferredRuntime`：wire 字符串 → [`WorkerRuntimeType`] 的解析包装。
struct PreferredRuntime(WorkerRuntimeType);

impl std::str::FromStr for PreferredRuntime {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        WorkerRuntimeType::all()
            .into_iter()
            .find(|runtime| runtime.as_str() == value)
            .map(Self)
            .ok_or(())
    }
}

/// Connection 解析器：复用现有 Provider 仓储 + SecretStore 体系。
struct ConnectionResolver {
    repository: Arc<dyn Repository>,
    secret_store: PlaintextSecretStore,
}

#[async_trait]
impl WorkerConnectionResolver for ConnectionResolver {
    async fn resolve(
        &self,
        connection_id: &str,
    ) -> Result<ResolvedWorkerConnection, WorkerRuntimeError> {
        let provider = self
            .repository
            .get_provider(connection_id)
            .map_err(|error| {
                WorkerRuntimeError::new(
                    WorkerRuntimeErrorKind::Internal,
                    format!("connection lookup failed: {error}"),
                )
            })?
            .ok_or_else(|| {
                WorkerRuntimeError::new(
                    WorkerRuntimeErrorKind::NotReady,
                    format!("connection '{connection_id}' does not exist: configuration required"),
                )
            })?;
        if !provider.enabled {
            return Err(WorkerRuntimeError::new(
                WorkerRuntimeErrorKind::NotReady,
                format!("connection '{connection_id}' is disabled: configuration required"),
            ));
        }
        // 密钥只在此处短暂解析进内存，随后仅注入子进程环境。
        let api_key = self.secret_store.resolve(&provider);
        Ok(agents::worker::resolve_provider_connection(
            &provider, api_key,
        ))
    }

    async fn profile(
        &self,
        runtime_type: WorkerRuntimeType,
    ) -> Result<Option<WorkerRuntimeProfile>, WorkerRuntimeError> {
        self.repository
            .list_worker_runtime_profiles(Some(runtime_type))
            .map_err(|error| {
                WorkerRuntimeError::new(
                    WorkerRuntimeErrorKind::Internal,
                    format!("worker profile lookup failed: {error}"),
                )
            })
            .map(|profiles| profiles.into_iter().find(|profile| profile.enabled))
    }

    async fn validate_connection(
        &self,
        connection_id: &str,
    ) -> Result<ResolvedWorkerConnection, WorkerRuntimeError> {
        let connection = self.resolve(connection_id).await?;
        if !agents::worker::connection_is_complete(&connection) {
            let missing = [
                (
                    connection
                        .base_url
                        .is_some_and(|url| !url.trim().is_empty()),
                    "base_url",
                ),
                (connection.api_key.is_some(), "api key"),
            ]
            .into_iter()
            .filter(|(present, _)| !present)
            .map(|(_, field)| field)
            .collect::<Vec<_>>()
            .join(", ");
            return Err(WorkerRuntimeError::new(
                WorkerRuntimeErrorKind::NotReady,
                format!(
                    "connection '{connection_id}' is incomplete (missing: {missing}): \
                     configuration required"
                ),
            ));
        }
        Ok(connection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_with_profile(connection_id: Option<&str>) -> (WorkerRegistry, tempfile::TempDir) {
        registry_with_profile_conn(connection_id, models::ProviderType::Anthropic)
    }

    fn registry_with_profile_conn(
        connection_id: Option<&str>,
        protocol: models::ProviderType,
    ) -> (WorkerRegistry, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = Arc::new(
            storage::SqliteRepository::open(dir.path().join("registry.sqlite3"))
                .expect("open repo"),
        );
        if let Some(connection_id) = connection_id {
            let mut provider = models::ProviderConfig::new(
                "claude-connection".to_string(),
                protocol,
            );
            provider.id = models::ProviderId::new(connection_id.to_string());
            provider.base_url = Some("https://api.anthropic.com".to_string());
            provider.model = Some("claude-sonnet-5".to_string());
            provider.encrypted_api_key = Some("sk-ant-test".to_string());
            repo.create_provider(&provider).expect("provider created");
            let profile = models::WorkerRuntimeProfile::new(
                WorkerRuntimeType::ClaudeCode,
                connection_id,
                models::WorkerExecutionEnvironment::Local,
                1,
                900,
            )
            .expect("profile");
            repo.upsert_worker_runtime_profile(&profile)
                .expect("profile saved");
        }
        let registry = WorkerRegistry::new(repo);
        (registry, dir)
    }

    #[tokio::test]
    async fn select_without_profile_fails_with_not_ready() {
        let (registry, _dir) = registry_with_profile(None);
        let error = match registry.select(None).await {
            Err(error) => error,
            Ok(_) => panic!("selection must fail closed without any ready runtime"),
        };
        assert_eq!(error.kind, WorkerRuntimeErrorKind::NotReady);
        assert!(
            error
                .message
                .contains("no external worker runtime is available")
        );
    }

    #[tokio::test]
    async fn probe_reports_unsupported_for_mismatched_protocol() {
        let (registry, _dir) =
            registry_with_profile_conn(Some("conn_openai"), models::ProviderType::OpenaiCompatible);
        let probe = WorkerProbe::new(
            WorkerRuntimeType::ClaudeCode,
            WorkerAvailability::Available,
            Vec::new(),
        );
        let merged = registry.merge_connection_readiness(probe).await;
        assert_eq!(merged.availability, WorkerAvailability::Unsupported);
        let detail = merged.detail.expect("mismatch detail must be recorded");
        assert!(
            detail.contains("does not support"),
            "unexpected detail: {detail}"
        );
    }

    #[tokio::test]
    async fn probe_keeps_available_for_matching_protocol() {
        let (registry, _dir) =
            registry_with_profile_conn(Some("conn_anthropic"), models::ProviderType::Anthropic);
        let probe = WorkerProbe::new(
            WorkerRuntimeType::ClaudeCode,
            WorkerAvailability::Available,
            Vec::new(),
        );
        let merged = registry.merge_connection_readiness(probe).await;
        assert_eq!(merged.availability, WorkerAvailability::Available);
    }

    #[tokio::test]
    async fn unknown_preferred_runtime_is_explicitly_rejected() {
        let (registry, _dir) = registry_with_profile(None);
        let error = match registry.select(Some("definitely_not_a_runtime")).await {
            Err(error) => error,
            Ok(_) => panic!("unknown runtime must be explicitly rejected"),
        };
        assert_eq!(error.kind, WorkerRuntimeErrorKind::Unsupported);
    }

    #[tokio::test]
    async fn selected_runtime_enforces_profile_concurrency() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct FakeRuntime {
            active: Arc<AtomicUsize>,
            peak: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl WorkerRuntime for FakeRuntime {
            fn runtime_type(&self) -> WorkerRuntimeType {
                WorkerRuntimeType::ClaudeCode
            }

            async fn probe(&self) -> WorkerProbe {
                WorkerProbe::new(
                    WorkerRuntimeType::ClaudeCode,
                    WorkerAvailability::Available,
                    Vec::new(),
                )
            }

            fn capabilities(&self) -> Vec<String> {
                Vec::new()
            }

            async fn start(
                &self,
                _request: agents::worker::WorkerExecutionRequest,
            ) -> Result<agents::worker::WorkerExecutionOutcome, WorkerRuntimeError> {
                let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
                let mut peak = self.peak.load(Ordering::SeqCst);
                while active > peak {
                    match self.peak.compare_exchange(
                        peak,
                        active,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    ) {
                        Ok(_) => break,
                        Err(observed) => peak = observed,
                    }
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
                self.active.fetch_sub(1, Ordering::SeqCst);
                Ok(agents::worker::WorkerExecutionOutcome {
                    run: models::WorkerRun::new(
                        models::ProjectId::new("p".to_string()),
                        WorkerRuntimeType::ClaudeCode,
                        "fake",
                    ),
                    invocation: models::WorkerInvocation::new(
                        WorkerRuntimeType::ClaudeCode,
                        models::WorkerInvocationPurpose::Start,
                        None,
                    ),
                    transcript: Vec::new(),
                })
            }

            async fn resume(
                &self,
                request: agents::worker::WorkerExecutionRequest,
            ) -> Result<agents::worker::WorkerExecutionOutcome, WorkerRuntimeError> {
                self.start(request).await
            }

            async fn events(
                &self,
                _session_ref: &str,
            ) -> Result<Vec<models::WorkerEvent>, WorkerRuntimeError> {
                Ok(Vec::new())
            }

            async fn cancel(&self, _session_ref: &str) -> Result<bool, WorkerRuntimeError> {
                Ok(true)
            }
        }

        let (registry, _dir) = registry_with_profile(Some("conn"));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let runtime = registry
            .limited_runtime(
                WorkerRuntimeType::ClaudeCode,
                Arc::new(FakeRuntime {
                    active: Arc::clone(&active),
                    peak: Arc::clone(&peak),
                }),
            )
            .await
            .expect("profile limiter");
        let mut handles = Vec::new();
        for _ in 0..6 {
            let runtime = Arc::clone(&runtime);
            handles.push(tokio::spawn(async move {
                runtime
                    .start(agents::worker::WorkerExecutionRequest::start("fake", 1))
                    .await
                    .expect("fake runtime");
            }));
        }
        for handle in handles {
            handle.await.expect("worker join");
        }
        assert_eq!(peak.load(Ordering::SeqCst), 1);
    }

#[tokio::test]
async fn profile_agent_preset_binding_is_readable_for_dispatch() {
    // WP4 收尾：runtime_options.agent_preset 绑定是解析优先级第二层的数据源。
    let (registry, _dir) = registry_with_profile(Some("claude-connection"));
    assert_eq!(
        registry
            .profile_agent_preset_key(WorkerRuntimeType::ClaudeCode)
            .await,
        None,
        "未绑定预设时不得上报 key"
    );

    let profiles = registry
        .resolver
        .profile(WorkerRuntimeType::ClaudeCode)
        .await
        .expect("profile lookup");
    let mut profile = profiles.expect("seeded profile");
    profile.runtime_options.insert(
        "agent_preset".to_string(),
        serde_json::Value::String("worker_instruction".to_string()),
    );
    registry
        .resolver
        .repository
        .upsert_worker_runtime_profile(&profile)
        .expect("upsert");

    assert_eq!(
        registry
            .profile_agent_preset_key(WorkerRuntimeType::ClaudeCode)
            .await
            .as_deref(),
        Some("worker_instruction"),
        "绑定的预设 key 必须对 dispatch 可见"
    );
}

#[tokio::test]
async fn begin_dispatch_persists_running_skeleton_with_full_attribution() {
    let (registry, _dir) = registry_with_profile(Some("claude-connection"));
    let attribution = agents::worker::DispatchAttribution {
        project_id: models::ProjectId::new("proj_dispatch".to_string()),
        mission_id: Some(models::MissionId::new("mission_dispatch".to_string())),
        branch_id: Some(models::BranchId::new("branch_dispatch".to_string())),
        run_id: models::RunId::new("run_dispatch".to_string()),
        task_id: models::TaskId::new("task_dispatch".to_string()),
        instruction: "recon the target".to_string(),
        runtime_type: WorkerRuntimeType::ClaudeCode,
        preferred_id: None,
        agent_preset_id: None,
    };

    let worker_run_id = registry
        .begin_dispatch(&attribution)
        .await
        .expect("骨架必须落库并返回 id");

    let stored = registry
        .resolver
        .repository
        .get_worker_run(&worker_run_id)
        .expect("lookup")
        .expect("skeleton must exist before the worker even starts");
    assert_eq!(
        stored.status,
        models::worker::WorkerRunStatus::Running,
        "骨架必须是 running——它记录的是'正在跑'，不是终态"
    );
    assert_eq!(stored.runtime, WorkerRuntimeType::ClaudeCode);
    assert_eq!(stored.task_id.as_ref().map(|id| id.as_str()), Some("task_dispatch"));
    assert_eq!(stored.run_id.as_ref().map(|id| id.as_str()), Some("run_dispatch"));
    assert_eq!(
        stored.mission_id.as_ref().map(|id| id.as_str()),
        Some("mission_dispatch")
    );
    assert_eq!(
        stored.branch_id.as_ref().map(|id| id.as_str()),
        Some("branch_dispatch")
    );
    assert_eq!(stored.project_id.as_str(), "proj_dispatch");
    assert!(
        stored.started_at.is_some(),
        "骨架要带上 started_at，前端才知道这是活跃派发"
    );
}

#[tokio::test]
async fn begin_dispatch_reuses_mcp_grant_id_and_carries_preset() {
    // MCP grant 按 `worker-run-{task_id}` 发了 scope，broker 的
    // insert_session 要拿这个 id 反查 agent_preset_id 收紧授权。骨架必须
    // 复用同一 id（否则同一 task 出现两条 worker run），并把 preset_id 带上
    // ——此前该字段只在跑完后才落库，收紧路径一直空转。
    let (registry, _dir) = registry_with_profile(Some("claude-connection"));
    let attribution = agents::worker::DispatchAttribution {
        project_id: models::ProjectId::new("proj_dispatch".to_string()),
        mission_id: None,
        branch_id: None,
        run_id: models::RunId::new("run_dispatch".to_string()),
        task_id: models::TaskId::new("task_dispatch".to_string()),
        instruction: "recon the target".to_string(),
        runtime_type: WorkerRuntimeType::ClaudeCode,
        preferred_id: Some("worker-run-task_dispatch".to_string()),
        agent_preset_id: Some("preset_worker".to_string()),
    };

    let worker_run_id = registry.begin_dispatch(&attribution).await.expect("skeleton");
    assert_eq!(
        worker_run_id, "worker-run-task_dispatch",
        "必须复用 MCP grant 的 id，否则同一 task 会出现两条 worker run"
    );

    let stored = registry
        .resolver
        .repository
        .get_worker_run(&worker_run_id)
        .expect("lookup")
        .expect("skeleton");
    assert_eq!(
        stored.agent_preset_id.as_deref(),
        Some("preset_worker"),
        "preset 必须在派发时就可见，MCP 连接时的授权收紧才有依据"
    );
}

#[tokio::test]
async fn begin_dispatch_is_idempotent_per_preallocated_id() {
    // dispatch 预分配 id → adapter 用同一 id upsert 覆盖终态。同一 id 落两次
    // 必须只剩一行（终态覆盖骨架），不产生重复记录。
    let (registry, _dir) = registry_with_profile(Some("claude-connection"));
    let attribution = agents::worker::DispatchAttribution {
        project_id: models::ProjectId::new("proj_dispatch".to_string()),
        mission_id: None,
        branch_id: None,
        run_id: models::RunId::new("run_dispatch".to_string()),
        task_id: models::TaskId::new("task_dispatch".to_string()),
        instruction: "recon the target".to_string(),
        runtime_type: WorkerRuntimeType::ClaudeCode,
        preferred_id: None,
        agent_preset_id: None,
    };
    let worker_run_id = registry.begin_dispatch(&attribution).await.expect("first");

    // 复用同一 id 覆盖（模拟 adapter 的 upsert）。
    let mut record = models::worker::WorkerRun::new(
        attribution.project_id.clone(),
        WorkerRuntimeType::Codex,
        attribution.instruction.clone(),
    );
    record.id = worker_run_id.clone();
    record.task_id = Some(attribution.task_id.clone());
    record.run_id = Some(attribution.run_id.clone());
    record.mark_started();
    record.finish(models::worker::WorkerRunStatus::Succeeded, Some("done".to_string()));
    registry
        .resolver
        .repository
        .upsert_worker_run(&record)
        .expect("final upsert");

    let rows = registry
        .resolver
        .repository
        .list_worker_runs(Some("proj_dispatch"), None, None, 64)
        .expect("list");
    assert_eq!(
        rows.len(),
        1,
        "同一 id 的骨架+终态必须合并成一行；重复行会让前端看到两个 harness"
    );
    assert_eq!(rows[0].runtime, WorkerRuntimeType::Codex, "终态覆盖骨架");
    assert_eq!(rows[0].status, models::worker::WorkerRunStatus::Succeeded);
}
}
