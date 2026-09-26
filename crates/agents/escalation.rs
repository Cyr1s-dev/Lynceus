//! EGUARD：升级预算闸 —— `server/core/agents/closure.py`
//! `EscalationGuard` 的移植。
//!
//! ESC 与 Branch Generator 之间强制的预算闸：丢弃重复方向（已探索过的
//! 假设）、强制 max-branch 上限与 Run 步数预算、限定单轮升级可新增的
//! 分支数。任何方向不经过这道闸不得成为 Branch，预算门因此不可绕过。

use std::collections::HashSet;

use models::closure::EscalationGuardRejection;
use models::closure::EscalationGuardVerdict;
use models::closure::ExitGateDecision;
use models::closure::MetacognitionDirection;
use models::ids::MissionId;
use models::mission::Branch;
use models::run::AuditRun;

/// 一次升级闸复审的输入（Python `review` 的 keyword-only 参数镜像）。
pub struct EscalationInput<'a> {
    /// 所属 Run。
    pub run: &'a AuditRun,
    /// 所属 Mission（Python 侧可为 `None`）。
    pub mission_id: Option<&'a MissionId>,
    /// 待复审的候选方向。
    pub directions: &'a [MetacognitionDirection],
    /// Mission 现有全部分支。
    pub branches: &'a [Branch],
    /// Run 剩余预算步数。
    pub budget_steps_remaining: i64,
    /// 触发复审的 MGATE 判定。
    pub exit_gate: &'a ExitGateDecision,
    /// 收口轮次。
    pub round_index: i64,
    /// 是否允许升级（收口轮上限的开关）。
    pub escalation_allowed: bool,
}

impl<'a> EscalationInput<'a> {
    /// 以默认参数构造（`round_index=0`，`escalation_allowed=true`）。
    #[must_use]
    pub fn new(
        run: &'a AuditRun,
        directions: &'a [MetacognitionDirection],
        branches: &'a [Branch],
        budget_steps_remaining: i64,
        exit_gate: &'a ExitGateDecision,
    ) -> Self {
        Self {
            run,
            mission_id: None,
            directions,
            branches,
            budget_steps_remaining,
            exit_gate,
            round_index: 0,
            escalation_allowed: true,
        }
    }
}

/// EGUARD：ESC 与 Branch Generator 之间的强制预算闸。
#[derive(Debug)]
pub struct EscalationGuard {
    max_branches: i64,
    max_per_round: i64,
}

