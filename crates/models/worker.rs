//! 外部 Worker Runtime 领域模型。
//!
//! Lynceus 自身不执行审计任务：实际执行交给用户本机安装的外部
//! Worker Runtime（`Codex CLI` / `Claude Code` / `Pi` / `DeepSeek Harness`）。
//! 本模块定义外部执行的**独立审计概念**——它们刻意不与
//! [`crate::tool_invocation::ToolInvocation`] 混用：
//!
//! - [`WorkerRun`](Self)：一次外部 worker 会话（含 session/continuation 引用、
//!   transcript 密封工件路径、里程碑事件流）。
//! - [`WorkerInvocation`](Self)：driver 对外部 runtime 的单次调用审计
//!   （probe / start / resume / cancel）。
//!
//! 红线：外部 worker 的 stdout/stderr 只能作为受控 observation 进入
//! `WorkerRun`（有界、脱敏、密封 transcript），**绝不直接变成
//! Finding**；真实 Lynceus 平台工具的执行仍走 `ToolInvocation` 并受
//! Evidence 三重门约束。
//!
//! 表结构为 Rust-native（`worker_runs` / `worker_invocations`），与
//! parity 冻结的 46 张表无关（dump 时按 `worker_%` 前缀排除）。

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::common::Timestamp;
use crate::common::new_id;
use crate::ids::BranchId;
use crate::ids::MissionId;
use crate::ids::ProjectId;
use crate::ids::RunId;
use crate::ids::TaskId;
use crate::provider::ProviderType;

/// 单个事件文本的确定性上限（超出截断并记录 notice）。
pub const MAX_WORKER_EVENT_CHARS: usize = 2000;

/// 每 run 事件数的确定性上限（超出丢最旧并记录 notice）。
pub const MAX_WORKER_EVENTS_PER_RUN: usize = 200;

/// 任务指令的持久化上限（完整 transcript 在密封工件里，不入库）。
pub const MAX_WORKER_INSTRUCTION_CHARS: usize = 8000;

/// 结果摘要的持久化上限（受控 observation，非 Finding）。
pub const MAX_WORKER_SUMMARY_CHARS: usize = 4000;

// ---------------------------------------------------------------------------
// 枚举
// ---------------------------------------------------------------------------

/// 显式支持的外部 Worker Runtime 类型。
///
/// 不提供"任意命令模板"式通用 CLI worker。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerRuntimeType {
    /// `Codex CLI`（`codex exec`）。
    Codex,
    /// `Claude Code CLI`（`claude -p` headless）。
    ClaudeCode,
    /// `Pi Coding Agent`（`pi --mode json`；Developer Preview）。
    Pi,
    /// `DeepSeek Harness`（`dsh --profile headless`；Developer Preview）。
    #[serde(rename = "deepseek_harness")]
    DeepSeekHarness,
}

impl WorkerRuntimeType {
    /// 全部显式支持类型（稳定顺序，用于注册表与 UI 列表）。
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [
            Self::ClaudeCode,
            Self::Codex,
            Self::Pi,
            Self::DeepSeekHarness,
        ]
    }

    /// wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude_code",
            Self::Codex => "codex",
            Self::Pi => "pi",
            Self::DeepSeekHarness => "deepseek_harness",
        }
    }

    /// 人类可读展示名（UI / 审计日志）。
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex CLI",
            Self::Pi => "Pi",
            Self::DeepSeekHarness => "DeepSeek Harness",
        }
    }
}

/// Worker runtime 的可用性状态（探测结论）。
///
/// 语义对齐目标规范：未安装 / 不可用 / 不受支持 / 未就绪 / 错误 / 可用。
/// **不伪造 Available**——探测失败永远落到显式失败态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerAvailability {
    /// 已安装、探测通过且绑定连接就绪，可以接受任务。
    Available,
    /// 本机未发现该 runtime。
    NotInstalled,
    /// 已发现但无法建立会话（连接失败 / 探测命令失败）。
    Unavailable,
    /// 已发现但版本/协议不兼容（Developer Preview 版本门）。
    Unsupported,
    /// 已安装但未绑定有效 Connection（Configuration Required）。
    /// 绝不允许静默回退到用户本机 CLI 登录态。
    NotReady,
    /// 探测过程本身出错。
    Error,
}

