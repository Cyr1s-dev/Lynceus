//! LiteLLM Gateway sidecar —— worker 适配器的统一模型网关。
//!
//! 从 `data/config/gateway.yaml` 读取声明（模型别名 → 上游
//! provider/model、runtime → 别名绑定），生成 LiteLLM proxy config 并作为
//! 受管子进程运行（spawn → 健康检查 → 树杀清理）。启用后，worker 适配器
//! 的上游改指本网关：CLI 侧永远看到自己认识的协议与模型名
//! （claude-* / /v1/responses），模型名改写与协议转换全部交给 LiteLLM。
//!
//! 红线：
//! - **密钥绝不写入落盘 config** —— yaml 声明 `api_key_env`（进程环境变量
//!   名），生成 config 以 `os.environ/<ENV>` 引用，spawn 时注入真实值；
//! - Gateway 未启用/未运行时行为与直连完全一致（不可用即显式失败，
//!   绝不回退内部执行）；
//! - 仅监听 `127.0.0.1`，不暴露公网。

use std::collections::BTreeMap;
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use crate::model_providers::SecretStore;
use models::provider::ProviderType;
use models::worker::WorkerRuntimeType;
use serde::Deserialize;

/// Gateway 声明文件路径（环境变量 `LYNCEUS_GATEWAY_CONFIG` 可覆盖）。
#[must_use]
pub fn gateway_config_path() -> PathBuf {
    std::env::var_os("LYNCEUS_GATEWAY_CONFIG").map_or_else(
        || PathBuf::from("data/config/gateway.yaml"),
        PathBuf::from,
    )
}

/// 运行期产物目录（生成的 litellm config 与日志）。
#[must_use]
pub fn gateway_runtime_dir() -> PathBuf {
    std::env::var_os("LYNCEUS_GATEWAY_RUNTIME_DIR").map_or_else(
        || PathBuf::from("data/gateway"),
        PathBuf::from,
    )
}

fn default_port() -> u16 {
    4141
}

/// 一个模型别名：CLI 看到的 `name` → 上游 `provider_type` 协议 + `model`。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GatewayModel {
    /// CLI 侧使用的模型名（如 `claude-sonnet-5` 满足 Claude Code 本地校验）。
    pub name: String,
    /// 上游协议 → LiteLLM 路由前缀（anthropic → `anthropic/<model>`，
    /// openai/openai_compatible → `openai/<model>`）。
    pub provider_type: ProviderType,
    /// 上游 base URL。`anthropic` 协议**不要**带 `/v1`（LiteLLM 自拼
    /// `/v1/messages`，带 `/v1` 会双写为 `/v1/v1/...` 被网关 403）。
    pub upstream_base: String,
    /// 上游真实模型名。
    pub model: String,
    /// 密钥所在的进程环境变量名（缺省派生 `LYNCEUS_GW_KEY_<n>`）。
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// 或：Lynceus Provider id —— 从仓储 SecretStore 解析（不落盘明文）。
    #[serde(default)]
    pub api_key_provider: Option<String>,
}

impl GatewayModel {
    /// spawn 环境变量名（显式 env 优先，否则确定性派生）。
    #[must_use]
    pub fn key_env_name(&self, index: usize) -> String {
        self.api_key_env
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| format!("LYNCEUS_GW_KEY_{index}"))
    }
}

/// Gateway 声明（`gateway.yaml`）。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct GatewaySettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_port")]
    pub port: u16,
    /// 覆盖启动命令；缺省依次探测 PATH 上的 `litellm` 与
    /// `uvx --from 'litellm[proxy]' litellm`。
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub models: Vec<GatewayModel>,
    /// runtime wire 名（`claude_code`/`codex`/`pi`/`deepseek_harness`）→
    /// 模型绑定。`provider` 指向 Lynceus Settings 里已配置的任意 Provider
    /// （上游地址/模型/密钥全部从该 Provider 记录与 SecretStore 动态派生，
    /// 中转站/网关数量不受代码限制）；省略 `provider` 时回退到 `models`
    /// 手写段中同名别名。
    #[serde(default)]
    pub agents: BTreeMap<String, GatewayBinding>,
}

/// runtime → 模型别名绑定（可引用任意已配置 Provider，零硬编码）。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct GatewayBinding {
    /// CLI 侧看到的模型别名（如 `claude-sonnet-5`）。
    pub alias: String,
    /// Lynceus Provider id（上游与密钥的唯一定义来源）。
    #[serde(default)]
    pub provider: Option<String>,
}

impl GatewaySettings {
    /// 从路径加载；文件不存在 → `Ok(None)`（Gateway 关闭，行为与直连一致）。
    ///
    /// # Errors
    /// 文件存在但 YAML 解析失败。
    pub fn load(path: &std::path::Path) -> Result<Option<Self>, String> {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
        };
        let settings: Self = serde_yaml::from_str(&raw)
            .map_err(|error| format!("invalid gateway config {}: {error}", path.display()))?;
        Ok(Some(settings))
    }

    /// 某 runtime 的模型别名绑定（必须指向已声明/已合成的模型）。
    #[must_use]
    pub fn binding_for(&self, runtime: WorkerRuntimeType) -> Option<&GatewayModel> {
        let binding = self.agents.get(runtime.as_str())?;
        self.models.iter().find(|model| model.name == binding.alias)
    }
}

