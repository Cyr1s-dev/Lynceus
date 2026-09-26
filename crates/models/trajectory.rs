//! `TrajectorySummary` —— `server/core/models/trajectory.py` 的移植。
//!
//! 长探索无法逐字回放进提示词。轨迹摘要器为每个 Branch 维护滚动派生视图
//! 供 Context Pack 使用，同时监控 token 压力，让 Watchdog 在 worker 静默
//! 截断自身上下文之前介入。

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
use crate::ids::TrajectorySummaryId;

/// 压力升级的 token 预算占比阈值（Python 模块常量）。
pub const ELEVATED_PRESSURE_RATIO: f64 = 0.6;
/// 临界压力的 token 预算占比阈值（Python 模块常量）。
pub const CRITICAL_PRESSURE_RATIO: f64 = 0.8;

/// 轨迹距离耗尽 token 预算的接近程度（`TokenPressure`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenPressure {
    /// 正常。
    Nominal,
    /// 升高。
    Elevated,
    /// 临界。
    Critical,
}

impl TokenPressure {
    /// 把 已用/预算 比值分类为压力级别（`from_ratio`）。
    #[must_use]
    pub fn from_ratio(ratio: f64) -> Self {
        if ratio >= CRITICAL_PRESSURE_RATIO {
            Self::Critical
        } else if ratio >= ELEVATED_PRESSURE_RATIO {
            Self::Elevated
        } else {
            Self::Nominal
        }
    }

    /// wire 值（Python `.value` 镜像，用于文本拼接）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            TokenPressure::Nominal => "nominal",
            TokenPressure::Elevated => "elevated",
            TokenPressure::Critical => "critical",
        }
    }
}

fn default_trajectory_id() -> TrajectorySummaryId {
    TrajectorySummaryId::new(new_id("traj"))
}

fn default_segment_index() -> i64 {
    0
}

fn default_covered_step_count() -> i64 {
    0
}

fn default_cumulative_step_count() -> i64 {
    0
}

fn default_raw_tokens() -> i64 {
    0
}

fn default_summary_tokens() -> i64 {
    0
}

fn default_token_budget() -> i64 {
    2048
}

fn default_pressure_ratio() -> f64 {
    0.0
}

fn default_pressure() -> TokenPressure {
    TokenPressure::Nominal
}

fn default_created_by() -> String {
    "trajectory_summarizer".to_string()
}

/// 一个 Branch 或 Run 的滚动轨迹摘要中的一段（`TrajectorySummary`）。
///
/// 段通过 `previous_summary_id` 链接，因此即便只有最新摘要反馈进上下文，
/// 完整历史依然可审计。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrajectorySummary {
    /// 摘要标识符。
    #[serde(default = "default_trajectory_id")]
    pub id: TrajectorySummaryId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Run。
    pub run_id: RunId,
    /// 所属 Mission。
    #[serde(default)]
    pub mission_id: Option<MissionId>,
    /// 所属 Branch。
    #[serde(default)]
    pub branch_id: Option<BranchId>,
    /// 关联 Task。
    #[serde(default)]
    pub task_id: Option<TaskId>,
    /// 段索引（从 0 起）。
    #[serde(default = "default_segment_index")]
    pub segment_index: i64,
    /// 前一段摘要 ID。
    #[serde(default)]
    pub previous_summary_id: Option<TrajectorySummaryId>,
    /// 折叠进本段的步数（非累计值）。
    #[serde(default = "default_covered_step_count")]
    pub covered_step_count: i64,
    /// 累计步数。
    #[serde(default = "default_cumulative_step_count")]
    pub cumulative_step_count: i64,
    /// 摘要正文。
    pub summary: String,
    /// 决策相关细节逐字保留，不被压缩掉。
    #[serde(default)]
    pub key_observations: Vec<String>,
    /// 失败边界。
    #[serde(default)]
    pub failure_boundaries: Vec<String>,
    /// 开放问题。
    #[serde(default)]
    pub open_questions: Vec<String>,
    /// 关联 Evidence ID 列表。
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    /// 关联 Finding ID 列表。
    #[serde(default)]
    pub finding_ids: Vec<String>,
    /// 关联 `ToolInvocation` ID 列表。
    #[serde(default)]
    pub tool_invocation_ids: Vec<String>,
    /// 原始 token 数（字符启发式估计，非分词器计数）。
    #[serde(default = "default_raw_tokens")]
    pub raw_tokens: i64,
    /// 摘要 token 数。
    #[serde(default = "default_summary_tokens")]
    pub summary_tokens: i64,
    /// token 预算。
    #[serde(default = "default_token_budget")]
    pub token_budget: i64,
    /// 已用/预算占比。
    #[serde(default = "default_pressure_ratio")]
    pub pressure_ratio: f64,
    /// 压力级别。
    #[serde(default = "default_pressure")]
    pub pressure: TokenPressure,
    /// 创建者。
    #[serde(default = "default_created_by")]
    pub created_by: String,
    /// 创建时间。
    #[serde(default = "crate::common::utcnow")]
    pub created_at: Timestamp,
    /// 附加元数据（键序 = 插入序）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