impl WorkerAvailability {
    /// wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::NotInstalled => "not_installed",
            Self::Unavailable => "unavailable",
            Self::Unsupported => "unsupported",
            Self::NotReady => "not_ready",
            Self::Error => "error",
        }
    }

    /// 是否可以接受任务。
    #[must_use]
    pub const fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }
}

/// 一次外部 worker 会话的生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerRunStatus {
    /// 已创建尚未启动。
    Pending,
    /// 外部进程/请求进行中。
    Running,
    /// 外部 runtime 正常完成。
    Succeeded,
    /// 外部 runtime 失败（非零退出 / 协议错误）。
    Failed,
    /// 超时被 driver 终止。
    Timeout,
    /// 被取消（mission 取消传播或显式 cancel）。
    Cancelled,
}

impl WorkerRunStatus {
    /// wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
        }
    }

    /// 是否为终态。
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Timeout | Self::Cancelled
        )
    }
}

/// driver 对外部 runtime 的调用目的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerInvocationPurpose {
    /// 探测可用性（--version / --help）。
    Probe,
    /// 启动新会话。
    Start,
    /// 在既有 session 上继续（continuation）。
    Resume,
    /// 终止进行中的会话。
    Cancel,
}

impl WorkerInvocationPurpose {
    /// wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Probe => "probe",
            Self::Start => "start",
            Self::Resume => "resume",
            Self::Cancel => "cancel",
        }
    }
}

/// 里程碑事件类别（有界审计流，不是全量 stdout 镜像）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerEventKind {
    /// 状态迁移（started / exited / killed）。
    State,
    /// 受控输出摘录（脱敏后有界截断）。
    Output,
    /// 失败 / 警告。
    Error,
    /// driver 注入的说明（如截断标记）。
    Notice,
}

impl WorkerEventKind {
    /// wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Output => "output",
            Self::Error => "error",
            Self::Notice => "notice",
        }
    }
}

// ---------------------------------------------------------------------------
// 记录
// ---------------------------------------------------------------------------

/// 外部 worker 的一个里程碑事件（有界、脱敏）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerEvent {
    /// run 内单调递增序号（从 1 开始）。
    pub seq: u64,
    /// 事件类别。
    pub kind: WorkerEventKind,
    /// 发生时间。
    pub at: Timestamp,
    /// 有界文本（截断由 [`WorkerRun::record_event`] 保证）。
    pub message: String,
}

/// 探测结论快照（API / UI 的 Worker Runtime 状态来源）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerProbe {
    /// runtime 类型。
    pub runtime: WorkerRuntimeType,
    /// 可用性结论。
    pub availability: WorkerAvailability,
    /// 探测到的版本号（尽力解析，未探测到为 None）。
    pub version: Option<String>,
    /// 人类可读结论（含不可用原因；**不含密钥**）。
    pub detail: Option<String>,
    /// 声明的能力（如 continuation / structured output）。
    pub capabilities: Vec<String>,
    /// 探测时间。
    pub checked_at: Timestamp,
}

impl WorkerProbe {
    /// 构造探测结论。
    #[must_use]
    pub fn new(
        runtime: WorkerRuntimeType,
        availability: WorkerAvailability,
        capabilities: Vec<String>,
    ) -> Self {
        Self {
            runtime,
            availability,
            version: None,
            detail: None,
            capabilities,
            checked_at: Timestamp::now(),
        }
    }
}

