//! Skill 台账模型（WP6）——故意无外键：调用统计比任务活得久。

use serde::{Deserialize, Serialize};

/// 一次 skill 调用的台账行（存在与否都记，found=false 构成缺口清单）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillUsageRow {
    /// 记录时间（RFC3339）。
    pub ts: String,
    /// 被点名的 skill 名（不存在也记）。
    pub skill: String,
    /// 调用方 worker 的 Agent 预设（可空）。
    #[serde(default)]
    pub agent_preset: Option<String>,
    /// 所属 mission（可空）。
    #[serde(default)]
    pub mission_id: Option<String>,
    /// 所属 run（可空）。
    #[serde(default)]
    pub run_id: Option<String>,
    /// 调用参数长度（模型想传多少材料，缺口分析信号）。
    #[serde(default)]
    pub args_len: i64,
    /// skill 是否存在。
    #[serde(default)]
    pub found: bool,
}

/// skill 缺口清单条目（found=false 按被点名次数排序）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillMissingEntry {
    /// 被点名但不存在的 skill 名。
    pub skill: String,
    /// 被点名次数。
    pub misses: i64,
    /// 最近一次点名时间。
    pub last_ts: String,
}
