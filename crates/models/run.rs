//! 执行单元 —— `server/core/models/run.py` 的移植。
//!
//! `AgentTask` 是求解器的工作单元；`AuditRun` 是可暂停/恢复的执行单位。
//! 字段顺序、默认值与 wire 格式冻结自 Python 模型。

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::common::Timestamp;
use crate::common::new_id;
use crate::common::utcnow;
use crate::ids::BranchId;
use crate::ids::MissionId;
use crate::ids::ProjectId;
use crate::ids::RunId;
use crate::ids::TaskId;
use crate::lifecycle::RunStatus;
use crate::lifecycle::TaskStatus;

fn default_task_id() -> TaskId {
    TaskId::new(new_id("task"))
}

fn default_task_status() -> TaskStatus {
    TaskStatus::Queued
}

fn default_task_budget_steps() -> i64 {
    8
}

fn default_run_id() -> RunId {
    RunId::new(new_id("run"))
}

fn default_run_status() -> RunStatus {
    RunStatus::Pending
}

fn default_run_max_total_steps() -> i64 {
    64
}

/// AgentTask：从 Intent 派生的单个求解器工作单元（`AgentTask`）。
///
/// Task 由 manager 创建、Solver 经 `TaskBackend` 执行并返回结构化的
/// `SolverResult`（Solver 绝不直接写核心状态）；`budget_steps` 限制该
/// task 的工具使用量。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTask {
    /// Task 标识符。
    #[serde(default = "default_task_id")]
    pub id: TaskId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Mission。
    #[serde(default)]
    pub mission_id: Option<MissionId>,
    /// 所属 Branch。
    #[serde(default)]
    pub branch_id: Option<BranchId>,
    /// 所属 Run（必填）。
    pub run_id: RunId,
    /// 派生来源 Intent。
    #[serde(default)]
    pub intent_id: Option<String>,
    /// 注册的求解器名（如 `web_sast` / `binary_analysis`）。
    pub solver: String,
    /// 生命周期状态。
    #[serde(default = "default_task_status")]
    pub status: TaskStatus,
    /// 交给求解器的输入（intent 上下文、目标信息、提示）。
    #[serde(default)]
    pub payload: Map<String, Value>,
    /// 工具步数预算。
    #[serde(default = "default_task_budget_steps")]
    pub budget_steps: i64,
    /// 产出的 Fact ID 列表。
    #[serde(default)]
    pub produced_fact_ids: Vec<String>,
    /// 产出的 Evidence ID 列表。
    #[serde(default)]
    pub produced_evidence_ids: Vec<String>,
    /// 产出的 Finding ID 列表。
    #[serde(default)]
    pub produced_finding_ids: Vec<String>,
    /// 关联的 `ToolInvocation` ID 列表。
    #[serde(default)]
    pub tool_invocation_ids: Vec<String>,
    /// 失败原因。
    #[serde(default)]
    pub error: Option<String>,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
    /// 开始时间。
    #[serde(default)]
    pub started_at: Option<Timestamp>,
    /// 结束时间。
    #[serde(default)]
    pub finished_at: Option<Timestamp>,
}

impl AgentTask {
    /// 以 Python 默认值构造（`AgentTask(project_id=..., run_id=...,
    /// solver=...)`）。
    #[must_use]
    pub fn new(project_id: ProjectId, run_id: RunId, solver: String) -> Self {
        Self {
            id: default_task_id(),
            project_id,
            mission_id: None,
            branch_id: None,
            run_id,
            intent_id: None,
            solver,
            status: default_task_status(),
            payload: Map::new(),
            budget_steps: default_task_budget_steps(),
            produced_fact_ids: Vec::new(),
            produced_evidence_ids: Vec::new(),
            produced_finding_ids: Vec::new(),
            tool_invocation_ids: Vec::new(),
            error: None,
            created_at: utcnow(),
            started_at: None,
            finished_at: None,
        }
    }
}

/// AuditRun：Project 上的一次可恢复审计执行（`AuditRun`）。
///
/// Run 是可恢复的单位：可暂停、恢复、进入评审、再到报告。计数器提供
/// 无需遍历全图的廉价进度视图。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditRun {
    /// Run 标识符。
    #[serde(default = "default_run_id")]
    pub id: RunId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Mission。
    #[serde(default)]
    pub mission_id: Option<MissionId>,
    /// 生命周期状态。
    #[serde(default = "default_run_status")]
    pub status: RunStatus,
    /// 高层运行配置（审计域/引擎、深度、预算）。
    #[serde(default)]
    pub config: Map<String, Value>,
    /// 全 Run 工具/Agent 步数总预算。
    #[serde(default = "default_run_max_total_steps")]
    pub max_total_steps: i64,
    /// 已消耗步数。
    #[serde(default)]
    pub steps_used: i64,
    /// 任务 ID 列表。
    #[serde(default)]
    pub task_ids: Vec<String>,
    /// 备注。
    #[serde(default)]
    pub note: Option<String>,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
    /// 开始时间。
    #[serde(default)]
    pub started_at: Option<Timestamp>,
    /// 结束时间。
    #[serde(default)]
    pub finished_at: Option<Timestamp>,
    /// 最后更新时间。
    #[serde(default = "crate::common::utcnow")]
    pub updated_at: Timestamp,
}

