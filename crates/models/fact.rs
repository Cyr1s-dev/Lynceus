//! Fact 模型 —— `server/core/models/fact.py` 的移植。

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::common::Timestamp;
use crate::common::new_id;
use crate::common::utcnow;
use crate::ids::BranchId;
use crate::ids::FactId;
use crate::ids::MissionId;
use crate::ids::ProjectId;
use crate::ids::RunId;
use crate::ids::TaskId;

/// 审计图中的节点类别（`GraphNodeType`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphNodeType {
    /// 起点 Fact。
    OriginFact,
    /// 目标 Fact。
    GoalFact,
    /// 普通 Fact。
    Fact,
    /// 意图。
    Intent,
    /// 提示。
    Hint,
    /// 证据。
    Evidence,
    /// 发现。
    Finding,
}

impl GraphNodeType {
    /// wire 值（Python `.value` 镜像，用于文本拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            GraphNodeType::OriginFact => "origin_fact",
            GraphNodeType::GoalFact => "goal_fact",
            GraphNodeType::Fact => "fact",
            GraphNodeType::Intent => "intent",
            GraphNodeType::Hint => "hint",
            GraphNodeType::Evidence => "evidence",
            GraphNodeType::Finding => "finding",
        }
    }
}

fn default_fact_id() -> FactId {
    FactId::new(new_id("fact"))
}

fn default_fact_node_type() -> GraphNodeType {
    GraphNodeType::Fact
}

fn default_fact_confidence() -> f64 {
    1.0
}

/// Fact：审计图中 append-only 的已确认知识（`Fact`）。
///
/// **Fact 是 append-only 的：绝不原地修改。** 状态变化通过追加新 Fact
/// 表达。Origin 与 Goal Fact 经 `node_type` 特判，保证图恒有起点与目标；
/// `derived_from` 记录产出该 Fact 的 Fact/Intent/Evidence——这正是审计
/// 可重放的原因。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fact {
    /// Fact 标识符。
    #[serde(default = "default_fact_id")]
    pub id: FactId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Mission。
    #[serde(default)]
    pub mission_id: Option<MissionId>,
    /// 所属 Branch。
    #[serde(default)]
    pub branch_id: Option<BranchId>,
    /// 图节点类别。
    #[serde(default = "default_fact_node_type")]
    pub node_type: GraphNodeType,
    /// 短机器标签（如 `user_input_point` / `calls_strcpy` / `taint_reaches_sink`）。
    pub kind: String,
    /// 人类可读的事实陈述。
    pub statement: String,
    /// 结构化载荷（位置、符号名、参数名等，键序 = 插入序）。
    #[serde(default)]
    pub data: Map<String, Value>,
    /// 出处：产出该 Fact 的 Fact/Intent/Evidence ID。
    #[serde(default)]
    pub derived_from: Vec<String>,
    /// 产出该 Fact 的 Task（用户/系统种子 origin/goal fact 为 `None`）。
    #[serde(default)]
    pub produced_by_task_id: Option<TaskId>,
    /// 所属 Run。
    #[serde(default)]
    pub run_id: Option<RunId>,
    /// 置信度（Python 侧约束 `[0.0, 1.0]`）。
    #[serde(default = "default_fact_confidence")]
    pub confidence: f64,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
}

impl Fact {
    /// 以 Python 默认值构造（`Fact(project_id=..., kind=...,
    /// statement=...)`）。
    #[must_use]
    pub fn new(project_id: ProjectId, kind: String, statement: String) -> Self {
        Self {
            id: default_fact_id(),
            project_id,
            mission_id: None,
            branch_id: None,
            node_type: default_fact_node_type(),
            kind,
            statement,
            data: Map::new(),
            derived_from: Vec::new(),
            produced_by_task_id: None,
            run_id: None,
            confidence: default_fact_confidence(),
            created_at: utcnow(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::assert_wire_values;

    #[test]
    fn graph_node_type_matches_python_wire_values() {
        assert_wire_values(&[
            (GraphNodeType::OriginFact, "origin_fact"),
            (GraphNodeType::GoalFact, "goal_fact"),
            (GraphNodeType::Fact, "fact"),
            (GraphNodeType::Intent, "intent"),
            (GraphNodeType::Hint, "hint"),
            (GraphNodeType::Evidence, "evidence"),
            (GraphNodeType::Finding, "finding"),
        ]);
    }

    #[test]
    #[allow(clippy::float_cmp)] // 默认值是精确字面量，位级相等即语义相等
    fn fact_defaults_match_python() {
        let fact = Fact::new(
            ProjectId::new("p".to_string()),
            "calls_strcpy".to_string(),
            "statement".to_string(),
        );
        assert!(fact.id.as_str().starts_with("fact_"));
        assert_eq!(fact.node_type, GraphNodeType::Fact);
        assert_eq!(fact.confidence, 1.0);
    }

    #[test]
    fn fact_rejects_unknown_fields() {
        let result: Result<Fact, _> =
            serde_json::from_str(r#"{"project_id":"p","kind":"k","statement":"s","extra":1}"#);
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }
}
