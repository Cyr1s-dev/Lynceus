//! Agent 叙事事件 —— `server/core/models/agent.py` `AgentNarrativeEvent`
//! 群的 Rust 镜像（`contracts/openapi.json` 同名 schema）。
//!
//! 「人读的 agent 注记」：`original_text` 原文不可变保留；翻译/展示文本
//! 走独立字段（当前无翻译管线，读取方回退原文）。物理布局为通用
//! payload 表 `agent_narrative_events`（scope 列 `project_id`/`run_id`，
//! `audit_run_id` 映射 `run_id` 列）。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::common::{Timestamp, new_id, utcnow};
use crate::ids::{AgentNarrativeEventId, BranchId, MissionId, ProjectId, RunId, TaskId};

/// 人类可读的 agent 叙事类别（Python `AgentNarrativeEventKind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentNarrativeEventKind {
    /// 执行进度注记。
    Progress,
    /// 推理摘要。
    ReasoningSummary,
    /// 失败分析。
    FailureAnalysis,
    /// 下一步行动。
    NextAction,
    /// 观察者注记。
    ObserverNote,
    /// 顾问注记。
    AdvisorNote,
    /// 反思者注记。
    ReflectorNote,
    /// Worker 终稿：一次派发执行完毕后回给用户的那一句结论。
    ///
    /// 与 [`Self::Progress`] 的区别是语义而非 severity：Progress 是执行
    /// 痕迹（"派发了 X"），WorkerSummary 是**产出**（"做完了，结论是…"）。
    /// settlement 契约：任何一次 run 结束（含超时 / 失败 / 预算
    /// 耗尽）都必须有一条用户可见的结论文本，否则跑完了却什么都没展示。
    WorkerSummary,
}

/// 人读 agent 注记（Python `AgentNarrativeEvent`）：原文不可变，展示层
/// 演进不回写原文。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentNarrativeEvent {
    /// 事件标识符。
    #[serde(default = "default_event_id")]
    pub id: AgentNarrativeEventId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Run（run 外的生命周期注记为 `None`）。
    #[serde(default)]
    pub audit_run_id: Option<RunId>,
    /// 所属 Mission。
    #[serde(default)]
    pub mission_id: Option<MissionId>,
    /// 所属 Branch。
    #[serde(default)]
    pub branch_id: Option<BranchId>,
    /// 关联 Task。
    #[serde(default)]
    pub task_id: Option<TaskId>,
    /// 产生注记的 agent 名（solver / 角色名）。
    pub source_agent: String,
    /// 叙事类别。
    pub event_kind: AgentNarrativeEventKind,
    /// 原文（不可变）。
    pub original_text: String,
    /// 原文语言（BCP-47 提示，可缺省）。
    #[serde(default)]
    pub original_language: Option<String>,
    /// 产生注记的模型调用审计 id。
    #[serde(default)]
    pub model_invocation_id: Option<String>,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
    /// 附加元数据（键序 = 插入序）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

/// 创建请求（Python `CreateAgentNarrativeRequest`）：`id` 与 `created_at`
/// 由服务端生成，调用方不可伪造。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateAgentNarrativeRequest {
    /// 所属 Run。
    #[serde(default)]
    pub audit_run_id: Option<RunId>,
    /// 所属 Branch。
    #[serde(default)]
    pub branch_id: Option<BranchId>,
    /// 叙事类别。
    pub event_kind: AgentNarrativeEventKind,
    /// 附加元数据。
    #[serde(default)]
    pub metadata: Map<String, Value>,
    /// 所属 Mission。
    #[serde(default)]
    pub mission_id: Option<MissionId>,
    /// 原文语言。
    #[serde(default)]
    pub original_language: Option<String>,
    /// 原文（不可变）。
    pub original_text: String,
    /// 产生注记的 agent 名。
    pub source_agent: String,
    /// 关联 Task。
    #[serde(default)]
    pub task_id: Option<TaskId>,
}

impl AgentNarrativeEvent {
    /// 服务端构造（POST 处理器路径）：`id` / `created_at` 服务端生成。
    #[must_use]
    pub fn new(project_id: ProjectId, request: CreateAgentNarrativeRequest) -> Self {
        Self {
            id: default_event_id(),
            project_id,
            audit_run_id: request.audit_run_id,
            mission_id: request.mission_id,
            branch_id: request.branch_id,
            task_id: request.task_id,
            source_agent: request.source_agent,
            event_kind: request.event_kind,
            original_text: request.original_text,
            original_language: request.original_language,
            model_invocation_id: None,
            created_at: utcnow(),
            metadata: request.metadata,
        }
    }
}

fn default_event_id() -> AgentNarrativeEventId {
    AgentNarrativeEventId::new(new_id("narr"))
}
