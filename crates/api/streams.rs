//! 实时流式端点 —— `server/api/routes/events.py`（SSE）与
//! `server/api/routes/readme_aliases.py` 的 WS bridge 的 Rust 镜像。
//!
//! - SSE 按 Python 语义每秒轮询仓储：新事件逐帧 `audit_event`（游标推进），
//!   空轮询发 `heartbeat`，客户端断开即结束。
//! - WS 会话先订阅 Notification Hub **再**重放（重放期间产生的通知不丢），
//!   重放的 event id 从实时流中抑制以免重复；此后按 3s 心跳节拍推送
//!   `notification`/`gap`/`heartbeat`，mission 状态在空闲心跳时刷新。

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderName, HeaderValue};
use axum::response::{IntoResponse, Response, Sse};
use futures_util::{SinkExt, StreamExt};
use models::{AuditEvent, Mission};
use runtime::{MissionNotification, NotificationHub, Subscription};
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use tokio_stream::wrappers::ReceiverStream;

use crate::ApiError;
use crate::ApiState;
use crate::EngineError;

/// Python `WS_HEARTBEAT_SECONDS`。
const WS_HEARTBEAT_SECONDS: f64 = 3.0;
/// Python `WS_REPLAY_LIMIT`。
const WS_REPLAY_LIMIT: usize = 200;
/// SSE 轮询间隔（Python `asyncio.sleep(1)`）。
const SSE_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// SSE 每轮拉取的事件数（Python `list_events(limit=50)`）。
const SSE_POLL_LIMIT: usize = 50;

// ---------------------------------------------------------------------------
// SSE — Project audit event stream
// ---------------------------------------------------------------------------

/// SSE 查询参数（Python `stream_events` 的 Query 镜像）。
#[derive(Debug, Deserialize)]
pub struct EventStreamQuery {
    /// 过滤单个 run。
    pub run_id: Option<String>,
    /// 续传：只推该事件 id 之后的事件。
    pub after_id: Option<String>,
}

/// `GET /projects/{project_id}/audit/events/stream`。
///
/// # Errors
/// Project 不存在（流打开前 404）。
pub(crate) async fn stream_project_events(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
    Query(query): Query<EventStreamQuery>,
) -> Result<Response, ApiError> {
    state
        .manager
        .repository()
        .get_project(&project_id)?
        .ok_or_else(|| EngineError::ProjectNotFound(format!("unknown project: {project_id}")))?;

    let repository = Arc::clone(state.manager.repository());
    let (tx, rx) = tokio::sync::mpsc::channel::<
        Result<axum::response::sse::Event, std::convert::Infallible>,
    >(16);
    let cursor = query.after_id;
    let run_id = query.run_id;
    tokio::spawn(async move {
        sse_poll_loop(repository, project_id, run_id, cursor, &tx).await;
    });
    let mut response = Sse::new(ReceiverStream::new(rx))
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response();
    let headers = response.headers_mut();
    for (name, value) in [("cache-control", "no-cache"), ("x-accel-buffering", "no")] {
        if let Ok(parsed) = HeaderValue::from_str(value) {
            headers.insert(HeaderName::from_static(name), parsed);
        }
    }
    Ok(response)
}

/// Python `_generate` 的镜像：游标推进 + 空轮询心跳，永不主动结束
/// （客户端断开时发送端被丢弃）。
async fn sse_poll_loop(
    repository: Arc<dyn storage::Repository>,
    project_id: String,
    run_id: Option<String>,
    mut cursor: Option<String>,
    tx: &tokio::sync::mpsc::Sender<Result<axum::response::sse::Event, std::convert::Infallible>>,
) {
    loop {
        let events = repository.list_events(
            &project_id,
            run_id.as_deref(),
            i64::try_from(SSE_POLL_LIMIT).unwrap_or(i64::MAX),
            cursor.as_deref(),
        );
        match events {
            Ok(events) if !events.is_empty() => {
                for event in events {
                    cursor = Some(event.id.clone());
                    let payload =
                        serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
                    let frame = axum::response::sse::Event::default()
                        .event("audit_event")
                        .data(payload);
                    if tx.send(Ok(frame)).await.is_err() {
                        return;
                    }
                }
            }
            // 空轮询或仓储读失败都发心跳：读失败不该断开观察者的流。
            Ok(_) | Err(_) => {
                let frame = axum::response::sse::Event::default()
                    .event("heartbeat")
                    .data("{}");
                if tx.send(Ok(frame)).await.is_err() {
                    return;
                }
            }
        }
        tokio::time::sleep(SSE_POLL_INTERVAL).await;
    }
}

