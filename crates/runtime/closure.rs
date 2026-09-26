//! 收口链 —— COV → META → MGATE → EGUARD（`manager.py` 的
//! `_run_mission_closure_chain` / `_closure_refusal_note` /
//! `_metacognition_provider_id` 及收口轮次上限解析）。
//!
//! 终止判定提出 COMPLETE 只是**候选**；出口闸（MGATE）裁决 Mission 能否
//! 真正收口。升级绝不绕过 EGUARD：放行方向经 Branch Generator 物化成
//! 分支，与初始分支一样过 Critique。DONE 的 EGUARD 什么都不放行——裁决
//! 留作诚实的审计记录，而不是列出一堆永远不会物化的方向。

// The closure chain is a deliberately linear, Python-parity pipeline. Keep
// its explicit inputs and append-only stages visible during the migration.
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::too_many_lines)]
#![allow(dead_code)]

use agents::branch_generator::DirectionMaterializationInput;
use agents::coverage::CoverageInput;
use agents::escalation::EscalationInput;
use agents::exit_gate::ExitGateInput;
use agents::metacognition::LlmDivergenceContext;
use agents::metacognition::MetacognitionInput;
use models::AuditRun;
use models::CoverageAssessment;
use models::EscalationGuardVerdict;
use models::ExitGateDecision;
use models::ExitGateDecisionValue;
use models::MetacognitionAssessment;
use models::MetacognitionTrigger;
use models::Mission;
use models::Project;
use models::TerminationAssessment;
use serde_json::Value;

use crate::errors::EngineError;
use crate::events::EventDraft;
use crate::events::ObservationDraft;
use crate::events::in_run_scope;
use crate::manager::AuditManager;
use crate::narratives::NarrativeDraft;

/// 一次 COV → META → MGATE → EGUARD 对候选完成判定的裁决
/// （Python `_ClosureChainOutcome`）。
pub(crate) struct ClosureChainOutcome {
    /// COV 输出。
    pub(crate) coverage: CoverageAssessment,
    /// META 输出。
    pub(crate) metacognition: MetacognitionAssessment,
    /// MGATE 输出。
    pub(crate) exit_gate: ExitGateDecision,
    /// EGUARD 输出。
    pub(crate) guard: EscalationGuardVerdict,
    /// EGUARD 放行且已物化为分支的方向。
    pub(crate) admitted_branch_ids: Vec<String>,
}

impl ClosureChainOutcome {
    /// 出口闸是否批准完成（Python `completion_approved`）。
    pub(crate) fn completion_approved(&self) -> bool {
        self.exit_gate.decision == ExitGateDecisionValue::Done
    }
}

