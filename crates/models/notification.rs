//! Mission 通知实体 —— `server/core/notifications.py` 的
//! `MissionNotification` / `NotificationKind` wire 镜像。
//!
//! 通知是 NOTIFY 平面的载体：写路径发布、WebSocket/SSE 订阅消费。
//! `project_id` 是路由主题；`mission_id`/`run_id` 让订阅方按 Mission 收窄
//! （一个 Project 可承载多个 Mission，无法归属本 Mission 的通知必须丢弃，
//! 不得泄露给错误观察者）。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::common::{Timestamp, new_id, utcnow};

/// 通知类别（Python `NotificationKind` 13 变体全镜像）。
///
/// 冻结流式协议只列了面向用户 10 个值；`snapshot`/`heartbeat`/`error` 在
/// Python 侧是帧级 `event` 名而非通知 kind，但枚举本身包含它们——枚举
/// 镜像 Python 全量，穷尽匹配保证新增变体时编译期暴露。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    /// Mission 快照（帧级事件名）。
    Snapshot,
    /// 一般 Mission 事件。
    MissionEvent,
    /// Mission 生命周期状态变化。
    StatusChanged,
    /// Agent 叙事进度。
    Narrative,
    /// 新发现。
    Finding,
    /// 确认的高危发现。
    HighRisk,
    /// 需要人工决策。
    DecisionRequired,
    /// 任务失败。
    TaskFailed,
    /// 完成。
    Completed,
    /// 失败。
    Failed,
    /// 心跳（帧级事件名）。
    Heartbeat,
    /// 慢消费者丢帧（帧级事件名）。
    Gap,
    /// 通道错误（帧级事件名）。
    Error,
}

impl NotificationKind {
    /// Python wire 值（`str` 枚举序列化）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Snapshot => "snapshot",
            Self::MissionEvent => "mission_event",
            Self::StatusChanged => "status_changed",
            Self::Narrative => "narrative",
            Self::Finding => "finding",
            Self::HighRisk => "high_risk",
            Self::DecisionRequired => "decision_required",
            Self::TaskFailed => "task_failed",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Heartbeat => "heartbeat",
            Self::Gap => "gap",
            Self::Error => "error",
        }
    }
}

/// 一条面向项目/运行/Mission 观察者的通知。
///
/// 通知只经内部构造与序列化，不经客户端输入解析；字段默认值镜像
/// Python `MissionNotification`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissionNotification {
    /// 通知标识（`notify_` 前缀）。
    #[serde(default = "default_notification_id")]
    pub id: String,
    /// 类别。
    pub kind: NotificationKind,
    /// 路由主题。
    pub project_id: String,
    /// 归属 Mission（事件级通知可能缺失）。
    pub mission_id: Option<String>,
    /// 归属运行。
    pub run_id: Option<String>,
    /// 归属分支。
    pub branch_id: Option<String>,
    /// 标题。
    pub title: String,
    /// 正文。
    pub message: Option<String>,
    /// 严重级（wire 字符串）。
    pub severity: Option<String>,
    /// 是否需要用户动作。
    pub requires_action: bool,
    /// 结构化附加数据。
    #[serde(default)]
    pub data: Map<String, Value>,
    /// 创建时间。
    #[serde(default = "utcnow")]
    pub created_at: Timestamp,
}

fn default_notification_id() -> String {
    new_id("notify")
}

impl MissionNotification {
    /// Python 默认值构造（`id`/`created_at` 工厂）。
    #[must_use]
    pub fn new(kind: NotificationKind, project_id: &str, title: &str) -> Self {
        Self {
            id: default_notification_id(),
            kind,
            project_id: project_id.to_string(),
            mission_id: None,
            run_id: None,
            branch_id: None,
            title: title.to_string(),
            message: None,
            severity: None,
            requires_action: false,
            data: Map::new(),
            created_at: utcnow(),
        }
    }
}
