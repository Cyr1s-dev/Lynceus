//! 外部 Worker Runtime 契约（trait-only，无实现——实现位于 engines）。
//!
//! Lynceus 不再自带执行层：分支的实际执行由用户本机安装的外部 Worker
//! Runtime（`Codex CLI` / `Claude Code` / `Pi` / `DeepSeek Harness`）完成。
//! 本模块只定义**最薄的外部边界**：
//!
//! - [`WorkerRuntime`]：probe / start / resume / events / cancel /
//!   capabilities。它只是外部 runtime driver，**不拥有任何调度状态机**
//!   ——Mission/Branch/AgentTask 的生命周期、取消与超时仍由编排层持有。
//! - [`WorkerRuntimeSelector`]：组合根注入的运行时选择面（编排层经
//!   `SolverContext.worker_runtime` 拿到已选定的 runtime）。
//!
//! 契约先例与 [`crate::llm::ProviderRuntime`] 一致：trait 定义在 agents
//! （编排可见），实现与进程 I/O 在 engines。

use std::path::PathBuf;
use std::{fmt, fmt::Formatter};

use async_trait::async_trait;
use models::provider::ProviderConfig;
use models::worker::ResolvedWorkerConnection;
use models::worker::WorkerEvent;
use models::worker::WorkerInvocation;
use models::worker::WorkerProbe;
use models::worker::WorkerRun;
use models::worker::WorkerRunStatus;
use models::worker::WorkerRuntimeProfile;
use models::worker::WorkerRuntimeType;

/// 外部执行不可用时必须显式失败（绝不回退内部执行）的错误分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerRuntimeErrorKind {
    /// 本机未发现该 runtime。
    NotInstalled,
    /// 已发现但调用失败（连接失败 / 非零退出 / 协议错误）。
    Unavailable,
    /// 版本或协议不兼容（Developer Preview 版本门）。
    Unsupported,
    /// 已安装但未绑定有效 Connection（Configuration Required）。
    NotReady,
    /// 外部执行超时，已被终止。
    Timeout,
    /// 调用被取消。
    Cancelled,
    /// 解析/审计基础设施自身出错（仓储读失败等）。
    Internal,
}

/// 外部 runtime 调用错误（消息已脱敏、有界）。
#[derive(Debug, thiserror::Error)]
#[error("worker runtime {kind:?}: {message}")]
pub struct WorkerRuntimeError {
    /// 错误分类。
    pub kind: WorkerRuntimeErrorKind,
    /// 有界、脱敏的消息。
    pub message: String,
}