impl AuditManager {
    /// 对一个候选完成判定跑一轮 COV → META → MGATE → EGUARD（Python
    /// `_run_mission_closure_chain`）。
    ///
    /// # Errors
    /// 仓储读写失败，或 EGUARD 放行方向的分支生成被拒（工具名标题，
    /// `ValueError` 族）。
    pub(crate) async fn run_mission_closure_chain(
        &self,
        mission: &Mission,
        project: &Project,
        run: &AuditRun,
        assessment: &TerminationAssessment,
        round_index: i64,
        allow_escalation: bool,
        trigger: MetacognitionTrigger,
    ) -> Result<ClosureChainOutcome, EngineError> {
        let branches = self.repository().list_branches(
            None,
            Some(mission.id.as_str()),
            Some(run.id.as_str()),
        )?;
        let evidence = self
            .repository()
            .list_evidence(project.id.as_str())?
            .into_iter()
            .filter(|item| in_run_scope(item.run_id.as_ref(), &run.id))
            .collect::<Vec<_>>();
        let tool_invocations = self
            .repository()
            .list_tool_invocations(Some(project.id.as_str()))?
            .into_iter()
            .filter(|inv| {
                inv.mission_id.as_ref() == Some(&mission.id)
                    || in_run_scope(inv.run_id.as_ref(), &run.id)
            })
            .collect::<Vec<_>>();
        let findings = self
            .repository()
            .list_findings(project.id.as_str())?
            .into_iter()
            .filter(|finding| in_run_scope(finding.run_id.as_ref(), &run.id))
            .collect::<Vec<_>>();

        // COV：只对真实信号做确定性覆盖计量。
        let modules = self.repository().list_modules()?;
        let coverage = self.coverage_checker.check(&CoverageInput {
            mission,
            run,
            branches: &branches,
            evidence: &evidence,
            tool_invocations: &tool_invocations,
            modules: &modules,
            round_index,
        });
        let coverage = self.repository().add_coverage_assessment(&coverage)?;

        // META：LLM 路径 + 确定性回退的发散评估。
        let provider_id = metacognition_provider_id(&run.config);
        let provider_runtime = self.provider_runtime();
        // 未钉定时交由网关按 `metacognition_divergence` 用途路由；只有
        // “无钉定、无启用路由、无默认 provider”才降级确定性框架，避免
        // 每轮收口都必然撞 provider 错误（Python 同源的降级决策）。
        let route_configured = provider_id.is_none()
            && self
                .repository()
                .list_provider_routes(Some(agents::metacognition::METACOGNITION_PURPOSE))
                .is_ok_and(|routes| routes.iter().any(|route| route.enabled));
        let has_default = provider_id.is_none()
            && !route_configured
            && match provider_runtime.as_ref() {
                Some(runtime) => {
                    matches!(runtime.resolve_default_provider().await, Ok(Some(_)))
                }
                None => false,
            };
        // 无 provider 可解析：只跑确定性框架，而不是每轮收口都必然
        // 撞 provider 错误（Python 同源的降级决策）。
        let provider_runtime = (provider_id.is_some()
            || route_configured
            || has_default
            || provider_runtime.is_none())
        .then_some(provider_runtime)
        .flatten();
        let meta_input = MetacognitionInput {
            mission,
            run,
            coverage: &coverage,
            termination: Some(assessment),
            branches: &branches,
            findings: &findings,
            trigger,
            round_index,
        };
        let metacognition = if let Some(runtime) = provider_runtime.as_ref() {
            let llm = LlmDivergenceContext {
                runtime: runtime.as_ref(),
                provider_id: provider_id.as_deref(),
            };
            self.metacognition_agent
                .assess(&meta_input, Some(llm))
                .await
        } else {
            self.metacognition_agent.assess(&meta_input, None).await
        };
        let metacognition = self
            .repository()
            .add_metacognition_assessment(&metacognition)?;

        // MGATE：META 的唯一下游。
        let exit_gate = self.metacog_exit_gate.decide(&ExitGateInput {
            run,
            mission_id: Some(&mission.id),
            coverage: &coverage,
            metacognition: &metacognition,
            termination: Some(assessment),
            round_index,
            closure_round: true,
            goal_outcome_type: Some(mission.goal_contract.outcome_type),
        });
        let exit_gate = self.repository().add_exit_gate_decision(&exit_gate)?;

        // EGUARD：ESC 与 Branch Generator 之间的强制预算闸。DONE 的闸不
        // 放行任何方向——裁决留作诚实的审计记录。
        let escalation_directions: &[models::MetacognitionDirection] =
            if exit_gate.decision == ExitGateDecisionValue::Escalate {
                &metacognition.directions
            } else {
                &[]
            };
        let guard = self.escalation_guard.review(&EscalationInput {
            run,
            mission_id: Some(&mission.id),
            directions: escalation_directions,
            branches: &branches,
            budget_steps_remaining: run.max_total_steps - run.steps_used,
            exit_gate: &exit_gate,
            round_index,
            escalation_allowed: allow_escalation,
        });
        let guard = self.repository().add_escalation_guard_verdict(&guard)?;

        let mut admitted_branch_ids: Vec<String> = Vec::new();
        if exit_gate.decision == ExitGateDecisionValue::Escalate
            && !guard.admitted_directions.is_empty()
        {
            let facts = self.repository().list_facts(project.id.as_str())?;
            let candidates =
                self.branch_generator
                    .generate_from_directions(&DirectionMaterializationInput {
                        mission,
                        project,
                        directions: &guard.admitted_directions,
                        facts: &facts,
                        run_id: Some(&run.id),
                        round_index,
                    })?;
            let known_fact_ids: Vec<String> = facts
                .iter()
                .map(|fact| fact.id.as_str().to_string())
                .collect();
            let admitted = self
                .critique_and_admit_branches(candidates, &known_fact_ids, run)
                .await?;
            admitted_branch_ids = admitted
                .iter()
                .map(|branch| branch.id.as_str().to_string())
                .collect();

            // 升级方向已物化：留下人读的"下一步行动"注记。
            let titles = exit_gate.new_direction_titles.join("; ");
            self.record_narrative_safe(NarrativeDraft {
                run_id: Some(&run.id),
                mission_id: Some(&mission.id),
                metadata: Some(serde_json::Map::from_iter([
                    ("round_index".to_string(), Value::from(round_index)),
                    (
                        "admitted_branch_ids".to_string(),
                        Value::Array(
                            admitted_branch_ids
                                .iter()
                                .map(|id| Value::String(id.clone()))
                                .collect(),
                        ),
                    ),
                ])),
                ..NarrativeDraft::new(
                    &project.id,
                    "closure_chain",
                    models::AgentNarrativeEventKind::NextAction,
                    &format!(
                        "Closure round {round_index}: escalation admitted {} branch(es); pursue: {titles}",
                        admitted_branch_ids.len()
                    ),
                )
            });
        }

        self.record_event_safe(EventDraft {
            run_id: Some(&run.id),
            status: Some(exit_gate.decision.as_str()),
            data: Some(serde_json::Map::from_iter([
                (
                    "mission_id".to_string(),
                    Value::String(mission.id.as_str().to_string()),
                ),
                ("round_index".to_string(), Value::from(round_index)),
                (
                    "coverage_assessment_id".to_string(),
                    Value::String(coverage.id.as_str().to_string()),
                ),
                (
                    "blind_spots".to_string(),
                    Value::Array(
                        coverage
                            .blind_spots
                            .iter()
                            .map(|domain| Value::String(domain.as_str().to_string()))
                            .collect(),
                    ),
                ),
                (
                    "metacognition_assessment_id".to_string(),
                    Value::String(metacognition.id.as_str().to_string()),
                ),
                (
                    "direction_count".to_string(),
                    Value::from(metacognition.directions.len() as i64),
                ),
                (
                    "exit_gate_decision_id".to_string(),
                    Value::String(exit_gate.id.as_str().to_string()),
                ),
                (
                    "escalation_guard_verdict_id".to_string(),
                    Value::String(guard.id.as_str().to_string()),
                ),
                (
                    "admitted_branch_ids".to_string(),
                    Value::Array(
                        admitted_branch_ids
                            .iter()
                            .map(|id| Value::String(id.clone()))
                            .collect(),
                    ),
                ),
                (
                    "escalation_allowed".to_string(),
                    Value::Bool(allow_escalation),
                ),
            ])),
            ..EventDraft::new(
                &project.id,
                models::AuditEventType::UserNote,
                "closure_chain",
                &format!(
                    "Mission closure chain round {round_index}: {}",
                    exit_gate.decision.as_str()
                ),
            )
        })
        .await;
        self.add_observation(ObservationDraft {
            mission_id: Some(&mission.id),
            worker_id: Some("closure_chain"),
            observation_type: models::ObservationType::Progress,
            data: Some(serde_json::Map::from_iter([
                ("round_index".to_string(), Value::from(round_index)),
                (
                    "exit_gate_decision".to_string(),
                    Value::String(exit_gate.decision.as_str().to_string()),
                ),
                (
                    "blind_spots".to_string(),
                    Value::Array(
                        coverage
                            .blind_spots
                            .iter()
                            .map(|domain| Value::String(domain.as_str().to_string()))
                            .collect(),
                    ),
                ),
                (
                    "new_directions".to_string(),
                    Value::Array(
                        exit_gate
                            .new_direction_titles
                            .iter()
                            .map(|title| Value::String(title.clone()))
                            .collect(),
                    ),
                ),
                (
                    "admitted_branch_ids".to_string(),
                    Value::Array(
                        admitted_branch_ids
                            .iter()
                            .map(|id| Value::String(id.clone()))
                            .collect(),
                    ),
                ),
            ])),
            ..ObservationDraft::new(
                &project.id,
                &run.id,
                &format!(
                    "Closure chain {}: {}",
                    exit_gate.decision.as_str(),
                    coverage.summary
                ),
            )
        })
        .await?;

        Ok(ClosureChainOutcome {
            coverage,
            metacognition,
            exit_gate,
            guard,
            admitted_branch_ids,
        })
    }

