//! 过程日志基础设施 —— `manager.py` 的事件记录与 journal 追加。
//!
//! 两条追加只写通道，语义互补：
//! - [`AuditEvent`]：结构化的"刚发生了什么"，进仓储（前端 SSE/轮询的
//!   稳定数据源）；
//! - `SwarmOperationJournal`：脱敏后的完整操作细节 JSONL（模型可读的
//!   无损轨迹，绝不摘要化）。
//!
//! 红线：**日志写入绝不打断编排**——`record_event_safe` /
//! `append_operation_journal` 吞掉一切失败（Python `except Exception:
//! return` 的镜像）；只有 `record_event` 本身向调用方传播仓储错误。

// Journal/event helpers keep async call sites and Python-compatible best
// effort behavior while the rest of the engine is being migrated.
#![allow(clippy::doc_markdown)]
#![allow(clippy::expect_used)]
#![allow(clippy::manual_let_else)]
#![allow(clippy::unnecessary_to_owned)]
#![allow(clippy::unused_async)]

use std::path::Path;

use models::agent::Observation;
use models::agent::ObservationType;
use models::event::AuditEvent;
use models::event::AuditEventType;
use models::ids::BranchId;
use models::ids::IntentId;
use models::ids::MissionId;
use models::ids::ProjectId;
use models::ids::RunId;
use models::ids::TaskId;
use models::ids::ToolInvocationId;
use serde_json::Map;
use serde_json::Value;
use storage::SwarmOperationRecord;

use crate::errors::EngineError;
use crate::manager::AuditManager;

/// `_record_event` / `_build_event` 的关键字参数组（Python kwargs 镜像）。
///
/// 必填四项经 [`EventDraft::new`] 给出，其余字段用结构体更新语法按需
/// 覆写（`..EventDraft::new(..)`）。
pub(crate) struct EventDraft<'a> {
    /// 所属 Project。
    pub project_id: &'a ProjectId,
    /// 事件类型。
    pub event_type: AuditEventType,
    /// 行为者。
    pub actor: &'a str,
    /// 标题。
    pub title: &'a str,
    /// 所属 Run。
    pub run_id: Option<&'a RunId>,
    /// 关联 Task。
    pub task_id: Option<&'a TaskId>,
    /// 关联 `ToolInvocation`。
    pub tool_invocation_id: Option<&'a ToolInvocationId>,
    /// 消息。
    pub message: Option<&'a str>,
    /// 严重级别。
    pub severity: Option<&'a str>,
    /// 状态。
    pub status: Option<&'a str>,
    /// 结构化数据（`None` 落盘为空对象）。
    pub data: Option<Map<String, Value>>,
}

impl<'a> EventDraft<'a> {
    /// 以必填四项构造，其余取 Python 默认（`None` / 空）。
    pub(crate) fn new(
        project_id: &'a ProjectId,
        event_type: AuditEventType,
        actor: &'a str,
        title: &'a str,
    ) -> Self {
        Self {
            project_id,
            event_type,
            actor,
            title,
            run_id: None,
            task_id: None,
            tool_invocation_id: None,
            message: None,
            severity: None,
            status: None,
            data: None,
        }
    }

    fn build(&self) -> AuditEvent {
        let mut event = AuditEvent::new(
            self.project_id.clone(),
            self.event_type,
            self.actor.to_string(),
            self.title.to_string(),
        );
        event.run_id = self.run_id.cloned();
        event.task_id = self.task_id.cloned();
        event.tool_invocation_id = self.tool_invocation_id.cloned();
        event.message = self.message.map(str::to_string);
        event.severity = self.severity.map(str::to_string);
        event.status = self.status.map(str::to_string);
        event.data = self.data.clone().unwrap_or_default();
        event
    }
}

/// journal 追加的关键字参数组（Python `_append_operation_journal` 镜像）。
pub(crate) struct JournalDraft<'a> {
    /// 所属 Project。
    pub project_id: &'a ProjectId,
    /// 所属 Run（`None` 直接跳过——无 run 无 journal）。
    pub run_id: Option<&'a RunId>,
    /// 行为者。
    pub actor: &'a str,
    /// 操作类型。
    pub operation_type: &'a str,
    /// 标题。
    pub title: &'a str,
    /// 结构化载荷。
    pub payload: Map<String, Value>,
    /// 关联 Task。
    pub task_id: Option<&'a TaskId>,
    /// 所属 Branch。
    pub branch_id: Option<&'a BranchId>,
    /// 执行 worker。
    pub worker_id: Option<&'a str>,
    /// 来源记录 ID。
    pub source_ids: Vec<String>,
    /// 关联 `ToolInvocation`。
    pub tool_invocation_id: Option<&'a ToolInvocationId>,
}

