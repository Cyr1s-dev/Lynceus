//! 仪表盘首行五卡聚合视图（`GET /stats/dashboard` 数据源）。
//!
//! 一次性返回五张卡片的数据，避免前端发五个请求各自对不上时刻。
//! 口径与成本红线一致：`token_usage.reported_runs == 0` 时前端必须显示
//! `—`，绝不把缺失冒充成 0。

use serde::{Deserialize, Serialize};

/// 活跃任务卡片：按状态拆分的任务计数。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MissionActivityStats {
    /// 执行中（`running`）。
    #[serde(default)]
    pub running: i64,
    /// 已暂停（`paused`）。
    #[serde(default)]
    pub paused: i64,
    /// 等待人工决策（`waiting_for_decision`）。
    #[serde(default)]
    pub waiting_for_decision: i64,
    /// 活跃合计（running + paused + waiting_for_decision）。
    #[serde(default)]
    pub total: i64,
}

/// 确认发现卡片：Confirmed Finding 按 Severity 拆分。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmedFindingStats {
    /// 严重（critical）。
    #[serde(default)]
    pub critical: i64,
    /// 高危（high）。
    #[serde(default)]
    pub high: i64,
    /// 中危（medium）。
    #[serde(default)]
    pub medium: i64,
    /// 低危（low）。
    #[serde(default)]
    pub low: i64,
    /// 信息级及其余未分级（缺省 Medium 之外的兜底桶）。
    #[serde(default)]
    pub info: i64,
    /// Confirmed 合计。
    #[serde(default)]
    pub total: i64,
}

/// 资产节点卡片：单个 asset_type 桶。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetTypeCount {
    /// asset_type wire 值（`domain`/`url`/`ip`/...）。
    #[serde(default)]
    pub asset_type: String,
    /// 该类型下去重 `normalized_value` 计数。
    #[serde(default)]
    pub count: i64,
}

/// 资产节点卡片：全局去重 + 按类型拆分。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetNodeStats {
    /// `COUNT(DISTINCT normalized_value)`（跨类型全局去重）。
    #[serde(default)]
    pub distinct_count: i64,
    /// 按 asset_type 拆分（计数降序）。
    #[serde(default)]
    pub by_type: Vec<AssetTypeCount>,
}

/// 工具调用卡片：调用总量 + 在途 worker。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCallStats {
    /// `tool_invocations` 总行数。
    #[serde(default)]
    pub total_invocations: i64,
    /// 非 pending/running 之外的活跃 worker_runs 在途数（pending + running）。
    #[serde(default)]
    pub active_worker_runs: i64,
}

/// Token 用量卡片：worker_usage 聚合（成本红线：无记录即无数据）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenUsageStats {
    /// 有真实 usage 上报的 run 数；0 = 无数据（前端显示 `—`）。
    #[serde(default)]
    pub reported_runs: i64,
    /// 输入 tokens 合计（未命中部分）。
    #[serde(default)]
    pub input_tokens: i64,
    /// 输出 tokens 合计。
    #[serde(default)]
    pub output_tokens: i64,
    /// 缓存命中的输入 tokens 合计。
    #[serde(default)]
    pub cached_input_tokens: i64,
    /// 有真实成本上报的 run 数（与 reported_runs 不同时成本不可信）。
    #[serde(default)]
    pub cost_reported_runs: i64,
    /// 聚合美元成本（仅全部 run 都真实报出时非 None）。
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

/// `GET /stats/dashboard` 响应：仪表盘首行五卡数据。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardStats {
    /// 卡片一：活跃任务。
    #[serde(default)]
    pub missions: MissionActivityStats,
    /// 卡片二：确认发现。
    #[serde(default)]
    pub confirmed_findings: ConfirmedFindingStats,
    /// 卡片三：资产节点。
    #[serde(default)]
    pub asset_nodes: AssetNodeStats,
    /// 卡片四：工具调用。
    #[serde(default)]
    pub tool_calls: ToolCallStats,
    /// 卡片五：Token 用量。
    #[serde(default)]
    pub token_usage: TokenUsageStats,
}