/// 合成有效模型列表：`agents.*.provider` 引用 Lynceus Provider 时，从
/// Provider 记录动态生成 `GatewayModel`（协议/上游/模型/密钥来源全部
/// 派生，不写死任何中转站）；`models` 手写段作为特例保留。
///
/// # Errors
/// 绑定的 provider 不存在/未启用，或缺模型定义。
pub fn resolve_models(
    settings: &GatewaySettings,
    repository: &std::sync::Arc<dyn storage::Repository>,
) -> Result<GatewaySettings, String> {
    let mut models = settings.models.clone();
    for (runtime, binding) in &settings.agents {
        let Some(provider_id) = binding.provider.as_deref() else {
            continue;
        };
        let provider = repository
            .get_provider(provider_id)
            .map_err(|error| format!("gateway binding for {runtime}: provider lookup failed: {error}"))?
            .ok_or_else(|| format!("gateway binding for {runtime}: unknown provider '{provider_id}'"))?;
        if !provider.enabled {
            return Err(format!("gateway binding for {runtime}: provider '{provider_id}' is disabled"));
        }
        let base_url = provider
            .base_url
            .as_deref()
            .filter(|raw| !raw.trim().is_empty())
            .ok_or_else(|| format!("gateway binding for {runtime}: provider '{provider_id}' has no base_url"))?;
        let model = provider
            .model
            .as_deref()
            .filter(|raw| !raw.trim().is_empty())
            .ok_or_else(|| format!("gateway binding for {runtime}: provider '{provider_id}' has no model"))?;
        // anthropic 路由前缀的 LiteLLM 客户端自拼 /v1/messages：上游
        // base_url 必须剥掉 /v1，否则 /v1/v1/messages 被网关 403（实测）。
        let upstream_base = if provider.provider_type == ProviderType::Anthropic {
            base_url
                .trim_end_matches('/')
                .strip_suffix("/v1")
                .unwrap_or(base_url.trim_end_matches('/'))
                .to_string()
        } else {
            base_url.trim_end_matches('/').to_string()
        };
        models.push(GatewayModel {
            name: binding.alias.clone(),
            provider_type: provider.provider_type,
            upstream_base,
            model: model.to_string(),
            api_key_env: None,
            api_key_provider: Some(provider_id.to_string()),
        });
    }
    let mut resolved = settings.clone();
    resolved.models = models;
    Ok(resolved)
}

/// 生成 LiteLLM proxy config（YAML 文本）。
///
/// 密钥以 `os.environ/<ENV>` 引用；`anthropic` 协议路由前缀
/// `anthropic/<model>`，OpenAI 系为 `openai/<model>`。
#[must_use]
pub fn render_litellm_config(settings: &GatewaySettings) -> String {
    let mut entries = Vec::new();
    for (index, model) in settings.models.iter().enumerate() {
        let prefix = match model.provider_type {
            ProviderType::Anthropic => "anthropic",
            ProviderType::Openai | ProviderType::OpenaiCompatible => "openai",
            _ => "openai",
        };
        // openai 系一律渲染 `openai/<model>`：LiteLLM 的 openai provider
        // 会自动把 `/v1/responses` 桥接为 chat/completions（实测直连
        // StepFun 上游即 200）。曾用 `openai/chat_completions/<model>`
        // 强制桥接，但 `chat_completions/` 不是合法 provider 前缀，会被
        // 当成上游模型名传出，中转站回 404 `model does not exist`。
        let routed_model = format!("{prefix}/{}", model.model);
        entries.push(format!(
            "  - model_name: {}\n    litellm_params:\n      model: {}\n      api_base: {}\n      api_key: os.environ/{}",
            model.name, routed_model, model.upstream_base, model.key_env_name(index)
        ));
    }
    format!(
        "# Generated by Lynceus (do not edit by hand) (source: data/config/gateway.yaml).\n\
         model_list:\n{}\n",
        entries.join("\n")
    )
}

/// 运行中的 Gateway sidecar。
struct GatewayRuntime {
    settings: GatewaySettings,
    port: u16,
    /// `Some` = 本进程派生的子进程；`None` = **收养的外部网关**（用户自己
    /// 拉起的 LiteLLM：`uv tool uvx` / docker / 系统服务）。区别只在
    /// [`GatewayManager::stop`]：不是自己派生的进程绝不 kill。
    pid: Option<u32>,
    config_path: PathBuf,
    started_at: String,
}

/// agents 绑定视图（API 序列化；`alias@provider` 或纯别名）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct GatewayBindingView {
    pub alias: String,
    pub provider: Option<String>,
}

/// Gateway 状态视图（API 序列化）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct GatewayStatus {
    pub enabled: bool,
    pub running: bool,
    pub port: Option<u16>,
    pub pid: Option<u32>,
    pub models: Vec<String>,
    pub agents: BTreeMap<String, GatewayBindingView>,
    pub config_path: Option<String>,
    pub started_at: Option<String>,
    pub last_error: Option<String>,
}

/// 清掉"自己派生的 sidecar 已死"的槽位；返回是否真的清了。
///
/// 收养的（`pid: None`）条目不受影响——外部网关的存活由探针负责。
fn reap_dead_spawn_slot(slot: &mut Option<GatewayRuntime>) -> bool {
    let dead = slot
        .as_ref()
        .is_some_and(|runtime| runtime.pid.is_some_and(|pid| !is_alive(pid)));
    if dead {
        *slot = None;
    }
    dead
}

/// 受管 LiteLLM sidecar（进程级单例，见 [`global_gateway`]）。
#[derive(Default)]
pub struct GatewayManager {
    runtime: Mutex<Option<GatewayRuntime>>,
    last_error: Mutex<Option<String>>,
    /// 模型解析用的仓储句柄（可选）。收养外部网关时要靠它把 `agents`
    /// 绑定展开成 `models`——否则 Gateway 页面上的模型列表是空的。
    /// 测试里不给（`adopt_settings` 直接收设置，不依赖它）。
    repository: Mutex<Option<std::sync::Arc<dyn storage::Repository>>>,
}