// ---------------------------------------------------------------------------
// WebSocket — Mission channel
// ---------------------------------------------------------------------------

/// WS 查询参数（Python `mission_websocket` 的 Query 镜像）。
#[derive(Debug, Deserialize)]
pub struct MissionWsQuery {
    /// 续传：重放该事件 id 之后的持久化事件。
    pub after_event_id: Option<String>,
}

/// WS 会话的写半边抽象（axum WebSocket 与测试通道双实现）。
pub trait MissionSink: Send {
    /// 发送一帧文本；返回连接是否仍然可用。
    fn send_text(&mut self, text: String) -> impl Future<Output = bool> + Send;
    /// 以给定状态码关闭连接。
    fn close(&mut self, code: u16) -> impl Future<Output = bool> + Send;
}

impl MissionSink for WebSocket {
    async fn send_text(&mut self, text: String) -> bool {
        self.send(Message::Text(text.into())).await.is_ok()
    }

    async fn close(&mut self, code: u16) -> bool {
        self.send(Message::Close(Some(CloseFrame {
            code,
            reason: "".into(),
        })))
        .await
        .is_ok()
    }
}

/// `WS /ws/missions/{mission_id}`（Python `mission_websocket`）。
pub async fn mission_ws(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
    Query(query): Query<MissionWsQuery>,
    ws: axum::extract::WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| {
        mission_socket_entry(state, mission_id, query.after_event_id, socket)
    })
}

/// 连接入口：Python 语义是先 accept 再查 Mission——缺失时发 error 帧
/// 并以 4404 关闭，而非 HTTP 404。
async fn mission_socket_entry(
    state: ApiState,
    mission_id: String,
    after_event_id: Option<String>,
    mut socket: WebSocket,
) {
    let mission = match state.manager.repository().get_mission(&mission_id) {
        Ok(Some(mission)) => mission,
        Ok(None) => {
            let _ = socket
                .send(Message::Text(
                    json!({
                        "event": "error",
                        "mission_id": mission_id,
                        "reason": "mission not found",
                    })
                    .to_string()
                    .into(),
                ))
                .await;
            let _ = socket
                .send(Message::Close(Some(CloseFrame {
                    code: 4404,
                    reason: "".into(),
                })))
                .await;
            return;
        }
        Err(_) => return,
    };
    let hub: Arc<NotificationHub> = state.manager.notification_hub();
    // 订阅先于重放：重放期间产生的通知不会丢失。
    let subscription = hub.subscribe(mission.project_id.as_str());
    let replay_events = mission
        .active_run_id
        .as_ref()
        .and_then(|run_id| {
            state
                .manager
                .repository()
                .list_events(
                    mission.project_id.as_str(),
                    Some(run_id.as_str()),
                    i64::try_from(WS_REPLAY_LIMIT).unwrap_or(i64::MAX),
                    after_event_id.as_deref(),
                )
                .ok()
        })
        .unwrap_or_default();
    // 客户端帧排空任务：断开被即时感知并驱动会话结束。
    let (mut sink, mut reader) = socket.split();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        while let Some(Ok(message)) = reader.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
        let _ = stop_tx.send(true);
    });
    let mut sink_struct = SplitSink(&mut sink);
    run_mission_session(
        &mut sink_struct,
        stop_rx,
        mission,
        replay_events,
        subscription,
        Arc::clone(state.manager.repository()),
        WS_HEARTBEAT_SECONDS,
    )
    .await;
}

