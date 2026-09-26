//! 人读 agent 叙事注记 —— 与 [`crate::events`] 平行的"人读"通道。
//!
//! [`AuditEvent`](models::AuditEvent) 是结构化的"刚发生了什么"（机器
//! 审计）；`AgentNarrativeEvent` 是给人看的注记（进度、推理摘要、失败
//! 分析、下一步行动与各角色注记），由执行管线在既有状态迁移点旁路
//! 追加，原文不可变。
//!
//! 红线复刻 events.rs：**叙事写入绝不打断编排**——
//! `AuditManager::record_narrative_safe` 吞掉一切失败；只有
//! `AuditManager::record_narrative` 本身向调用方传播仓储错误。

use models::ids::{BranchId, MissionId, ProjectId, RunId, TaskId};
use models::{AgentNarrativeEvent, AgentNarrativeEventKind, CreateAgentNarrativeRequest};
use serde_json::{Map, Value};

use crate::errors::EngineError;
use crate::manager::AuditManager;

/// 一条叙事注记的关键字参数组（镜像 [`crate::events::EventDraft`] 的
/// 结构体更新语法：`..NarrativeDraft::new(..)`）。
pub(crate) struct NarrativeDraft<'a> {
    /// 所属 Project。
    pub project_id: &'a ProjectId,
    /// 产生注记的 agent 名（solver / 角色名）。
    pub source_agent: &'a str,
    /// 叙事类别。
    pub event_kind: AgentNarrativeEventKind,
    /// 原文（不可变）。
    pub original_text: &'a str,
    /// 所属 Run。
    pub run_id: Option<&'a RunId>,
    /// 所属 Mission。
    pub mission_id: Option<&'a MissionId>,
    /// 所属 Branch。
    pub branch_id: Option<&'a BranchId>,
    /// 关联 Task。
    pub task_id: Option<&'a TaskId>,
    /// 附加元数据（`None` 落盘为空对象）。
    pub metadata: Option<Map<String, Value>>,
}

impl<'a> NarrativeDraft<'a> {
    /// 以必填四项构造，其余取默认（`None` / 空）。
    pub(crate) fn new(
        project_id: &'a ProjectId,
        source_agent: &'a str,
        event_kind: AgentNarrativeEventKind,
        original_text: &'a str,
    ) -> Self {
        Self {
            project_id,
            source_agent,
            event_kind,
            original_text,
            run_id: None,
            mission_id: None,
            branch_id: None,
            task_id: None,
            metadata: None,
        }
    }
}

impl AuditManager {
    /// 创建并持久化一条 [`AgentNarrativeEvent`]。
    ///
    /// 失败向上传播（与 [`AuditManager::record_narrative_safe`] 相对）。
    ///
    /// # Errors
    /// 仓储写入失败。
    pub(crate) fn record_narrative(
        &self,
        draft: NarrativeDraft<'_>,
    ) -> Result<AgentNarrativeEvent, EngineError> {
        let event = AgentNarrativeEvent::new(
            draft.project_id.clone(),
            CreateAgentNarrativeRequest {
                audit_run_id: draft.run_id.cloned(),
                branch_id: draft.branch_id.cloned(),
                event_kind: draft.event_kind,
                metadata: draft.metadata.unwrap_or_default(),
                mission_id: draft.mission_id.cloned(),
                original_language: None,
                original_text: draft.original_text.to_string(),
                source_agent: draft.source_agent.to_string(),
                task_id: draft.task_id.cloned(),
            },
        );
        Ok(self.repository().add_agent_narrative_event(&event)?)
    }

    /// 记录叙事注记并吞掉一切失败：主流程绝不因注记写入被打断。
    pub(crate) fn record_narrative_safe(
        &self,
        draft: NarrativeDraft<'_>,
    ) -> Option<AgentNarrativeEvent> {
        self.record_narrative(draft).ok()
    }
}

#[cfg(test)]
mod tests {
    use storage::Repository;

    use super::*;

    fn test_manager() -> (AuditManager, tempfile::TempDir, ProjectId, MissionId, RunId) {
        let dir = tempfile::tempdir().expect("临时目录必须可创建");
        let repo =
            storage::SqliteRepository::open(dir.path().join("ev.sqlite3")).expect("库必须可打开");
        let project = models::project::Project::new(
            "narratives-probe".to_string(),
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

    #[test]
    fn record_narrative_persists_scoped_event() {
        let (manager, _dir, project_id, mission_id, run_id) = test_manager();

        let saved = manager
            .record_narrative(NarrativeDraft {
                run_id: Some(&run_id),
                mission_id: Some(&mission_id),
                metadata: Some(Map::from_iter([(
                    "ready_to_report".to_string(),
                    Value::Bool(true),
                )])),
                ..NarrativeDraft::new(
                    &project_id,
                    "observer",
                    AgentNarrativeEventKind::ObserverNote,
                    "2 finding(s) reviewed, run is ready to report",
                )
            })
            .expect("叙事注记必须可持久化");

        assert_eq!(saved.event_kind, AgentNarrativeEventKind::ObserverNote);
        assert_eq!(saved.source_agent, "observer");
        assert_eq!(saved.audit_run_id.as_ref(), Some(&run_id));
        assert_eq!(saved.mission_id.as_ref(), Some(&mission_id));
        assert!(saved.branch_id.is_none());
        assert!(saved.task_id.is_none());

        let events = manager
            .repository()
            .list_agent_narrative_events(
                project_id.as_str(),
                Some(run_id.as_str()),
                None,
                None,
                None,
            )
            .expect("叙事事件必须可列出");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, saved.id);
        assert_eq!(
            events[0].original_text,
            "2 finding(s) reviewed, run is ready to report"
        );
        assert_eq!(
            events[0].metadata.get("ready_to_report"),
            Some(&Value::Bool(true))
        );
    }

    #[test]
    fn record_narrative_safe_returns_saved_event() {
        let (manager, _dir, project_id, _mission_id, run_id) = test_manager();

        let outcome = manager.record_narrative_safe(NarrativeDraft {
            run_id: Some(&run_id),
            ..NarrativeDraft::new(
                &project_id,
                "manager",
                AgentNarrativeEventKind::Progress,
                "Solver task started",
            )
        });
        assert!(outcome.is_some());

        let events = manager
            .repository()
            .list_agent_narrative_events(project_id.as_str(), None, None, None, None)
            .expect("叙事事件必须可列出");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_kind, AgentNarrativeEventKind::Progress);
    }

    #[test]
    fn narrative_events_do_not_leak_across_projects() {
        let (manager, _dir, project_id, _mission_id, run_id) = test_manager();
        manager.record_narrative_safe(NarrativeDraft {
            run_id: Some(&run_id),
            ..NarrativeDraft::new(
                &project_id,
                "reflector",
                AgentNarrativeEventKind::FailureAnalysis,
                "timeout: task timed out",
            )
        });

        let other = manager
            .repository()
            .list_agent_narrative_events("proj_other", None, None, None, None)
            .expect("空项目必须可列出");
        assert!(other.is_empty());
    }
}