impl AuditRun {
    /// 以 Python 默认值构造（`AuditRun(project_id=...)`）。
    #[must_use]
    pub fn new(project_id: ProjectId) -> Self {
        Self {
            id: default_run_id(),
            project_id,
            mission_id: None,
            status: default_run_status(),
            config: Map::new(),
            max_total_steps: default_run_max_total_steps(),
            steps_used: 0,
            task_ids: Vec::new(),
            note: None,
            created_at: utcnow(),
            started_at: None,
            finished_at: None,
            updated_at: utcnow(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timestamp() -> Timestamp {
        "2026-08-24T12:00:00.123456Z"
            .parse()
            .unwrap_or_else(|error| panic!("固定时间必须可解析: {error}"))
    }

    #[test]
    fn audit_run_serializes_to_python_wire_bytes() {
        // 期望串逐字节来自 scripts/probe_parity_wire.py 探针输出。
        let expected = concat!(
            r#"{"id":"run_fix_0001","project_id":"proj_parity","#,
            r#""mission_id":"mission_fix_0001","status":"running","#,
            r#""config":{"depth":2},"max_total_steps":64,"steps_used":3,"#,
            r#""task_ids":["task_fix_0001"],"note":null,"#,
            r#""created_at":"2026-08-24T12:00:00.123456Z","#,
            r#""started_at":"2026-08-24T12:00:00.123456Z","finished_at":null,"#,
            r#""updated_at":"2026-08-24T12:00:00.123456Z"}"#
        );
        let run = AuditRun {
            id: RunId::new("run_fix_0001".to_string()),
            project_id: ProjectId::new("proj_parity".to_string()),
            mission_id: Some(MissionId::new("mission_fix_0001".to_string())),
            status: RunStatus::Running,
            config: [("depth", Value::from(2))]
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
            max_total_steps: 64,
            steps_used: 3,
            task_ids: vec!["task_fix_0001".to_string()],
            note: None,
            created_at: timestamp(),
            started_at: Some(timestamp()),
            finished_at: None,
            updated_at: timestamp(),
        };
        let json = serde_json::to_string(&run)
            .unwrap_or_else(|error| panic!("AuditRun 序列化不会失败: {error}"));
        assert_eq!(json, expected);

        let back: AuditRun = serde_json::from_str(&json)
            .unwrap_or_else(|error| panic!("自身输出必须可解析: {error}"));
        assert_eq!(back, run);
    }

    #[test]
    fn agent_task_serializes_to_python_wire_bytes() {
        // 期望串逐字节来自 scripts/probe_parity_wire.py 探针输出。
        let expected = concat!(
            r#"{"id":"task_fix_0001","project_id":"proj_parity","#,
            r#""mission_id":"mission_fix_0001","branch_id":"branch_fix_0001","#,
            r#""run_id":"run_fix_0001","intent_id":null,"solver":"web_sast","#,
            r#""status":"succeeded","payload":{"step":1},"budget_steps":8,"#,
            r#""produced_fact_ids":["fact_1"],"produced_evidence_ids":["evd_fix_0001"],"#,
            r#""produced_finding_ids":["find_fix_0001"],"#,
            r#""tool_invocation_ids":["tool_fix_0001"],"error":null,"#,
            r#""created_at":"2026-08-24T12:00:00.123456Z","#,
            r#""started_at":"2026-08-24T12:00:00.123456Z","#,
            r#""finished_at":"2026-08-24T12:00:00.123456Z"}"#
        );
        let task = AgentTask {
            id: TaskId::new("task_fix_0001".to_string()),
            project_id: ProjectId::new("proj_parity".to_string()),
            mission_id: Some(MissionId::new("mission_fix_0001".to_string())),
            branch_id: Some(BranchId::new("branch_fix_0001".to_string())),
            run_id: RunId::new("run_fix_0001".to_string()),
            intent_id: None,
            solver: "web_sast".to_string(),
            status: TaskStatus::Succeeded,
            payload: [("step", Value::from(1))]
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
            budget_steps: 8,
            produced_fact_ids: vec!["fact_1".to_string()],
            produced_evidence_ids: vec!["evd_fix_0001".to_string()],
            produced_finding_ids: vec!["find_fix_0001".to_string()],
            tool_invocation_ids: vec!["tool_fix_0001".to_string()],
            error: None,
            created_at: timestamp(),
            started_at: Some(timestamp()),
            finished_at: Some(timestamp()),
        };
        let json = serde_json::to_string(&task)
            .unwrap_or_else(|error| panic!("AgentTask 序列化不会失败: {error}"));
        assert_eq!(json, expected);

        let back: AgentTask = serde_json::from_str(&json)
            .unwrap_or_else(|error| panic!("自身输出必须可解析: {error}"));
        assert_eq!(back, task);
    }

    #[test]
    fn run_and_task_defaults_match_python() {
        let run = AuditRun::new(ProjectId::new("p".to_string()));
        assert_eq!(run.status, RunStatus::Pending);
        assert_eq!(run.max_total_steps, 64);
        assert_eq!(run.steps_used, 0);
        assert!(run.task_ids.is_empty());
        assert!(run.id.as_str().starts_with("run_"));

        let task = AgentTask::new(
            ProjectId::new("p".to_string()),
            RunId::new("run_x".to_string()),
            "web_sast".to_string(),
        );
        assert_eq!(task.status, TaskStatus::Queued);
        assert_eq!(task.budget_steps, 8);
        assert!(task.id.as_str().starts_with("task_"));
    }

    #[test]
    fn run_deserialize_rejects_unknown_fields() {
        let result: Result<AuditRun, _> = serde_json::from_str(r#"{"project_id":"p","extra":1}"#);
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }
}