struct SplitSink<'a>(&'a mut futures_util::stream::SplitSink<WebSocket, Message>);

impl MissionSink for SplitSink<'_> {
    async fn send_text(&mut self, text: String) -> bool {
        self.0.send(Message::Text(text.into())).await.is_ok()
    }

    async fn close(&mut self, code: u16) -> bool {
        self.0
            .send(Message::Close(Some(CloseFrame {
                code,
                reason: "".into(),
            })))
            .await
            .is_ok()
    }
}

/// WS 会话主体（Python 循环体的镜像，`MissionSink` 抽象使其可用测试
/// 通道驱动）。
pub async fn run_mission_session(
    sink: &mut impl MissionSink,
    mut stop: tokio::sync::watch::Receiver<bool>,
    mut mission: Mission,
    replay: Vec<AuditEvent>,
    subscription: Arc<Subscription>,
    repository: Arc<dyn storage::Repository>,
    heartbeat_seconds: f64,
) {
    let snapshot = json!({
        "event": "snapshot",
        "mission_id": mission.id.as_str(),
        "project_id": mission.project_id.as_str(),
        "status": mission.status.as_str(),
        "active_run_id": mission.active_run_id,
        "user_goal": mission.user_goal,
        "timestamp": mission.updated_at,
    });
    if !sink.send_text(snapshot.to_string()).await {
        return;
    }
    let mut replayed: HashSet<String> = HashSet::new();
    for event in &replay {
        let frame = json!({
            "event": "replay",
            "mission_id": mission.id.as_str(),
            "event_id": event.id,
            "type": event.event_type.as_str(),
            "title": event.title,
            "message": event.message,
            "severity": event.severity,
            "status": event.status,
            "data": event.data,
            "created_at": event.created_at,
        });
        replayed.insert(event.id.clone());
        if !sink.send_text(frame.to_string()).await {
            return;
        }
    }
    let ready = json!({
        "event": "ready",
        "mission_id": mission.id.as_str(),
        "replayed": replay.len(),
        "heartbeat_seconds": heartbeat_seconds,
    });
    if !sink.send_text(ready.to_string()).await {
        return;
    }

    loop {
        let dropped = subscription.take_dropped();
        if dropped > 0 {
            let gap = json!({
                "event": "gap",
                "mission_id": mission.id.as_str(),
                "dropped": dropped,
                "reason": "client too slow; oldest notifications discarded",
            });
            if !sink.send_text(gap.to_string()).await {
                return;
            }
        }
        tokio::select! {
            _ = stop.changed() => break,
            notification = subscription.get() => {
                if !is_relevant(&notification, &mission) {
                    continue;
                }
                let event_id = notification.data.get("event_id").and_then(Value::as_str);
                if event_id.is_some_and(|id| replayed.contains(id)) {
                    continue;
                }
                // 通知载荷平铺在路由包装帧上：notification 自身的
                // mission_id 键覆盖路由包装键（冻结协议 2.1 节）。
                let Ok(Value::Object(payload)) = serde_json::to_value(&notification) else {
                    continue;
                };
                let mut frame = serde_json::Map::new();
                frame.insert("event".to_string(), json!("notification"));
                frame.insert(
                    "mission_id".to_string(),
                    json!(mission.id.as_str()),
                );
                frame.extend(payload);
                if !sink.send_text(serde_json::Value::Object(frame).to_string()).await {
                    return;
                }
            }
            () = tokio::time::sleep(Duration::from_secs_f64(heartbeat_seconds)) => {
                // 空闲：刷新 Mission，让心跳里的 status/active_run_id 以及
                // 后续相关性过滤保持准确（Python 循环的 refresh 分支）。
                if let Ok(Some(refreshed)) = repository.get_mission(mission.id.as_str()) {
                    mission = refreshed;
                }
                let heartbeat = json!({
                    "event": "heartbeat",
                    "mission_id": mission.id.as_str(),
                    "status": mission.status.as_str(),
                    "active_run_id": mission.active_run_id,
                });
                if !sink.send_text(heartbeat.to_string()).await {
                    return;
                }
            }
        }
    }
}