impl GatewayManager {
    /// 读取声明文件（不存在 → `enabled=false` 的空状态）。
    ///
    /// # Errors
    /// 声明文件存在但解析失败。
    pub fn load_settings() -> Result<GatewaySettings, String> {
        Ok(GatewaySettings::load(&gateway_config_path())?
            .unwrap_or_default())
    }

    /// 注入模型解析用的仓储（组合根调用；缺失时收养仍可工作，只是
    /// Gateway 页面上的模型列表为空）。
    pub fn set_repository(&self, repository: std::sync::Arc<dyn storage::Repository>) {
        *self.repository.lock().expect("gateway lock") = Some(repository);
    }

    /// 取一份仓储克隆（供 `resolve_models` 展开 `agents` 绑定）。
    fn repository(&self) -> Option<std::sync::Arc<dyn storage::Repository>> {
        self.repository.lock().expect("gateway lock").clone()
    }

    /// 启动 sidecar（生成 config → spawn → 阻塞等待 TCP 就绪，60s 超时）。
    /// 已在运行时幂等返回当前状态。
    ///
    /// # Errors
    /// 声明缺失/未启用、命令探测失败、config 写盘失败或启动超时。
    /// 环境变量密钥路径（无仓储时；`api_key_provider` 声明会被拒绝）。
    pub async fn start(&self) -> Result<GatewayStatus, String> {
        self.start_inner(None).await
    }

    /// 生产路径：带仓储，支持 `api_key_provider` 从 SecretStore 解析。
    ///
    /// # Errors
    /// 同 [`GatewayManager::start`]。
    pub async fn start_with_repository(
        &self,
        repository: std::sync::Arc<dyn storage::Repository>,
    ) -> Result<GatewayStatus, String> {
        self.start_inner(Some(repository)).await
    }

    async fn start_inner(
        &self,
        repository: Option<std::sync::Arc<dyn storage::Repository>>,
    ) -> Result<GatewayStatus, String> {
        let repository = repository;
        if self.status().running {
            return Ok(self.status());
        }
        let settings = Self::load_settings()?;
        if !settings.enabled {
            return Err("gateway is not enabled: write data/config/gateway.yaml \
                        with `enabled: true` first"
                .to_string());
        }
        let settings = match repository.as_ref() {
            Some(repository) => resolve_models(&settings, repository)?,
            None => settings,
        };
        if settings.models.is_empty() {
            return Err("gateway config declares no models".to_string());
        }
        let port = self.pick_port(settings.port);
        let runtime_dir = gateway_runtime_dir();
        std::fs::create_dir_all(&runtime_dir)
            .map_err(|error| format!("cannot create {}: {error}", runtime_dir.display()))?;
        let config_path = runtime_dir.join("litellm.yaml");
        std::fs::write(&config_path, render_litellm_config(&settings))
            .map_err(|error| format!("cannot write {}: {error}", config_path.display()))?;

        let program = self.resolve_command(&settings)?;
        // 密钥解析：进程环境变量优先；`api_key_provider` 从 Lynceus 仓储
        // SecretStore 解析。缺失即显式失败（绝不让子进程起来后 401）。
        let mut key_envs: Vec<(String, String)> = Vec::new();
        for (index, model) in settings.models.iter().enumerate() {
            let env_name = model.key_env_name(index);
            let value = std::env::var(&env_name)
                .ok()
                .filter(|value| !value.is_empty())
                .or_else(|| {
                    model.api_key_provider.as_deref().and_then(|provider_id| {
                        resolve_provider_key(repository.as_ref(), provider_id)
                    })
                });
            let Some(value) = value else {
                return Err(format!(
                    "gateway api key for model '{}' is not resolvable (set env '{}'                      or point api_key_provider at a Lynceus provider)",
                    model.name, env_name
                ));
            };
            key_envs.push((env_name, value));
        }

        let log_path = runtime_dir.join("litellm.log");
        let child = spawn_sidecar(&program, &config_path, port, &log_path, &settings, &key_envs)?;
        let pid = child.id();
        // 就绪等待放到 blocking 线程，避免占死 async 执行器。
        let wait = tokio::task::spawn_blocking(move || wait_for_port(port, Duration::from_secs(60)));
        match wait.await.map_err(|error| format!("join failed: {error}"))? {
            Ok(()) => {
                let runtime = GatewayRuntime {
                    settings,
                    port,
                    pid: Some(pid),
                    config_path: config_path.clone(),
                    started_at: utc_now_rfc3339(),
                };
                *self.runtime.lock().expect("gateway lock") = Some(runtime);
                *self.last_error.lock().expect("gateway lock") = None;
                Ok(self.status())
            }
            Err(error) => {
                // 就绪超时 ≠ 进程没起来：uvx 冷启动要现下载 litellm 包，
                // 子进程可能在等待窗口结束后才绑上端口。放任它变成孤儿，
                // 下次 start 再 spawn 一个就会端口冲突互杀（实测 3740
                // 这样死掉，管理器还卡在 "gateway process exited"）。
                // 超时即树杀，干净失败；重试时包已缓存，秒起。
                kill_tree(pid);
                let message = format!("gateway did not become ready on port {port}: {error} \
                                       (log: {})", log_path.display());
                *self.last_error.lock().expect("gateway lock") = Some(message.clone());
                Err(message)
            }
        }
    }

