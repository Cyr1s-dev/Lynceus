//! Notification Hub：Mission 通知的进程内扇出（`core/notifications.py`）。
//!
//! README 的 NOTIFY 平面要求用户确认、高危发现、任务失败与长任务完成
//! **实时**到达用户而非等下一次轮询。两个性质比吞吐更重要：
//!
//! - **发布绝不阻塞、绝不报错。** `publish` 同步且尽力而为，卡死或已死
//!   的 WebSocket 客户端不可能拖慢——更不可能打断——审计写路径。
//! - **慢订阅者丢最旧通知而非最新。** 每个订阅有界队列，溢出时丢弃最旧
//!   条目并计入 `dropped`，客户端据此感知缺口并恢复。
//!
//! Hub 刻意保持在进程内：换成 Redis pub/sub 或 NATS 只需新的
//! publish/subscribe 实现，其余代码不感知通知如何传输。

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};

use models::{
    AuditEvent, AuditEventType, DecisionGate, DecisionGateStatus, Finding, FindingStatus, Mission,
    MissionNotification, MissionStatus, Severity,
};
use serde_json::{Map, Value};
use tokio::sync::Notify;

/// 每个订阅的默认队列容量（Python `DEFAULT_QUEUE_SIZE`）。
pub const DEFAULT_QUEUE_SIZE: usize = 512;

/// 达到该严重级且已确认的发现升级为高危通知（Python `HIGH_RISK_SEVERITIES`）。
const HIGH_RISK_SEVERITIES: [Severity; 2] = [Severity::High, Severity::Critical];

/// 一个项目主题的有界、丢最旧通知队列。
pub struct Subscription {
    queue: StdMutex<VecDeque<MissionNotification>>,
    capacity: usize,
    notify: Notify,
    dropped: AtomicU64,
}

impl Subscription {
    fn new(capacity: usize) -> Self {
        Self {
            queue: StdMutex::new(VecDeque::new()),
            capacity,
            notify: Notify::new(),
            dropped: AtomicU64::new(0),
        }
    }