/// 一次外部 worker 会话（session / continuation / transcript 的载体）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerRun {
    /// 实体 ID（`wkrun_` 前缀）。
    pub id: String,
    /// 所属 `Project`。
    pub project_id: ProjectId,
    /// 所属 `Mission`（独立执行时可为 None）。
    #[serde(default)]
    pub mission_id: Option<MissionId>,
    /// 所属 `AuditRun`。
    #[serde(default)]
    pub run_id: Option<RunId>,
    /// 所属 `Branch`。
    #[serde(default)]
    pub branch_id: Option<BranchId>,
    /// 所属 `AgentTask`。
    #[serde(default)]
    pub task_id: Option<TaskId>,
    /// 使用的 runtime 类型。
    pub runtime: WorkerRuntimeType,
    /// 绑定的 `WorkerRuntimeProfile id`（None = 未绑定配置）。
    #[serde(default)]
    pub profile_id: Option<String>,
    /// 使用的 Connection（复用 `ProviderConfig` 的 id；**不含密钥**）。
    #[serde(default)]
    pub connection_id: Option<String>,
    /// 实际使用的模型（profile override > connection default）。
    #[serde(default)]
    pub model: Option<String>,
    /// 本次执行使用的 Agent 预设 key（内置默认为 None；WP4 审计列）。
    #[serde(default)]
    pub agent_preset_id: Option<String>,
    /// 执行位置（local / container）。
    pub execution_environment: WorkerExecutionEnvironment,
    /// 生命周期状态。
    pub status: WorkerRunStatus,
    /// 交给外部 runtime 的任务指令（有界）。
    pub instruction: String,
    /// 外部 runtime 的会话引用（continuation 锚点；协议相关字符串）。
    #[serde(default)]
    pub session_ref: Option<String>,
    /// 密封 transcript 工件路径（`SealedArtifact`，SHA-256 绑定）。
    #[serde(default)]
    pub transcript_path: Option<String>,
    /// 有界结果摘要（受控 observation，非 Finding）。
    #[serde(default)]
    pub summary: Option<String>,
    /// 失败原因（有界、脱敏）。
    #[serde(default)]
    pub error: Option<String>,
    /// 外部进程退出码（被信号/强杀时为 None）。
    #[serde(default)]
    pub exit_code: Option<i64>,
    /// 里程碑事件流（有界）。
    #[serde(default)]
    pub events: Vec<WorkerEvent>,
    /// 创建时间。
    pub created_at: Timestamp,
    /// 开始时间。
    #[serde(default)]
    pub started_at: Option<Timestamp>,
    /// 结束时间。
    #[serde(default)]
    pub finished_at: Option<Timestamp>,
    /// 总时长毫秒。
    #[serde(default)]
    pub duration_ms: Option<i64>,
    /// 扩展元数据（driver 特定；**不含密钥**）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
    /// 结构化 token/成本用量（CLI 事件流上报；None = 该 runtime 无法回收）。
    #[serde(default)]
    pub usage: Option<WorkerUsage>,
}

/// 一次 Worker 执行的结构化用量（agent 事件流上报的真实数据；
/// `cost_usd` 仅在 CLI/网关真实报出时为 `Some`，绝不伪造）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct WorkerUsage {
    /// 输入 tokens。
    #[serde(default)]
    pub input_tokens: i64,
    /// 输出 tokens。
    #[serde(default)]
    pub output_tokens: i64,
    /// 命中缓存的输入 tokens。
    #[serde(default)]
    pub cached_input_tokens: i64,
    /// 推理 tokens。
    #[serde(default)]
    pub reasoning_tokens: i64,
    /// 真实美元成本（仅 CLI/网关真实报出；订阅/OAuth 模式为 None）。
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// 会话轮数。
    #[serde(default)]
    pub num_turns: Option<i64>,
    /// 模型 API 累计耗时毫秒。
    #[serde(default)]
    pub duration_api_ms: Option<i64>,
    /// 实际请求的模型名（可能与 run.model 不同：网关别名展开后）。
    #[serde(default)]
    pub requested_model: Option<String>,
}

/// 用量聚合视图（按 project / runtime 维度求和；cost 仅真实值求和，
/// 全程 None 则保持 None，不冒充精确成本）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerUsageSummary {
    /// 计入聚合的 run 数。
    #[serde(default)]
    pub runs: i64,
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub cached_input_tokens: i64,
    #[serde(default)]
    pub reasoning_tokens: i64,
    /// 聚合美元成本（仅所有 run 都真实报出时有意义；部分缺失为 None）。
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