impl Default for EscalationGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl EscalationGuard {
    /// 构造器（Python 默认 `max_branches=16, max_branches_per_round=3`，
    /// 两者均经 `max(1, …)` 下限钳制）。
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(16, 3)
    }

    /// 指定上限构造。
    #[must_use]
    pub fn with_limits(max_branches: i64, max_branches_per_round: i64) -> Self {
        Self {
            max_branches: max_branches.max(1),
            max_per_round: max_branches_per_round.max(1),
        }
    }

    /// 复审一轮候选方向（纯计算，无 I/O）。
    ///
    /// 拒绝顺序即优先级：收口轮上限 → 预算耗尽 → 分支数/单轮上限 →
    /// 重复假设。被拒方向逐条记录理由，审计链完整可回放。
    #[must_use]
    pub fn review(&self, input: &EscalationInput<'_>) -> EscalationGuardVerdict {
        // Python `{" ".join(b.hypothesis.lower().split()) for b in branches}`。
        let existing_hypotheses: HashSet<String> = input
            .branches
            .iter()
            .map(|branch| normalize_hypothesis(&branch.hypothesis))
            .collect();
        let branch_count = i64::try_from(input.branches.len()).unwrap_or(i64::MAX);
        let mut slots = (self.max_branches - branch_count).max(0);
        let mut reasons: Vec<String> = Vec::new();
        let mut rejected: Vec<EscalationGuardRejection> = Vec::new();
        let mut admitted: Vec<MetacognitionDirection> = Vec::new();
        let blocked_by_budget = input.budget_steps_remaining <= 0;

        if !input.escalation_allowed {
            reasons.push(
                "escalation disabled: closure round cap reached; no new branches may enter \
                 the Mission without a user resume"
                    .to_string(),
            );
        }
        if blocked_by_budget {
            reasons.push(format!(
                "run budget exhausted ({}/{} steps); no escalation branch can execute",
                input.run.steps_used, input.run.max_total_steps
            ));
        }
        if slots == 0 {
            reasons.push(format!(
                "max branch count reached ({}/{})",
                branch_count, self.max_branches
            ));
        }

        for direction in input.directions {
            if !input.escalation_allowed {
                rejected.push(EscalationGuardRejection {
                    title: direction.title.clone(),
                    reason: "escalation disabled: closure round cap reached".to_string(),
                });
                continue;
            }
            if blocked_by_budget {
                rejected.push(EscalationGuardRejection {
                    title: direction.title.clone(),
                    reason: "run step budget exhausted before dispatch".to_string(),
                });
                continue;
            }
            // max_per_round 经构造钳制 ≥ 1，转换不会失败；usize::MAX
            // 兜底仅作类型安全，不改变语义。
            let per_round_cap = usize::try_from(self.max_per_round).unwrap_or(usize::MAX);
            if slots <= 0 || admitted.len() >= per_round_cap {
                rejected.push(EscalationGuardRejection {
                    title: direction.title.clone(),
                    reason: if slots <= 0 {
                        "max branch count reached".to_string()
                    } else {
                        format!("per-round escalation cap reached ({})", self.max_per_round)
                    },
                });
                continue;
            }
            if existing_hypotheses.contains(&direction.normalized_hypothesis()) {
                rejected.push(EscalationGuardRejection {
                    title: direction.title.clone(),
                    reason: "hypothesis duplicates an already-explored branch".to_string(),
                });
                continue;
            }
            admitted.push(direction.clone());
            slots -= 1;
        }

        if !admitted.is_empty() {
            reasons.push(format!(
                "admitted {} direction(s) to the Branch Generator",
                admitted.len()
            ));
        } else if rejected.is_empty() {
            reasons.push("no directions were submitted for escalation".to_string());
        }

        tracing::info!(
            run = %input.run.id,
            mission = ?input.mission_id,
            admitted = admitted.len(),
            rejected = rejected.len(),
            blocked_by_budget,
            "EGUARD escalation review"
        );

        let mut verdict =
            EscalationGuardVerdict::new(input.run.project_id.clone(), input.run.id.clone());
        verdict.mission_id = input.mission_id.cloned();
        verdict.round_index = input.round_index;
        verdict.admitted_directions = admitted;
        verdict.rejected_directions = rejected;
        verdict.branch_count_before = branch_count;
        verdict.branch_count_after = branch_count
            .saturating_add(i64::try_from(verdict.admitted_directions.len()).unwrap_or(0));
        verdict.max_branches = self.max_branches;
        verdict.budget_steps_remaining = input.budget_steps_remaining.max(0);
        verdict.blocked_by_budget = blocked_by_budget;
        verdict.reasons = reasons;
        verdict.exit_gate_decision_id = Some(input.exit_gate.id.clone());
        verdict
    }
}