    /// 停止 sidecar（Windows `taskkill /F /T`，POSIX SIGTERM→SIGKILL）。
    ///
    /// **收养的外部网关只解除收养，绝不 kill**——那不是本进程派生的，
    /// 杀掉它会破坏用户自己拉起的服务（uv tool / docker / 系统服务）。
    pub fn stop(&self) -> GatewayStatus {
        let runtime = self.runtime.lock().expect("gateway lock").take();
        if let Some(runtime) = runtime
            && let Some(pid) = runtime.pid
        {
            kill_tree(pid);
        }
        self.status()
    }

    /// 收养一个已在 `settings.port` 上健康运行的外部网关。
    ///
    /// **为什么需要**：此前 `rewrite_connection` 只认自己 spawn 的子进程，
    /// 用户手工启动的 LiteLLM（`uv tool uvx …`）对 Lynceus 完全不可见——
    /// 协议改写从不发生，`claude_code` 拿到上游的 `openai_compatible` 就被
    /// capability gate 判成 Unsupported，而 `gateway.yaml` 里明明写着
    /// `claude_code: { alias: claude-sonnet-5 }`、协议转换本该由网关做。
    /// 这不是 LiteLLM 升级带来的问题，是"网关必须由 Lynceus 派生"这个假设
    /// 带来的。
    ///
    /// best-effort：任何一步不成立就静默返回（调用方按"未启用"处理）。
    fn adopt_external(&self) {
        // 已有状态（自己派生的，或已收养过）就不重复读盘探测。
        if self.runtime.lock().expect("gateway lock").is_some() {
            return;
        }
        let Ok(Some(settings)) = GatewaySettings::load(&gateway_config_path()) else {
            return;
        };
        self.adopt_settings(settings);
    }

    /// 用一捆已解析的设置收养外部网关：TCP + `/health` 双探通过才落状态。
    ///
    /// 从 [`Self::adopt_external`] 拆出来是为了可测——测试不必碰进程级环境
    /// 变量（workspace `unsafe_code = "forbid"`，`env::set_var` 用不了）。
    /// 返回是否真的落下了收养状态。
    fn adopt_settings(&self, settings: GatewaySettings) -> bool {
        if self.runtime.lock().expect("gateway lock").is_some() {
            return false;
        }
        if !settings.enabled {
            return false;
        }
        // `models` 在 yaml 里通常不写，是由 `agents` 绑定 + 仓储展开出来的
        // （见 `resolve_models`）。收养路径同样要展开，否则 Gateway 页面的
        // 模型列表是空的。展开不了（无仓储 / provider 缺失）时退回只看
        // `agents`：绑定才是 `rewrite_connection` 真正要用的东西。
        let resolved = match self.repository() {
            Some(repository) => resolve_models(&settings, &repository).unwrap_or_else(|error| {
                tracing::debug!(%error, "gateway model resolution failed during adoption");
                settings.clone()
            }),
            None => settings.clone(),
        };
        if resolved.models.is_empty() && settings.agents.is_empty() {
            return false;
        }
        let port = resolved.port;
        if !probe_health(port) {
            return false;
        }
        let mut guard = self.runtime.lock().expect("gateway lock");
        if guard.is_some() {
            return false; // 竞争：另一路已经收养/启动了
        }
        tracing::info!(
            port,
            "adopted an externally-started LiteLLM gateway (not spawned by Lynceus)"
        );
        *guard = Some(GatewayRuntime {
            settings: resolved,
            port,
            pid: None,
            config_path: gateway_runtime_dir().join("litellm.yaml"),
            started_at: utc_now_rfc3339(),
        });
        true
    }

    /// runtime 为空时收养外部网关（幂等）。`status` 与 `rewrite_connection`
    /// 共用，避免两处各写一遍"判空 → 收养"。
    fn ensure_adopted(&self) {
        // 清尸：自己派生的 sidecar 死了但槽位还占着时，`adopt_settings` 开头
        // 的 `is_some() → return false` 会把"dead"状态永久卡住——端口上
        // 哪怕跑着健康的网关也收养不进来，`rewrite_connection` 从此落空，
        // 全体 worker 掉直连（实测 pi 401 / dsh Connection error）。
        // 先放掉死条目，再让收养逻辑把端口上的健康实例接回来。
        let needs_adoption = {
            let mut guard = self.runtime.lock().expect("gateway lock");
            if reap_dead_spawn_slot(&mut guard) {
                tracing::info!(
                    "reaped dead gateway sidecar entry; re-adopting if the port is healthy"
                );
            }
            guard.is_none()
        };
        if needs_adoption {
            self.adopt_external();
        }
    }

    /// 当前状态视图。`runtime` 为空时先尝试收养外部网关——否则用户手工
    /// 拉起的 LiteLLM 在 Gateway 页面上永远显示"未运行"。
    #[must_use]
    pub fn status(&self) -> GatewayStatus {
        self.ensure_adopted();
        let guard = self.runtime.lock().expect("gateway lock");
        let running = guard
            .as_ref()
            .map(|runtime| match runtime.pid {
                Some(pid) => is_alive(pid),
                // 收养的进程没有 pid 可查：探端口健康（外部可能已停掉）。
                None => probe_health(runtime.port),
            })
            .unwrap_or(false);
        match guard.as_ref() {
            Some(runtime) if running => GatewayStatus {
                enabled: true,
                running: true,
                port: Some(runtime.port),
                // 收养的外部网关没有 pid 可报（不是我们派生的）。
                pid: runtime.pid,
                models: runtime
                    .settings
                    .models
                    .iter()
                    .map(|model| model.name.clone())
                    .collect(),
                agents: binding_views(&runtime.settings),
                config_path: Some(runtime.config_path.display().to_string()),
                started_at: Some(runtime.started_at.clone()),
                last_error: self.last_error.lock().expect("gateway lock").clone(),
            },
            Some(runtime) => GatewayStatus {
                enabled: runtime.settings.enabled,
                running: false,
                port: Some(runtime.port),
                pid: None,
                models: runtime
                    .settings
                    .models
                    .iter()
                    .map(|model| model.name.clone())
                    .collect(),
                agents: binding_views(&runtime.settings),
                config_path: Some(runtime.config_path.display().to_string()),
                started_at: None,
                last_error: Some("gateway process exited".to_string()),
            },
            None => GatewayStatus {
                enabled: false,
                running: false,
                port: None,
                pid: None,
                models: Vec::new(),
                agents: BTreeMap::new(),
                config_path: None,
                started_at: None,
                last_error: self.last_error.lock().expect("gateway lock").clone(),
            },
        }
    }