/// 单日用量切片（LiteLLM/网关统计页每日活动图数据源）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerUsageDailyPoint {
    /// UTC 日期（`YYYY-MM-DD`，取自 usage 记录 `created_at` 前缀）。
    #[serde(default)]
    pub day: String,
    /// 当日记账的 run 数。
    #[serde(default)]
    pub runs: i64,
    /// 当日输入 tokens 合计。
    #[serde(default)]
    pub input_tokens: i64,
    /// 当日输出 tokens 合计。
    #[serde(default)]
    pub output_tokens: i64,
    /// 当日命中缓存的输入 tokens 合计。
    #[serde(default)]
    pub cached_input_tokens: i64,
    /// 当日部分 run 缺真实成本时为 None（不冒充精确值）。
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

/// runtime × model 维度用量切片（统计页"分析"分组柱状图数据源）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerUsageModelSlice {
    /// 外部 worker runtime wire 名（`claude_code`/`codex`/...）。
    #[serde(default)]
    pub runtime: String,
    /// 网关别名（CLI 侧模型名）。
    #[serde(default)]
    pub model: Option<String>,
    /// 网关展开后的上游真实模型名。
    #[serde(default)]
    pub requested_model: Option<String>,
    /// 该分组的 run 数。
    #[serde(default)]
    pub runs: i64,
    /// 输入 tokens 合计。
    #[serde(default)]
    pub input_tokens: i64,
    /// 输出 tokens 合计。
    #[serde(default)]
    pub output_tokens: i64,
    /// 命中缓存的输入 tokens 合计。
    #[serde(default)]
    pub cached_input_tokens: i64,
    /// 该分组部分 run 缺真实成本时为 None。
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

/// 用量分组维度（`/gateway/usage?group_by=`；对应统计页「按 Runtime / 按模型」tabs）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerUsageDimension {
    /// 按外部 worker runtime 分组。
    Runtime,
    /// 按网关模型别名分组。
    Model,
}

impl WorkerUsageDimension {
    /// wire 值（查询参数与前端 tab key）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::Model => "model",
        }
    }
}

/// 分组每日用量点（`group_by=runtime|model` 时按（日 × 维度键）聚合）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerUsageGroupedDailyPoint {
    /// UTC 日期（`YYYY-MM-DD`）。
    #[serde(default)]
    pub day: String,
    /// 维度键：runtime wire 名或模型别名；模型未记录时为 None。
    #[serde(default)]
    pub key: Option<String>,
    /// 当日该分组记账的 run 数。
    #[serde(default)]
    pub runs: i64,
    /// 输入 tokens 合计（未命中缓存部分）。
    #[serde(default)]
    pub input_tokens: i64,
    /// 输出 tokens 合计。
    #[serde(default)]
    pub output_tokens: i64,
    /// 命中缓存的输入 tokens 合计。
    #[serde(default)]
    pub cached_input_tokens: i64,
    /// 该分组部分 run 缺真实成本时为 None（不冒充精确值）。
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

/// 用量多维报表（网关统计页一次性取数；时间窗口由 storage 侧换算）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerUsageBreakdown {
    /// 窗口内总量聚合。
    #[serde(default)]
    pub summary: WorkerUsageSummary,
    /// 按日升序。
    #[serde(default)]
    pub daily: Vec<WorkerUsageDailyPoint>,
    /// runtime × 模型切片，按总 token 降序。
    #[serde(default)]
    pub by_model: Vec<WorkerUsageModelSlice>,
    /// 按 runtime 聚合的切片（同按总 token 降序）。
    #[serde(default)]
    pub by_runtime: Vec<WorkerUsageModelSlice>,
    /// `group_by` 指定维度时的（日 × 维度键）聚合；未指定为空。
    #[serde(default)]
    pub grouped_daily: Vec<WorkerUsageGroupedDailyPoint>,
}

impl WorkerRun {
    /// 构造一次待启动的 worker 会话（指令超界截断）。
    #[must_use]
    pub fn new(
        project_id: ProjectId,
        runtime: WorkerRuntimeType,
        instruction: impl Into<String>,
    ) -> Self {
        Self {
            id: new_id("wkrun"),
            project_id,
            mission_id: None,
            run_id: None,
            branch_id: None,
            task_id: None,
            runtime,
            profile_id: None,
            connection_id: None,
            model: None,
            agent_preset_id: None,
            execution_environment: WorkerExecutionEnvironment::Local,
            status: WorkerRunStatus::Pending,
            instruction: truncate_chars(&instruction.into(), MAX_WORKER_INSTRUCTION_CHARS),
            session_ref: None,
            transcript_path: None,
            summary: None,
            error: None,
            exit_code: None,
            events: Vec::new(),
            created_at: Timestamp::now(),
            started_at: None,
            finished_at: None,
            duration_ms: None,
            metadata: Map::new(),
            usage: None,
        }
    }