impl TrajectorySummary {
    /// 以 Python 默认值构造（`TrajectorySummary(project_id=..., run_id=...,
    /// summary=...)`，其余字段取模型默认）。
    #[must_use]
    pub fn new(project_id: ProjectId, run_id: RunId, summary: String) -> Self {
        Self {
            id: default_trajectory_id(),
            project_id,
            run_id,
            mission_id: None,
            branch_id: None,
            task_id: None,
            segment_index: default_segment_index(),
            previous_summary_id: None,
            covered_step_count: default_covered_step_count(),
            cumulative_step_count: default_cumulative_step_count(),
            summary,
            key_observations: Vec::new(),
            failure_boundaries: Vec::new(),
            open_questions: Vec::new(),
            evidence_ids: Vec::new(),
            finding_ids: Vec::new(),
            tool_invocation_ids: Vec::new(),
            raw_tokens: default_raw_tokens(),
            summary_tokens: default_summary_tokens(),
            token_budget: default_token_budget(),
            pressure_ratio: default_pressure_ratio(),
            pressure: default_pressure(),
            created_by: default_created_by(),
            created_at: crate::common::utcnow(),
            metadata: Map::new(),
        }
    }

    /// 摘要 token 占原始轨迹 token 的比例（`compression_ratio`）。
    ///
    /// token 计数远小于 2^53，`as f64` 无精度损失。
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn compression_ratio(&self) -> f64 {
        if self.raw_tokens <= 0 {
            return 0.0;
        }
        self.summary_tokens as f64 / self.raw_tokens as f64
    }

    /// token 压力是否需要 Watchdog 关注（`needs_watchdog_attention`）。
    #[must_use]
    pub fn needs_watchdog_attention(&self) -> bool {
        self.pressure == TokenPressure::Critical
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::utcnow;
    use crate::testutil::assert_wire_values;

    fn project_id() -> ProjectId {
        ProjectId::new("proj_test".to_string())
    }

    fn run_id() -> RunId {
        RunId::new("run_x".to_string())
    }

    fn summary(summary_text: String, raw: i64, summarized: i64) -> TrajectorySummary {
        TrajectorySummary {
            id: default_trajectory_id(),
            project_id: project_id(),
            run_id: run_id(),
            mission_id: None,
            branch_id: None,
            task_id: None,
            segment_index: 0,
            previous_summary_id: None,
            covered_step_count: 0,
            cumulative_step_count: 0,
            summary: summary_text,
            key_observations: Vec::new(),
            failure_boundaries: Vec::new(),
            open_questions: Vec::new(),
            evidence_ids: Vec::new(),
            finding_ids: Vec::new(),
            tool_invocation_ids: Vec::new(),
            raw_tokens: raw,
            summary_tokens: summarized,
            token_budget: 2048,
            pressure_ratio: 0.0,
            pressure: TokenPressure::Nominal,
            created_by: default_created_by(),
            created_at: utcnow(),
            metadata: Map::new(),
        }
    }

    #[test]
    fn token_pressure_matches_python_wire_values_and_ratios() {
        assert_wire_values(&[
            (TokenPressure::Nominal, "nominal"),
            (TokenPressure::Elevated, "elevated"),
            (TokenPressure::Critical, "critical"),
        ]);
        assert_eq!(TokenPressure::from_ratio(0.5), TokenPressure::Nominal);
        assert_eq!(TokenPressure::from_ratio(0.6), TokenPressure::Elevated);
        assert_eq!(TokenPressure::from_ratio(0.8), TokenPressure::Critical);
        assert_eq!(TokenPressure::from_ratio(0.95), TokenPressure::Critical);
    }

    #[test]
    fn trajectory_summary_derived_properties_match_python() {
        let mut item = summary("s".to_string(), 100, 25);
        assert!((item.compression_ratio() - 0.25).abs() < f64::EPSILON);
        assert!(!item.needs_watchdog_attention());
        item.pressure = TokenPressure::Critical;
        assert!(item.needs_watchdog_attention());
        let empty = summary("s".to_string(), 0, 0);
        assert_eq!(empty.compression_ratio().to_bits(), 0.0_f64.to_bits());
    }

    #[test]
    fn trajectory_summary_rejects_unknown_fields() {
        let result: Result<TrajectorySummary, _> =
            serde_json::from_str(r#"{"project_id":"p","run_id":"r","summary":"s","extra":1}"#);
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }
}
