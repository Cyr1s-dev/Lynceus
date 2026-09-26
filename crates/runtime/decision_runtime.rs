//! `DecisionGate` 生命周期操作。
//!
//! 决策门是编排状态机的唯一人工暂停点。API、桌面端和未来 MCP 入口都
//! 通过这里校验状态与选项，再写入仓储；它们不能直接把 `pending` 改成
//! `answered`，否则会绕过审计事件和非法选项保护。

use serde_json::{Map, Value};

use models::{AuditEventType, DecisionAnswer, DecisionGate, DecisionGateStatus};

use crate::errors::EngineError;
use crate::events::EventDraft;
use crate::manager::AuditManager;

impl AuditManager {
    /// 在决策门全部解除后恢复一个既有 Run。
    ///
    /// 该入口只负责 Run 状态和审计事件；真正的 Mission driver 由
    /// `resume_mission` 统一调度，避免同一 Run 被启动两次。
    ///
    /// # Errors
    /// Run/Project 不存在或仓储写入失败。
    pub async fn resume_run(&self, run_id: &str) -> Result<models::AuditRun, EngineError> {
        let mut run = self
            .repository()
            .get_run(run_id)?
            .ok_or_else(|| EngineError::RunNotFound(run_id.to_string()))?;
        self.repository()
            .get_project(run.project_id.as_str())?
            .ok_or_else(|| EngineError::ProjectNotFound(run.project_id.as_str().to_string()))?;
        let pending = self
            .repository()
            .list_decision_gates(Some(run.project_id.as_str()), Some(run.id.as_str()))?
            .into_iter()
            .any(|gate| {
                gate.kind == models::DecisionGateKind::Blocking
                    && gate.status == DecisionGateStatus::Pending
            });
        if pending {
            run.status = models::RunStatus::WaitingForDecision;
            run.note = Some("waiting for decision gate".to_string());
        } else {
            run.status = models::RunStatus::Running;
            run.note = Some("resumed after decision gate answer".to_string());
        }
        run.updated_at = models::utcnow();
        let saved = self.repository().update_run(&run)?;
        self.record_event_safe(EventDraft {
            run_id: Some(&saved.id),
            actor: "mission_control",
            status: Some(if pending {
                "waiting_for_decision"
            } else {
                "running"
            }),
            ..EventDraft::new(
                &saved.project_id,
                if pending {
                    AuditEventType::RunWaitingForDecision
                } else {
                    AuditEventType::RunResumed
                },
                "mission_control",
                if pending {
                    "Audit run remains waiting for decision"
                } else {
                    "Audit run resumed after decision"
                },
            )
        })
        .await;
        Ok(saved)
    }

    /// 回答一个待处理的决策门。
    ///
    /// 必须提供合法选项或非空自由文本；重复回答、未知选项均拒绝，
    /// 且不会产生半写状态。
    ///
    /// # Errors
    /// 决策门不存在、状态不是 pending、回答无内容/选项非法，或仓储写入失败。
    pub async fn answer_decision_gate(
        &self,
        gate_id: &str,
        answer: DecisionAnswer,
    ) -> Result<DecisionGate, EngineError> {
        let gate = self
            .repository()
            .get_decision_gate(gate_id)?
            .ok_or_else(|| EngineError::DecisionGateNotFound(gate_id.to_string()))?;
        if gate.status != DecisionGateStatus::Pending {
            return Err(EngineError::DecisionGateStateError(format!(
                "decision gate {gate_id} is {}; cannot answer again",
                decision_status_str(gate.status)
            )));
        }
        if answer.option_id.is_none()
            && answer
                .freeform_text
                .as_deref()
                .is_none_or(|text| text.trim().is_empty())
        {
            return Err(EngineError::InvalidDecisionAnswerError(
                "answer must include option_id or freeform_text".to_string(),
            ));
        }
        if let Some(option_id) = answer.option_id.as_deref()
            && !gate.has_option(option_id)
        {
            return Err(EngineError::InvalidDecisionAnswerError(format!(
                "invalid option_id for decision gate {gate_id}: {option_id}"
            )));
        }
        let updated = self.repository().answer_decision_gate(gate_id, &answer)?;
        {
            let notification = crate::notifications::notification_for_decision_gate(&updated);
            self.publish_notification(&notification);
        }
        self.record_event_safe(EventDraft {
            run_id: Some(&updated.audit_run_id),
            actor: &answer.answered_by,
            status: Some(decision_status_str(updated.status)),
            data: Some(Map::from_iter([
                (
                    "decision_gate_id".to_string(),
                    Value::String(updated.id.as_str().to_string()),
                ),
                (
                    "option_id".to_string(),
                    answer.option_id.clone().map_or(Value::Null, Value::String),
                ),
                (
                    "has_freeform_text".to_string(),
                    Value::Bool(answer.freeform_text.is_some()),
                ),
            ])),
            ..EventDraft::new(
                &updated.project_id,
                AuditEventType::DecisionGateAnswered,
                &answer.answered_by,
                &format!("DecisionGate answered: {}", truncate(&updated.question, 80)),
            )
        })
        .await;
        Ok(updated)
    }

