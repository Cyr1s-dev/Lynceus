//! 持久执行控制面模型 —— `server/core/models/execution.py` 的移植。
//!
//! 这些类型只描述“请求执行什么”和“实际发生了什么”。任意命令是否可执行
//! 由受限后端决定；模型中保留 `command` 并不授权 shell 执行。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::common::{Timestamp, new_id, utcnow};
use crate::ids::{
    BranchId, ExecutionArtifactId, ExecutionId, ExecutionResultId, MissionId, ProjectId, RunId,
    TaskId, ToolInvocationId,
};
use crate::retrieval::ArtifactKind;

/// 可插拔执行后端家族。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionBackendType {
    /// 本进程安全适配器。
    Local,
    /// Docker 容器。
    Docker,
    /// Kubernetes Job。
    KubernetesJob,
    /// Argo Workflow。
    ArgoWorkflow,
    /// Nomad 作业。
    Nomad,
    /// 云批处理服务。
    CloudBatch,
}

impl ExecutionBackendType {
    /// 稳定 wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Docker => "docker",
            Self::KubernetesJob => "kubernetes_job",
            Self::ArgoWorkflow => "argo_workflow",
            Self::Nomad => "nomad",
            Self::CloudBatch => "cloud_batch",
        }
    }
}

/// 一次执行的生命周期与结局。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    /// 已持久化并等待 worker。
    Queued,
    /// 后端已接单但尚未运行。
    Pending,
    /// 正在运行。
    Running,
    /// 已由真实后端成功完成。
    Succeeded,
    /// 执行失败。
    Failed,
    /// 后端报告软超时。
    Timeout,
    /// 控制面强制终止的硬超时。
    HardTimeout,
    /// 用户或控制面取消。
    Cancelled,
    /// 安全策略拒绝执行。
    Denied,
    /// 所属进程消失，无法继续监督。
    Orphaned,
}

impl ExecutionStatus {
    /// 稳定 wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
            Self::HardTimeout => "hard_timeout",
            Self::Cancelled => "cancelled",
            Self::Denied => "denied",
            Self::Orphaned => "orphaned",
        }
    }

    /// 是否为不可继续迁移的终态。
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::Failed
                | Self::Timeout
                | Self::HardTimeout
                | Self::Cancelled
                | Self::Denied
                | Self::Orphaned
        )
    }
}

fn default_execution_artifact_id() -> ExecutionArtifactId {
    ExecutionArtifactId::new(new_id("artifact"))
}

/// 工具或沙箱产生的工件引用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionArtifact {
    /// 工件 ID。
    #[serde(default = "default_execution_artifact_id")]
    pub id: ExecutionArtifactId,
    /// 所属 Project。
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    /// 所属 Run。
    #[serde(default)]
    pub run_id: Option<RunId>,
    /// 所属执行。
    #[serde(default)]
    pub execution_id: Option<ExecutionId>,
    /// 工件类型。
    #[serde(default = "default_artifact_kind")]
    pub kind: ArtifactKind,
    /// 受控存储路径。
    pub path: String,
    /// 脱敏摘要。
    #[serde(default)]
    pub summary: String,
    /// MIME 类型。
    #[serde(default)]
    pub mime_type: Option<String>,
    /// 字节大小。
    #[serde(default)]
    pub size_bytes: Option<i64>,
    /// 内容 SHA-256。
    #[serde(default)]
    pub sha256: Option<String>,
    /// 创建时间。
    #[serde(default = "utcnow")]
    pub created_at: Timestamp,
}

const fn default_artifact_kind() -> ArtifactKind {
    ArtifactKind::Other
}

/// 可选资源限制。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ResourceLimits {
    /// CPU 核数上限。
    #[serde(default)]
    pub cpu_cores: Option<f64>,
    /// 内存上限（MiB）。
    #[serde(default)]
    pub memory_mb: Option<i64>,
    /// 磁盘上限（MiB）。
    #[serde(default)]
    pub disk_mb: Option<i64>,
    /// 是否允许网络。
    #[serde(default)]
    pub network_enabled: Option<bool>,
}

fn default_execution_id() -> ExecutionId {
    ExecutionId::new(new_id("exec"))
}

const fn default_timeout_seconds() -> u64 {
    60
}

fn default_backend_type() -> ExecutionBackendType {
    ExecutionBackendType::Local
}