/// 项目主题通知是否属于该 Mission（Python `_is_relevant`）：无法归属本
/// Mission 的通知丢弃而非泄露给错误观察者。
fn is_relevant(notification: &MissionNotification, mission: &Mission) -> bool {
    if let Some(mission_id) = notification.mission_id.as_deref() {
        return mission_id == mission.id.as_str();
    }
    if let (Some(run_id), Some(active_run_id)) = (
        notification.run_id.as_deref(),
        mission.active_run_id.as_ref().map(models::RunId::as_str),
    ) {
        return run_id == active_run_id;
    }
    false
}

/// 通知 → 事件帧的载荷翻译辅助（供测试复用，与 `notification_for_event`
/// 翻译共同锁定 wire 形态）。
#[must_use]
pub fn notification_event_id(notification: &MissionNotification) -> Option<&str> {
    notification.data.get("event_id").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use models::{AuditEvent, AuditEventType, Mission, ProjectId, RunId};
    use runtime::{MissionNotification, NotificationHub, NotificationKind};
    use serde_json::Value;

    use super::*;

    fn memory_repository() -> Arc<dyn storage::Repository> {
        Arc::new(storage::SqliteRepository::open(":memory:").expect("库必须可打开"))
    }

    fn seeded_mission_with_events(repo: &dyn storage::Repository) -> (Mission, Vec<AuditEvent>) {
        let project = models::project::Project::new(
            "stream-probe".to_string(),
            models::domain::AuditDomain::WebRecon,
        );
        let project_id = project.id.clone();
        repo.create_project(&project).expect("项目必须可创建");
        let mut mission = Mission::new(project_id.clone(), "stream goal".to_string());
        mission.id = models::MissionId::new("mission_stream".to_string());
        mission.status = models::MissionStatus::Running;
        repo.create_mission(&mission).expect("Mission 必须可创建");
        let mut run = models::run::AuditRun::new(project_id.clone());
        run.mission_id = Some(mission.id.clone());
        let run_id = run.id.clone();
        repo.create_run(&run).expect("Run 必须可创建");
        mission.active_run_id = Some(run_id.clone());
        repo.update_mission(&mission).expect("Mission 必须可更新");

        let mut events = Vec::new();
        for index in 0..2 {
            let mut event = AuditEvent::new(
                project_id.clone(),
                AuditEventType::SolverCompleted,
                "manager".to_string(),
                format!("event {index}"),
            );
            event.run_id = Some(run_id.clone());
            events.push(repo.add_event(&event).expect("事件必须可写入"));
        }
        (mission, events)
    }

    #[derive(Clone, Default)]
    struct MockSink {
        frames: Arc<StdMutex<Vec<Value>>>,
        closed: Arc<StdMutex<Option<u16>>>,
    }

    impl MockSink {
        fn snapshot(&self) -> Vec<Value> {
            self.frames
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    // trait 方法签名为 async（axum 实现需要真正的 await），mock 实现体
    // 纯同步内存操作。
    impl MissionSink for MockSink {
        async fn send_text(&mut self, text: String) -> bool {
            // 显式让出：mock 是纯内存操作，但会话帧顺序不应依赖同步执行。
            tokio::task::yield_now().await;
            let value: Value = serde_json::from_str(&text).expect("会话帧必须是合法 JSON");
            self.frames
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(value);
            true
        }

        async fn close(&mut self, code: u16) -> bool {
            tokio::task::yield_now().await;
            *self
                .closed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(code);
            true
        }
    }

    /// 等待直到帧数达到 `count` 或超时。
    async fn wait_for_frames(sink: &MockSink, count: usize) -> Vec<Value> {
        for _ in 0..500 {
            let frames = sink.snapshot();
            if frames.len() >= count {
                return frames;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        sink.snapshot()
    }

    #[tokio::test]
    async fn session_pushes_snapshot_replay_ready_then_live_notifications() {
        let repo = memory_repository();
        let (mission, replay) = seeded_mission_with_events(repo.as_ref());
        let hub = Arc::new(NotificationHub::new());
        let subscription = hub.subscribe(mission.project_id.as_str());

        // 重放期间发布：event_id 已重放的通知必须被抑制；新通知必须到达；
        // 无关 Mission 的通知必须被丢弃。
        let replayed_event_id = replay[0].id.clone();
        let suppressed = MissionNotification::new(
            NotificationKind::MissionEvent,
            mission.project_id.as_str(),
            "suppressed",
        );
        let mut suppressed = suppressed;
        suppressed.mission_id = Some(mission.id.as_str().to_string());
        suppressed
            .data
            .insert("event_id".to_string(), Value::String(replayed_event_id));
        hub.publish(&suppressed);

        let fresh = MissionNotification::new(
            NotificationKind::Finding,
            mission.project_id.as_str(),
            "fresh finding",
        );
        let mut fresh = fresh;
        fresh.mission_id = Some(mission.id.as_str().to_string());
        hub.publish(&fresh);

        let irrelevant = MissionNotification::new(
            NotificationKind::MissionEvent,
            mission.project_id.as_str(),
            "other mission",
        );
        let mut irrelevant = irrelevant;
        irrelevant.mission_id = Some("mission_other".to_string());
        hub.publish(&irrelevant);

        let sink = MockSink::default();
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let session = tokio::spawn({
            let sink = sink.clone();
            let subscription = Arc::clone(&subscription);
            let repo = Arc::clone(&repo);
            let mission = mission.clone();
            let replay = replay.clone();
            async move {
                let mut mock = sink;
                run_mission_session(
                    &mut mock,
                    stop_rx,
                    mission,
                    replay,
                    subscription,
                    repo,
                    30.0,
                )
                .await;
            }
        });
        let frames = wait_for_frames(&sink, 5).await;
        let _ = stop_tx.send(true);
        let _ = session.await;

        assert_eq!(frames[0]["event"], "snapshot");
        assert_eq!(frames[1]["event"], "replay");
        assert_eq!(frames[1]["event_id"], replay[0].id.as_str());
        assert_eq!(frames[2]["event"], "replay");
        assert_eq!(frames[3]["event"], "ready");
        assert_eq!(frames[3]["replayed"], 2);
        assert_eq!(frames[4]["event"], "notification");
        assert_eq!(frames[4]["title"], "fresh finding");
        assert_eq!(frames[4]["kind"], "finding");
        // 路由包装的 mission_id 被通知自身的 mission_id 覆盖（平铺语义）。
        assert_eq!(frames[4]["mission_id"], mission.id.as_str());
        // 只有 5 帧：被抑制与无关通知都不产生帧。
        assert_eq!(sink.snapshot().len(), 5);
    }

    #[tokio::test]
    async fn session_reports_gap_when_client_is_slow() {
        let repo = memory_repository();
        let (mission, replay) = seeded_mission_with_events(repo.as_ref());
        let hub = Arc::new(NotificationHub::with_queue_size(1));
        let subscription = hub.subscribe(mission.project_id.as_str());
        for index in 0..3 {
            let mut notification = MissionNotification::new(
                NotificationKind::MissionEvent,
                mission.project_id.as_str(),
                &format!("n{index}"),
            );
            notification.mission_id = Some(mission.id.as_str().to_string());
            hub.publish(&notification);
        }

        let sink = MockSink::default();
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let session = tokio::spawn({
            let sink = sink.clone();
            let subscription = Arc::clone(&subscription);
            let repo = Arc::clone(&repo);
            let mission = mission.clone();
            let replay = replay.clone();
            async move {
                let mut mock = sink;
                run_mission_session(
                    &mut mock,
                    stop_rx,
                    mission,
                    replay,
                    subscription,
                    repo,
                    30.0,
                )
                .await;
            }
        });
        // snapshot + replay×2 + ready + gap + 1 条存活通知。
        let frames = wait_for_frames(&sink, 6).await;
        let _ = stop_tx.send(true);
        let _ = session.await;

        assert_eq!(
            frames[4]["event"], "gap",
            "gap must precede stale frames: {frames:?}"
        );
        assert_eq!(frames[4]["dropped"], 2);
        assert_eq!(frames[5]["event"], "notification");
        assert_eq!(
            frames[5]["title"], "n2",
            "newest must survive, oldest dropped"
        );
    }

    #[tokio::test]
    async fn session_heartbeats_when_idle_and_refreshes_mission() {
        let repo = memory_repository();
        let (mut mission, replay) = seeded_mission_with_events(repo.as_ref());
        let hub = Arc::new(NotificationHub::new());
        let subscription = hub.subscribe(mission.project_id.as_str());
        let sink = MockSink::default();
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let session_repo = Arc::clone(&repo);
        let spawned_mission = mission.clone();
        let spawned_replay = replay.clone();
        let session = tokio::spawn({
            let sink = sink.clone();
            let subscription = Arc::clone(&subscription);
            async move {
                let mut mock = sink;
                run_mission_session(
                    &mut mock,
                    stop_rx,
                    spawned_mission,
                    spawned_replay,
                    subscription,
                    session_repo,
                    0.05,
                )
                .await;
            }
        });
        // 心跳帧出现后，mission 在仓储里变为 paused → 下一帧心跳反映新状态。
        let _first_frames = wait_for_frames(&sink, 5).await;
        mission.status = models::MissionStatus::Paused;
        repo.update_mission(&mission).expect("Mission 必须可更新");
        let frames = wait_for_frames(&sink, 7).await;
        let _ = stop_tx.send(true);
        let _ = session.await;

        let ready_position = frames
            .iter()
            .position(|frame| frame["event"] == "ready")
            .expect("ready 帧必须存在");
        let heartbeats: Vec<&Value> = frames
            .iter()
            .filter(|frame| frame["event"] == "heartbeat")
            .collect();
        assert!(
            heartbeats.len() >= 2,
            "idle session must heartbeat, got {frames:?}"
        );
        // 前一段心跳是 running；mission 被刷新后的心跳反映 paused。
        assert_eq!(heartbeats[0]["status"], "running");
        let refreshed = heartbeats.iter().any(|frame| frame["status"] == "paused");
        assert!(refreshed, "heartbeat must reflect refreshed mission status");
        let _ = ready_position;
    }

    #[test]
    fn relevance_filter_drops_unattributable_notifications() {
        let mut mission = Mission::new(ProjectId::new("proj".to_string()), "goal".to_string());
        mission.id = models::MissionId::new("mission_1".to_string());
        mission.active_run_id = Some(RunId::new("run_1".to_string()));

        let mut scoped = MissionNotification::new(NotificationKind::MissionEvent, "proj", "m");
        scoped.mission_id = Some("mission_1".to_string());
        assert!(is_relevant(&scoped, &mission));

        let mut scoped_other =
            MissionNotification::new(NotificationKind::MissionEvent, "proj", "m");
        scoped_other.mission_id = Some("mission_2".to_string());
        assert!(!is_relevant(&scoped_other, &mission));

        let mut run_scoped = MissionNotification::new(NotificationKind::MissionEvent, "proj", "m");
        run_scoped.run_id = Some("run_1".to_string());
        assert!(is_relevant(&run_scoped, &mission));

        let mut run_other = MissionNotification::new(NotificationKind::MissionEvent, "proj", "m");
        run_other.run_id = Some("run_2".to_string());
        assert!(!is_relevant(&run_other, &mission));

        assert!(!is_relevant(
            &MissionNotification::new(NotificationKind::MissionEvent, "proj", "m"),
            &mission
        ));
    }
}