    /// 追加一个里程碑事件（有界：超限截断文本、超量丢最旧并记 notice）。
    pub fn record_event(&mut self, kind: WorkerEventKind, message: impl Into<String>) {
        let message = truncate_chars(&message.into(), MAX_WORKER_EVENT_CHARS);
        let seq = self.events.last().map_or(1, |last| last.seq + 1);
        self.events.push(WorkerEvent {
            seq,
            kind,
            at: Timestamp::now(),
            message,
        });
        if self.events.len() > MAX_WORKER_EVENTS_PER_RUN {
            // 多丢一条给截断 notice 留位，保证 push 后仍不超上限。
            let overflow = self.events.len() - MAX_WORKER_EVENTS_PER_RUN + 1;
            self.events.drain(0..overflow);
            let seq = self.events.last().map_or(1, |last| last.seq + 1);
            self.events.push(WorkerEvent {
                seq,
                kind: WorkerEventKind::Notice,
                at: Timestamp::now(),
                message: format!("event log truncated; dropped {overflow} oldest event(s)"),
            });
        }
    }

    /// 标记启动。
    pub fn mark_started(&mut self) {
        self.status = WorkerRunStatus::Running;
        self.started_at.get_or_insert_with(Timestamp::now);
        self.record_event(WorkerEventKind::State, "worker run started");
    }

    /// 以终态收口（幂等：已终态不覆盖）。
    pub fn finish(&mut self, status: WorkerRunStatus, summary: Option<String>) {
        if self.status.is_terminal() {
            return;
        }
        self.status = status;
        self.summary = summary.map(|value| truncate_chars(&value, MAX_WORKER_SUMMARY_CHARS));
        self.finished_at = Some(Timestamp::now());
        if let (Some(started), Some(finished)) = (self.started_at, self.finished_at) {
            self.duration_ms = Some(finished.elapsed_milliseconds_since(&started));
        }
        let message = match status {
            WorkerRunStatus::Succeeded => "worker run succeeded".to_string(),
            WorkerRunStatus::Failed => "worker run failed".to_string(),
            WorkerRunStatus::Timeout => "worker run timed out".to_string(),
            WorkerRunStatus::Cancelled => "worker run cancelled".to_string(),
            WorkerRunStatus::Pending | WorkerRunStatus::Running => {
                format!("worker run state: {}", status.as_str())
            }
        };
        let kind = match status {
            WorkerRunStatus::Failed | WorkerRunStatus::Timeout => WorkerEventKind::Error,
            _ => WorkerEventKind::State,
        };
        self.record_event(kind, message);
    }
}

/// driver 对外部 runtime 的单次调用审计记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerInvocation {
    /// 实体 ID（`wkinv_` 前缀）。
    pub id: String,
    /// 所属 `Project`（probe 调用可为 None）。
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    /// 关联的 worker run（probe 调用可为 None）。
    #[serde(default)]
    pub worker_run_id: Option<String>,
    /// runtime 类型。
    pub runtime: WorkerRuntimeType,
    /// 使用的 Connection id（**不含密钥**）。
    #[serde(default)]
    pub connection_id: Option<String>,
    /// 实际使用的模型。
    #[serde(default)]
    pub model: Option<String>,
    /// 调用目的。
    pub purpose: WorkerInvocationPurpose,
    /// 结果状态。
    pub status: WorkerRunStatus,
    /// 有界摘要（受控 observation）。
    #[serde(default)]
    pub summary: Option<String>,
    /// 失败原因（有界、脱敏）。
    #[serde(default)]
    pub error: Option<String>,
    /// 退出码。
    #[serde(default)]
    pub exit_code: Option<i64>,
    /// 开始时间。
    pub started_at: Timestamp,
    /// 结束时间。
    #[serde(default)]
    pub finished_at: Option<Timestamp>,
    /// 时长毫秒。
    #[serde(default)]
    pub duration_ms: Option<i64>,
}