    fn offer(&self, notification: MissionNotification) {
        {
            let mut queue = self
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if queue.len() >= self.capacity {
                queue.pop_front();
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            queue.push_back(notification);
        }
        self.notify.notify_one();
    }

    /// 等待下一条通知（Python `await subscription.get()`）。
    pub async fn get(&self) -> MissionNotification {
        loop {
            if let Some(notification) = self
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
            {
                return notification;
            }
            self.notify.notified().await;
        }
    }

    /// 返回自上次调用以来累计的丢帧数并清零（Python `take_dropped`）。
    pub fn take_dropped(&self) -> u64 {
        self.dropped.swap(0, Ordering::Relaxed)
    }

    fn len(&self) -> usize {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

/// 进程内发布/订阅扇出，按 `project_id` 分主题。
#[derive(Default)]
pub struct NotificationHub {
    queue_size: usize,
    subscriptions: StdMutex<HashMap<String, Vec<Arc<Subscription>>>>,
}

impl NotificationHub {
    /// 以默认队列容量构造。
    #[must_use]
    pub fn new() -> Self {
        Self::with_queue_size(DEFAULT_QUEUE_SIZE)
    }

    /// 以显式队列容量构造（测试注入小容量验证丢帧）。
    #[must_use]
    pub fn with_queue_size(queue_size: usize) -> Self {
        Self {
            queue_size,
            subscriptions: StdMutex::new(HashMap::new()),
        }
    }

    /// 打开 `project_id` 主题的订阅（Python `subscribe`）。
    #[must_use]
    pub fn subscribe(self: &Arc<Self>, project_id: &str) -> Arc<Subscription> {
        self.subscribe_with_queue_size(project_id, self.queue_size)
    }

    /// 打开指定容量的订阅。
    #[must_use]
    pub fn subscribe_with_queue_size(
        self: &Arc<Self>,
        project_id: &str,
        queue_size: usize,
    ) -> Arc<Subscription> {
        let subscription = Arc::new(Subscription::new(queue_size));
        self.subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(project_id.to_string())
            .or_default()
            .push(Arc::clone(&subscription));
        subscription
    }

    /// 摘除一个订阅；未知订阅静默忽略。
    pub fn unsubscribe(&self, project_id: &str, subscription: &Arc<Subscription>) {
        let mut subscriptions = self
            .subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let remove_topic = if let Some(subscribers) = subscriptions.get_mut(project_id) {
            subscribers.retain(|candidate| !Arc::ptr_eq(candidate, subscription));
            subscribers.is_empty()
        } else {
            false
        };
        if remove_topic {
            subscriptions.remove(project_id);
        }
    }

    /// `project_id` 主题当前活跃订阅数（Python `subscriber_count`）。
    #[must_use]
    pub fn subscriber_count(&self, project_id: &str) -> usize {
        self.subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(project_id)
            .map_or(0, Vec::len)
    }

    /// 把通知扇出给当前订阅者，返回扇出规模（Python `publish`）。
    ///
    /// 同步且不抛错：调用方在审计写路径上，绝不能被通知消费者阻塞或破坏。
    pub fn publish(&self, notification: &MissionNotification) -> usize {
        let subscribers = self
            .subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&notification.project_id)
            .cloned()
            .unwrap_or_default();
        for subscription in &subscribers {
            subscription.offer(notification.clone());
        }
        subscribers.len()
    }

    /// 批量发布，返回总扇出计数（Python `publish_many`）。
    pub fn publish_many(&self, notifications: &[MissionNotification]) -> usize {
        notifications
            .iter()
            .map(|notification| self.publish(notification))
            .sum()
    }
}

// ---------------------------------------------------------------------------
// 领域对象 -> 通知翻译（Python notification_for_* 镜像）
// ---------------------------------------------------------------------------

/// 对观看用户有具体含义的审计事件映射（Python `_EVENT_KIND`）。
fn event_kind(event_type: AuditEventType) -> crate::NotificationKind {
    match event_type {
        AuditEventType::RunWaitingForDecision | AuditEventType::DecisionGateCreated => {
            crate::NotificationKind::DecisionRequired
        }
        AuditEventType::RunCompleted => crate::NotificationKind::Completed,
        AuditEventType::RunFailed => crate::NotificationKind::Failed,
        AuditEventType::TaskFailed
        | AuditEventType::SolverFailed
        | AuditEventType::ToolInvocationFailed => crate::NotificationKind::TaskFailed,
        AuditEventType::FindingAdded => crate::NotificationKind::Finding,
        _ => crate::NotificationKind::MissionEvent,
    }
}

/// Mission 生命周期状态到通知类别的映射（Python `_MISSION_STATUS_KIND`）。
fn mission_status_kind(status: MissionStatus) -> crate::NotificationKind {
    match status {
        MissionStatus::Completed => crate::NotificationKind::Completed,
        MissionStatus::Failed => crate::NotificationKind::Failed,
        MissionStatus::WaitingForDecision => crate::NotificationKind::DecisionRequired,
        _ => crate::NotificationKind::StatusChanged,
    }
}

/// 把 `AuditEvent` 翻译成面向用户的通知（Python `notification_for_event`）。
#[must_use]
pub fn notification_for_event(event: &AuditEvent, mission_id: Option<&str>) -> MissionNotification {
    let kind = event_kind(event.event_type);
    let mut data = Map::new();
    data.insert("event_id".to_string(), Value::String(event.id.clone()));
    data.insert(
        "event_type".to_string(),
        Value::String(event.event_type.as_str().to_string()),
    );
    data.insert("actor".to_string(), Value::String(event.actor.clone()));
    data.insert(
        "status".to_string(),
        event.status.clone().map_or(Value::Null, Value::String),
    );
    data.insert(
        "task_id".to_string(),
        event
            .task_id
            .as_ref()
            .map_or(Value::Null, |id| Value::String(id.as_str().to_string())),
    );
    data.insert(
        "tool_invocation_id".to_string(),
        event
            .tool_invocation_id
            .as_ref()
            .map_or(Value::Null, |id| Value::String(id.as_str().to_string())),
    );
    data.extend(event.data.clone());
    let mut notification = MissionNotification::new(kind, event.project_id.as_str(), &event.title);
    notification.mission_id = mission_id.map(str::to_string);
    notification.run_id = event
        .run_id
        .as_ref()
        .map(|run_id| run_id.as_str().to_string());
    notification.message.clone_from(&event.message);
    notification.severity.clone_from(&event.severity);
    notification.requires_action = kind == crate::NotificationKind::DecisionRequired;
    notification.data = data;
    notification
}

/// 把 `Finding` 翻译成通知，确认的高危升级为 `high_risk`（Python
/// `notification_for_finding`）。
#[must_use]
pub fn notification_for_finding(finding: &Finding) -> MissionNotification {
    let high_risk = HIGH_RISK_SEVERITIES.contains(&finding.severity)
        && finding.status == FindingStatus::Confirmed;
    let mut kind = if high_risk {
        crate::NotificationKind::HighRisk
    } else {
        crate::NotificationKind::Finding
    };
    if matches!(
        finding.status,
        FindingStatus::Gap | FindingStatus::Phenomenon
    ) {
        kind = crate::NotificationKind::Gap;
    }
    let mut notification =
        MissionNotification::new(kind, finding.project_id.as_str(), &finding.title);
    notification.mission_id = finding
        .mission_id
        .as_ref()
        .map(|id| id.as_str().to_string());
    notification.run_id = finding.run_id.as_ref().map(|id| id.as_str().to_string());
    notification.branch_id = finding.branch_id.as_ref().map(|id| id.as_str().to_string());
    notification.message.clone_from(&finding.description);
    notification.severity = Some(finding.severity.as_str().to_string());
    notification.requires_action = high_risk;
    notification.data = Map::from_iter([
        (
            "finding_id".to_string(),
            Value::String(finding.id.as_str().to_string()),
        ),
        (
            "status".to_string(),
            Value::String(finding.status.as_str().to_string()),
        ),
        (
            "cwe".to_string(),
            finding.cwe.clone().map_or(Value::Null, Value::String),
        ),
        (
            "rule_id".to_string(),
            finding.rule_id.clone().map_or(Value::Null, Value::String),
        ),
        (
            "evidence_ids".to_string(),
            Value::Array(
                finding
                    .evidence_ids
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        ),
    ]);
    notification
}

/// 把 `DecisionGate` 翻译成 `action-required` 通知（Python
/// `notification_for_decision_gate`）。
#[must_use]
pub fn notification_for_decision_gate(gate: &DecisionGate) -> MissionNotification {
    let mut notification = MissionNotification::new(
        crate::NotificationKind::DecisionRequired,
        gate.project_id.as_str(),
        &gate.question,
    );
    notification.run_id = Some(gate.audit_run_id.as_str().to_string());
    notification.message = if gate.context_summary.is_empty() {
        None
    } else {
        Some(gate.context_summary.clone())
    };
    notification.severity = Some(gate.severity.as_str().to_string());
    notification.requires_action = gate.status == DecisionGateStatus::Pending;
    notification.data = Map::from_iter([
        (
            "decision_gate_id".to_string(),
            Value::String(gate.id.as_str().to_string()),
        ),
        (
            "kind".to_string(),
            Value::String(gate.kind.as_str().to_string()),
        ),
        (
            "status".to_string(),
            Value::String(gate.status.as_str().to_string()),
        ),
        (
            "recommended_option_id".to_string(),
            Value::String(gate.recommended_option_id.clone()),
        ),
        (
            "options".to_string(),
            Value::Array(
                gate.options
                    .iter()
                    .map(|option| {
                        Value::Object(Map::from_iter([
                            ("id".to_string(), Value::String(option.id.clone())),
                            ("label".to_string(), Value::String(option.label.clone())),
                            (
                                "description".to_string(),
                                Value::String(option.description.clone()),
                            ),
                            ("impact".to_string(), Value::String(option.impact.clone())),
                            ("risk".to_string(), Value::String(option.risk.clone())),
                            (
                                "is_recommended".to_string(),
                                Value::Bool(option.is_recommended),
                            ),
                        ]))
                    })
                    .collect(),
            ),
        ),
    ]);
    notification
}

/// 把 Mission 生命周期变化翻译成通知（Python `notification_for_mission_status`）。
#[must_use]
pub fn notification_for_mission_status(mission: &Mission) -> MissionNotification {
    let kind = mission_status_kind(mission.status);
    let mut notification = MissionNotification::new(
        kind,
        mission.project_id.as_str(),
        &format!("Mission {}", mission.status.as_str()),
    );
    notification.mission_id = Some(mission.id.as_str().to_string());
    notification.run_id = mission
        .active_run_id
        .as_ref()
        .map(|id| id.as_str().to_string());
    notification.message = Some(mission.user_goal.clone());
    notification.requires_action = kind == crate::NotificationKind::DecisionRequired;
    notification.data = Map::from_iter([
        (
            "mission_id".to_string(),
            Value::String(mission.id.as_str().to_string()),
        ),
        (
            "status".to_string(),
            Value::String(mission.status.as_str().to_string()),
        ),
    ]);
    notification
}

/// 订阅队列深度（测试与运维观测用）。
#[must_use]
pub fn subscription_depth(subscription: &Subscription) -> usize {
    subscription.len()
}

// ---------------------------------------------------------------------------
// Manager 写路径发布钩子（Python NotifyingRepository 的 Rust 等价收口：
// manager 是仓储唯一写者，钩子收在 manager 方法内）
// ---------------------------------------------------------------------------

use crate::errors::EngineError;
use crate::manager::AuditManager;
use models::RunId;

impl AuditManager {
    /// Manager 持有的通知扇出器（Python `Runtime.notifications`）。
    #[must_use]
    pub fn notification_hub(&self) -> Arc<NotificationHub> {
        Arc::clone(&self.hub)
    }

    /// run → mission 解析（Python `NotifyingRepository` 的 run→mission
    /// 学习映射等价实现：`run.mission_id` 创建后不变，直接查库恒正确，
    /// 无需学习缓存）。
    pub(crate) fn mission_for_run(&self, run_id: Option<&RunId>) -> Option<String> {
        let run_id = run_id?;
        self.repository()
            .get_run(run_id.as_str())
            .ok()
            .flatten()?
            .mission_id
            .map(|id| id.as_str().to_string())
    }

    /// 发布一条通知（尽力而为：扇出失败/无人订阅都不影响主流程）。
    pub(crate) fn publish_notification(&self, notification: &MissionNotification) {
        self.hub.publish(notification);
    }

    /// 持久化 Mission 并在状态变化时发布通知
    /// （Python `NotifyingRepository.update_mission`）。
    ///
    /// # Errors
    /// 仓储读写失败。
    pub(crate) fn persist_mission_notifying(
        &self,
        mission: &Mission,
    ) -> Result<Mission, EngineError> {
        let previous = self.repository().get_mission(mission.id.as_str())?;
        let stored = self.repository().update_mission(mission)?;
        if previous
            .as_ref()
            .is_none_or(|previous| previous.status != stored.status)
        {
            let notification = notification_for_mission_status(&stored);
            self.publish_notification(&notification);
        }
        Ok(stored)
    }

    /// solver 原子提交后批量播报其事件与发现
    /// （Python `NotifyingRepository.commit_solver_result`）。
    pub(crate) fn announce_solver_commit(
        &self,
        mission_id: Option<&str>,
        events: &[AuditEvent],
        findings: &[Finding],
    ) {
        for event in events {
            let mission = mission_id
                .map(str::to_string)
                .or_else(|| self.mission_for_run(event.run_id.as_ref()));
            let notification = notification_for_event(event, mission.as_deref());
            self.publish_notification(&notification);
        }
        for finding in findings {
            let notification = notification_for_finding(finding);
            self.publish_notification(&notification);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::NotificationKind;
    use models::{
        AuditEventType, DecisionGate, DecisionGateKind, DecisionGateStatus, DecisionOption,
        DecisionSeverity, Finding, FindingStatus, Mission, ProjectId, RunId, Severity,
    };
    use storage::Repository as _;

    fn sample_event(event_type: AuditEventType) -> AuditEvent {
        let mut event = AuditEvent::new(
            ProjectId::new("proj".to_string()),
            event_type,
            "manager".to_string(),
            "Solver completed".to_string(),
        );
        event.run_id = Some(RunId::new("run_1".to_string()));
        event
            .data
            .insert("extra".to_string(), serde_json::json!("value"));
        event
    }

    #[tokio::test]
    async fn hub_fans_out_to_project_subscribers_only() {
        let hub = Arc::new(NotificationHub::new());
        let a = hub.subscribe("proj_a");
        let b = hub.subscribe("proj_b");
        assert_eq!(hub.subscriber_count("proj_a"), 1);

        let notification =
            MissionNotification::new(NotificationKind::MissionEvent, "proj_a", "hello");
        let fanned = hub.publish(&notification);
        assert_eq!(fanned, 1);

        let received = tokio::time::timeout(Duration::from_millis(100), a.get())
            .await
            .expect("subscriber a must receive");
        assert_eq!(received.title, "hello");
        let empty = tokio::time::timeout(Duration::from_millis(20), b.get())
            .await
            .is_err();
        assert!(empty, "proj_b must not receive proj_a notifications");
        hub.unsubscribe("proj_a", &a);
        assert_eq!(hub.subscriber_count("proj_a"), 0);
    }

    #[tokio::test]
    async fn subscription_drops_oldest_when_full() {
        let hub = Arc::new(NotificationHub::with_queue_size(2));
        let subscription = hub.subscribe("proj");
        for index in 0..3 {
            let notification = MissionNotification::new(
                NotificationKind::MissionEvent,
                "proj",
                &format!("n{index}"),
            );
            hub.publish(&notification);
        }
        assert_eq!(subscription.take_dropped(), 1);
        assert_eq!(subscription.take_dropped(), 0);
        let mut titles = Vec::new();
        for _ in 0..2 {
            let notification = tokio::time::timeout(Duration::from_millis(50), subscription.get())
                .await
                .expect("queued notification must be readable");
            titles.push(notification.title);
        }
        assert_eq!(titles, ["n1", "n2"], "oldest must be dropped, newest kept");
        let drained = tokio::time::timeout(Duration::from_millis(50), subscription.get())
            .await
            .is_err();
        assert!(drained, "exactly two notifications must remain");
    }

    #[test]
    fn notification_for_event_maps_kinds_and_flattens_data() {
        let notification = notification_for_event(
            &sample_event(AuditEventType::SolverCompleted),
            Some("mission_1"),
        );
        assert_eq!(notification.kind, NotificationKind::MissionEvent);
        assert!(!notification.requires_action);
        assert_eq!(notification.mission_id.as_deref(), Some("mission_1"));
        assert_eq!(notification.run_id.as_deref(), Some("run_1"));
        assert_eq!(notification.data["extra"], serde_json::json!("value"));

        let decision =
            notification_for_event(&sample_event(AuditEventType::RunWaitingForDecision), None);
        assert_eq!(decision.kind, NotificationKind::DecisionRequired);
        assert!(decision.requires_action);

        let completed = notification_for_event(&sample_event(AuditEventType::RunCompleted), None);
        assert_eq!(completed.kind, NotificationKind::Completed);
        let failed =
            notification_for_event(&sample_event(AuditEventType::ToolInvocationFailed), None);
        assert_eq!(failed.kind, NotificationKind::TaskFailed);
        let finding = notification_for_event(&sample_event(AuditEventType::FindingAdded), None);
        assert_eq!(finding.kind, NotificationKind::Finding);
    }

    fn sample_finding(status: FindingStatus, severity: Severity) -> Finding {
        let mut finding = Finding::new(
            ProjectId::new("proj".to_string()),
            "RCE in login".to_string(),
        );
        finding.status = status;
        finding.severity = severity;
        finding.cwe = Some("CWE-78".to_string());
        finding.rule_id = Some("appsec.command-injection".to_string());
        finding.evidence_ids = vec!["evi_1".to_string()];
        finding
    }

    #[test]
    fn notification_for_finding_escalates_confirmed_high_risk() {
        let high_risk =
            notification_for_finding(&sample_finding(FindingStatus::Confirmed, Severity::High));
        assert_eq!(high_risk.kind, NotificationKind::HighRisk);
        assert!(high_risk.requires_action);
        assert_eq!(high_risk.severity.as_deref(), Some("high"));
        assert_eq!(high_risk.data["cwe"], serde_json::json!("CWE-78"));
        assert_eq!(high_risk.data["evidence_ids"], serde_json::json!(["evi_1"]));

        let gap = notification_for_finding(&sample_finding(FindingStatus::Gap, Severity::Low));
        assert_eq!(gap.kind, NotificationKind::Gap);
        assert!(!gap.requires_action);

        // 未确认的 Critical 不升级。
        let candidate = notification_for_finding(&sample_finding(
            FindingStatus::Candidate,
            Severity::Critical,
        ));
        assert_eq!(candidate.kind, NotificationKind::Finding);
    }

    #[test]
    fn notification_for_decision_gate_requires_action_when_pending() {
        let mut gate = DecisionGate {
            id: models::DecisionGateId::new("gate_notify".to_string()),
            project_id: ProjectId::new("proj".to_string()),
            audit_run_id: RunId::new("run_1".to_string()),
            created_by: "test".to_string(),
            kind: DecisionGateKind::Blocking,
            severity: DecisionSeverity::High,
            question: "Proceed with exploit validation?".to_string(),
            context_summary: String::new(),
            recommended_option_id: "opt_yes".to_string(),
            options: Vec::new(),
            status: DecisionGateStatus::Pending,
            answer: None,
            created_at: models::utcnow(),
            answered_at: None,
            expires_at: None,
            related_fact_ids: Vec::new(),
            related_evidence_ids: Vec::new(),
            related_finding_ids: Vec::new(),
            metadata: serde_json::Map::new(),
        };
        gate.recommended_option_id = "opt_yes".to_string();
        gate.options = vec![DecisionOption {
            id: "opt_yes".to_string(),
            label: "Yes".to_string(),
            description: "Continue".to_string(),
            impact: "bounded".to_string(),
            risk: "low".to_string(),
            is_recommended: true,
        }];
        let notification = notification_for_decision_gate(&gate);
        assert_eq!(notification.kind, NotificationKind::DecisionRequired);
        assert!(notification.requires_action);
        assert_eq!(notification.run_id.as_deref(), Some("run_1"));
        assert_eq!(notification.data["kind"], serde_json::json!("blocking"));
        assert_eq!(
            notification.data["recommended_option_id"],
            serde_json::json!("opt_yes")
        );
        assert_eq!(
            notification.data["options"][0]["is_recommended"],
            serde_json::json!(true)
        );

        gate.status = DecisionGateStatus::Answered;
        assert!(!notification_for_decision_gate(&gate).requires_action);
    }

    #[test]
    fn notification_for_mission_status_kinds() {
        let mut mission = Mission::new(ProjectId::new("proj".to_string()), "goal".to_string());
        mission.id = models::MissionId::new("mission_1".to_string());

        mission.status = MissionStatus::Running;
        let running = notification_for_mission_status(&mission);
        assert_eq!(running.kind, NotificationKind::StatusChanged);
        assert_eq!(running.title, "Mission running");
        assert_eq!(running.data["status"], serde_json::json!("running"));

        mission.status = MissionStatus::WaitingForDecision;
        let waiting = notification_for_mission_status(&mission);
        assert_eq!(waiting.kind, NotificationKind::DecisionRequired);
        assert!(waiting.requires_action);

        mission.status = MissionStatus::Completed;
        assert_eq!(
            notification_for_mission_status(&mission).kind,
            NotificationKind::Completed
        );
        mission.status = MissionStatus::Failed;
        assert_eq!(
            notification_for_mission_status(&mission).kind,
            NotificationKind::Failed
        );
    }

    // ---- Manager 写路径发布钩子（tempfile SqliteRepository 惯例）----

    fn test_manager_with_hub() -> (
        AuditManager,
        Arc<NotificationHub>,
        tempfile::TempDir,
        String,
        String,
        String,
    ) {
        let dir = tempfile::tempdir().expect("临时目录必须可创建");
        let repo = storage::SqliteRepository::open(dir.path().join("notify.sqlite3"))
            .expect("库必须可打开");
        let project = models::project::Project::new(
            "notify-probe".to_string(),
            models::domain::AuditDomain::WebRecon,
        );
        let project_id = project.id.clone();
        repo.create_project(&project).expect("项目必须可创建");
        let mission = Mission::new(project_id.clone(), "notify goal".to_string());
        let mission_id = mission.id.clone();
        repo.create_mission(&mission).expect("Mission 必须可创建");
        let mut run = models::run::AuditRun::new(project_id.clone());
        run.mission_id = Some(mission_id.clone());
        let run_id = run.id.clone();
        repo.create_run(&run).expect("Run 必须可创建");

        let manager = AuditManager::new(
            std::sync::Arc::new(repo),
            agents::solver::SolverRegistry::new(),
            std::sync::Arc::new(crate::task_backend::InMemoryTaskBackend::default()),
        );
        let hub = manager.notification_hub();
        (
            manager,
            hub,
            dir,
            project_id.as_str().to_string(),
            mission_id.as_str().to_string(),
            run_id.as_str().to_string(),
        )
    }

    #[tokio::test]
    async fn record_event_publishes_mission_scoped_notification() {
        let (manager, hub, _dir, project_id, mission_id, run_id) = test_manager_with_hub();
        let subscription = hub.subscribe(&project_id);

        let saved = manager
            .record_event(crate::events::EventDraft {
                run_id: Some(&RunId::new(run_id.clone())),
                ..crate::events::EventDraft::new(
                    &ProjectId::new(project_id.clone()),
                    AuditEventType::SolverCompleted,
                    "manager",
                    "Solver completed",
                )
            })
            .await
            .expect("事件必须落库");

        let notification = tokio::time::timeout(Duration::from_millis(200), subscription.get())
            .await
            .expect("事件通知必须发布");
        assert_eq!(notification.kind, NotificationKind::MissionEvent);
        assert_eq!(
            notification.mission_id.as_deref(),
            Some(mission_id.as_str()),
            "run→mission 解析必须生效"
        );
        assert_eq!(
            notification.data["event_id"],
            serde_json::json!(saved.id.as_str())
        );
        assert_eq!(
            notification.data["event_type"],
            serde_json::json!("solver_completed")
        );
    }

    #[tokio::test]
    async fn mission_status_change_publishes_notification() {
        let (manager, hub, _dir, project_id, mission_id, _run_id) = test_manager_with_hub();
        let subscription = hub.subscribe(&project_id);

        let repo = manager.repository().clone();
        let mut mission = repo
            .get_mission(&mission_id)
            .expect("读取")
            .expect("Mission 必须存在");
        mission.status = MissionStatus::Running;
        manager
            .update_mission_record(&mission)
            .expect("更新必须成功");

        let notification = tokio::time::timeout(Duration::from_millis(200), subscription.get())
            .await
            .expect("状态变化通知必须发布");
        assert_eq!(notification.kind, NotificationKind::StatusChanged);
        assert_eq!(
            notification.mission_id.as_deref(),
            Some(mission_id.as_str())
        );
        assert_eq!(notification.data["status"], serde_json::json!("running"));

        // 同状态重写不发布（Python: previous.status != stored.status 才发布）。
        manager
            .update_mission_record(&mission)
            .expect("更新必须成功");
        let silent = tokio::time::timeout(Duration::from_millis(100), subscription.get())
            .await
            .is_err();
        assert!(silent, "unchanged status must not publish");
    }
}
