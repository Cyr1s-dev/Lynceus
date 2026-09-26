//! `AuditEvent` —— `server/core/models/event.py` 的移植。
//!
//! 事件是追加只写的**过程日志**，不是审计状态的真源（Fact / Evidence /
//! Finding / `ToolInvocation` 才是规范实体）：给前端稳定的类型化"刚发生
//! 了什么"流，为编排决策留轻量审计痕迹，支撑 SSE/轮询实时 UX。

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::common::Timestamp;
use crate::common::new_id;
use crate::ids::ProjectId;
use crate::ids::RunId;
use crate::ids::TaskId;
use crate::ids::ToolInvocationId;

/// 审计事件流的稳定事件类型词表（`AuditEventType`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    /// Project 创建。
    ProjectCreated,
    /// Run 启动。
    RunStarted,
    /// Run 完成。
    RunCompleted,
    /// Run 失败。
    RunFailed,
    /// Run 暂停。
    RunPaused,
    /// Run 等待决策。
    RunWaitingForDecision,
    /// Run 恢复。
    RunResumed,
    /// Run 取消。
    RunCancelled,
    /// 决策门创建。
    DecisionGateCreated,
    /// 决策门被回答。
    DecisionGateAnswered,
    /// 决策门被取消。
    DecisionGateCancelled,
    /// 决策门过期。
    DecisionGateExpired,
    /// Task 创建。
    TaskCreated,
    /// Task 启动。
    TaskStarted,
    /// Task 成功。
    TaskSucceeded,
    /// Task 失败。
    TaskFailed,
    /// 求解器启动。
    SolverStarted,
    /// 求解器完成。
    SolverCompleted,
    /// 求解器失败。
    SolverFailed,
    /// 工具调用完成。
    ToolInvocationCompleted,
    /// 工具调用等待确认。
    ToolInvocationWaitingForConfirmation,
    /// 工具调用失败。
    ToolInvocationFailed,
    /// 资产被发现。
    AssetDiscovered,
    /// 证据已加入。
    EvidenceAdded,
    /// 发现已加入。
    FindingAdded,
    /// Finding 被人工 triage（状态/严重度改写）。
    FindingTriageUpdated,
    /// Observer 评审完成。
    ObserverReviewed,
    /// SARIF 导出。
    SarifExported,
    /// 用户备注。
    UserNote,
}

impl AuditEventType {
    /// wire 值（Python `.value` 镜像，用于文本拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            AuditEventType::ProjectCreated => "project_created",
            AuditEventType::RunStarted => "run_started",
            AuditEventType::RunCompleted => "run_completed",
            AuditEventType::RunFailed => "run_failed",
            AuditEventType::RunPaused => "run_paused",
            AuditEventType::RunWaitingForDecision => "run_waiting_for_decision",
            AuditEventType::RunResumed => "run_resumed",
            AuditEventType::RunCancelled => "run_cancelled",
            AuditEventType::DecisionGateCreated => "decision_gate_created",
            AuditEventType::DecisionGateAnswered => "decision_gate_answered",
            AuditEventType::DecisionGateCancelled => "decision_gate_cancelled",
            AuditEventType::DecisionGateExpired => "decision_gate_expired",
            AuditEventType::TaskCreated => "task_created",
            AuditEventType::TaskStarted => "task_started",
            AuditEventType::TaskSucceeded => "task_succeeded",
            AuditEventType::TaskFailed => "task_failed",
            AuditEventType::SolverStarted => "solver_started",
            AuditEventType::SolverCompleted => "solver_completed",
            AuditEventType::SolverFailed => "solver_failed",
            AuditEventType::ToolInvocationCompleted => "tool_invocation_completed",
            AuditEventType::ToolInvocationWaitingForConfirmation => {
                "tool_invocation_waiting_for_confirmation"
            }
            AuditEventType::ToolInvocationFailed => "tool_invocation_failed",
            AuditEventType::AssetDiscovered => "asset_discovered",
            AuditEventType::EvidenceAdded => "evidence_added",
            AuditEventType::FindingAdded => "finding_added",
            AuditEventType::FindingTriageUpdated => "finding_triage_updated",
            AuditEventType::ObserverReviewed => "observer_reviewed",
            AuditEventType::SarifExported => "sarif_exported",
            AuditEventType::UserNote => "user_note",
        }
    }
}

fn default_event_id() -> String {
    new_id("event")
}

impl AuditEvent {
    /// 以 Python 默认值构造（`AuditEvent(project_id=..., type=..., actor=...,
    /// title=...)`，其余字段取模型默认）。
    #[must_use]
    pub fn new(
        project_id: ProjectId,
        event_type: AuditEventType,
        actor: String,
        title: String,
    ) -> Self {
        Self {
            id: default_event_id(),
            project_id,
            run_id: None,
            task_id: None,
            tool_invocation_id: None,
            event_type,
            actor,
            title,
            message: None,
            severity: None,
            status: None,
            data: Map::new(),
            created_at: crate::common::utcnow(),
        }
    }
}