impl AuditManager {
    /// 创建并持久化一条 [`AuditEvent`]（Python `_record_event`）。
    ///
    /// 失败向上传播（与 `_record_event_safe` 相对）。
    ///
    /// # Errors
    /// 仓储写入失败（事件/journal 任一环节）。
    pub(crate) async fn record_event(
        &self,
        draft: EventDraft<'_>,
    ) -> Result<AuditEvent, EngineError> {
        let event = draft.build();
        let saved = self.repository().add_event(&event)?;
        self.append_operation_journal(JournalDraft {
            project_id: draft.project_id,
            run_id: draft.run_id,
            actor: draft.actor,
            operation_type: draft.event_type.as_str(),
            title: draft.title,
            payload: Map::from_iter([(
                "event".to_string(),
                serde_json::to_value(&saved).unwrap_or(Value::Null),
            )]),
            task_id: draft.task_id,
            branch_id: None,
            worker_id: None,
            source_ids: [
                Some(saved.id.clone()),
                draft.task_id.map(|id| id.as_str().to_string()),
                draft.tool_invocation_id.map(|id| id.as_str().to_string()),
            ]
            .into_iter()
            .flatten()
            .collect(),
            tool_invocation_id: draft.tool_invocation_id,
        })
        .await;
        // NOTIFY 平面：事件落库即发布（Python NotifyingRepository.add_event）。
        let mission = self.mission_for_run(saved.run_id.as_ref());
        let notification = crate::notifications::notification_for_event(&saved, mission.as_deref());
        self.publish_notification(&notification);
        Ok(saved)
    }

    /// 记录事件并吞掉一切失败（Python `_record_event_safe`）：主流程
    /// 绝不因日志写入被打断。
    pub(crate) async fn record_event_safe(&self, draft: EventDraft<'_>) -> Option<AuditEvent> {
        self.record_event(draft).await.ok()
    }

    /// 追加脱敏的完整操作细节（Python `_append_operation_journal`）。
    ///
    /// 前置守卫逐层短路：无 run / run 无 mission / config 缺
    /// `mission_workspace_path`+`log_dir` / log_dir 不在 workspace 内。
    /// 任何失败（含存储错误）静默返回——日志绝不影响编排。
    pub(crate) async fn append_operation_journal(&self, draft: JournalDraft<'_>) {
        let Some(run_id) = draft.run_id else {
            return;
        };
        let run = match self.repository().get_run(run_id.as_str()) {
            Ok(Some(run)) => run,
            _ => return,
        };
        let Some(mission_id) = run.mission_id.clone() else {
            return;
        };
        let workspace_raw = run.config.get("mission_workspace_path");
        let log_dir_raw = run.config.get("log_dir");
        let (Some(workspace_raw), Some(log_dir_raw)) = (workspace_raw, log_dir_raw) else {
            return;
        };
        let (Some(workspace_raw), Some(log_dir_raw)) =
            (workspace_raw.as_str(), log_dir_raw.as_str())
        else {
            return;
        };
        let workspace = storage::journal::normalize(Path::new(workspace_raw));
        let log_dir = storage::journal::normalize(Path::new(log_dir_raw));
        if !log_dir.starts_with(&workspace) {
            // Python 侧 `log_dir.relative_to(workspace)` 抛 ValueError 被
            // 吞掉——越界目录不落盘。
            return;
        }

        let branch_id = match draft.branch_id {
            Some(branch_id) => Some(branch_id.clone()),
            None => match draft.task_id {
                Some(task_id) => self
                    .repository()
                    .get_task(task_id.as_str())
                    .ok()
                    .flatten()
                    .and_then(|task| task.branch_id),
                None => None,
            },
        };

        let provider = run
            .config
            .get("resolved_provider_id")
            .and_then(Value::as_str)
            .and_then(|provider_id| self.repository().get_provider(provider_id).ok().flatten());
        let model = provider
            .as_ref()
            .and_then(|provider| provider.model.clone());
        let role = operation_role(draft.actor);
        let model_label = safe_actor_component(model.as_deref().unwrap_or("local"));
        let actor_label = format!("[{model_label}-{role}]");

        let mut payload = draft.payload;
        if let Some(tool_invocation_id) = draft.tool_invocation_id {
            let invocation = self
                .repository()
                .list_tool_invocations(Some(draft.project_id.as_str()))
                .ok()
                .and_then(|invocations| {
                    invocations
                        .into_iter()
                        .find(|item| item.id.as_str() == tool_invocation_id.as_str())
                });
            if let Some(invocation) = invocation {
                payload.insert(
                    "tool_invocation".to_string(),
                    serde_json::to_value(&invocation).unwrap_or(Value::Null),
                );
            }
        }

        let mut record = SwarmOperationRecord::new(
            draft.project_id.clone(),
            run_id.clone(),
            actor_label.clone(),
            draft.operation_type.to_string(),
            format!("{actor_label} {}", draft.title),
        );
        record.mission_id = Some(mission_id);
        record.branch_id = branch_id;
        record.task_id = draft.task_id.cloned();
        record.worker_id = Some(draft.worker_id.unwrap_or(draft.actor).to_string());
        record.provider_id = provider
            .as_ref()
            .map(|provider| provider.id.as_str().to_string());
        record.model = model;
        record.role = role.to_string();
        record.payload = payload;
        record.source_ids = draft.source_ids;

        let _ = self.operation_journal().append(&log_dir, record);
    }