impl WorkerInvocation {
    /// 构造一条调用审计记录。
    #[must_use]
    pub fn new(
        runtime: WorkerRuntimeType,
        purpose: WorkerInvocationPurpose,
        worker_run_id: Option<String>,
    ) -> Self {
        Self {
            id: new_id("wkinv"),
            project_id: None,
            worker_run_id,
            runtime,
            connection_id: None,
            model: None,
            purpose,
            status: WorkerRunStatus::Running,
            summary: None,
            error: None,
            exit_code: None,
            started_at: Timestamp::now(),
            finished_at: None,
            duration_ms: None,
        }
    }

    /// 以终态收口（幂等）。
    pub fn finish(&mut self, status: WorkerRunStatus) {
        if self.status.is_terminal() {
            return;
        }
        self.status = status;
        self.finished_at = Some(Timestamp::now());
        self.duration_ms = self
            .finished_at
            .map(|finished| finished.elapsed_milliseconds_since(&self.started_at));
    }
}

// ---------------------------------------------------------------------------
// Worker Profile / Connection（认证与模型配置，复用 Provider 体系）
// ---------------------------------------------------------------------------

/// Agent CLI 的执行位置。**只表示执行位置，不表示认证来源**——两种模式
/// 的 API URL / Key / Model 都必须来自 Lynceus 配置的 Connection。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerExecutionEnvironment {
    /// 宿主机直接运行 Agent CLI。
    Local,
    /// 容器内运行（第一阶段未实现，显式 Unsupported）。
    Container,
}

impl WorkerExecutionEnvironment {
    /// wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Container => "container",
        }
    }
}

/// Worker Runtime Profile：runtime → Connection 的绑定与执行约束。
///
/// 认证链路（不可绕过）：Agent CLI 子进程**只**使用本 Profile 绑定的
/// Lynceus Connection（API Base URL / Secret / Model）。未绑定有效
/// Connection 的 runtime 状态必须是 NotReady（Configuration Required），
/// 绝不回退用户本机的 CLI 登录 / OAuth / shell 环境变量。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerRuntimeProfile {
    /// 实体 ID（`wkprof_` 前缀）。
    pub id: String,
    /// runtime 类型（每个类型至多一个启用 Profile）。
    pub runtime_type: WorkerRuntimeType,
    /// 绑定的 Connection（复用 `ProviderConfig` 的 id）。
    pub connection_id: String,
    /// 模型覆盖（优先于 Connection 默认模型）。
    #[serde(default)]
    pub model_override: Option<String>,
    /// 执行位置。
    pub execution_environment: WorkerExecutionEnvironment,
    /// 该 runtime 的最大并发会话数（≥1）。
    pub max_concurrency: u32,
    /// 单次调用超时秒数（run config 可覆盖）。
    pub timeout_seconds: u64,
    /// adapter 特定选项（adapter 声明可接受键，未知键拒绝）。
    #[serde(default)]
    pub runtime_options: Map<String, Value>,
    /// 是否启用。
    pub enabled: bool,
    /// 创建时间。
    pub created_at: Timestamp,
    /// 更新时间。
    pub updated_at: Timestamp,
}

impl WorkerRuntimeProfile {
    /// 构造 Profile 并校验不变量。
    ///
    /// # Errors
    /// `connection_id` 为空、`max_concurrency` 为 0 或 `timeout_seconds` 为 0。
    pub fn new(
        runtime_type: WorkerRuntimeType,
        connection_id: impl Into<String>,
        execution_environment: WorkerExecutionEnvironment,
        max_concurrency: u32,
        timeout_seconds: u64,
    ) -> Result<Self, WorkerProfileError> {
        let connection_id = connection_id.into();
        if connection_id.trim().is_empty() {
            return Err(WorkerProfileError(
                "connection_id must be non-empty".to_string(),
            ));
        }
        if max_concurrency == 0 {
            return Err(WorkerProfileError(
                "max_concurrency must be >= 1".to_string(),
            ));
        }
        if timeout_seconds == 0 {
            return Err(WorkerProfileError(
                "timeout_seconds must be >= 1".to_string(),
            ));
        }
        Ok(Self {
            id: new_id("wkprof"),
            runtime_type,
            connection_id,
            model_override: None,
            execution_environment,
            max_concurrency,
            timeout_seconds,
            runtime_options: Map::new(),
            enabled: true,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        })
    }