    /// 某 runtime 经 Gateway 的改写连接（未启用/未运行/无绑定 → None）。
    ///
    /// 自己派生的网关和**收养的外部网关**一视同仁：两者都意味着
    /// "CLI→网关段的协议就是 CLI 原生协议，上游 provider 是什么协议不再
    /// 约束本 CLI"。
    #[must_use]
    pub fn rewrite_connection(
        &self,
        runtime: WorkerRuntimeType,
        base_url: &mut Option<String>,
        default_model: &mut Option<String>,
    ) -> bool {
        // 自己没派生子进程时，先看端口上是不是已经有一个健康的外部网关。
        // 走 `ensure_adopted`：它负责"放锁 → 收养 → 再锁"的时序。
        self.ensure_adopted();
        let guard = self.runtime.lock().expect("gateway lock");
        let Some(state) = guard.as_ref() else {
            return false;
        };
        let alive = match state.pid {
            Some(pid) => is_alive(pid),
            None => probe_health(state.port),
        };
        if !alive {
            return false;
        }
        let Some(model) = state.settings.binding_for(runtime) else {
            return false;
        };
        *base_url = Some(format!("http://127.0.0.1:{}", state.port));
        *default_model = Some(model.name.clone());
        true
    }

    /// 网关改写生效时 CLI→网关段的协议：即 CLI 原生协议（别名改写与协议
    /// 转换由网关完成，上游 provider 可以是任意协议）。claude_code→
    /// Anthropic、codex→OpenAI、pi→Anthropic（models.json 固定
    /// anthropic-messages，实测 {baseUrl}/v1/messages）；dsh 的 API 随
    /// Connection 协议（openai-completions/anthropic-messages），无固定
    /// 原生协议 → None，保持 Connection 原值。
    #[must_use]
    pub fn native_protocol(runtime: WorkerRuntimeType) -> Option<ProviderType> {
        match runtime {
            WorkerRuntimeType::ClaudeCode | WorkerRuntimeType::Pi => Some(ProviderType::Anthropic),
            WorkerRuntimeType::Codex => Some(ProviderType::Openai),
            WorkerRuntimeType::DeepSeekHarness => None,
        }
    }

    fn pick_port(&self, preferred: u16) -> u16 {
        let mut port = preferred;
        while TcpStream::connect(("127.0.0.1", port)).is_ok() {
            port = port.wrapping_add(1);
        }
        port
    }

    fn resolve_command(&self, settings: &GatewaySettings) -> Result<Vec<String>, String> {
        if let Some(raw) = settings.command.as_deref().filter(|raw| !raw.trim().is_empty()) {
            return Ok(shell_split(raw));
        }
        for candidate in ["litellm", "litellm.exe"] {
            if locate_on_path(candidate).is_some() {
                return Ok(vec![candidate.to_string(), "--host".into(), "127.0.0.1".into()]);
            }
        }
        if locate_on_path("uvx").is_some() || locate_on_path("uvx.exe").is_some() {
            return Ok(vec![
                "uvx".into(),
                "--from".into(),
                "litellm[proxy]".into(),
                "litellm".into(),
                "--host".into(),
                "127.0.0.1".into(),
            ]);
        }
        Err("no LiteLLM launcher found: install litellm or uvx, or set `command` \
             in gateway.yaml"
            .to_string())
    }
}

fn binding_views(settings: &GatewaySettings) -> BTreeMap<String, GatewayBindingView> {
    settings
        .agents
        .iter()
        .map(|(runtime, binding)| {
            (
                runtime.clone(),
                GatewayBindingView {
                    alias: binding.alias.clone(),
                    provider: binding.provider.clone(),
                },
            )
        })
        .collect()
}

/// 解析 GatewayManager 单例（未初始化 → None）。
#[must_use]
pub fn global_gateway() -> Option<std::sync::Arc<GatewayManager>> {
    GLOBAL_GATEWAY.get().cloned()
}

/// 登记进程级 GatewayManager（幂等：重复 attach 返回首个实例）。
pub fn attach_global_gateway(manager: std::sync::Arc<GatewayManager>) {
    let _ = GLOBAL_GATEWAY.set(manager);
}

static GLOBAL_GATEWAY: std::sync::OnceLock<std::sync::Arc<GatewayManager>> =
    std::sync::OnceLock::new();

// ── 平台细节：spawn / 探活 / 树杀 ─────────────────────────────────────────

/// 近似 shell 分词（带引号片段）；gateway.yaml 的 `command` 是受信配置。
fn shell_split(raw: &str) -> Vec<String> {
    raw.split_whitespace()
        .map(|part| part.trim_matches('"').to_string())
        .collect()
}