/// 一次结构化工具执行请求。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionRequest {
    /// 稳定执行 ID；同时作为 durable job ID。
    #[serde(default = "default_execution_id")]
    pub id: ExecutionId,
    /// 所属 Project。
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    /// 所属 Mission。
    #[serde(default)]
    pub mission_id: Option<MissionId>,
    /// 所属 Branch。
    #[serde(default)]
    pub branch_id: Option<BranchId>,
    /// 所属 Run。
    #[serde(default)]
    pub run_id: Option<RunId>,
    /// 所属 Task。
    #[serde(default)]
    pub task_id: Option<TaskId>,
    /// 可复用沙箱会话 ID。
    #[serde(default)]
    pub session_id: Option<String>,
    /// 请求所有者。
    #[serde(default)]
    pub owner_id: Option<String>,
    /// 适配器工具名。
    pub tool_name: String,
    /// 后端类型。
    #[serde(default = "default_backend_type")]
    pub backend_type: ExecutionBackendType,
    /// 首选的结构化参数。
    #[serde(default)]
    pub args: Map<String, Value>,
    /// 为未来隔离后端保留的命令；安全本地后端绝不执行它。
    #[serde(default)]
    pub command: Vec<String>,
    /// 控制面硬超时（秒）。
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    /// 可选资源限制。
    #[serde(default)]
    pub resource_limits: Option<ResourceLimits>,
    /// 预期工件路径。
    #[serde(default)]
    pub artifact_paths: Vec<String>,
    /// 附加元数据。
    #[serde(default)]
    pub metadata: Map<String, Value>,
    /// 创建时间。
    #[serde(default = "utcnow")]
    pub created_at: Timestamp,
}

impl ExecutionRequest {
    /// 以 Python 默认值构造请求。
    #[must_use]
    pub fn new(tool_name: String) -> Self {
        Self {
            id: default_execution_id(),
            project_id: None,
            mission_id: None,
            branch_id: None,
            run_id: None,
            task_id: None,
            session_id: None,
            owner_id: None,
            tool_name,
            backend_type: default_backend_type(),
            args: Map::new(),
            command: Vec::new(),
            timeout_seconds: default_timeout_seconds(),
            resource_limits: None,
            artifact_paths: Vec::new(),
            metadata: Map::new(),
            created_at: utcnow(),
        }
    }
}

fn default_execution_result_id() -> ExecutionResultId {
    ExecutionResultId::new(new_id("execres"))
}

/// 后端返回的结构化结局。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionResult {
    /// 结果 ID。
    #[serde(default = "default_execution_result_id")]
    pub id: ExecutionResultId,
    /// 原请求 ID。
    pub request_id: ExecutionId,
    /// 所属 Project。
    #[serde(default)]
    pub project_id: Option<ProjectId>,
    /// 所属 Run。
    #[serde(default)]
    pub run_id: Option<RunId>,
    /// 所属 Task。
    #[serde(default)]
    pub task_id: Option<TaskId>,
    /// 会话 ID。
    #[serde(default)]
    pub session_id: Option<String>,
    /// 工具名。
    pub tool_name: String,
    /// 实际后端。
    pub backend_type: ExecutionBackendType,
    /// 后端结局。
    pub status: ExecutionStatus,
    /// 脱敏标准输出摘要。
    #[serde(default)]
    pub stdout_summary: String,
    /// 脱敏标准错误摘要。
    #[serde(default)]
    pub stderr_summary: String,
    /// 退出码。
    #[serde(default)]
    pub exit_code: Option<i64>,
    /// 错误摘要。
    #[serde(default)]
    pub error: Option<String>,
    /// 工件列表。
    #[serde(default)]
    pub artifacts: Vec<ExecutionArtifact>,
    /// 开始时间。
    #[serde(default = "utcnow")]
    pub started_at: Timestamp,
    /// 结束时间。
    #[serde(default)]
    pub completed_at: Option<Timestamp>,
}

fn default_execution_status() -> ExecutionStatus {
    ExecutionStatus::Queued
}

const fn default_max_attempts() -> u8 {
    1
}

const fn default_version() -> u64 {
    1
}