    /// 持久化一条结构化 [`Observation`]（Python `_add_observation`）。
    ///
    /// 与核心图解耦的过程记录：不触发任何状态迁移。
    ///
    /// # Errors
    /// 仓储写入失败。
    pub(crate) async fn add_observation(
        &self,
        draft: ObservationDraft<'_>,
    ) -> Result<Observation, EngineError> {
        let mut observation = Observation::new(
            draft.project_id.clone(),
            draft.run_id.clone(),
            draft.summary.to_string(),
        );
        observation.mission_id = draft.mission_id.cloned();
        observation.branch_id = draft.branch_id.cloned();
        observation.task_id = draft.task_id.cloned();
        observation.intent_id = draft.intent_id.cloned();
        observation.worker_id = draft.worker_id.map(str::to_string);
        observation.observation_type = draft.observation_type;
        observation.source = draft.worker_id.unwrap_or("manager").to_string();
        observation.actor = Some(
            draft
                .actor
                .or(draft.worker_id)
                .unwrap_or("manager")
                .to_string(),
        );
        observation.data = draft.data.unwrap_or_default();
        observation.related_fact_ids = draft.related_fact_ids;
        observation.related_evidence_ids = draft.related_evidence_ids;
        observation.related_finding_ids = draft.related_finding_ids;
        observation.related_tool_invocation_ids = draft.related_tool_invocation_ids;
        observation.related_task_ids = draft.related_task_ids;
        observation.related_intent_ids = draft.related_intent_ids;

        let saved = self.repository().add_observation(&observation)?;
        self.append_operation_journal(JournalDraft {
            project_id: &saved.project_id,
            run_id: Some(&saved.run_id),
            actor: saved.actor.as_deref().unwrap_or(&saved.source),
            operation_type: &format!("observation.{}", saved.observation_type.as_str()),
            title: &saved.summary,
            payload: Map::from_iter([(
                "observation".to_string(),
                serde_json::to_value(&saved).unwrap_or(Value::Null),
            )]),
            task_id: saved.task_id.as_ref(),
            branch_id: saved.branch_id.as_ref(),
            worker_id: draft.worker_id,
            source_ids: std::iter::once(saved.id.as_str().to_string())
                .chain(saved.related_task_ids.iter().cloned())
                .chain(saved.related_intent_ids.iter().cloned())
                .chain(saved.related_tool_invocation_ids.iter().cloned())
                .chain(saved.related_evidence_ids.iter().cloned())
                .chain(saved.related_finding_ids.iter().cloned())
                .collect(),
            tool_invocation_id: None,
        })
        .await;
        Ok(saved)
    }
}