/// 过程日志中的单条审计事件（`AuditEvent`）。
///
/// 轻量、追加只写，绝不替代核心领域实体。ID 在 Python 侧是普通字符串
/// （`new_id("event")`），此处保持字符串以对齐 wire 格式。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditEvent {
    /// 事件标识符。
    #[serde(default = "default_event_id")]
    pub id: String,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Run。
    #[serde(default)]
    pub run_id: Option<RunId>,
    /// 关联 Task。
    #[serde(default)]
    pub task_id: Option<TaskId>,
    /// 关联 `ToolInvocation`。
    #[serde(default)]
    pub tool_invocation_id: Option<ToolInvocationId>,
    /// 事件类型。
    #[serde(rename = "type")]
    pub event_type: AuditEventType,
    /// 行为者。
    pub actor: String,
    /// 标题。
    pub title: String,
    /// 消息。
    #[serde(default)]
    pub message: Option<String>,
    /// 严重级别（自由字符串）。
    #[serde(default)]
    pub severity: Option<String>,
    /// 状态（自由字符串）。
    #[serde(default)]
    pub status: Option<String>,
    /// 结构化数据（键序 = 插入序）。
    #[serde(default)]
    pub data: Map<String, Value>,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::assert_wire_values;

    #[test]
    fn audit_event_type_matches_python_wire_values() {
        assert_wire_values(&[
            (AuditEventType::ProjectCreated, "project_created"),
            (AuditEventType::RunStarted, "run_started"),
            (AuditEventType::RunCompleted, "run_completed"),
            (AuditEventType::RunFailed, "run_failed"),
            (AuditEventType::RunPaused, "run_paused"),
            (
                AuditEventType::RunWaitingForDecision,
                "run_waiting_for_decision",
            ),
            (AuditEventType::RunResumed, "run_resumed"),
            (AuditEventType::RunCancelled, "run_cancelled"),
            (AuditEventType::DecisionGateCreated, "decision_gate_created"),
            (
                AuditEventType::DecisionGateAnswered,
                "decision_gate_answered",
            ),
            (
                AuditEventType::DecisionGateCancelled,
                "decision_gate_cancelled",
            ),
            (AuditEventType::DecisionGateExpired, "decision_gate_expired"),
            (AuditEventType::TaskCreated, "task_created"),
            (AuditEventType::TaskStarted, "task_started"),
            (AuditEventType::TaskSucceeded, "task_succeeded"),
            (AuditEventType::TaskFailed, "task_failed"),
            (AuditEventType::SolverStarted, "solver_started"),
            (AuditEventType::SolverCompleted, "solver_completed"),
            (AuditEventType::SolverFailed, "solver_failed"),
            (
                AuditEventType::ToolInvocationCompleted,
                "tool_invocation_completed",
            ),
            (
                AuditEventType::ToolInvocationWaitingForConfirmation,
                "tool_invocation_waiting_for_confirmation",
            ),
            (
                AuditEventType::ToolInvocationFailed,
                "tool_invocation_failed",
            ),
            (AuditEventType::AssetDiscovered, "asset_discovered"),
            (AuditEventType::EvidenceAdded, "evidence_added"),
            (AuditEventType::FindingAdded, "finding_added"),
            (AuditEventType::ObserverReviewed, "observer_reviewed"),
            (AuditEventType::SarifExported, "sarif_exported"),
            (AuditEventType::UserNote, "user_note"),
        ]);
    }

    #[test]
    fn audit_event_serializes_with_type_key() {
        let event = AuditEvent {
            id: "event_x".to_string(),
            project_id: ProjectId::new("proj_test".to_string()),
            run_id: None,
            task_id: None,
            tool_invocation_id: None,
            event_type: AuditEventType::UserNote,
            actor: "manager".to_string(),
            title: "t".to_string(),
            message: None,
            severity: None,
            status: None,
            data: Map::new(),
            created_at: "2026-08-26T00:00:00Z"
                .parse()
                .unwrap_or_else(|error| panic!("固定时间必须可解析: {error}")),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], serde_json::json!("user_note"));
        assert_eq!(json["actor"], serde_json::json!("manager"));
    }

    #[test]
    fn audit_event_rejects_unknown_fields() {
        let result: Result<AuditEvent, _> = serde_json::from_str(
            r#"{"project_id":"p","actor":"a","title":"t","type":"user_note","extra":1}"#,
        );
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }
}