fn locate_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        for ext in [".exe", ".cmd", ".bat"] {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn spawn_sidecar(
    program: &[String],
    config_path: &std::path::Path,
    port: u16,
    log_path: &std::path::Path,
    settings: &GatewaySettings,
    key_envs: &[(String, String)],
) -> Result<std::process::Child, String> {
    let Some((binary, prefix)) = program.split_first() else {
        return Err("gateway command is empty".to_string());
    };
    let mut command = Command::new(binary);
    command
        .args(prefix)
        .args(["--port", &port.to_string()])
        .arg("--config")
        .arg(config_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(log_path).map_err(|error| {
            format!("cannot open gateway log {}: {error}", log_path.display())
        })?)
        .stderr(std::process::Stdio::from(
            std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(log_path)
                .map_err(|error| {
                    format!("cannot open gateway log {}: {error}", log_path.display())
                })?,
        ));
    // uvx 的包参数在 `--from` 之后、`--port` 之前是位置敏感的：
    // `litellm --host 127.0.0.1 --port N --config <path>` 为最终 CLI 形状，
    // `--from 'litellm[proxy]'` 段已在 program/prefix 中按序携带。
    let _ = settings;
    // 清洗代理变量（畸形系统代理会让 LiteLLM 内部 httpx 解析崩溃）并放行
    // 本机回环；其余环境继承（PATH/Python 工具链/密钥 env）。
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        command.env_remove(key);
    }
    command.env("NO_PROXY", "127.0.0.1,localhost");
    // /v1/messages（claude_code）打到 openai 系上游时强制走 chat/completions
    // 转换，而不是 LiteLLM 1.99+ 缺省的 Responses API 桥（中转站普遍不支持
    // 原生 /v1/responses，实测 422 Unsupported conversion）。对 anthropic
    // 原生 passthrough 上游无影响。
    command.env("LITELLM_USE_CHAT_COMPLETIONS_URL_FOR_ANTHROPIC_MESSAGES", "1");
    // 密钥只经子进程环境注入（值绝不进日志/config 文件）。
    for (name, value) in key_envs {
        command.env(name, value);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn().map_err(|error| {
        format!("cannot spawn gateway '{}': {error}", binary)
    })
}

/// 阻塞等待端口可连（LiteLLM 就绪的近似信号）。
fn wait_for_port(port: u16, timeout: Duration) -> Result<(), String> {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err("timed out".to_string())
}

/// 端口上是否有一个**就绪的 LiteLLM**：TCP 可连且 `GET /v1/models` 返回
/// 200。
///
/// 为什么不用 `/health`：实测它在 LiteLLM 冷启动后要 1.2～6.3s 才响应
/// （内部要拉模型列表），拿它当探针会把健康网关误判成死的。`/v1/models`
/// 稳定 3ms，且路径本身就是 LiteLLM/OpenAI 系特有——比裸 TCP 更能说明
/// "这真是模型网关"。
///
/// 用于收养用户自己拉起的网关。手写 HTTP/1.0 而不引 reqwest：
/// `rewrite_connection` / `status` 都是同步函数，而本 crate 的 reqwest
/// 没开 `blocking` feature。
fn probe_health(port: u16) -> bool {
    use std::io::{Read, Write};
    let socket = ([127, 0, 0, 1], port);
    let Ok(mut stream) = TcpStream::connect_timeout(&socket.into(), Duration::from_millis(600))
    else {
        return false;
    };
    let timeout = Duration::from_millis(2_000);
    if stream.set_read_timeout(Some(timeout)).is_err()
        || stream.set_write_timeout(Some(timeout)).is_err()
    {
        return false;
    }
    // HTTP/1.0：服务端发完即关，不需要算 Content-Length。
    let request = format!("GET /v1/models HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut buffer = [0u8; 256];
    let read = match stream.read(&mut buffer) {
        Ok(read) => read,
        Err(_) => return false,
    };
    let head = String::from_utf8_lossy(&buffer[..read]);
    head.starts_with("HTTP/1.1 200") || head.starts_with("HTTP/1.0 200")
}

/// 进程是否仍在运行。
#[must_use]
pub fn is_alive(pid: u32) -> bool {
    kill_tree_probe(pid)
}

#[cfg(windows)]
fn kill_tree_probe(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn kill_tree_probe(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// 树杀：Windows `taskkill /F /T`；POSIX 先 SIGTERM，短等后 SIGKILL。
fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .output();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill").arg(pid.to_string()).output();
        std::thread::sleep(Duration::from_millis(500));
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).output();
    }
}

fn utc_now_rfc3339() -> String {
    // 与 storage 其余时间戳一致的 ISO-8601 形态（无外部 chrono 依赖）。
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let days = secs / 86_400;
    let (year, month, day) = civil_from_days(i64::try_from(days).unwrap_or(0));
    let rem = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// 天数 → (年, 月, 日)（Howard Hinnant 算法，公历）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    ((if m <= 2 { y + 1 } else { y }), u32::try_from(m).unwrap_or(1), u32::try_from(d).unwrap_or(1))
}

/// 从 Lynceus 仓储解析 Provider 密钥（api_key_provider 声明用；
/// SecretStore 短时解析，绝不写入 config/日志）。
fn resolve_provider_key(
    repository: Option<&std::sync::Arc<dyn storage::Repository>>,
    provider_id: &str,
) -> Option<String> {
    let repository = repository?;
    let provider = repository.get_provider(provider_id).ok()??;
    crate::model_providers::PlaintextSecretStore::default().resolve(&provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_litellm_config_with_env_key_references() {
        let yaml = r#"
enabled: true
port: 4141
models:
  - name: claude-sonnet-5
    provider_type: anthropic
    upstream_base: https://api.b.ai
    model: glm-5.3-flash
    api_key_env: BAI_API_KEY
  - name: gpt-main
    provider_type: openai_compatible
    upstream_base: https://api.gmi-serving.com/v1
    model: MiniMaxAI/MiniMax-M3
    api_key_env: MINIMAX_API_KEY
agents:
  claude_code: { alias: claude-sonnet-5 }
  codex: { alias: gpt-main }
"#;
        let settings: GatewaySettings = serde_yaml::from_str(yaml).expect("settings");
        assert!(settings.enabled);
        assert_eq!(settings.agents.get("codex").map(|binding| binding.alias.as_str()), Some("gpt-main"));

        let rendered = render_litellm_config(&settings);
        assert!(rendered.contains("model_name: claude-sonnet-5"));
        assert!(rendered.contains("model: anthropic/glm-5.3-flash"));
        assert!(rendered.contains("api_base: https://api.b.ai"));
        // openai 系（含 codex 绑定）一律渲染 openai/<model>：/v1/responses
        // 的桥交由 LiteLLM 自己做，手加 chat_completions/ 前缀会 404。
        assert!(rendered.contains("model: openai/MiniMaxAI/MiniMax-M3"));
        assert!(rendered.contains("api_key: os.environ/BAI_API_KEY"));
        // 密钥原文绝不出现在生成 config。
        assert!(!rendered.contains("sk-"));
    }

    #[test]
    fn binding_for_resolves_runtime_alias() {
        let yaml = r#"
enabled: true
models:
  - name: claude-sonnet-5
    provider_type: anthropic
    upstream_base: https://api.b.ai
    model: glm-5.3-flash
    api_key_env: BAI_API_KEY
agents:
  claude_code: { alias: claude-sonnet-5 }
"#;
        let settings: GatewaySettings = serde_yaml::from_str(yaml).expect("settings");
        let binding = settings
            .binding_for(WorkerRuntimeType::ClaudeCode)
            .expect("claude binding");
        assert_eq!(binding.model, "glm-5.3-flash");
        assert!(settings.binding_for(WorkerRuntimeType::Codex).is_none());
    }

    #[test]
    fn missing_config_file_loads_as_none() {
        let result = GatewaySettings::load(std::path::Path::new(
            "definitely/missing/gateway.yaml",
        ));
        assert!(matches!(result, Ok(None)));
    }

    /// 起一个只回固定状态行的假 HTTP 服务，返回其端口。
    fn fake_http_server(status_line: &'static str) -> u16 {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            // 只服务少数几个连接：探活用不完，也不会挂住测试进程。
            for stream in listener.incoming().take(4) {
                let Ok(mut stream) = stream else { continue };
                let mut buffer = [0u8; 512];
                let _ = stream.read(&mut buffer);
                let _ = stream.write_all(
                    format!("{status_line}\r\nContent-Length: 2\r\n\r\n{{}}").as_bytes(),
                );
            }
        });
        port
    }

    #[test]
    fn if_condition_guard_is_released_before_the_block_runs() {
        // 锁时序语义的记录（edition 2024，实测确证）。
        //
        // 曾怀疑 `if self.runtime.lock()...is_none() { self.adopt_external(); }`
        // 会把 guard 带进块内、让收养路径重锁同一把不可重入的 std::sync::Mutex，
        // 从而挂死整个 tokio 运行时。**该诊断是错的**：本测试证明 guard 在块
        // 执行前就已释放（edition 2024 下 `if` 条件的临时量活到条件结尾）。
        //
        // 保留它是因为：这个结论反直觉，且"锁时序"是这类进程级单例最容易
        // 踩的坑。用 `try_lock` 观察而不是真的去死锁。
        let lock = std::sync::Mutex::new(1u8);
        if *lock.lock().expect("gateway lock") == 1 {
            assert!(
                lock.try_lock().is_ok(),
                "if 条件里的 guard 在块内已释放——嵌套加锁不会死锁（edition 2024）"
            );
        }
    }

    #[test]
    fn ensure_adopted_does_not_deadlock_on_a_fresh_manager() {
        // 全新 manager（runtime 为空）走一遍收养入口：配置不存在时应当安静
        // 返回，绝不能挂在锁上。测试跑在有超时的 harness 里，挂住即失败。
        let manager = GatewayManager::default();
        manager.ensure_adopted();
        assert!(
            !manager.status().running,
            "没有可用网关时 status 必须报未运行"
        );
        // status() 自己也会触发收养：再走一遍，确认幂等且不挂。
        assert!(!manager.status().running);
    }

    fn runtime_with_pid(pid: Option<u32>) -> GatewayRuntime {
        GatewayRuntime {
            settings: GatewaySettings::default(),
            port: 4141,
            pid,
            config_path: PathBuf::from("data/gateway/litellm.yaml"),
            started_at: "test".to_string(),
        }
    }

    #[test]
    fn reap_dead_spawn_slot_clears_only_dead_self_spawns() {
        // 回归：sidecar 死了但槽位占着 → adopt_settings 的 is_some() 早退把
        // "gateway process exited" 永久卡死，端口上健康的网关收养不进来，
        // rewrite_connection 落空 → 全体 worker 掉直连（pi 401 / dsh
        // Connection error）。死条目必须被清掉，活条目和收养态必须原样。
        let mut exited = std::process::Command::new("cmd")
            .args(["/C", "exit 0"])
            .spawn()
            .expect("spawn");
        let dead_pid = exited.id();
        exited.wait().expect("wait");

        let mut dead_slot = Some(runtime_with_pid(Some(dead_pid)));
        assert!(
            reap_dead_spawn_slot(&mut dead_slot),
            "自己派生的 sidecar 已死：必须报告已清尸"
        );
        assert!(dead_slot.is_none(), "死条目清掉后槽位必须为空");

        let mut live_slot = Some(runtime_with_pid(Some(std::process::id())));
        assert!(
            !reap_dead_spawn_slot(&mut live_slot),
            "活着的 sidecar 绝不能被清"
        );
        assert!(live_slot.is_some());

        let mut adopted_slot = Some(runtime_with_pid(None));
        assert!(
            !reap_dead_spawn_slot(&mut adopted_slot),
            "收养的外部网关（pid=None）不由清尸逻辑管辖"
        );
        assert!(adopted_slot.is_some());

        assert!(!reap_dead_spawn_slot(&mut None), "空槽位无需清");
    }

    #[test]
    fn probe_health_requires_tcp_and_http_200() {
        // 就绪：TCP + 200。
        assert!(probe_health(fake_http_server("HTTP/1.1 200 OK")));
        // TCP 通但回 500：端口上有服务，但不是就绪的模型网关。
        assert!(!probe_health(fake_http_server(
            "HTTP/1.1 500 Internal Server Error"
        )));
        // 没人监听：直接 false。
        let orphan = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let dead_port = orphan.local_addr().expect("addr").port();
        drop(orphan);
        assert!(!probe_health(dead_port));
    }

    #[test]
    fn probe_health_reads_only_the_status_line() {
        // 探针只读 256 字节：响应体远大于此也不能让它判 false（LiteLLM 的
        // /v1/models 返回几百字节 JSON，而真实响应可能更大）。
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(2) {
                let Ok(mut stream) = stream else { continue };
                let mut buffer = [0u8; 512];
                let _ = stream.read(&mut buffer);
                let body = r#"{"data":[{},{}]}"#;
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                );
            }
        });
        assert!(probe_health(port));
    }

    /// gateway.yaml 的一段最小合法声明（port 由调用方填）。
    fn gateway_yaml(port: u16) -> String {
        format!(
            "enabled: true\nport: {port}\nmodels:\n  - name: claude-sonnet-5\n    \
             provider_type: openai_compatible\n    \
             upstream_base: https://api.stepfun.com/step_plan/v1\n    \
             model: step-5-preview\n    api_key_env: K\nagents:\n  \
             claude_code: {{ alias: claude-sonnet-5 }}\n"
        )
    }

    #[test]
    fn adopts_external_gateway_and_rewrites_claude_code_connection() {
        // 复现用户的场景：LiteLLM 由外部（uv tool uvx）拉起，Lynceus 自己
        // 没有派生它。收养之后 claude_code 必须拿到网关端口 + 别名，
        // 上游的 openai_compatible 不再约束它。
        let port = fake_http_server("HTTP/1.1 200 OK");
        let settings: GatewaySettings =
            serde_yaml::from_str(&gateway_yaml(port)).expect("settings");

        let manager = GatewayManager::default();
        assert!(
            manager.adopt_settings(settings),
            "健康的外部网关必须被收养"
        );
        assert_eq!(
            manager.status().running,
            true,
            "收养后 status 必须报 running，Gateway 页面才看得见"
        );

        let mut base_url = Some("https://api.stepfun.com/step_plan/v1".to_string());
        let mut model = Some("step-5-preview".to_string());
        let rewritten = manager.rewrite_connection(
            WorkerRuntimeType::ClaudeCode,
            &mut base_url,
            &mut model,
        );
        assert!(rewritten, "收养后改写必须生效");
        assert_eq!(base_url.as_deref(), Some(format!("http://127.0.0.1:{port}").as_str()));
        assert_eq!(model.as_deref(), Some("claude-sonnet-5"));
        // 收养态没有 pid：status 不谎报进程身份。
        assert_eq!(manager.status().pid, None);
    }

    #[test]
    fn refuses_to_adopt_unhealthy_or_disabled_gateway() {
        // 端口上回 500 → 不是 LiteLLM，不能收养（否则会把随便什么服务
        // 当成模型网关，worker 的流量就送错了）。
        let broken = fake_http_server("HTTP/1.1 500 Internal Server Error");
        let settings: GatewaySettings =
            serde_yaml::from_str(&gateway_yaml(broken)).expect("settings");
        assert!(
            !GatewayManager::default().adopt_settings(settings),
            "非 200 的 /health 不得收养"
        );

        // 健康端口但 enabled: false → 不收养。
        let port = fake_http_server("HTTP/1.1 200 OK");
        let disabled = gateway_yaml(port).replacen("enabled: true", "enabled: false", 1);
        let settings: GatewaySettings = serde_yaml::from_str(&disabled).expect("settings");
        assert!(
            !GatewayManager::default().adopt_settings(settings),
            "未启用的网关不得收养"
        );
    }

    #[test]
    fn stop_never_kills_a_gateway_it_did_not_spawn() {
        // 收养态的 pid 是 None：stop 只解除收养。这里用一个真实长寿子进程
        // 当"用户自己的 LiteLLM"，收养后 stop，它必须还活着——杀掉用户的
        // 服务是这条路径最不能出的错。
        let port = fake_http_server("HTTP/1.1 200 OK");
        let settings: GatewaySettings =
            serde_yaml::from_str(&gateway_yaml(port)).expect("settings");
        let manager = GatewayManager::default();
        assert!(manager.adopt_settings(settings));

        // 一个与收养无关的长寿进程，用来证明 stop 没有顺手杀东西。
        let mut sleeper = Command::new("cmd.exe")
            .args(["/C", "ping -n 60 127.0.0.1 >nul"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sleeper");
        let sleeper_pid = sleeper.id();

        let status = manager.stop();
        assert!(
            !status.running,
            "stop 之后状态必须回到未运行（收养被解除）"
        );
        assert!(
            is_alive(sleeper_pid),
            "stop 绝不能在收养态下杀进程——那不是 Lynceus 派生的"
        );

        let _ = sleeper.kill();
        let _ = sleeper.wait();
    }
}