/// 一次有界后端执行的持久状态。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionJob {
    /// 与 request 共用的稳定 ID。
    #[serde(default = "default_execution_id")]
    pub id: ExecutionId,
    /// 已脱敏的持久请求视图。
    pub request: ExecutionRequest,
    /// 原始请求的稳定 SHA-256。
    pub request_digest: String,
    /// 当前状态。
    #[serde(default = "default_execution_status")]
    pub status: ExecutionStatus,
    /// 请求所有者。
    #[serde(default)]
    pub owner_id: Option<String>,
    /// 失败后是否允许自动重试。
    #[serde(default)]
    pub safe_to_retry: bool,
    /// 幂等键。
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// 已尝试次数。
    #[serde(default)]
    pub attempts: u8,
    /// 最大尝试次数。
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u8,
    /// 是否已请求取消。
    #[serde(default)]
    pub cancel_requested: bool,
    /// 脱敏取消说明。
    #[serde(default)]
    pub cancel_note: Option<String>,
    /// 有界进度摘要。
    #[serde(default)]
    pub partial_output_summary: String,
    /// 最终结果。
    #[serde(default)]
    pub result: Option<ExecutionResult>,
    /// 关联的工具调用审计 ID。
    #[serde(default)]
    pub tool_invocation_id: Option<ToolInvocationId>,
    /// 脱敏错误。
    #[serde(default)]
    pub error: Option<String>,
    /// 乐观版本号。
    #[serde(default = "default_version")]
    pub version: u64,
    /// 提交时间。
    #[serde(default = "utcnow")]
    pub submitted_at: Timestamp,
    /// 首次开始时间。
    #[serde(default)]
    pub started_at: Option<Timestamp>,
    /// 最近心跳。
    #[serde(default)]
    pub heartbeat_at: Option<Timestamp>,
    /// 结束时间。
    #[serde(default)]
    pub completed_at: Option<Timestamp>,
    /// 控制面元数据。
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

impl ExecutionJob {
    /// 从已脱敏请求构造 queued job。
    #[must_use]
    pub fn queued(request: ExecutionRequest, request_digest: String) -> Self {
        Self {
            id: request.id.clone(),
            owner_id: request.owner_id.clone(),
            request,
            request_digest,
            status: default_execution_status(),
            safe_to_retry: false,
            idempotency_key: None,
            attempts: 0,
            max_attempts: default_max_attempts(),
            cancel_requested: false,
            cancel_note: None,
            partial_output_summary: String::new(),
            result: None,
            tool_invocation_id: None,
            error: None,
            version: default_version(),
            submitted_at: utcnow(),
            started_at: None,
            heartbeat_at: None,
            completed_at: None,
            metadata: Map::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::assert_wire_values;

    #[test]
    fn execution_enums_match_python_wire_values() {
        assert_wire_values(&[
            (ExecutionBackendType::Local, "local"),
            (ExecutionBackendType::Docker, "docker"),
            (ExecutionBackendType::KubernetesJob, "kubernetes_job"),
            (ExecutionBackendType::ArgoWorkflow, "argo_workflow"),
            (ExecutionBackendType::Nomad, "nomad"),
            (ExecutionBackendType::CloudBatch, "cloud_batch"),
        ]);
        assert_wire_values(&[
            (ExecutionStatus::Queued, "queued"),
            (ExecutionStatus::Pending, "pending"),
            (ExecutionStatus::Running, "running"),
            (ExecutionStatus::Succeeded, "succeeded"),
            (ExecutionStatus::Failed, "failed"),
            (ExecutionStatus::Timeout, "timeout"),
            (ExecutionStatus::HardTimeout, "hard_timeout"),
            (ExecutionStatus::Cancelled, "cancelled"),
            (ExecutionStatus::Denied, "denied"),
            (ExecutionStatus::Orphaned, "orphaned"),
        ]);
    }

    #[test]
    fn request_defaults_are_safe_and_match_contract() {
        let request: ExecutionRequest = serde_json::from_str(r#"{"tool_name":"scanner"}"#)
            .unwrap_or_else(|error| panic!("最小请求应可解析: {error}"));
        assert_eq!(request.backend_type, ExecutionBackendType::Local);
        assert_eq!(request.timeout_seconds, 60);
        assert!(request.command.is_empty());
        assert!(request.id.as_str().starts_with("exec_"));
    }

    #[test]
    fn execution_status_terminal_set_is_explicit() {
        assert!(!ExecutionStatus::Queued.is_terminal());
        assert!(!ExecutionStatus::Running.is_terminal());
        assert!(ExecutionStatus::Succeeded.is_terminal());
        assert!(ExecutionStatus::Denied.is_terminal());
        assert!(ExecutionStatus::Orphaned.is_terminal());
    }
}