impl WorkerRuntimeError {
    /// 构造错误。
    #[must_use]
    pub fn new(kind: WorkerRuntimeErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// 一次外部 worker 执行请求。
#[derive(Clone)]
pub struct WorkerExecutionRequest {
    /// 交给外部 runtime 的任务指令（编排层已脱敏、有界）。
    pub instruction: String,
    /// 整体超时秒数（driver 强杀，映射 `WorkerRunStatus::Timeout`）。
    pub timeout_seconds: u64,
    /// continuation 锚点：`resume` 必填，`start` 必须为 None。
    pub session_ref: Option<String>,
    /// Coordinator 预分配的 WorkerRun id；用于 MCP grant 与 adapter 记录一致。
    pub worker_run_id: Option<String>,
    /// 外部进程工作目录（可选；由调用方保证存在）。
    pub workdir: Option<PathBuf>,
    /// 外部 CLI 的隔离 home/config 根目录。start 与 resume 必须复用同一路径；
    /// 未提供时 driver 使用由 workdir/runtime 推导的隔离目录，绝不使用用户 home。
    pub config_dir: Option<PathBuf>,
    /// Lynceus MCP endpoint；不把 endpoint 之外的 MCP 配置交给 worker。
    pub mcp_url: Option<String>,
    /// 当前 Worker grant bearer；只用于子进程内存环境注入，不进入 argv/config 文件。
    pub mcp_bearer_token: Option<String>,
    /// 命令禁则前缀（command policy）：adapter 译成各自原生的 deny 面
    /// （Claude Code `--disallowedTools`）；对没有 per-command 原生层的
    /// runtime 仅作记录，禁示由提示词块承载。
    pub denied_command_prefixes: Vec<String>,
}

impl fmt::Debug for WorkerExecutionRequest {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerExecutionRequest")
            .field("instruction", &self.instruction)
            .field("timeout_seconds", &self.timeout_seconds)
            .field("session_ref", &self.session_ref)
            .field("worker_run_id", &self.worker_run_id)
            .field("workdir", &self.workdir)
            .field("config_dir", &self.config_dir)
            .field("mcp_url", &self.mcp_url)
            .field(
                "mcp_bearer_token",
                &self.mcp_bearer_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl WorkerExecutionRequest {
    /// 启动新会话的请求。
    #[must_use]
    pub fn start(instruction: impl Into<String>, timeout_seconds: u64) -> Self {
        Self {
            instruction: instruction.into(),
            timeout_seconds,
            session_ref: None,
            worker_run_id: None,
            workdir: None,
            config_dir: None,
            mcp_url: None,
            mcp_bearer_token: None,
            denied_command_prefixes: Vec::new(),
        }
    }

    /// continuation 请求（在既有 session 上继续）。
    #[must_use]
    pub fn resume(
        session_ref: impl Into<String>,
        instruction: impl Into<String>,
        timeout_seconds: u64,
    ) -> Self {
        Self {
            instruction: instruction.into(),
            timeout_seconds,
            session_ref: Some(session_ref.into()),
            worker_run_id: None,
            workdir: None,
            config_dir: None,
            mcp_url: None,
            mcp_bearer_token: None,
            denied_command_prefixes: Vec::new(),
        }
    }

    /// 绑定同一 Worker session 要复用的隔离配置目录。
    #[must_use]
    pub fn with_config_dir(mut self, config_dir: PathBuf) -> Self {
        self.config_dir = Some(config_dir);
        self
    }

    /// 绑定本次 Worker 使用的 Lynceus MCP grant。
    #[must_use]
    pub fn with_mcp(mut self, url: impl Into<String>, bearer_token: impl Into<String>) -> Self {
        self.mcp_url = Some(url.into());
        self.mcp_bearer_token = Some(bearer_token.into());
        self
    }
}

/// Coordinator 绑定到一次 WorkerRun 的 MCP 凭据和隔离配置。
///
/// bearer token 只保存在内存请求对象中；Debug 明确不输出它，避免误入日志。
#[derive(Clone)]
pub struct WorkerMcpBinding {
    /// MCP Streamable HTTP endpoint。
    pub endpoint: String,
    /// grant id（非 secret），用于撤销。
    pub grant_id: String,
    /// 短期 bearer；仅供 adapter 注入子进程环境。
    pub bearer_token: String,
    /// 与 WorkerRun 一致的稳定 id。
    pub worker_run_id: String,
    /// start/resume 共用的隔离 CLI home/config 根目录。
    pub config_dir: PathBuf,
}

impl fmt::Debug for WorkerMcpBinding {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerMcpBinding")
            .field("endpoint", &self.endpoint)
            .field("grant_id", &self.grant_id)
            .field("bearer_token", &"<redacted>")
            .field("worker_run_id", &self.worker_run_id)
            .field("config_dir", &self.config_dir)
            .finish()
    }
}

/// 一次外部执行的结果（driver 产出，provenance 由编排层回填）。
#[derive(Debug, Clone)]
pub struct WorkerExecutionOutcome {
    /// 会话记录（状态 / 事件 / `summary` / `session_ref`）。
    pub run: WorkerRun,
    /// 本次调用的审计记录。
    pub invocation: WorkerInvocation,
    /// 原始输出字节（adapter 侧已脱敏前的有界捕获；由编排层密封为
    /// transcript 工件后丢弃）。
    pub transcript: Vec<u8>,
}

/// 外部 Worker Runtime 的最小边界。
///
/// 实现职责：把任务交给外部进程/HTTP/JSON-RPC runtime 并把结果收回为
/// 受控 observation。**不实现**工具循环、命令拼接或任何 Lynceus 侧
/// Harness 逻辑；外部输出经密封 transcript + 有界事件回流。
#[async_trait]
pub trait WorkerRuntime: Send + Sync {
    /// runtime 类型标识。
    fn runtime_type(&self) -> WorkerRuntimeType;

    /// 探测本机可用性（发现 + 版本门）。探测永不 panic、不伪造
    /// Available；结论是 [`WorkerProbe`]（含显式失败态）。
    async fn probe(&self) -> WorkerProbe;

    /// 声明的能力（continuation / structured output / transcript…）。
    fn capabilities(&self) -> Vec<String>;

    /// 启动一次新会话并等待其终态（或超时/取消）。
    ///
    /// # Errors
    /// runtime 不可用、版本不兼容、进程失败或超时。
    async fn start(
        &self,
        request: WorkerExecutionRequest,
    ) -> Result<WorkerExecutionOutcome, WorkerRuntimeError>;

    /// 在既有 session 上继续（continuation）。
    ///
    /// # Errors
    /// runtime 不可用、不支持 continuation 或会话引用失效。
    async fn resume(
        &self,
        request: WorkerExecutionRequest,
    ) -> Result<WorkerExecutionOutcome, WorkerRuntimeError>;

    /// 拉取一个（进行中或已结束）会话的事件快照。
    ///
    /// # Errors
    /// 会话引用未知或 runtime 不可用。
    async fn events(&self, session_ref: &str) -> Result<Vec<WorkerEvent>, WorkerRuntimeError>;

    /// 请求取消一个进行中的会话。返回是否确认终止。
    ///
    /// # Errors
    /// runtime 不可用或会话引用未知。
    async fn cancel(&self, session_ref: &str) -> Result<bool, WorkerRuntimeError>;
}

/// 运行时选择面：编排层经组合根注入，按配置提示挑选已探测可用的
/// runtime。**没有可用 runtime 时必须显式失败**——编排层据此返回
/// unavailable，绝不回退内部执行。
#[async_trait]
pub trait WorkerRuntimeSelector: Send + Sync {
    /// 选择一个 runtime：优先命中显式偏好，否则按稳定顺序取第一个
    /// 可用项；`preferred` 无法解析或不可用时返回显式错误。
    ///
    /// # Errors
    /// 没有任何可用 runtime（消息区分 not installed / unsupported）。
    async fn select(
        &self,
        preferred: Option<&str>,
    ) -> Result<std::sync::Arc<dyn WorkerRuntime>, WorkerRuntimeError>;

    /// 全部显式适配器（探测快照按 runtime 类型对齐返回）。
    async fn probes(&self) -> Vec<WorkerProbe>;

    /// 按类型取单个适配器（不做可用性判定）。
    fn runtime(&self, runtime_type: WorkerRuntimeType)
    -> Option<std::sync::Arc<dyn WorkerRuntime>>;

    /// 该 runtime 的 Profile 绑定的 Agent 预设 key（WP4 解析优先级
    /// mission config > profile > 内置默认中的第二层）。缺省 None，
    /// 测试替身无需实现。
    async fn profile_agent_preset_key(&self, _runtime_type: WorkerRuntimeType) -> Option<String> {
        None
    }

    /// 派发开始前落一条 `WorkerRun` 骨架，返回其 id。
    ///
    /// **为什么需要**：runtime 是 [`Self::select`] 里才定下来的（无偏好时
    /// 按注册表游标轮转），而 `WorkerRun` 此前只在 solver 返回**之后**才
    /// 落库。于是 worker 正在跑的那几分钟里，前端没有任何来源能回答
    /// "这次是谁在干"——会话面板只能显示通用的
    /// "Worker · 未关联意图 #xxx"，看不到 Codex / Pi / DSH。
    ///
    /// 返回的 id 由调用方填进 `WorkerExecutionRequest::worker_run_id`，
    /// adapter 随即复用同一 id，最终 `upsert` 覆盖同一行（不会产生重复
    /// 记录）。
    ///
    /// best-effort：落库失败返回 `None`（执行照旧，只是前端晚一点才知道
    /// 归属）。**默认实现返回 `None`**，测试替身无需操心。
    async fn begin_dispatch(&self, _attribution: &DispatchAttribution) -> Option<String> {
        None
    }

    /// 中断一个正在运行的 worker 并登记用户的注入消息。
    ///
    /// 语义 = 终端里的 Ctrl+C 后再输入：取消信号立即杀进程，消息留在
    /// 注入表里等派发层收尸——派发层看到 `cancelled` 结果时取走消息，
    /// 用 resume（保留 worker 会话记忆）或 fresh start（全量指令重发）
    /// 把任务继续跑下去，**不**走失败路径。
    ///
    /// `worker_run_id` 是派发预分配的骨架行 id，同时是 inflight 的取消
    /// 键。返回 `false` = 该 worker 不在本进程的 inflight 表里（从未
    /// 启动或已结束）。
    ///
    /// **默认实现返回 `false`**，测试替身无需操心。
    async fn interrupt_worker(&self, _worker_run_id: &str, _message: &str) -> bool {
        false
    }

    /// 取走某 worker 的中注入消息（一次性；派发层消费后清空）。
    ///
    /// **默认实现返回 `None`。**
    async fn take_interrupt_message(&self, _worker_run_id: &str) -> Option<String> {
        None
    }

    /// 某 worker 运行中上报过的会话引用（codex thread id / claude
    /// session id）。resume 需要它；进程重启后自然为空（那时的 worker
    /// 也已不存在，无法中断）。
    ///
    /// **默认实现返回 `None`。**
    async fn worker_session_ref(&self, _worker_run_id: &str) -> Option<String> {
        None
    }

    /// 正在运行（inflight 注册过）的 worker run id 列表。
    ///
    /// API 层据此回答"这个 mission 有没有可中断的 worker"，无需翻库。
    /// **默认实现返回空。**
    async fn inflight_worker_run_ids(&self) -> Vec<String> {
        Vec::new()
    }

    /// 终结清理：某 worker 的会话引用与中断注入登记。
    ///
    /// 派发层在消费完结终态（成功/失败/超时/取消）后必须调用，防止
    /// 簿记泄漏。**默认实现无操作。**
    async fn forget_worker(&self, _worker_run_id: &str) {}
}

/// 一次派发的归属信息：落 `WorkerRun` 骨架用，让"哪个 harness 在跑哪个
/// task"在 worker 还在运行时就已可见。
#[derive(Debug, Clone)]
pub struct DispatchAttribution {
    /// 所属 Project。
    pub project_id: models::ProjectId,
    /// 所属 Mission。
    pub mission_id: Option<models::MissionId>,
    /// 所属 Branch。
    pub branch_id: Option<models::BranchId>,
    /// 所属 Run。
    pub run_id: models::RunId,
    /// 关联 Task。
    pub task_id: models::TaskId,
    /// 交给 runtime 的指令（有界化后）。
    pub instruction: String,
    /// 本次选定的 runtime（`select` 的产物）。
    pub runtime_type: WorkerRuntimeType,
    /// 期望的行 id：MCP grant 已按 `worker-run-{task_id}` 发了 scope，
    /// broker 的 `insert_session` 要拿这个 id 反查 `agent_preset_id` 收紧
    /// 授权。给上它，骨架就复用同一 id（否则会出现两行）。
    pub preferred_id: Option<String>,
    /// 本次派发实际使用的 Agent 预设 id。写进骨架，MCP worker 连接时
    /// `insert_session` 才拿得到它——此前这个字段只在跑完后才落库，
    /// 预设收紧路径一直空转。
    pub agent_preset_id: Option<String>,
}

/// Connection 解析面：driver 在启动子进程前经此取得 Lynceus 配置的
/// API Base URL / Secret / Model。**Agent CLI 绝不复用用户本机的 CLI
/// 登录、OAuth 或 shell 环境密钥**——认证来源只有这里绑定的
/// Connection（复用现有 `ProviderConfig` + `SecretStore` 体系）。
#[async_trait]
pub trait WorkerConnectionResolver: Send + Sync {
    /// 解析一个 Connection。实现负责经 `SecretStore` 短暂解析密钥，
    /// 并保证 `ResolvedWorkerConnection` 不进入日志/事件/UI。
    ///
    /// # Errors
    /// Connection 不存在或未启用。
    async fn resolve(
        &self,
        connection_id: &str,
    ) -> Result<ResolvedWorkerConnection, WorkerRuntimeError>;

    /// 读取某个 runtime 绑定的 Worker Profile（未绑定为 `None`）。
    async fn profile(
        &self,
        runtime_type: WorkerRuntimeType,
    ) -> Result<Option<WorkerRuntimeProfile>, WorkerRuntimeError>;

    /// 校验 Connection 配置完整性（`base_url` / `key` / `model` 齐备且启用）。
    /// 不完整时返回 `WorkerRuntimeErrorKind::NotReady`。
    ///
    /// # Errors
    /// 配置缺失（`NotReady`）或 Connection 不存在（`NotInstalled` 语义）。
    async fn validate_connection(
        &self,
        connection_id: &str,
    ) -> Result<ResolvedWorkerConnection, WorkerRuntimeError>;
}

/// Connection 解析 + 完整性校验的共享实现辅助（供 engines registry 复用）。
///
/// 只做字段级校验，不做协议判定（协议兼容性是各 adapter 的职责）。
#[must_use]
pub fn connection_is_complete(connection: &ResolvedWorkerConnection) -> bool {
    connection
        .base_url
        .as_deref()
        .is_some_and(|url| !url.trim().is_empty())
        && connection
            .api_key
            .as_deref()
            .is_some_and(|key| !key.trim().is_empty())
}

/// 从 `ProviderConfig` 解析 Connection（经 `SecretStore`，密钥短暂存在于内存）。
#[must_use]
pub fn resolve_provider_connection(
    provider: &ProviderConfig,
    api_key: Option<String>,
) -> ResolvedWorkerConnection {
    ResolvedWorkerConnection {
        connection_id: provider.id.as_str().to_string(),
        protocol: provider.provider_type,
        base_url: provider.base_url.clone(),
        api_key,
        default_model: provider.model.clone(),
    }
}

/// 判定一个 [`WorkerRun`] 是否以失败语义结束（编排层据此走失败提交路径）。
#[must_use]
pub fn worker_run_failed(run: &WorkerRun) -> bool {
    matches!(
        run.status,
        WorkerRunStatus::Failed | WorkerRunStatus::Timeout | WorkerRunStatus::Cancelled
    )
}