    /// 取消一个待处理的决策门，并保留取消理由。
    ///
    /// # Errors
    /// 决策门不存在、状态不是 pending，或仓储写入失败。
    pub async fn cancel_decision_gate(
        &self,
        gate_id: &str,
        cancelled_by: &str,
        rationale: Option<String>,
    ) -> Result<DecisionGate, EngineError> {
        let gate = self
            .repository()
            .get_decision_gate(gate_id)?
            .ok_or_else(|| EngineError::DecisionGateNotFound(gate_id.to_string()))?;
        if gate.status != DecisionGateStatus::Pending {
            return Err(EngineError::DecisionGateStateError(format!(
                "decision gate {gate_id} is {}; cannot cancel",
                decision_status_str(gate.status)
            )));
        }
        let answer = DecisionAnswer {
            option_id: None,
            freeform_text: None,
            answered_by: cancelled_by.to_string(),
            rationale: rationale.clone(),
        };
        let mut cancelled = gate;
        cancelled.status = DecisionGateStatus::Cancelled;
        cancelled.answer = Some(answer);
        cancelled.answered_at = Some(models::utcnow());
        let updated = self.repository().update_decision_gate(&cancelled)?;
        {
            let notification = crate::notifications::notification_for_decision_gate(&updated);
            self.publish_notification(&notification);
        }
        self.record_event_safe(EventDraft {
            run_id: Some(&updated.audit_run_id),
            actor: cancelled_by,
            status: Some(decision_status_str(updated.status)),
            data: Some(Map::from_iter([
                (
                    "decision_gate_id".to_string(),
                    Value::String(updated.id.as_str().to_string()),
                ),
                (
                    "rationale".to_string(),
                    rationale.map_or(Value::Null, Value::String),
                ),
            ])),
            ..EventDraft::new(
                &updated.project_id,
                AuditEventType::DecisionGateCancelled,
                cancelled_by,
                &format!(
                    "DecisionGate cancelled: {}",
                    truncate(&updated.question, 80)
                ),
            )
        })
        .await;
        Ok(updated)
    }
}

fn truncate(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

const fn decision_status_str(status: DecisionGateStatus) -> &'static str {
    match status {
        DecisionGateStatus::Pending => "pending",
        DecisionGateStatus::Answered => "answered",
        DecisionGateStatus::Expired => "expired",
        DecisionGateStatus::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use agents::solver::SolverRegistry;
    use models::{AuditDomain, DecisionAnswer, DecisionGate, DecisionOption};
    use storage::{Repository, SqliteRepository};

    use super::*;

    fn manager() -> (AuditManager, DecisionGate) {
        let dir = tempfile::tempdir().expect("temporary directory must exist");
        let repo = SqliteRepository::open(dir.path().join("decision.sqlite3"))
            .expect("sqlite repository must open");
        std::mem::forget(dir);
        let project = models::Project::new("p".to_string(), AuditDomain::WebRecon);
        let project_id = project.id.clone();
        repo.create_project(&project).expect("project must persist");
        let run = models::AuditRun::new(project_id.clone());
        let run_id = run.id.clone();
        repo.create_run(&run).expect("run must persist");
        let option = DecisionOption {
            id: "continue".to_string(),
            label: "Continue".to_string(),
            description: "continue execution".to_string(),
            impact: "more evidence".to_string(),
            risk: "low".to_string(),
            is_recommended: true,
        };
        let gate = DecisionGate {
            id: models::DecisionGateId::new("gate_test".to_string()),
            project_id,
            audit_run_id: run_id,
            created_by: "test".to_string(),
            kind: models::DecisionGateKind::Blocking,
            severity: models::DecisionSeverity::Medium,
            question: "Continue?".to_string(),
            context_summary: String::new(),
            recommended_option_id: "continue".to_string(),
            options: vec![option],
            status: DecisionGateStatus::Pending,
            answer: None,
            created_at: models::utcnow(),
            answered_at: None,
            expires_at: None,
            related_fact_ids: Vec::new(),
            related_evidence_ids: Vec::new(),
            related_finding_ids: Vec::new(),
            metadata: Map::new(),
        };
        repo.add_decision_gate(&gate).expect("gate must persist");
        let manager = AuditManager::new(
            Arc::new(repo),
            SolverRegistry::new(),
            Arc::new(crate::task_backend::InMemoryTaskBackend::default()),
        );
        (manager, gate)
    }

    #[tokio::test]
    async fn answer_validates_and_persists_gate() {
        let (manager, gate) = manager();
        let answered = manager
            .answer_decision_gate(
                gate.id.as_str(),
                DecisionAnswer {
                    option_id: Some("continue".to_string()),
                    freeform_text: None,
                    answered_by: "user".to_string(),
                    rationale: None,
                },
            )
            .await
            .expect("valid answer must persist");
        assert_eq!(answered.status, DecisionGateStatus::Answered);
        let invalid = manager
            .answer_decision_gate(
                gate.id.as_str(),
                DecisionAnswer {
                    option_id: Some("unknown".to_string()),
                    freeform_text: None,
                    answered_by: "user".to_string(),
                    rationale: None,
                },
            )
            .await;
        assert!(matches!(
            invalid,
            Err(EngineError::DecisionGateStateError(_))
        ));
    }

    #[tokio::test]
    async fn cancel_marks_gate_cancelled() {
        let (manager, gate) = manager();
        let cancelled = manager
            .cancel_decision_gate(gate.id.as_str(), "user", Some("not now".to_string()))
            .await
            .expect("cancel must persist");
        assert_eq!(cancelled.status, DecisionGateStatus::Cancelled);
        assert_eq!(
            cancelled
                .answer
                .as_ref()
                .and_then(|a| a.rationale.as_deref()),
            Some("not now")
        );
    }
}
