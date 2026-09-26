//! MGATE：元认知出口判定 —— `server/core/agents/closure.py`
//! `MetacogExitGate` 与 `branch_kind_for_domains` 的移植。
//!
//! 收口轮无新方向即 DONE；发散或新方向即 ESC。MGATE 绝不自行创建分支
//! ——升级必须流经 EGUARD，预算闸因此不可绕过。
//!
//! # 红线语义：结果导向权威规则
//!
//! 结果导向 Mission（flag / confirmed finding / verified evidence）的目标
//! 契约一旦满足，已验证的交付物即产品本身：覆盖盲区只能作为后续信息
//! 记录在判定上，绝不能否决一个已通过目标门的成果。这与 Termination
//! Judge 施加的是同一条权威规则。

use models::agent::TerminationAssessment;
use models::closure::CoverageAssessment;
use models::closure::ExitGateDecision;
use models::closure::ExitGateDecisionValue;
use models::closure::MetacognitionAssessment;
use models::domain::AuditDomain;
use models::ids::MissionId;
use models::mission::GoalOutcomeType;
use models::run::AuditRun;

/// 一次出口判定的输入（Python `decide` 的 keyword-only 参数镜像）。
pub struct ExitGateInput<'a> {
    /// 所属 Run。
    pub run: &'a AuditRun,
    /// 所属 Mission（Python 侧可为 `None`）。
    pub mission_id: Option<&'a MissionId>,
    /// COV 输出。
    pub coverage: &'a CoverageAssessment,
    /// META 输出。
    pub metacognition: &'a MetacognitionAssessment,
    /// 终止评估（`None` 时权威规则不触发）。
    pub termination: Option<&'a TerminationAssessment>,
    /// 收口轮次。
    pub round_index: i64,
    /// 是否收口轮。
    pub closure_round: bool,
    /// 目标契约的结果类别（`None` 时权威规则不触发）。
    pub goal_outcome_type: Option<GoalOutcomeType>,
}

impl<'a> ExitGateInput<'a> {
    /// 以默认轮参数构造（`round_index=0`，`closure_round=true`）。
    #[must_use]
    pub fn new(
        run: &'a AuditRun,
        coverage: &'a CoverageAssessment,
        metacognition: &'a MetacognitionAssessment,
    ) -> Self {
        Self {
            run,
            mission_id: None,
            coverage,
            metacognition,
            termination: None,
            round_index: 0,
            closure_round: true,
            goal_outcome_type: None,
        }
    }
}

/// 判定是否落在结果导向集合内（Python `_RESULT_ORIENTED_OUTCOMES`）。
///
/// 这三类结果的已验证交付物就是 Mission 产品本身。
#[must_use]
pub const fn is_result_oriented(outcome_type: GoalOutcomeType) -> bool {
    matches!(
        outcome_type,
        GoalOutcomeType::FlagCapture
            | GoalOutcomeType::ConfirmedFinding
            | GoalOutcomeType::VerifiedEvidence
    )
}

/// MGATE：META 的唯一下游，裁决 DONE 与 ESCALATE。
#[derive(Debug, Default)]
pub struct MetacogExitGate;