/// Python `" ".join(text.lower().split())`：小写化 + 任意空白折叠为单空格。
///
/// 重复检测按归一化形态比较——大小写或换行差异不得让同一假设
/// 二次入池。
fn normalize_hypothesis(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::closure::MetacognitionFramework;
    use models::ids::ProjectId;
    use models::ids::RunId;
    use models::mission::Mission;

    const PROJECT_ID_VALUE: &str = "proj_test";

    fn project_id() -> ProjectId {
        ProjectId::new(PROJECT_ID_VALUE.to_string())
    }

    /// Python `_run()`：`max_total_steps=64`，`steps_used=0`。
    fn run() -> AuditRun {
        let mut run = AuditRun::new(project_id());
        run.id = RunId::new("run_x".to_string());
        run.max_total_steps = 64;
        run
    }

    /// Python `_direction()`：默认标题与假设。
    fn direction(title: &str) -> MetacognitionDirection {
        MetacognitionDirection {
            title: title.to_string(),
            hypothesis: "an unexamined surface may hold the missing path forward".to_string(),
            rationale: String::new(),
            framework: MetacognitionFramework::Analogy,
            related_blind_spots: Vec::new(),
            related_unmet_requirements: Vec::new(),
        }
    }

    /// Python `_branch(kind, hypothesis=…)`。
    fn branch(hypothesis: &str) -> Branch {
        let mission = Mission::new(project_id(), "goal".to_string());
        Branch::new(
            project_id(),
            mission.id,
            "Branch".to_string(),
            hypothesis.to_string(),
        )
    }

    /// Python `_guard()`：`max_branches=4`，`max_branches_per_round=2`。
    fn guard() -> EscalationGuard {
        EscalationGuard::with_limits(4, 2)
    }

    /// Python `_gate(ESCALATE)`。
    fn gate() -> ExitGateDecision {
        let mut gate = ExitGateDecision::new(project_id(), RunId::new("run_x".to_string()));
        gate.decision = models::closure::ExitGateDecisionValue::Escalate;
        gate
    }

    #[test]
    fn admits_within_caps() {
        // Python test_escalation_guard_admits_within_caps：guard(6, 2)，
        // 2 条既有分支 + 3 个方向 → A/B 放行，C 撞单轮上限。
        let run = run();
        let directions = vec![direction("A"), direction("B"), direction("C")];
        let branches = vec![
            branch("explored hypothesis number one"),
            branch("explored hypothesis number two"),
        ];
        let exit_gate = gate();
        let input = EscalationInput::new(&run, &directions, &branches, 10, &exit_gate);
        let verdict = EscalationGuard::with_limits(6, 2).review(&input);

        let titles: Vec<&str> = verdict
            .admitted_directions
            .iter()
            .map(|direction| direction.title.as_str())
            .collect();
        assert_eq!(titles, ["A", "B"]);
        assert_eq!(verdict.branch_count_after, 4);
        assert!(!verdict.blocked_by_budget);
        let rejection = &verdict.rejected_directions[0];
        assert_eq!(rejection.title, "C");
        assert!(rejection.reason.contains("per-round escalation cap"));
    }

    #[test]
    fn rejects_duplicate_hypotheses() {
        let run = run();
        let directions = vec![direction("A direction")];
        let branches = vec![branch(
            "An unexamined surface may hold the missing path forward",
        )];
        let exit_gate = gate();
        let input = EscalationInput::new(&run, &directions, &branches, 10, &exit_gate);
        let verdict = guard().review(&input);

        assert!(verdict.admitted_directions.is_empty());
        assert!(
            verdict.rejected_directions[0]
                .reason
                .contains("duplicates an already-explored branch")
        );
    }

    #[test]
    fn blocks_on_exhausted_budget() {
        let run = run();
        let directions = vec![direction("A direction")];
        let exit_gate = gate();
        let input = EscalationInput::new(&run, &directions, &[], 0, &exit_gate);
        let verdict = guard().review(&input);

        assert!(verdict.blocked_by_budget);
        assert!(verdict.admitted_directions.is_empty());
        assert!(
            verdict.rejected_directions[0]
                .reason
                .contains("run step budget exhausted")
        );
    }

    #[test]
    fn enforces_closure_round_cap() {
        let run = run();
        let directions = vec![direction("A direction")];
        let exit_gate = gate();
        let mut input = EscalationInput::new(&run, &directions, &[], 10, &exit_gate);
        input.escalation_allowed = false;
        let verdict = guard().review(&input);

        assert!(verdict.admitted_directions.is_empty());
        assert!(
            verdict.rejected_directions[0]
                .reason
                .contains("closure round cap reached")
        );
        assert!(
            verdict
                .reasons
                .iter()
                .any(|reason| reason.contains("escalation disabled"))
        );
    }

    #[test]
    fn blocks_when_max_branches_reached() {
        let run = run();
        let directions = vec![direction("A direction")];
        let branches = vec![
            branch("hypothesis one explored here"),
            branch("hypothesis two explored here"),
            branch("hypothesis three explored here"),
            branch("hypothesis four explored here"),
        ];
        let exit_gate = gate();
        let input = EscalationInput::new(&run, &directions, &branches, 10, &exit_gate);
        let verdict = guard().review(&input);

        assert!(verdict.admitted_directions.is_empty());
        assert!(
            verdict.rejected_directions[0]
                .reason
                .contains("max branch count reached")
        );
    }
}