    /// 解释收口链为何拒绝一个候选完成（Python `_closure_refusal_note`）。
    pub(crate) fn closure_refusal_note(closure: &ClosureChainOutcome) -> String {
        let mut parts = closure.exit_gate.reasons.clone();
        if !closure.guard.rejected_directions.is_empty() {
            parts.push(format!(
                "escalation guard rejected: {}",
                closure
                    .guard
                    .rejected_directions
                    .iter()
                    .map(|rejection| { format!("{} ({})", rejection.title, rejection.reason) })
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        format!("closure chain refused completion: {}", parts.join("; "))
    }
}

/// 从 run config 解析发散 provider id（Python `_metacognition_provider_id`）：
/// `metacognition.provider_id` 非空白字符串。
fn metacognition_provider_id(config: &serde_json::Map<String, Value>) -> Option<String> {
    let raw = config.get("metacognition")?;
    let provider_id = raw.get("provider_id")?;
    let provider_id = provider_id.as_str()?;
    let trimmed = provider_id.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 收口链升级轮次上限（Python `_resolve_max_closure_rounds`）：
/// `max_closure_rounds`（默认 2），钳制到 `[1, 5]`。
pub(crate) fn resolve_max_closure_rounds(run: &AuditRun) -> i64 {
    resolve_bounded_config_int(&run.config, "max_closure_rounds", 2, 1, 5)
}

/// branch runtime 的最大 pass 数（Python `_resolve_max_branch_passes`）：
/// `max_branch_passes`（默认 10），钳制到 `[1, 50]`。
pub(crate) fn resolve_max_branch_passes(run: &AuditRun) -> i64 {
    resolve_bounded_config_int(&run.config, "max_branch_passes", 10, 1, 50)
}

/// run config 的 int 解析（Python `_resolve_max_closure_rounds` /
/// `_resolve_max_branch_passes` 共有的容错形状：bool/非法字符串 → 默认，
/// int/可解析字符串 → 值，最终钳制）。
fn resolve_bounded_config_int(
    config: &serde_json::Map<String, Value>,
    key: &str,
    default: i64,
    minimum: i64,
    maximum: i64,
) -> i64 {
    let value = match config.get(key) {
        None | Some(Value::Null) => default,
        Some(Value::Number(number)) => number.as_i64().unwrap_or(default),
        Some(Value::String(raw)) => raw.trim().parse::<i64>().unwrap_or(default),
        Some(_) => default,
    };
    value.clamp(minimum, maximum)
}

#[cfg(test)]
mod tests {
    use models::AuditRun;
    use models::ProjectId;
    use serde_json::json;

    use super::*;

    fn run_with_config(config: serde_json::Value) -> AuditRun {
        let mut run = AuditRun::new(ProjectId::new("p1".to_string()));
        run.config = json!(config).as_object().cloned().unwrap_or_default();
        run
    }

    #[test]
    fn max_closure_rounds_defaults_and_clamps() {
        assert_eq!(resolve_max_closure_rounds(&run_with_config(json!({}))), 2);
        assert_eq!(
            resolve_max_closure_rounds(&run_with_config(json!({
                "max_closure_rounds": 9
            }))),
            5
        );
        assert_eq!(
            resolve_max_closure_rounds(&run_with_config(json!({
                "max_closure_rounds": 0
            }))),
            1
        );
        // Python：bool 不算 int，字符串可解析。
        assert_eq!(
            resolve_max_closure_rounds(&run_with_config(json!({
                "max_closure_rounds": true
            }))),
            2
        );
        assert_eq!(
            resolve_max_closure_rounds(&run_with_config(json!({
                "max_closure_rounds": "3"
            }))),
            3
        );
        assert_eq!(
            resolve_max_closure_rounds(&run_with_config(json!({
                "max_closure_rounds": "not-a-number"
            }))),
            2
        );
    }

    #[test]
    fn max_branch_passes_defaults_and_clamps() {
        assert_eq!(resolve_max_branch_passes(&run_with_config(json!({}))), 10);
        assert_eq!(
            resolve_max_branch_passes(&run_with_config(json!({
                "max_branch_passes": 100
            }))),
            50
        );
        assert_eq!(
            resolve_max_branch_passes(&run_with_config(json!({
                "max_branch_passes": "7"
            }))),
            7
        );
    }

    #[test]
    fn metacognition_provider_id_requires_non_blank_string() {
        assert_eq!(
            metacognition_provider_id(
                &json!({"metacognition": {"provider_id": "openai"}})
                    .as_object()
                    .cloned()
                    .unwrap_or_default()
            ),
            Some("openai".to_string())
        );
        // 空白 → None；strip 语义（" openai " → "openai"）。
        assert_eq!(
            metacognition_provider_id(
                &json!({"metacognition": {"provider_id": "   "}})
                    .as_object()
                    .cloned()
                    .unwrap_or_default()
            ),
            None
        );
        assert_eq!(
            metacognition_provider_id(
                &json!({"metacognition": {"provider_id": " x "}})
                    .as_object()
                    .cloned()
                    .unwrap_or_default()
            ),
            Some("x".to_string())
        );
        assert_eq!(
            metacognition_provider_id(
                &json!({"metacognition": {"provider_id": 42}})
                    .as_object()
                    .cloned()
                    .unwrap_or_default()
            ),
            None
        );
        assert_eq!(metacognition_provider_id(&serde_json::Map::new()), None);
    }
}