impl MetacogExitGate {
    /// 执行一次出口判定（纯计算，无 I/O）。
    ///
    /// 判定顺序（顺序即权威层级）：
    /// 1. 目标已满足且结果导向 → DONE（权威规则，覆盖盲区不可否决）；
    /// 2. 有新方向 → ESCALATE；
    /// 3. 有覆盖盲区 → ESCALATE；
    /// 4. 否则 → DONE（诚实收口）。
    #[must_use]
    pub fn decide(&self, input: &ExitGateInput<'_>) -> ExitGateDecision {
        let mut reasons: Vec<String> = Vec::new();
        let new_directions = &input.metacognition.directions;
        let blind_spots = &input.coverage.blind_spots;

        let goal_satisfied = input
            .termination
            .is_some_and(|assessment| assessment.goal_satisfied == Some(true));
        let decision = if goal_satisfied && input.goal_outcome_type.is_some_and(is_result_oriented)
        {
            // 红线：已验证的交付物是权威成果——盲区降级为可选后续，
            // 不再是阻塞项。
            reasons.push(
                "the verified Mission deliverable is authoritative: the goal contract is \
                 satisfied, so closure is approved"
                    .to_string(),
            );
            if !blind_spots.is_empty() {
                reasons.push(format!(
                    "unexercised domains remain recorded as optional follow-up, not as \
                     blockers: {}",
                    blind_spots
                        .iter()
                        .map(|domain| domain.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            ExitGateDecisionValue::Done
        } else if !new_directions.is_empty() {
            reasons.push(format!(
                "metacognition found {} new direction(s): {}",
                new_directions.len(),
                new_directions
                    .iter()
                    .map(|direction| direction.title.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
            ExitGateDecisionValue::Escalate
        } else if !blind_spots.is_empty() {
            reasons.push(format!(
                "coverage still reports blind spots that metacognition could not turn into \
                 directions: {}",
                blind_spots
                    .iter()
                    .map(|domain| domain.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            ExitGateDecisionValue::Escalate
        } else {
            reasons.push(
                "closure round with no new directions: all domains relevant to the Mission \
                 were exercised and metacognition found no unexamined path"
                    .to_string(),
            );
            ExitGateDecisionValue::Done
        };

        tracing::info!(
            run = %input.run.id,
            mission = ?input.mission_id,
            decision = ?decision,
            reason = reasons.first().map(String::as_str).unwrap_or_default(),
            "MGATE exit-gate verdict"
        );

        let mut decision_record =
            ExitGateDecision::new(input.run.project_id.clone(), input.run.id.clone());
        decision_record.mission_id = input.mission_id.cloned();
        decision_record.round_index = input.round_index;
        decision_record.decision = decision;
        decision_record.closure_round = input.closure_round;
        decision_record.new_direction_titles = new_directions
            .iter()
            .map(|direction| direction.title.clone())
            .collect();
        decision_record.blind_spots.clone_from(blind_spots);
        decision_record.reasons = reasons;
        decision_record.termination_assessment_id =
            input.termination.map(|assessment| assessment.id.clone());
        decision_record.coverage_assessment_id = Some(input.coverage.id.clone());
        decision_record.metacognition_assessment_id = Some(input.metacognition.id.clone());
        decision_record
    }
}

/// 盲区域 → 可派发分支种类的静态表（Python `_DOMAIN_BRANCH_KINDS` 的
/// 按分支种类聚合视图，首见顺序与 Python dict 插入序一致）。
///
/// 首见顺序决定并列得分时的胜者：`score > best_score` 严格比较，先入者赢，
/// 因此该表顺序是冻结语义，不得按字母序重排。
const KIND_DOMAINS: [(&str, &[AuditDomain]); 11] = [
    (
        "mixed.initial_surface",
        &[AuditDomain::AssetRecon, AuditDomain::Composite],
    ),
    (
        "url.surface_mapping",
        &[AuditDomain::WebRecon, AuditDomain::ContentDiscovery],
    ),
    (
        "url.known_exposure",
        &[
            AuditDomain::FingerprintIntelligence,
            AuditDomain::ExposureIntelligence,
        ],
    ),
    (
        "source.source_sink",
        &[AuditDomain::WebSast, AuditDomain::CodeDeepSast],
    ),
    (
        "url.input_validation",
        &[
            AuditDomain::WebDast,
            AuditDomain::WebIast,
            AuditDomain::WebValidation,
            AuditDomain::ExploitabilityValidation,
            AuditDomain::Exploitability,
            AuditDomain::InternalSurface,
        ],
    ),
    (
        "traffic.parameter_analysis",
        &[AuditDomain::TrafficIntelligence],
    ),
    ("binary.parser_surface", &[AuditDomain::BinaryStatic]),
    ("binary.dangerous_api", &[AuditDomain::BinaryDynamic]),
    ("binary.heap_stack", &[AuditDomain::Fuzzing]),
    ("source.dependency_config", &[AuditDomain::SupplyChain]),
    ("mixed.classification", &[AuditDomain::CloudNative]),
];

/// 选择覆盖最多给定域的可派发分支种类（Python `branch_kind_for_domains`）。
///
/// 盲区由此映射回可执行分支种类，升级分支无须发明平行路由方案即可被
/// Capability Router 派发。并列得分时首见种类获胜；空输入回落
/// `"mixed.initial_surface"`。
#[must_use]
pub fn branch_kind_for_domains(domains: &[AuditDomain]) -> String {
    // Python `set(domains)`：输入先去重再计分。
    let unique: std::collections::HashSet<AuditDomain> = domains.iter().copied().collect();
    let mut best_kind = "mixed.initial_surface";
    let mut best_score: usize = 0;
    let mut best_score_set = false;
    for (kind, kind_domain_list) in KIND_DOMAINS {
        let score: usize = unique
            .iter()
            .filter(|domain| kind_domain_list.contains(domain))
            .count();
        if !best_score_set || score > best_score {
            best_score_set = true;
            best_score = score;
            best_kind = kind;
        }
    }
    best_kind.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::agent::TerminationStatus;
    use models::closure::MetacognitionDirection;
    use models::ids::ProjectId;
    use models::ids::RunId;
    use models::mission::Mission;

    fn project_id() -> ProjectId {
        ProjectId::new("proj_test".to_string())
    }

    fn run() -> AuditRun {
        let mut run = AuditRun::new(project_id());
        run.id = RunId::new("run_x".to_string());
        run
    }

    fn mission() -> Mission {
        Mission::new(project_id(), "goal".to_string())
    }

    fn direction(title: &str) -> MetacognitionDirection {
        MetacognitionDirection {
            title: title.to_string(),
            hypothesis: "A concrete falsifiable hypothesis of sufficient length".to_string(),
            rationale: String::new(),
            framework: models::closure::MetacognitionFramework::Analogy,
            related_blind_spots: Vec::new(),
            related_unmet_requirements: Vec::new(),
        }
    }

    fn coverage_with(blind_spots: &[AuditDomain]) -> CoverageAssessment {
        let mut coverage = CoverageAssessment::new(project_id(), RunId::new("run_x".to_string()));
        coverage.blind_spots = blind_spots.to_vec();
        coverage
    }

    fn metacognition_with(directions: Vec<MetacognitionDirection>) -> MetacognitionAssessment {
        let mut meta = models::closure::MetacognitionAssessment::new(
            project_id(),
            RunId::new("run_x".to_string()),
        );
        meta.directions = directions;
        meta
    }

    fn termination(goal_satisfied: Option<bool>) -> TerminationAssessment {
        let mut assessment =
            TerminationAssessment::new(project_id(), RunId::new("run_x".to_string()));
        assessment.status = TerminationStatus::Complete;
        assessment.goal_satisfied = goal_satisfied;
        assessment
    }

    #[test]
    fn escalates_on_new_direction() {
        let run = run();
        let coverage = coverage_with(&[]);
        let meta = metacognition_with(vec![direction("Probe the upload handler")]);
        let input = ExitGateInput::new(&run, &coverage, &meta);
        let decision = MetacogExitGate.decide(&input);
        assert_eq!(decision.decision, ExitGateDecisionValue::Escalate);
        assert_eq!(
            decision.new_direction_titles,
            ["Probe the upload handler".to_string()]
        );
        assert!(
            decision
                .reasons
                .first()
                .is_some_and(|reason| reason.contains("1 new direction(s)"))
        );
    }

    #[test]
    fn escalates_on_blind_spots_without_directions() {
        let run = run();
        let coverage = coverage_with(&[AuditDomain::WebDast]);
        let meta = metacognition_with(Vec::new());
        let input = ExitGateInput::new(&run, &coverage, &meta);
        let decision = MetacogExitGate.decide(&input);
        assert_eq!(decision.decision, ExitGateDecisionValue::Escalate);
        assert!(
            decision
                .reasons
                .first()
                .is_some_and(|reason| reason.contains("web_dast"))
        );
    }

    #[test]
    fn done_when_converged() {
        let run = run();
        let coverage = coverage_with(&[]);
        let meta = metacognition_with(Vec::new());
        let input = ExitGateInput::new(&run, &coverage, &meta);
        let decision = MetacogExitGate.decide(&input);
        assert_eq!(decision.decision, ExitGateDecisionValue::Done);
        assert!(
            decision
                .reasons
                .first()
                .is_some_and(|reason| reason.contains("no new directions"))
        );
    }

    #[test]
    fn result_oriented_goal_is_authoritative_over_blind_spots() {
        // 红线：flag 类目标已满足时，盲区不能否决 DONE。
        let run = run();
        let coverage = coverage_with(&[AuditDomain::WebDast, AuditDomain::WebSast]);
        let meta = metacognition_with(Vec::new());
        let term = termination(Some(true));
        let input = ExitGateInput {
            run: &run,
            mission_id: Some(&mission().id),
            coverage: &coverage,
            metacognition: &meta,
            termination: Some(&term),
            round_index: 0,
            closure_round: true,
            goal_outcome_type: Some(GoalOutcomeType::FlagCapture),
        };
        let decision = MetacogExitGate.decide(&input);
        assert_eq!(decision.decision, ExitGateDecisionValue::Done);
        assert!(
            decision
                .reasons
                .first()
                .is_some_and(|reason| reason.contains("authoritative"))
        );
        assert!(decision.reasons.len() > 1);
        assert!(decision.reasons[1].contains("optional follow-up"));
        assert_eq!(decision.blind_spots.len(), 2);
    }

    #[test]
    fn coverage_goal_still_requires_full_surface() {
        // coverage 类目标非结果导向：即使 goal_satisfied，盲区仍触发 ESC。
        let run = run();
        let coverage = coverage_with(&[AuditDomain::WebDast]);
        let meta = metacognition_with(Vec::new());
        let term = termination(Some(true));
        let input = ExitGateInput {
            run: &run,
            mission_id: None,
            coverage: &coverage,
            metacognition: &meta,
            termination: Some(&term),
            round_index: 0,
            closure_round: true,
            goal_outcome_type: Some(GoalOutcomeType::Coverage),
        };
        let decision = MetacogExitGate.decide(&input);
        assert_eq!(decision.decision, ExitGateDecisionValue::Escalate);
    }

    #[test]
    fn goal_satisfied_false_does_not_trigger_authority() {
        let run = run();
        let coverage = coverage_with(&[AuditDomain::WebDast]);
        let meta = metacognition_with(Vec::new());
        let term = termination(Some(false));
        let input = ExitGateInput {
            run: &run,
            mission_id: None,
            coverage: &coverage,
            metacognition: &meta,
            termination: Some(&term),
            round_index: 0,
            closure_round: true,
            goal_outcome_type: Some(GoalOutcomeType::FlagCapture),
        };
        let decision = MetacogExitGate.decide(&input);
        assert_eq!(decision.decision, ExitGateDecisionValue::Escalate);
    }

    #[test]
    fn branch_kind_picks_best_covering_kind() {
        assert_eq!(
            branch_kind_for_domains(&[AuditDomain::WebDast, AuditDomain::WebIast]),
            "url.input_validation"
        );
        assert_eq!(
            branch_kind_for_domains(&[AuditDomain::WebRecon, AuditDomain::ContentDiscovery]),
            "url.surface_mapping"
        );
        // 并列得分（均为 1）时首见种类 mixed.initial_surface 胜出。
        assert_eq!(
            branch_kind_for_domains(&[AuditDomain::Fuzzing]),
            "binary.heap_stack"
        );
        assert_eq!(
            branch_kind_for_domains(&[AuditDomain::SupplyChain]),
            "source.dependency_config"
        );
        // 空输入与并列 0 分：回落 mixed.initial_surface。
        assert_eq!(
            branch_kind_for_domains(&[]),
            "mixed.initial_surface".to_string()
        );
    }
}