    /// 生效模型：override 优先，回退 Connection 默认模型。
    #[must_use]
    pub fn effective_model<'a>(
        &'a self,
        connection_default_model: Option<&'a str>,
    ) -> Option<&'a str> {
        self.model_override
            .as_deref()
            .filter(|model| !model.trim().is_empty())
            .or(connection_default_model)
    }
}

/// Profile 校验错误。
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct WorkerProfileError(pub String);

/// 已解析 Connection 的**脱敏视图**（secret 已解析但绝不序列化/展示）。
///
/// 由 driver 在启动子进程前短暂持有；进入日志、事件、UI 的任何路径都
/// 必须改用 [`WorkerConnectionView`]。
#[derive(Debug, Clone)]
pub struct ResolvedWorkerConnection {
    /// Connection（`ProviderConfig`）id。
    pub connection_id: String,
    /// 协议族（ProviderType）。
    pub protocol: ProviderType,
    /// API Base URL。
    pub base_url: Option<String>,
    /// 已解析的 API Key（**敏感：仅启动注入，绝不落日志**）。
    pub api_key: Option<String>,
    /// Connection 默认模型。
    pub default_model: Option<String>,
}

/// Connection 的公开视图（API 列表 / UI / 审计），**永不携带密钥**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerConnectionView {
    /// Connection id。
    pub connection_id: String,
    /// 协议族。
    pub protocol: ProviderType,
    /// 是否配置了 Base URL。
    pub has_base_url: bool,
    /// 是否配置了可解析密钥（只回显布尔，绝不回显值）。
    pub has_api_key: bool,
    /// 默认模型。
    #[serde(default)]
    pub default_model: Option<String>,
    /// Connection 是否启用。
    pub enabled: bool,
}

impl ResolvedWorkerConnection {
    /// 投影为不含密钥的公开视图。
    #[must_use]
    pub fn view(&self, enabled: bool) -> WorkerConnectionView {
        WorkerConnectionView {
            connection_id: self.connection_id.clone(),
            protocol: self.protocol,
            has_base_url: self
                .base_url
                .as_deref()
                .is_some_and(|url| !url.trim().is_empty()),
            has_api_key: self.api_key.is_some(),
            default_model: self.default_model.clone(),
            enabled,
        }
    }
}

fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_type_wire_values_are_stable() {
        crate::testutil::assert_wire_values(&[
            (WorkerRuntimeType::Codex, "codex"),
            (WorkerRuntimeType::ClaudeCode, "claude_code"),

            (WorkerRuntimeType::DeepSeekHarness, "deepseek_harness"),
        ]);
    }

    #[test]
    fn availability_wire_values_are_stable() {
        crate::testutil::assert_wire_values(&[
            (WorkerAvailability::Available, "available"),
            (WorkerAvailability::NotInstalled, "not_installed"),
            (WorkerAvailability::Unavailable, "unavailable"),
            (WorkerAvailability::Unsupported, "unsupported"),
            (WorkerAvailability::NotReady, "not_ready"),
            (WorkerAvailability::Error, "error"),
        ]);
    }

    #[test]
    fn run_status_wire_values_and_terminality() {
        crate::testutil::assert_wire_values(&[
            (WorkerRunStatus::Pending, "pending"),
            (WorkerRunStatus::Running, "running"),
            (WorkerRunStatus::Succeeded, "succeeded"),
            (WorkerRunStatus::Failed, "failed"),
            (WorkerRunStatus::Timeout, "timeout"),
            (WorkerRunStatus::Cancelled, "cancelled"),
        ]);
        assert!(!WorkerRunStatus::Running.is_terminal());
        assert!(WorkerRunStatus::Timeout.is_terminal());
    }

    #[test]
    fn finish_is_idempotent_and_only_transitions_non_terminal() {
        // 暂停 mission 时 `finalize_inflight_worker_runs` 依赖这条契约：
        // Running/Pending → Cancelled 收口成功；已终态不被二次 finish 覆盖
        // （幂等），否则迟到的正常收口会被暂停动作改写。
        let mut run = WorkerRun::new(
            ProjectId::new("proj_finish".to_string()),
            WorkerRuntimeType::ClaudeCode,
            "audit this target",
        );
        run.mark_started();
        assert_eq!(run.status, WorkerRunStatus::Running);

        run.finish(WorkerRunStatus::Cancelled, Some("mission paused".to_string()));
        assert_eq!(run.status, WorkerRunStatus::Cancelled);
        assert!(run.finished_at.is_some());
        assert_eq!(run.summary.as_deref(), Some("mission paused"));

        // 已终态：再次 finish（含误传 Running）不覆盖状态、不清空 finished_at。
        let finished_at = run.finished_at;
        run.finish(WorkerRunStatus::Running, Some("late settle".to_string()));
        assert_eq!(run.status, WorkerRunStatus::Cancelled);
        assert_eq!(run.finished_at, finished_at);
    }

    #[test]
    fn worker_run_records_bounded_events() {
        let mut run = WorkerRun::new(
            ProjectId::new("proj_worker".to_string()),
            WorkerRuntimeType::ClaudeCode,
            "audit this target",
        );
        run.mark_started();
        for index in 0..(MAX_WORKER_EVENTS_PER_RUN + 10) {
            run.record_event(WorkerEventKind::Output, format!("line {index}"));
        }
        assert_eq!(run.events.len(), MAX_WORKER_EVENTS_PER_RUN);
        assert!(run.events.last().is_some_and(|event| {
            event.kind == WorkerEventKind::Notice && event.message.contains("truncated")
        }));
        // 长文本截断。
        let long = "x".repeat(MAX_WORKER_EVENT_CHARS + 100);
        run.record_event(WorkerEventKind::Output, long);
        let last = run.events.last().expect("event must exist");
        assert!(last.message.chars().count() <= MAX_WORKER_EVENT_CHARS + 1);
        run.finish(WorkerRunStatus::Succeeded, Some("done".to_string()));
        assert!(run.status.is_terminal());
        assert!(run.finished_at.is_some());
        // 幂等收口。
        run.finish(WorkerRunStatus::Failed, None);
        assert_eq!(run.status, WorkerRunStatus::Succeeded);
    }

    #[test]
    fn worker_runtime_profile_validates_invariants() {
        let profile = WorkerRuntimeProfile::new(
            WorkerRuntimeType::ClaudeCode,
            "prov_anthropic",
            WorkerExecutionEnvironment::Local,
            2,
            900,
        )
        .expect("valid profile");
        assert_eq!(
            profile.effective_model(Some("claude-sonnet-5")),
            Some("claude-sonnet-5")
        );
        let mut override_profile = profile.clone();
        override_profile.model_override = Some("claude-opus-5".to_string());
        assert_eq!(
            override_profile.effective_model(Some("claude-sonnet-5")),
            Some("claude-opus-5")
        );
        for (connection_id, concurrency, timeout) in
            [("", 1, 1), ("prov_x", 0, 1), ("prov_x", 1, 0)]
        {
            assert!(
                WorkerRuntimeProfile::new(
                    WorkerRuntimeType::Codex,
                    connection_id,
                    WorkerExecutionEnvironment::Local,
                    concurrency,
                    timeout,
                )
                .is_err(),
                "invalid profile must be rejected"
            );
        }
        // Connection 视图永不携带密钥。
        let resolved = ResolvedWorkerConnection {
            connection_id: "prov_anthropic".to_string(),
            protocol: ProviderType::Anthropic,
            base_url: Some("https://api.anthropic.com".to_string()),
            api_key: Some("sk-ant-secret".to_string()),
            default_model: None,
        };
        let json = serde_json::to_string(&resolved.view(true)).expect("view must serialize");
        assert!(!json.contains("sk-ant-secret"), "secret must never leak");
    }
}
