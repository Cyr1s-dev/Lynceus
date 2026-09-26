//! Intent 模型 —— `server/core/models/intent.py` 的移植。

use serde::Deserialize;
use serde::Serialize;

use crate::common::Timestamp;
use crate::common::new_id;
use crate::common::utcnow;
use crate::ids::BranchId;
use crate::ids::IntentId;
use crate::ids::MissionId;
use crate::ids::ProjectId;
use crate::ids::RunId;
use crate::ids::TaskId;

/// 探索意图生命周期（`IntentStatus`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentStatus {
    /// 待处理。
    Pending,
    /// 已认领。
    Claimed,
    /// 执行中。
    InProgress,
    /// 已解决。
    Resolved,
    /// 已驳回。
    Dismissed,
}

impl IntentStatus {
    /// wire 值（Python `.value` 镜像，用于文本拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            IntentStatus::Pending => "pending",
            IntentStatus::Claimed => "claimed",
            IntentStatus::InProgress => "in_progress",
            IntentStatus::Resolved => "resolved",
            IntentStatus::Dismissed => "dismissed",
        }
    }
}

fn default_intent_id() -> IntentId {
    IntentId::new(new_id("intent"))
}

fn default_intent_status() -> IntentStatus {
    IntentStatus::Pending
}

fn default_intent_priority() -> i64 {
    50
}

fn default_intent_max_steps() -> i64 {
    8
}

fn default_intent_created_by() -> String {
    "manager".to_string()
}

/// Intent：审计图中的下一步探索目标（`Intent`）。
///
/// Intent 从一个或多个 Fact 出发，目标是产出新的 Fact / Evidence /
/// Finding；`solver` 提示让 manager 把意图路由到正确的 Solver，
/// `priority` 决定工作队列顺序。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    /// Intent 标识符。
    #[serde(default = "default_intent_id")]
    pub id: IntentId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Mission。
    #[serde(default)]
    pub mission_id: Option<MissionId>,
    /// 所属 Branch。
    #[serde(default)]
    pub branch_id: Option<BranchId>,
    /// 生命周期状态。
    #[serde(default = "default_intent_status")]
    pub status: IntentStatus,
    /// 标题。
    pub title: String,
    /// 描述。
    #[serde(default)]
    pub description: Option<String>,
    /// 出发点的 Fact ID 列表。
    #[serde(default)]
    pub source_fact_ids: Vec<String>,
    /// 建议的求解器名（如 `web_sast` / `binary_analysis`），manager 有最终决定权。
    #[serde(default)]
    pub solver: Option<String>,
    /// 优先级（Python 侧约束 `[0, 100]`）。
    #[serde(default = "default_intent_priority")]
    pub priority: i64,
    /// 预算守卫：manager 在此最多消耗的工具/agent 步数。
    #[serde(default = "default_intent_max_steps")]
    pub max_steps: i64,
    /// 创建者（`manager` / `observer` / `user`）。
    #[serde(default = "default_intent_created_by")]
    pub created_by: String,
    /// 所属 Run。
    #[serde(default)]
    pub run_id: Option<RunId>,
    /// 认领该 Intent 的 Task。
    #[serde(default)]
    pub claimed_by_task_id: Option<TaskId>,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
    /// 最后更新时间。
    #[serde(default = "crate::common::utcnow")]
    pub updated_at: Timestamp,
}

impl Intent {
    /// 以 Python 默认值构造（`Intent(project_id=..., title=...)`）。
    #[must_use]
    pub fn new(project_id: ProjectId, title: String) -> Self {
        Self {
            id: default_intent_id(),
            project_id,
            mission_id: None,
            branch_id: None,
            status: default_intent_status(),
            title,
            description: None,
            source_fact_ids: Vec::new(),
            solver: None,
            priority: default_intent_priority(),
            max_steps: default_intent_max_steps(),
            created_by: default_intent_created_by(),
            run_id: None,
            claimed_by_task_id: None,
            created_at: utcnow(),
            updated_at: utcnow(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::assert_wire_values;

    #[test]
    fn intent_status_matches_python_wire_values() {
        assert_wire_values(&[
            (IntentStatus::Pending, "pending"),
            (IntentStatus::Claimed, "claimed"),
            (IntentStatus::InProgress, "in_progress"),
            (IntentStatus::Resolved, "resolved"),
            (IntentStatus::Dismissed, "dismissed"),
        ]);
    }

    #[test]
    fn intent_defaults_match_python() {
        let intent = Intent::new(ProjectId::new("p".to_string()), "t".to_string());
        assert!(intent.id.as_str().starts_with("intent_"));
        assert_eq!(intent.status, IntentStatus::Pending);
        assert_eq!(intent.priority, 50);
        assert_eq!(intent.max_steps, 8);
        assert_eq!(intent.created_by, "manager");
    }

    #[test]
    fn intent_rejects_unknown_fields() {
        let result: Result<Intent, _> =
            serde_json::from_str(r#"{"project_id":"p","title":"t","extra":1}"#);
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }
}