/// `_add_observation` 的关键字参数组（Python kwargs 镜像）。
pub(crate) struct ObservationDraft<'a> {
    /// 所属 Project。
    pub project_id: &'a ProjectId,
    /// 所属 Run。
    pub run_id: &'a RunId,
    /// 摘要。
    pub summary: &'a str,
    /// 观察类别（Python 默认 `PROGRESS`）。
    pub observation_type: ObservationType,
    /// 所属 Mission。
    pub mission_id: Option<&'a MissionId>,
    /// 所属 Branch。
    pub branch_id: Option<&'a BranchId>,
    /// 关联 Task。
    pub task_id: Option<&'a TaskId>,
    /// 关联 Intent。
    pub intent_id: Option<&'a IntentId>,
    /// 执行 worker。
    pub worker_id: Option<&'a str>,
    /// 行为者。
    pub actor: Option<&'a str>,
    /// 结构化数据。
    pub data: Option<Map<String, Value>>,
    /// 关联 Fact ID。
    pub related_fact_ids: Vec<String>,
    /// 关联 Evidence ID。
    pub related_evidence_ids: Vec<String>,
    /// 关联 Finding ID。
    pub related_finding_ids: Vec<String>,
    /// 关联 `ToolInvocation` ID。
    pub related_tool_invocation_ids: Vec<String>,
    /// 关联 Task ID。
    pub related_task_ids: Vec<String>,
    /// 关联 Intent ID。
    pub related_intent_ids: Vec<String>,
}

impl<'a> ObservationDraft<'a> {
    /// 以必填三项构造，其余取 Python 默认（`PROGRESS` / 空）。
    pub(crate) fn new(project_id: &'a ProjectId, run_id: &'a RunId, summary: &'a str) -> Self {
        Self {
            project_id,
            run_id,
            summary,
            observation_type: ObservationType::Progress,
            mission_id: None,
            branch_id: None,
            task_id: None,
            intent_id: None,
            worker_id: None,
            actor: None,
            data: None,
            related_fact_ids: Vec::new(),
            related_evidence_ids: Vec::new(),
            related_finding_ids: Vec::new(),
            related_tool_invocation_ids: Vec::new(),
            related_task_ids: Vec::new(),
            related_intent_ids: Vec::new(),
        }
    }
}

/// 行为者 → journal 角色缩写（Python `_operation_role`）。
pub(crate) fn operation_role(actor: &str) -> &'static str {
    let lowered = actor.to_lowercase();
    if lowered.contains("manager") || lowered.contains("blackboard") {
        "mgr"
    } else if lowered.contains("observer") || lowered.contains("guardian") {
        "obs"
    } else if lowered.contains("advisor") {
        "adv"
    } else if lowered.contains("reflector") || lowered.contains("critique") {
        "rev"
    } else {
        "sol"
    }
}

/// 模型名等自由文本 → 安全 journal 组件（Python `_safe_actor_component`）：
/// 非 `[A-Za-z0-9._-]` 连续段折叠为 `-`，首尾 `-` 剥除，空则 `agent`。
pub(crate) fn safe_actor_component(value: &str) -> String {
    static SANITIZE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let sanitize =
        SANITIZE.get_or_init(|| regex::Regex::new(r"[^a-zA-Z0-9._-]+").expect("静态正则合法"));
    let normalized = sanitize
        .replace_all(value.trim(), "-")
        .trim_matches('-')
        .to_string();
    if normalized.is_empty() {
        "agent".to_string()
    } else {
        normalized
    }
}

/// 节点是否属于该 run 或为项目全局（Python `_in_run_scope`）。
pub(crate) fn in_run_scope(item_run_id: Option<&RunId>, run_id: &RunId) -> bool {
    item_run_id.is_none() || item_run_id == Some(run_id)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use storage::Repository;

    use super::*;

    #[test]
    fn operation_role_classification() {
        assert_eq!(operation_role("AuditManager"), "mgr");
        assert_eq!(operation_role("blackboard-coordinator"), "mgr");
        assert_eq!(operation_role("Observer"), "obs");
        assert_eq!(operation_role("guardian"), "obs");
        assert_eq!(operation_role("advisor"), "adv");
        assert_eq!(operation_role("Reflector"), "rev");
        assert_eq!(operation_role("critique_agent"), "rev");
        assert_eq!(operation_role("web_recon"), "sol");
        assert_eq!(operation_role("SOLVER-9"), "sol");
    }

    #[test]
    fn safe_actor_component_sanitizes_and_falls_back() {
        assert_eq!(safe_actor_component("gpt-4o"), "gpt-4o");
        assert_eq!(
            safe_actor_component("model with spaces!"),
            "model-with-spaces"
        );
        assert_eq!(safe_actor_component("---"), "agent");
        assert_eq!(safe_actor_component("  "), "agent");
        assert_eq!(safe_actor_component("-lead-"), "lead");
    }

    #[test]
    fn in_run_scope_matches_global_or_same_run() {
        let run = RunId::new("run_1".to_string());
        let other = RunId::new("run_2".to_string());
        assert!(in_run_scope(None, &run));
        assert!(in_run_scope(Some(&run), &run));
        assert!(!in_run_scope(Some(&other), &run));
    }

    #[test]
    fn event_draft_builds_python_shaped_event() {
        let project = ProjectId::new("prj".to_string());
        let draft = EventDraft::new(
            &project,
            AuditEventType::RunStarted,
            "mission_control",
            "Mission run started",
        );
        let event = draft.build();
        assert_eq!(event.project_id.as_str(), "prj");
        assert_eq!(event.event_type, AuditEventType::RunStarted);
        assert_eq!(event.actor, "mission_control");
        assert!(event.run_id.is_none());
        assert!(event.data.is_empty());
    }

    // ---- 集成路径：事件 + journal 落盘（tempfile SqliteRepository 惯例）----

    fn test_manager() -> (AuditManager, tempfile::TempDir, ProjectId, MissionId, RunId) {
        let dir = tempfile::tempdir().expect("临时目录必须可创建");
        let repo =
            storage::SqliteRepository::open(dir.path().join("ev.sqlite3")).expect("库必须可打开");
        let project = models::project::Project::new(
            "events-probe".to_string(),
            models::domain::AuditDomain::WebRecon,
        );
        let project_id = project.id.clone();
        repo.create_project(&project).expect("项目必须可创建");

        let mission = models::mission::Mission::new(project_id.clone(), "probe goal".to_string());
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
        (manager, dir, project_id, mission_id, run_id)
    }

    fn configure_run_log_dir(
        repo: &dyn Repository,
        run: &mut models::run::AuditRun,
        workspace: &Path,
    ) -> PathBuf {
        let log_dir = workspace.join("logs");
        run.config.insert(
            "mission_workspace_path".to_string(),
            Value::String(workspace.to_string_lossy().into_owned()),
        );
        run.config.insert(
            "log_dir".to_string(),
            Value::String(log_dir.to_string_lossy().into_owned()),
        );
        repo.update_run(run).expect("run 配置必须可更新");
        log_dir
    }

    #[tokio::test]
    async fn record_event_persists_event_and_appends_journal() {
        let (manager, dir, project_id, mission_id, run_id) = test_manager();
        let repo = manager.repository().clone();
        let mut run = repo
            .get_run(run_id.as_str())
            .expect("run 必须存在")
            .expect("run 必须存在");
        let log_dir = configure_run_log_dir(repo.as_ref(), &mut run, &dir.path().to_path_buf());

        let saved = manager
            .record_event(EventDraft {
                run_id: Some(&run_id),
                status: Some("running"),
                data: Some(Map::from_iter([(
                    "mission_id".to_string(),
                    Value::String(mission_id.as_str().to_string()),
                )])),
                ..EventDraft::new(
                    &project_id,
                    AuditEventType::RunStarted,
                    "mission_control",
                    "Mission run started",
                )
            })
            .await
            .expect("事件记录必须成功");

        // 事件已进仓储。
        let events = repo
            .list_events(project_id.as_str(), None, 10, None)
            .expect("事件必须可列出");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, saved.id);

        // journal 已在 log_dir 下落盘一条脱敏记录。
        let page = manager
            .operation_journal()
            .read_page(&log_dir, storage::PageQuery::new())
            .expect("journal 必须可读");
        assert_eq!(page.records.len(), 1);
        let record = &page.records[0];
        assert_eq!(record.operation_type, "run_started");
        assert_eq!(record.mission_id.as_ref(), Some(&mission_id));
        // "mission_control" 不含任何角色关键词 → sol（与 Python 一致）。
        assert_eq!(record.role, "sol");
        assert_eq!(record.actor_label, "[local-sol]", "无 provider → local");
        assert!(record.payload.contains_key("event"), "payload 携带事件全文");
    }

    #[tokio::test]
    async fn record_event_without_log_dir_skips_journal_silently() {
        let (manager, _dir, project_id, _mission_id, run_id) = test_manager();

        let saved = manager
            .record_event(EventDraft {
                run_id: Some(&run_id),
                ..EventDraft::new(
                    &project_id,
                    AuditEventType::UserNote,
                    "manager",
                    "no journal configured",
                )
            })
            .await
            .expect("事件记录本身必须成功");

        let repo = manager.repository();
        let events = repo
            .list_events(project_id.as_str(), None, 10, None)
            .expect("事件必须可列出");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, saved.id);
    }

    #[tokio::test]
    async fn record_event_safe_swallows_unknown_run_journal_errors() {
        let (manager, _dir, project_id, _mission_id, _run_id) = test_manager();

        // run 不存在 → journal 短路返回，但事件本体也先写库（事件先落库，
        // journal 是 best-effort）。
        let bogus_run = RunId::new("run_missing".to_string());
        let outcome = manager
            .record_event_safe(EventDraft {
                run_id: Some(&bogus_run),
                ..EventDraft::new(
                    &project_id,
                    AuditEventType::UserNote,
                    "manager",
                    "bogus run journal",
                )
            })
            .await;
        // _record_event 本身成功（仓储写入不受 run 存在性影响）。
        assert!(outcome.is_some());
    }

    #[tokio::test]
    async fn add_observation_persists_and_journals() {
        let (manager, dir, project_id, mission_id, run_id) = test_manager();
        let repo = manager.repository().clone();
        let mut run = repo
            .get_run(run_id.as_str())
            .expect("run 必须存在")
            .expect("run 必须存在");
        let log_dir = configure_run_log_dir(repo.as_ref(), &mut run, &dir.path().to_path_buf());

        let saved = manager
            .add_observation(ObservationDraft {
                mission_id: Some(&mission_id),
                observation_type: ObservationType::Blockage,
                worker_id: Some("branch_runtime"),
                data: Some(Map::from_iter([("steps_used".to_string(), Value::from(3))])),
                ..ObservationDraft::new(
                    &project_id,
                    &run_id,
                    "Branch budget exhausted before dispatch",
                )
            })
            .await
            .expect("观察必须可持久化");

        assert_eq!(saved.source, "branch_runtime");
        assert_eq!(saved.actor.as_deref(), Some("branch_runtime"));
        assert_eq!(saved.observation_type, ObservationType::Blockage);

        let page = manager
            .operation_journal()
            .read_page(&log_dir, storage::PageQuery::new())
            .expect("journal 必须可读");
        assert_eq!(page.records.len(), 1);
        assert_eq!(
            page.records[0].operation_type, "observation.blockage",
            "operation_type = observation.{{type}}"
        );
        assert_eq!(
            page.records[0].role, "sol",
            "branch_runtime 无角色关键词 → sol"
        );
        assert_eq!(page.records[0].worker_id.as_deref(), Some("branch_runtime"));
    }

    #[tokio::test]
    async fn journal_rejects_log_dir_outside_workspace() {
        let (manager, dir, project_id, _mission_id, run_id) = test_manager();
        let repo = manager.repository().clone();
        let mut run = repo
            .get_run(run_id.as_str())
            .expect("run 必须存在")
            .expect("run 必须存在");

        // log_dir 指向 workspace 之外：Python relative_to 抛错被吞 → 不落盘。
        let workspace = dir.path().join("workspace");
        let outside = dir.path().join("other-logs");
        run.config.insert(
            "mission_workspace_path".to_string(),
            Value::String(workspace.to_string_lossy().into_owned()),
        );
        run.config.insert(
            "log_dir".to_string(),
            Value::String(outside.to_string_lossy().into_owned()),
        );
        repo.update_run(&run).expect("run 配置必须可更新");

        let saved = manager
            .record_event(EventDraft {
                run_id: Some(&run_id),
                ..EventDraft::new(
                    &project_id,
                    AuditEventType::UserNote,
                    "manager",
                    "out of bounds log dir",
                )
            })
            .await
            .expect("事件本体仍必须成功");

        let events = repo
            .list_events(project_id.as_str(), None, 10, None)
            .expect("事件必须可列出");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, saved.id);
    }
}
