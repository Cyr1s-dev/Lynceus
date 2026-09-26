//! Ralph-Loop 式终止判定 —— `server/core/agents/termination.py`
//! `TerminationEvaluator` 的移植。
//!
//! 外置的确定性 stop / pause / continue / `needs_human_decision` 评估，
//! 不改写 Run。目标契约评估（`_evaluate_goal`）消费 Mission 的
//! 结构化契约而非用户措辞：flag 捕获类目标必须持有通过产品验证回执
//! （SHA-256 逐字节校验）的正向成果——"我尽力了"不构成完成。

use std::collections::HashMap;
use std::collections::HashSet;

use models::agent::Observation;
use models::agent::ObservationType;
use models::agent::TerminationAssessment;
use models::agent::TerminationStatus;
use models::agent::WorkerLease;
use models::agent::WorkerLeaseStatus;
use models::decision::DecisionGate;
use models::decision::DecisionGateStatus;
use models::evidence::Evidence;
use models::finding::Finding;
use models::ids::ProjectId;
use models::intent::Intent;
use models::intent::IntentStatus;
use models::lifecycle::FindingStatus;
use models::lifecycle::Severity;
use models::lifecycle::TaskStatus;
use models::mission::Branch;
use models::mission::BranchStatus;
use models::mission::GoalContractStatus;
use models::mission::GoalOutcomeType;
use models::mission::Mission;
use models::run::AgentTask;
use models::run::AuditRun;
use sha2::Digest;
use sha2::Sha256;

/// 一次终止评估的输入（Python `evaluate` 的 keyword-only 参数镜像）。
#[derive(Debug)]
pub struct TerminationInput<'a> {
    /// 所属 Project。
    pub project_id: &'a ProjectId,
    /// 被评估的 Run。
    pub run: &'a AuditRun,
    /// 全部 Intent（Mission 域过滤在评估内完成）。
    pub intents: &'a [Intent],
    /// 全部 Finding。
    pub findings: &'a [Finding],
    /// 全部 worker 租约。
    pub worker_leases: &'a [WorkerLease],
    /// 全部决策门。
    pub decision_gates: &'a [DecisionGate],
    /// 全部观察（默认空）。
    pub observations: &'a [Observation],
    /// 所属 Mission（经典项目 Run 传 `None`）。
    pub mission: Option<&'a Mission>,
    /// 全部 Task（默认空）。
    pub tasks: &'a [AgentTask],
    /// 全部分支（默认空）。
    pub branches: &'a [Branch],
    /// 全部证据（默认空）。
    pub evidence: &'a [Evidence],
}

impl<'a> TerminationInput<'a> {
    /// 以经典项目 Run 的最小输入构造（无 mission / 可选集合为空）。
    #[must_use]
    pub fn new(project_id: &'a ProjectId, run: &'a AuditRun) -> Self {
        Self {
            project_id,
            run,
            intents: &[],
            findings: &[],
            worker_leases: &[],
            decision_gates: &[],
            observations: &[],
            mission: None,
            tasks: &[],
            branches: &[],
            evidence: &[],
        }
    }
}

/// 目标评估的解构结果（Python `_evaluate_goal` 的四元组）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct GoalEvaluation {
    /// 目标是否满足（`None` = 无 Mission 目标契约适用）。
    satisfied: Option<bool>,
    /// 全部目标需求描述。
    requirements: Vec<String>,
    /// 已满足的需求描述。
    satisfied_requirements: Vec<String>,
    /// 未满足的需求描述。
    unmet_requirements: Vec<String>,
    /// 目标契约本身无法机器校验（`needs_review` / `custom` /
    /// `auto_complete=false`），需要操作者先分类再谈满足。
    ///
    /// 与 `satisfied == Some(false)` 的区别：后者是"判据明确但没达到"，
    /// 前者是"根本没有判据"。前者落 `Pause` 会让 Mission 停在一个
    /// 用户既无法行动、也不知道该做什么的状态——目标
    /// 分解产出空时由规则兜底拆出节点，不让这种状态存在；Lynceus 保留
    /// 人工分类这道安全闸，但必须把它变成**可行动的**待决，而不是静默的
    /// 暂停。
    needs_contract_review: bool,
}

/// 一次评估的运行域收集结果（Python `evaluate` 的局部集合镜像）。
///
/// 全部集合都已按 run / mission 域过滤——状态阶梯只消费过滤后的信号。
#[derive(Debug)]
struct TerminationScope<'a> {
    /// 本 Run 的 Task。
    run_tasks: Vec<&'a AgentTask>,
    /// 本 Run（及无 run 归属）的分支。
    run_branches: Vec<&'a Branch>,
    /// 本 Run（及无 run 归属）的证据。
    run_evidence: Vec<&'a Evidence>,
    /// Mission 域内（经典项目则为 run 域内）的 Intent。
    scoped_intents: Vec<&'a Intent>,
    /// 未解决 Intent 的 ID（输出字段）。
    unresolved_intent_ids: Vec<String>,
    /// 未解决且可运行（非 Pending 或已绑定 solver）的 Intent。
    runnable_unresolved: Vec<&'a Intent>,
    /// 本 Run 的活跃租约。
    active_leases: Vec<&'a WorkerLease>,
    /// 本 Run 的待答决策门。
    pending_gates: Vec<&'a DecisionGate>,
    /// 状态为 PROPOSED/ACTIVE 且步数未耗尽的分支 ID。
    unresolved_branch_ids: Vec<String>,
    /// 状态为 PROPOSED/ACTIVE 且步数已耗尽的分支 ID。
    exhausted_open_branch_ids: Vec<String>,
    /// 高危且待审的 Finding。
    high_open: Vec<&'a Finding>,
    /// 证据缺口数。
    evidence_gap_count: i64,
    /// Run 预算是否耗尽。
    run_budget_exhausted: bool,
}

/// 收集一次评估的全部运行域信号（Python `evaluate` 的收集段）。
fn collect_scope<'a>(input: &'a TerminationInput<'_>) -> TerminationScope<'a> {
    let run = input.run;
    let run_tasks: Vec<&AgentTask> = input
        .tasks
        .iter()
        .filter(|task| task.run_id == run.id)
        .collect();
    let run_branches: Vec<&Branch> = input
        .branches
        .iter()
        .filter(|branch| {
            (branch.run_id.is_none() || branch.run_id.as_ref() == Some(&run.id))
                && input
                    .mission
                    .is_none_or(|mission| branch.mission_id == mission.id)
        })
        .collect();
    let run_evidence: Vec<&Evidence> = input
        .evidence
        .iter()
        .filter(|item| item.run_id.is_none() || item.run_id.as_ref() == Some(&run.id))
        .collect();

    // Mission 完成只对实际被接纳进该 Mission 的工作计分。项目级规划器
    // 建议可能共享 run id，但它们不是被调度的 Mission 工作，不能让
    // 已完成的分支运行时永远活着。
    let scoped_intents: Vec<&Intent> = input
        .intents
        .iter()
        .filter(|intent| match input.mission {
            Some(mission) => intent.mission_id.as_ref() == Some(&mission.id),
            None => intent.run_id.is_none() || intent.run_id.as_ref() == Some(&run.id),
        })
        .collect();
    let unresolved_intents: Vec<&Intent> = scoped_intents
        .iter()
        .copied()
        .filter(|intent| {
            matches!(
                intent.status,
                IntentStatus::Pending | IntentStatus::Claimed | IntentStatus::InProgress
            )
        })
        .collect();
    let unresolved_intent_ids: Vec<String> = unresolved_intents
        .iter()
        .map(|intent| intent.id.as_str().to_string())
        .collect();
    let runnable_unresolved: Vec<&Intent> = unresolved_intents
        .iter()
        .copied()
        .filter(|intent| intent.status != IntentStatus::Pending || intent.solver.is_some())
        .collect();
    let active_leases: Vec<&WorkerLease> = input
        .worker_leases
        .iter()
        .filter(|lease| lease.run_id == run.id && lease.status == WorkerLeaseStatus::Active)
        .collect();
    let pending_gates: Vec<&DecisionGate> = input
        .decision_gates
        .iter()
        .filter(|gate| gate.audit_run_id == run.id && gate.status == DecisionGateStatus::Pending)
        .collect();
    let unresolved_branch_ids: Vec<String> = run_branches
        .iter()
        .filter(|branch| {
            matches!(branch.status, BranchStatus::Proposed | BranchStatus::Active)
                && branch.steps_used < branch.budget_steps
        })
        .map(|branch| branch.id.as_str().to_string())
        .collect();
    let exhausted_open_branch_ids: Vec<String> = run_branches
        .iter()
        .filter(|branch| {
            matches!(branch.status, BranchStatus::Proposed | BranchStatus::Active)
                && branch.steps_used >= branch.budget_steps
        })
        .map(|branch| branch.id.as_str().to_string())
        .collect();
    let evidence_gap_count = evidence_gap_count(input.findings, input.observations);
    let high_open: Vec<&Finding> = input
        .findings
        .iter()
        .filter(|finding| {
            (finding.run_id.is_none() || finding.run_id.as_ref() == Some(&run.id))
                && matches!(finding.severity, Severity::High | Severity::Critical)
                && finding.status == FindingStatus::NeedsReview
        })
        .collect();

    TerminationScope {
        run_tasks,
        run_branches,
        run_evidence,
        scoped_intents,
        unresolved_intent_ids,
        runnable_unresolved,
        active_leases,
        pending_gates,
        unresolved_branch_ids,
        exhausted_open_branch_ids,
        high_open,
        evidence_gap_count,
        run_budget_exhausted: run.max_total_steps > 0 && run.steps_used >= run.max_total_steps,
    }
}

/// 状态阶梯裁决（Python `evaluate` 的阶梯段）：返回状态与理由列表。
///
/// # 红线语义
///
/// 已验证的产品成果是权威的：目标契约满足时 `COMPLETE` 无条件胜出。
fn resolve_status(
    scope: &TerminationScope<'_>,
    goal: &GoalEvaluation,
    has_mission: bool,
) -> (TerminationStatus, Vec<String>) {
    let mut reasons: Vec<String> = Vec::new();
    let status = if goal.satisfied == Some(true) {
        // 已验证的产品成果是权威的。
        reasons.push("the persisted Mission goal contract is satisfied".to_string());
        TerminationStatus::Complete
    } else if !scope.pending_gates.is_empty() {
        reasons.push("pending decision gate".to_string());
        TerminationStatus::NeedsHumanDecision
    } else if !scope.active_leases.is_empty() {
        reasons.push("active worker lease".to_string());
        TerminationStatus::Continue
    } else if !scope.runnable_unresolved.is_empty() {
        reasons.push("runnable unresolved intents remain".to_string());
        TerminationStatus::Continue
    } else if !has_mission {
        // 经典项目 Run 只报告执行生命周期。非阻塞的覆盖/证据建议仍
        // 在各自队列中可见，但不得把成功完成的扫描改写成 paused。
        if scope.run_budget_exhausted {
            reasons.push("run budget exhausted".to_string());
            TerminationStatus::Pause
        } else {
            reasons.push("all required audit execution completed".to_string());
            TerminationStatus::Complete
        }
    } else if !scope.unresolved_branch_ids.is_empty() {
        reasons.push("runnable Mission branches remain".to_string());
        TerminationStatus::Continue
        } else if goal.needs_contract_review {
            // 目标契约本身无法机器校验（needs_review / custom /
            // auto_complete=false），且已无可运行工作（租约/Intent/分支
            // 均为空——上面的阶梯已经接走还有工作的情况）。
            //
            // 产品决策（2026-09-23，用户实测"你好"后拍板）：**不停机等待**。
            // 参考实现对模糊输入不停机——任务把能做的做完就收尾，答案
            // （无论好坏）直接交付，运营者事后再看证据。停在
            // `NeedsHumanDecision` 是个无人区：前端没有任何写入面能
            // 补齐目标契约（`/missions/{id}` 只有 GET/DELETE），任务树
            // 上永远是个玫红脉冲的"需要决策"，用户既无法行动也没有下一
            // 步——正是用户投诉"搞不明白怎么做"的那个状态。
            //
            // 安全属性不变：完成的是 **run 生命周期**，不是对目标达成的
            // 虚假宣称——`goal_satisfied` 仍为 `false`，理由里如实写明
            // "目标不可机器判定，需人工复核证据"。Finding 的确认走独立的
            // 三重门，不受Mission 终态影响。
            reasons.push(
                "Mission goal contract is not machine-classifiable (needs_review / custom); \
                 the run finished all possible work and completed without an automatic \
                 goal-satisfaction verdict — review the evidence manually"
                    .to_string(),
            );
            reasons.extend(goal.unmet_requirements.iter().cloned());
            TerminationStatus::Complete
        } else if goal.satisfied == Some(false) {
        // 走到这里已经没有任何可运行的工作（租约/Intent/分支均为空）。
        // 待审高危发现与证据缺口是持久的审查信号——Observation 与
        // Finding 都是 append-only 记录，永远不会自行"解决"；把它们判成
        // Continue 会让 Run 停在 Running 却再无任何调度（Rust 端
        // run_mission_runtime 单遍执行，没有 Python 侧 Ralph 外层循环
        // 重新拉起），Mission 就此永久假死。它们只能作为暂停理由交由
        // 操作者处置（补充指令、复审 Finding 或重新评估），不能充当
        // "工作仍在进行"的依据。
        if !scope.high_open.is_empty() {
            reasons.push("high severity finding still needs review".to_string());
        }
        if scope.evidence_gap_count > 0 {
            reasons.push("evidence gaps remain".to_string());
        }
        reasons.extend(goal.unmet_requirements.iter().cloned());
        if scope.run_budget_exhausted {
            reasons.push("run budget exhausted before the Mission goal was satisfied".to_string());
        }
        TerminationStatus::Pause
    } else if scope.run_budget_exhausted {
        reasons.push("run budget exhausted".to_string());
        TerminationStatus::Pause
    } else {
        reasons.push("no pending work remains and no Mission goal contract applies".to_string());
        TerminationStatus::Complete
    };
    (status, reasons)
}

/// Ralph-Loop 式终止判定器：外置评估，不改写 Run。
#[derive(Debug, Default)]
pub struct TerminationEvaluator;

impl TerminationEvaluator {
    /// 评估一次终止判定（纯计算，无 I/O）。
    ///
    /// 状态阶梯（顺序即优先级）：目标已满足 → `COMPLETE`；待答决策门 →
    /// `NEEDS_HUMAN_DECISION`；活跃租约 / 可运行未决 Intent → `CONTINUE`；
    /// 无 Mission → 预算耗尽 `PAUSE` 否则 `COMPLETE`；未决可运行分支 →
    /// `CONTINUE`；**目标契约无法机器校验 → `NEEDS_HUMAN_DECISION`**（等
    /// 操作者分类，绝不自动完成）；目标明确未满足 → `PAUSE`（待审高危发现 /
    /// 证据缺口是持久的审查信号，只记入理由，不再单独制造 `CONTINUE`——
    /// 无可运行时 `CONTINUE` 会让 Run 永远停在 `Running` 而无人再调度）；
    /// 预算耗尽 → `PAUSE`；否则 `COMPLETE`。
    ///
    /// # 红线语义
    ///
    /// 已验证的产品成果是权威的：目标契约满足时 COMPLETE 无条件胜出——
    /// 规划队列、覆盖建议与预算只能解释一次未满足的运行为何停止，
    /// 不能否决一个已通过目标门的成果。
    #[must_use]
    pub fn evaluate(&self, input: &TerminationInput<'_>) -> TerminationAssessment {
        let run = input.run;
        let scope = collect_scope(input);
        let goal = evaluate_goal(
            input.mission,
            run,
            &scope.run_tasks,
            input.findings,
            &scope.run_evidence,
            &scope.exhausted_open_branch_ids,
        );

        let (status, reasons) = resolve_status(&scope, &goal, input.mission.is_some());

        let resolved_branch_count = scope.run_branches.len() - scope.unresolved_branch_ids.len();
        let mut coverage_summary = format!(
            "{}/{} Mission intents resolved; {} task(s); {}/{} branches resolved; {} evidence \
             gap(s); {} active lease(s).",
            scope.scoped_intents.len() - scope.unresolved_intent_ids.len(),
            scope.scoped_intents.len(),
            scope.run_tasks.len(),
            resolved_branch_count,
            scope.run_branches.len(),
            scope.evidence_gap_count,
            scope.active_leases.len(),
        );
        if let Some(satisfied) = goal.satisfied {
            // 目标段直接写入已分配的 String，避免 format! 临时串再拷贝；
            // String 的 fmt 写入在容量充足时不会失败。
            use std::fmt::Write as _;
            let _ = write!(
                coverage_summary,
                " Mission goal satisfied: {}; {}/{} goal requirement(s) met.",
                if satisfied { "yes" } else { "no" },
                goal.satisfied_requirements.len(),
                goal.requirements.len(),
            );
        }
        let confidence = confidence(
            status,
            scope.unresolved_intent_ids.len(),
            scope.unresolved_branch_ids.len(),
            scope.evidence_gap_count,
            scope.active_leases.len(),
            scope.pending_gates.len(),
            goal.unmet_requirements.len(),
        );

        let mut high_value_open_questions: Vec<String> = scope
            .high_open
            .iter()
            .map(|finding| format!("{}: {}", finding.severity.as_str(), finding.title))
            .collect();
        high_value_open_questions.extend(
            goal.unmet_requirements
                .iter()
                .map(|item| format!("Goal requirement: {item}")),
        );

        tracing::info!(
            run = %run.id,
            mission = ?input.mission.map(|mission| &mission.id),
            status = ?status,
            reason = reasons.first().map(String::as_str).unwrap_or_default(),
            "termination evaluation"
        );

        let mut assessment = TerminationAssessment::new(input.project_id.clone(), run.id.clone());
        assessment.status = status;
        assessment.reasons = reasons;
        assessment.coverage_summary = coverage_summary;
        assessment.unresolved_intent_ids = scope.unresolved_intent_ids;
        assessment.unresolved_branch_ids = scope.unresolved_branch_ids;
        assessment.evidence_gap_count = scope.evidence_gap_count;
        assessment.high_value_open_questions = high_value_open_questions;
        assessment.goal_satisfied = goal.satisfied;
        assessment.goal_requirements = goal.requirements;
        assessment.satisfied_goal_requirements = goal.satisfied_requirements;
        assessment.unmet_goal_requirements = goal.unmet_requirements;
        assessment.confidence = confidence;
        assessment
    }
}

/// Python `TerminationEvaluator._evaluate_goal`。
fn evaluate_goal(
    mission: Option<&Mission>,
    run: &AuditRun,
    tasks: &[&AgentTask],
    findings: &[Finding],
    evidence: &[&Evidence],
    exhausted_open_branch_ids: &[String],
) -> GoalEvaluation {
    let Some(mission) = mission else {
        return GoalEvaluation::default();
    };

    let mut requirements: Vec<String> = vec!["required execution tasks completed".to_string()];
    let mut satisfied: Vec<String> = Vec::new();
    let mut unmet: Vec<String> = Vec::new();

    apply_task_requirement(run, tasks, &requirements[0], &mut satisfied, &mut unmet);

    let contract = &mission.goal_contract;
    let contract_requirement = "machine-checkable Mission goal contract";
    requirements.push(contract_requirement.to_string());
    if contract.status != GoalContractStatus::Resolved
        || !contract.auto_complete
        || contract.outcome_type == GoalOutcomeType::Custom
    {
        unmet.push(
            "Mission success is not structurally classified; automatic completion is disabled \
             until the goal contract is reviewed"
                .to_string(),
        );
        return GoalEvaluation {
            satisfied: Some(false),
            requirements,
            satisfied_requirements: satisfied,
            unmet_requirements: unmet,
            needs_contract_review: true,
        };
    }
    satisfied.push(contract_requirement.to_string());

    let outcome_requirement = format!(
        "verified outcome ({} required): {}",
        contract.minimum_count, contract.description
    );
    requirements.push(outcome_requirement.clone());
    let outcome_count = contract_outcome_count(mission, run, findings, evidence);
    if outcome_count >= contract.minimum_count {
        satisfied.push(outcome_requirement);
    } else {
        unmet.push(format!(
            "Mission goal contract requires {} {} result(s), but only {} passed the \
             verification gate",
            contract.minimum_count,
            contract.outcome_type.as_str(),
            outcome_count
        ));
    }

    // 结果导向的协作止步于其已验证的交付物。覆盖耗尽只在覆盖本身是
    // 请求的产品时才相关；它绝不能替代缺失的 Flag / finding / evidence
    // 成果。
    if contract.outcome_type == GoalOutcomeType::Coverage {
        apply_coverage_requirements(
            run,
            tasks,
            exhausted_open_branch_ids,
            &mut requirements,
            &mut satisfied,
            &mut unmet,
        );
    }

    GoalEvaluation {
        satisfied: Some(unmet.is_empty()),
        requirements,
        satisfied_requirements: satisfied,
        unmet_requirements: unmet,
        needs_contract_review: false,
    }
}

/// 需求一"必需执行任务全部落盘且成功"的判定（Python `_evaluate_goal`
/// 的任务段）。
///
/// 四分支：记录缺失 → 未满足；全部成功 → 满足；无必需任务 → 未满足
/// （没有任何执行记录证明 Mission 被执行过）；其余 → 部分未完成。
fn apply_task_requirement(
    run: &AuditRun,
    tasks: &[&AgentTask],
    requirement: &str,
    satisfied: &mut Vec<String>,
    unmet: &mut Vec<String>,
) {
    let required_ids: HashSet<&str> = run.task_ids.iter().map(String::as_str).collect();
    let required_tasks: Vec<&&AgentTask> = tasks
        .iter()
        .filter(|task| required_ids.is_empty() || required_ids.contains(task.id.as_str()))
        .collect();
    let persisted_ids: HashSet<&str> = required_tasks.iter().map(|task| task.id.as_str()).collect();
    let mut missing_task_ids: Vec<&str> =
        required_ids.difference(&persisted_ids).copied().collect();
    missing_task_ids.sort_unstable();
    if !missing_task_ids.is_empty() {
        unmet.push(format!(
            "required execution task records are missing ({})",
            missing_task_ids.join(", ")
        ));
    } else if !required_tasks.is_empty()
        && required_tasks
            .iter()
            .all(|task| task.status == TaskStatus::Succeeded)
    {
        satisfied.push(requirement.to_string());
    } else if required_tasks.is_empty() {
        unmet.push(
            "no persisted execution task proves that the Mission was carried out".to_string(),
        );
    } else {
        let states = required_tasks
            .iter()
            .filter(|task| task.status != TaskStatus::Succeeded)
            .map(|task| format!("{}:{}", task.solver, task.status.as_str()))
            .collect::<Vec<_>>()
            .join(", ");
        unmet.push(format!(
            "required execution tasks are incomplete ({states})"
        ));
    }
}

/// Coverage 类目标的附加需求（Python `_evaluate_goal` 的覆盖段）：
/// 请求域逐一核验 + 能力缺口 + 覆盖完成前耗尽的分支。
fn apply_coverage_requirements(
    run: &AuditRun,
    tasks: &[&AgentTask],
    exhausted_open_branch_ids: &[String],
    requirements: &mut Vec<String>,
    satisfied: &mut Vec<String>,
    unmet: &mut Vec<String>,
) {
    let raw_domains = run
        .config
        .get("requested_audit_domains")
        .or_else(|| run.config.get("audit_domains"));
    for domain in string_list(raw_domains) {
        let requirement = format!("audit domain covered: {domain}");
        requirements.push(requirement.clone());
        if domain_covered(&domain, tasks) {
            satisfied.push(requirement);
        } else {
            unmet.push(format!(
                "requested audit domain was not completed: {domain}"
            ));
        }
    }

    if let Some(capability_gaps) = run.config.get("capability_gaps")
        && let Some(gaps) = capability_gaps.as_array()
        && !gaps.is_empty()
    {
        requirements.push("no unresolved capability gaps".to_string());
        unmet.push("one or more requested capabilities were unavailable".to_string());
    }

    if !exhausted_open_branch_ids.is_empty() {
        requirements.push("no branch exhausted before coverage completed".to_string());
        unmet.push(format!(
            "branch budget exhausted before coverage completed: {}",
            exhausted_open_branch_ids.join(", ")
        ));
    }
}

/// Python `TerminationEvaluator._contract_outcome_count`。
fn contract_outcome_count(
    mission: &Mission,
    run: &AuditRun,
    findings: &[Finding],
    evidence: &[&Evidence],
) -> i64 {
    let contract = &mission.goal_contract;
    if contract.outcome_type == GoalOutcomeType::Coverage {
        return contract.minimum_count;
    }

    let scoped_evidence: Vec<&&Evidence> = evidence
        .iter()
        .filter(|item| {
            (item.run_id.is_none() || item.run_id.as_ref() == Some(&run.id))
                && (item.mission_id.is_none() || item.mission_id.as_ref() == Some(&mission.id))
        })
        .collect();
    if contract.outcome_type == GoalOutcomeType::VerifiedEvidence {
        let allowed_kinds = &contract.evidence_kinds;
        let count = scoped_evidence
            .iter()
            .filter(|item| {
                (allowed_kinds.is_empty() || allowed_kinds.contains(&item.kind))
                    && (!contract.require_provenance || evidence_has_provenance(item))
            })
            .count();
        return i64::try_from(count).unwrap_or(i64::MAX);
    }

    let evidence_by_id: HashMap<&str, &Evidence> = scoped_evidence
        .iter()
        .map(|item| (item.id.as_str(), **item))
        .collect();
    let allowed_rules: HashSet<&str> = contract
        .finding_rule_ids
        .iter()
        .map(String::as_str)
        .collect();
    let count = findings
        .iter()
        .filter(|finding| {
            (finding.run_id.is_none() || finding.run_id.as_ref() == Some(&run.id))
                && (finding.mission_id.is_none()
                    || finding.mission_id.as_ref() == Some(&mission.id))
                && (allowed_rules.is_empty()
                    || allowed_rules.contains(finding.rule_id.as_deref().unwrap_or_default()))
                && (!contract.require_confirmed_findings
                    || finding.status == FindingStatus::Confirmed)
                && !matches!(
                    finding.status,
                    FindingStatus::FalsePositive | FindingStatus::Duplicate
                )
                && (contract.outcome_type != GoalOutcomeType::FlagCapture
                    || has_product_verification_receipt(finding, "flag_capture", &evidence_by_id))
        })
        .filter(|finding| {
            !contract.require_provenance || finding_has_provenance(finding, &evidence_by_id)
        })
        .count();
    i64::try_from(count).unwrap_or(i64::MAX)
}

/// Python `TerminationEvaluator._has_product_verification_receipt`。
///
/// 回执必须逐字段通过 `product-verification.v1` schema 校验，且候选
/// flag 字节的 SHA-256 与回执中的 `candidate_sha256` 一致——指纹即
/// 磁盘字节，绝不从变换后的内容计算。
fn has_product_verification_receipt(
    finding: &Finding,
    kind: &str,
    evidence_by_id: &HashMap<&str, &Evidence>,
) -> bool {
    let Some(receipt) = finding
        .review
        .get("product_verification")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    let evidence_id = receipt
        .get("evidence_id")
        .and_then(serde_json::Value::as_str);
    let tool_invocation_id = receipt
        .get("tool_invocation_id")
        .and_then(serde_json::Value::as_str);
    let artifact_sha256 = receipt
        .get("artifact_sha256")
        .and_then(serde_json::Value::as_str);
    let candidate_sha256 = receipt
        .get("candidate_sha256")
        .and_then(serde_json::Value::as_str);
    let (
        Some(evidence_id),
        Some(tool_invocation_id),
        Some(artifact_sha256),
        Some(candidate_sha256),
    ) = (
        evidence_id,
        tool_invocation_id,
        artifact_sha256,
        candidate_sha256,
    )
    else {
        return false;
    };
    let schema_ok = receipt
        .get("schema_version")
        .and_then(serde_json::Value::as_str)
        == Some("product-verification.v1");
    let verified_ok = receipt.get("verified") == Some(&serde_json::Value::Bool(true));
    let kind_ok = receipt.get("kind").and_then(serde_json::Value::as_str) == Some(kind);
    let linked_ok = finding
        .evidence_ids
        .iter()
        .any(|linked| linked == evidence_id);
    if !(schema_ok && verified_ok && kind_ok && linked_ok) {
        return false;
    }
    let Some(evidence) = evidence_by_id.get(evidence_id) else {
        return false;
    };
    let Some(candidate) = evidence
        .content
        .get("flag")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    let tool_ok = evidence
        .produced_by_tool_invocation_id
        .as_ref()
        .is_some_and(|id| id.as_str() == tool_invocation_id);
    // Python `evidence.fingerprint and evidence.fingerprint.removeprefix(
    // "sha256:").lower() == artifact_sha256.lower()`。
    let fingerprint_ok = evidence.fingerprint.as_deref().is_some_and(|fingerprint| {
        !fingerprint.is_empty()
            && fingerprint
                .strip_prefix("sha256:")
                .unwrap_or(fingerprint)
                .to_lowercase()
                == artifact_sha256.to_lowercase()
    });
    tool_ok && fingerprint_ok && sha256_hex(candidate) == candidate_sha256.to_lowercase()
}

/// Python `TerminationEvaluator._finding_has_provenance`。
fn finding_has_provenance(finding: &Finding, evidence_by_id: &HashMap<&str, &Evidence>) -> bool {
    if finding.evidence_ids.is_empty() {
        return false;
    }
    finding.evidence_ids.iter().all(|evidence_id| {
        evidence_by_id
            .get(evidence_id.as_str())
            .is_some_and(|item| evidence_has_provenance(item))
    })
}

/// Python `TerminationEvaluator._evidence_has_provenance`。
fn evidence_has_provenance(evidence: &Evidence) -> bool {
    evidence
        .evidence_path
        .as_deref()
        .is_some_and(|path| !path.is_empty())
        && evidence
            .fingerprint
            .as_deref()
            .is_some_and(|fingerprint| !fingerprint.is_empty())
        && evidence
            .produced_by_tool_invocation_id
            .as_ref()
            .is_some_and(|id| !id.as_str().is_empty())
}

/// Python `TerminationEvaluator._domain_covered`。
fn domain_covered(domain: &str, tasks: &[&AgentTask]) -> bool {
    tasks.iter().any(|task| {
        task.status == TaskStatus::Succeeded && task_audit_domains(task).contains(domain)
    })
}

/// Python `TerminationEvaluator._task_audit_domains`。
fn task_audit_domains(task: &AgentTask) -> HashSet<String> {
    let mut declared: HashSet<String> = HashSet::new();
    if let Some(raw) = task
        .payload
        .get("audit_domains")
        .and_then(|value| value.as_array())
    {
        declared.extend(
            raw.iter()
                .filter_map(|item| item.as_str())
                .filter(|item| !item.is_empty())
                .map(str::to_string),
        );
    }
    if let Some(dispatch) = task
        .payload
        .get("capability_dispatch")
        .and_then(|value| value.as_object())
        && let Some(audit_domain) = dispatch
            .get("audit_domain")
            .and_then(|value| value.as_str())
        && !audit_domain.is_empty()
    {
        declared.insert(audit_domain.to_string());
    }
    // 遗留经典任务把 solver 名作为唯一结构化域标识；新任务在上方持久化
    // 显式 audit_domains。
    declared.insert(task.solver.clone());
    declared
}

/// Python `TerminationEvaluator._string_list`。
fn string_list(raw: Option<&serde_json::Value>) -> Vec<String> {
    match raw {
        Some(serde_json::Value::String(text)) => text
            .replace(',', " ")
            .split_whitespace()
            .map(str::to_string)
            .collect(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_str())
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// Python `TerminationEvaluator._evidence_gap_count`。
fn evidence_gap_count(findings: &[Finding], observations: &[Observation]) -> i64 {
    let finding_gaps = findings
        .iter()
        .filter(|finding| {
            matches!(
                finding.status,
                FindingStatus::Candidate | FindingStatus::NeedsReview
            ) && finding.evidence_ids.is_empty()
        })
        .count();
    let observation_gaps = observations
        .iter()
        .filter(|obs| obs.observation_type == ObservationType::EvidenceGap)
        .count();
    i64::try_from(finding_gaps + observation_gaps).unwrap_or(i64::MAX)
}

/// Python `TerminationEvaluator._confidence`。
fn confidence(
    status: TerminationStatus,
    unresolved_count: usize,
    unresolved_branch_count: usize,
    evidence_gap_count: i64,
    active_lease_count: usize,
    pending_gate_count: usize,
    unmet_goal_count: usize,
) -> f64 {
    if status == TerminationStatus::Complete {
        return 0.9;
    }
    let penalty = (0.1 * usize_as_f64(unresolved_count)
        + 0.1 * usize_as_f64(unresolved_branch_count)
        + 0.15 * i64_as_f64(evidence_gap_count)
        + 0.2 * usize_as_f64(active_lease_count)
        + 0.25 * usize_as_f64(pending_gate_count)
        + 0.15 * usize_as_f64(unmet_goal_count))
    .min(0.7);
    (0.8 - penalty).max(0.2)
}

/// `usize` → f64（罚分计算专用）：真实计数远小于 2^53，饱和转换即
/// 无损；罚分在 0.7 处截断，极端饱和不改变结果。
fn usize_as_f64(count: usize) -> f64 {
    f64::from(u32::try_from(count).unwrap_or(u32::MAX))
}

/// `i64` → f64（罚分计算专用）：计数非负且远小于 2^53，同上饱和理由。
fn i64_as_f64(count: i64) -> f64 {
    f64::from(i32::try_from(count.max(0)).unwrap_or(i32::MAX))
}

/// `sha256(text).hexdigest()`（小写十六进制）。
fn sha256_hex(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        // 字节转十六进制不会失败（容量已预留）。
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::agent::WorkerLease;
    use models::common::Timestamp;
    use models::decision::DecisionGateKind;
    use models::decision::DecisionOption;
    use models::decision::DecisionSeverity;
    use models::ids::DecisionGateId;
    use models::ids::EvidenceId;
    use models::ids::FindingId;
    use models::ids::IntentId;
    use models::ids::MissionId;
    use models::ids::ProjectId;
    use models::ids::RunId;
    use models::ids::TaskId;
    use models::ids::ToolInvocationId;
    use models::lifecycle::EvidenceKind;
    use models::mission::GoalContractSource;
    use models::mission::MissionGoalContract;
    use serde_json::json;

    fn project_id() -> ProjectId {
        ProjectId::new("p".to_string())
    }

    fn run_id() -> RunId {
        RunId::new("r".to_string())
    }

    fn mission_id() -> MissionId {
        MissionId::new("m".to_string())
    }

    fn timestamp() -> Timestamp {
        "2026-08-24T12:00:00Z"
            .parse()
            .unwrap_or_else(|error| panic!("固定时间必须可解析: {error}"))
    }

    fn run(max_total_steps: i64, steps_used: i64) -> AuditRun {
        let mut run = AuditRun::new(project_id());
        run.id = run_id();
        run.max_total_steps = max_total_steps;
        run.steps_used = steps_used;
        run
    }

    fn flag_capture_mission() -> Mission {
        let mut mission = Mission::new(project_id(), "solve it".to_string());
        mission.id = mission_id();
        mission.goal_contract = MissionGoalContract {
            outcome_type: GoalOutcomeType::FlagCapture,
            status: GoalContractStatus::Resolved,
            description: "Recover one verified challenge Flag".to_string(),
            source: GoalContractSource::IntakeModel,
            ..MissionGoalContract::default()
        }
        .validated()
        .unwrap_or_else(|error| panic!("合法契约必须通过校验: {error}"));
        mission
    }

    fn succeeded_task(mission_id: &MissionId, solver: &str) -> AgentTask {
        let mut task = AgentTask::new(project_id(), run_id(), solver.to_string());
        task.id = TaskId::new("task_1".to_string());
        task.mission_id = Some(mission_id.clone());
        task.status = TaskStatus::Succeeded;
        task
    }

    /// Python `test_termination_evaluator_statuses`。
    #[test]
    fn evaluator_statuses() {
        let evaluator = TerminationEvaluator;
        let active_run = run(10, 1);

        let complete = evaluator.evaluate(&TerminationInput::new(&project_id(), &active_run));
        assert_eq!(complete.status, TerminationStatus::Complete);

        let lease = WorkerLease::new(
            project_id(),
            run_id(),
            IntentId::new("i".to_string()),
            "w".to_string(),
            timestamp(),
        );
        let continued = evaluator.evaluate(&TerminationInput {
            worker_leases: &[lease],
            ..TerminationInput::new(&project_id(), &active_run)
        });
        assert_eq!(continued.status, TerminationStatus::Continue);

        let option = DecisionOption {
            id: "yes".to_string(),
            label: "yes".to_string(),
            description: "go".to_string(),
            impact: "go".to_string(),
            risk: "low".to_string(),
            is_recommended: true,
        };
        let mut gate = DecisionGate {
            id: DecisionGateId::new("decision_1".to_string()),
            project_id: project_id(),
            audit_run_id: run_id(),
            created_by: "manager".to_string(),
            kind: DecisionGateKind::Blocking,
            severity: DecisionSeverity::Medium,
            question: "continue?".to_string(),
            context_summary: String::new(),
            recommended_option_id: "yes".to_string(),
            options: vec![option],
            status: models::decision::DecisionGateStatus::Pending,
            answer: None,
            created_at: timestamp(),
            answered_at: None,
            expires_at: None,
            related_fact_ids: Vec::new(),
            related_evidence_ids: Vec::new(),
            related_finding_ids: Vec::new(),
            metadata: serde_json::Map::new(),
        };
        gate = gate
            .validated()
            .unwrap_or_else(|error| panic!("合法决策门必须通过校验: {error}"));
        let needs_human = evaluator.evaluate(&TerminationInput {
            decision_gates: &[gate],
            ..TerminationInput::new(&project_id(), &active_run)
        });
        assert_eq!(needs_human.status, TerminationStatus::NeedsHumanDecision);

        let exhausted_run = run(1, 1);
        let paused = evaluator.evaluate(&TerminationInput::new(&project_id(), &exhausted_run));
        assert_eq!(paused.status, TerminationStatus::Pause);

        let mut mission_run = run(10, 1);
        mission_run.mission_id = Some(mission_id());
        let mut finding = Finding::new(project_id(), "critical needs review".to_string());
        finding.id = FindingId::new("find_1".to_string());
        finding.run_id = Some(run_id());
        finding.severity = Severity::Critical;
        finding.status = FindingStatus::NeedsReview;
        finding.evidence_ids = vec!["evd_1".to_string()];
        // 必须用**已解析**的契约：默认契约是 needs_review，会先被
        // "契约待分类" 分支接走（→ Complete），测不到高危待审这条暂停理由。
        let mission = flag_capture_mission();
        let high_open = evaluator.evaluate(&TerminationInput {
            findings: &[finding],
            mission: Some(&mission),
            ..TerminationInput::new(&project_id(), &mission_run)
        });
        // 与 Python 阶梯的有意偏差：无可运行工作时高危待审只记入暂停
        // 理由——Rust 端没有外层 Ralph Loop 会在 Continue 后重新调度。
        assert_eq!(high_open.status, TerminationStatus::Pause);
        assert!(
            high_open
                .reasons
                .iter()
                .any(|reason| reason == "high severity finding still needs review")
        );
    }

    /// needs_review 契约不得把 Mission 永久停在无人区：无可运行工作时
    /// **正常完成**（参考实现对模糊输入不停机，答案直接交付），而不是
    /// `NeedsHumanDecision` 那个前端无法行动、也没有下一步的"需要决策"。
    /// 安全属性不变——`goal_satisfied` 仍为 false，理由如实说明需人工复核。
    #[test]
    fn unclassified_goal_contract_completes_without_parking() {
        let evaluator = TerminationEvaluator;
        let mut mission_run = run(20, 1);
        mission_run.mission_id = Some(mission_id());
        // 默认契约 = NeedsReview + Custom + auto_complete:false，正是
        // "你好"这类输入走到的状态。
        let mission = Mission::new(project_id(), "你好".to_string());
        let assessment = evaluator.evaluate(&TerminationInput {
            mission: Some(&mission),
            ..TerminationInput::new(&project_id(), &mission_run)
        });
        assert_eq!(assessment.status, TerminationStatus::Complete);
        assert!(
            assessment
                .reasons
                .iter()
                .any(|reason| reason.contains("not machine-classifiable")),
            "理由必须说清缺什么，实际: {:?}",
            assessment.reasons
        );
        // 完成的是 run 生命周期，不是目标达成的虚假宣称。
        assert_eq!(assessment.goal_satisfied, Some(false));
    }

    /// 已解析的契约不受影响：该满足满足、该暂停暂停，不被新分支抢走。
    #[test]
    fn resolved_goal_contract_keeps_existing_ladder() {
        let evaluator = TerminationEvaluator;
        let mut mission_run = run(20, 1);
        mission_run.mission_id = Some(mission_id());
        let mission = flag_capture_mission();
        let assessment = evaluator.evaluate(&TerminationInput {
            mission: Some(&mission),
            ..TerminationInput::new(&project_id(), &mission_run)
        });
        assert_ne!(assessment.status, TerminationStatus::NeedsHumanDecision);
    }

    /// Python `test_termination_requires_positive_result_for_explicit_goal`。
    #[test]
    fn requires_positive_result_for_explicit_goal() {
        let evaluator = TerminationEvaluator;
        let mission = flag_capture_mission();
        let mut mission_run = run(20, 2);
        mission_run.mission_id = Some(mission.id.clone());
        mission_run.task_ids = vec!["task_1".to_string()];
        let task = succeeded_task(&mission.id, "web_exploit");

        let assessment = evaluator.evaluate(&TerminationInput {
            mission: Some(&mission),
            tasks: &[task],
            ..TerminationInput::new(&project_id(), &mission_run)
        });

        assert_eq!(assessment.status, TerminationStatus::Pause);
        assert_eq!(assessment.goal_satisfied, Some(false));
        assert!(
            assessment
                .unmet_goal_requirements
                .iter()
                .any(|item| item.contains("flag_capture"))
        );
    }

    /// Python `test_mission_termination_ignores_unadmitted_project_planner_suggestions`。
    #[test]
    fn ignores_unadmitted_project_planner_suggestions() {
        let evaluator = TerminationEvaluator;
        let mission = flag_capture_mission();
        let mut mission_run = run(20, 0);
        mission_run.mission_id = Some(mission.id.clone());
        mission_run.task_ids = vec!["task_1".to_string()];
        let task = succeeded_task(&mission.id, "web_exploit");
        let mut suggestion =
            Intent::new(project_id(), "Optional project-wide follow-up".to_string());
        suggestion.id = IntentId::new("project_suggestion".to_string());
        suggestion.run_id = Some(run_id());
        suggestion.mission_id = None;
        suggestion.solver = Some("web_recon".to_string());
        suggestion.created_by = "agent_planner".to_string();

        let assessment = evaluator.evaluate(&TerminationInput {
            intents: &[suggestion],
            mission: Some(&mission),
            tasks: &[task],
            ..TerminationInput::new(&project_id(), &mission_run)
        });

        assert_eq!(assessment.status, TerminationStatus::Pause);
        assert!(assessment.unresolved_intent_ids.is_empty());
        assert_eq!(assessment.goal_satisfied, Some(false));
    }

    /// Python `test_termination_completes_explicit_goal_only_with_provenance`。
    #[test]
    fn completes_explicit_goal_only_with_provenance() {
        let evaluator = TerminationEvaluator;
        let mission = flag_capture_mission();
        let mut mission_run = run(20, 2);
        mission_run.mission_id = Some(mission.id.clone());
        mission_run.task_ids = vec!["task_1".to_string()];
        let task = succeeded_task(&mission.id, "web_exploit");

        let mut proof = Evidence::new(
            project_id(),
            EvidenceKind::PocDescription,
            "Captured CTF2{proof}".to_string(),
        );
        proof.id = EvidenceId::new("evd_flag".to_string());
        proof.mission_id = Some(mission.id.clone());
        proof.run_id = Some(run_id());
        proof
            .content
            .insert("flag".to_string(), json!("CTF2{proof}"));
        proof.produced_by_tool_invocation_id = Some(ToolInvocationId::new("tool_1".to_string()));
        proof.evidence_path = Some("artifacts/flag.txt".to_string());
        proof.fingerprint = Some("sha256:proof".to_string());

        let mut finding = Finding::new(project_id(), "Flag captured: CTF2{proof}".to_string());
        finding.id = FindingId::new("find_flag".to_string());
        finding.mission_id = Some(mission.id.clone());
        finding.run_id = Some(run_id());
        finding.rule_id = Some("web_exploit.flag_capture".to_string());
        finding.status = FindingStatus::Confirmed;
        finding.evidence_ids = vec![proof.id.as_str().to_string()];
        finding.review.insert(
            "product_verification".to_string(),
            json!({
                "schema_version": "product-verification.v1",
                "verified": true,
                "kind": "flag_capture",
                "evidence_id": proof.id.as_str(),
                "tool_invocation_id": "tool_1",
                "artifact_sha256": "proof",
                "candidate_sha256": sha256_hex("CTF2{proof}"),
            }),
        );

        let mut scoped_intent = Intent::new(
            project_id(),
            "No longer needed after verified product result".to_string(),
        );
        scoped_intent.mission_id = Some(mission.id.clone());
        scoped_intent.run_id = Some(run_id());
        scoped_intent.solver = Some("web_recon".to_string());

        let assessment = evaluator.evaluate(&TerminationInput {
            intents: &[scoped_intent],
            findings: &[finding],
            mission: Some(&mission),
            tasks: &[task],
            evidence: &[proof],
            ..TerminationInput::new(&project_id(), &mission_run)
        });

        assert_eq!(assessment.status, TerminationStatus::Complete);
        assert_eq!(assessment.goal_satisfied, Some(true));
    }

    /// Python `test_termination_distinguishes_audit_completion_from_find_goal`。
    #[test]
    fn distinguishes_audit_completion_from_find_goal() {
        let evaluator = TerminationEvaluator;
        let mut mission_run = run(20, 2);
        mission_run.mission_id = Some(mission_id());
        mission_run.task_ids = vec!["task_1".to_string()];
        let task = succeeded_task(&mission_id(), "web_sast");

        let mut audit_only = Mission::new(project_id(), "Audit the source tree".to_string());
        audit_only.id = mission_id();
        audit_only.goal_contract = MissionGoalContract {
            outcome_type: GoalOutcomeType::Coverage,
            status: GoalContractStatus::Resolved,
            description: "Complete the requested audit coverage".to_string(),
            source: GoalContractSource::IntakeModel,
            ..MissionGoalContract::default()
        }
        .validated()
        .unwrap_or_else(|error| panic!("合法契约必须通过校验: {error}"));

        let mut find_vulnerability = Mission::new(
            project_id(),
            "Find a vulnerability in the source tree".to_string(),
        );
        find_vulnerability.id = mission_id();
        find_vulnerability.goal_contract = MissionGoalContract {
            outcome_type: GoalOutcomeType::ConfirmedFinding,
            status: GoalContractStatus::Resolved,
            description: "Produce one confirmed vulnerability finding".to_string(),
            source: GoalContractSource::IntakeModel,
            ..MissionGoalContract::default()
        }
        .validated()
        .unwrap_or_else(|error| panic!("合法契约必须通过校验: {error}"));

        let completed_audit = evaluator.evaluate(&TerminationInput {
            mission: Some(&audit_only),
            tasks: std::slice::from_ref(&task),
            ..TerminationInput::new(&project_id(), &mission_run)
        });
        let unmet_discovery = evaluator.evaluate(&TerminationInput {
            mission: Some(&find_vulnerability),
            tasks: &[task],
            ..TerminationInput::new(&project_id(), &mission_run)
        });

        assert_eq!(completed_audit.status, TerminationStatus::Complete);
        assert_eq!(unmet_discovery.status, TerminationStatus::Pause);
        assert_eq!(unmet_discovery.goal_satisfied, Some(false));
    }

    /// 回执校验的反例：候选 flag 与回执哈希不一致时不放行。
    #[test]
    fn receipt_rejects_mismatched_candidate_hash() {
        let mission = flag_capture_mission();
        let mut mission_run = run(20, 2);
        mission_run.mission_id = Some(mission.id.clone());
        mission_run.task_ids = vec!["task_1".to_string()];

        let mut proof = Evidence::new(
            project_id(),
            EvidenceKind::PocDescription,
            "Captured CTF2{proof}".to_string(),
        );
        proof.id = EvidenceId::new("evd_flag".to_string());
        proof.mission_id = Some(mission.id.clone());
        proof.run_id = Some(run_id());
        proof
            .content
            .insert("flag".to_string(), json!("CTF2{tampered}"));
        proof.produced_by_tool_invocation_id = Some(ToolInvocationId::new("tool_1".to_string()));
        proof.evidence_path = Some("artifacts/flag.txt".to_string());
        proof.fingerprint = Some("sha256:proof".to_string());

        let mut finding = Finding::new(project_id(), "Flag captured: CTF2{proof}".to_string());
        finding.mission_id = Some(mission.id.clone());
        finding.run_id = Some(run_id());
        finding.rule_id = Some("web_exploit.flag_capture".to_string());
        finding.status = FindingStatus::Confirmed;
        finding.evidence_ids = vec![proof.id.as_str().to_string()];
        finding.review.insert(
            "product_verification".to_string(),
            json!({
                "schema_version": "product-verification.v1",
                "verified": true,
                "kind": "flag_capture",
                "evidence_id": proof.id.as_str(),
                "tool_invocation_id": "tool_1",
                "artifact_sha256": "proof",
                "candidate_sha256": sha256_hex("CTF2{proof}"),
            }),
        );

        let evidence_by_id: HashMap<&str, &Evidence> =
            [(proof.id.as_str(), &proof)].into_iter().collect();
        assert!(!has_product_verification_receipt(
            &finding,
            "flag_capture",
            &evidence_by_id
        ));
    }

    /// 回归：能力缺口场景（分支全部 Blocked + `EvidenceGap` 观察 + 零持久化
    /// 任务 + 未分类目标契约）不得再把 Run 打回 Running 假死——Rust 端
    /// 单遍 runtime 没有任何外层循环会在 Continue 后重新调度，无可运行
    /// 工作时必须 Pause，把处置权交还操作者。
    #[test]
    fn evidence_gaps_without_runnable_work_pause_instead_of_spinning() {
        let evaluator = TerminationEvaluator;
        let mut mission = Mission::new(project_id(), "hi".to_string());
        mission.id = mission_id();
        // 已解析契约：否则"契约待分类"分支会先接走（→ Complete），
        // 测不到证据缺口这条暂停理由。
        mission.goal_contract = MissionGoalContract {
            outcome_type: GoalOutcomeType::FlagCapture,
            status: GoalContractStatus::Resolved,
            description: "Recover one verified challenge Flag".to_string(),
            source: GoalContractSource::IntakeModel,
            ..MissionGoalContract::default()
        }
        .validated()
        .unwrap_or_else(|error| panic!("合法契约必须通过校验: {error}"));
        let mut mission_run = run(10, 1);
        mission_run.mission_id = Some(mission_id());

        let mut blocked_a = Branch::new(
            project_id(),
            mission_id(),
            "Target classification and scope clarification".to_string(),
            "hypothesis".to_string(),
        );
        blocked_a.status = BranchStatus::Blocked;
        let mut blocked_b = Branch::new(
            project_id(),
            mission_id(),
            "Initial reachable surface hypothesis".to_string(),
            "hypothesis".to_string(),
        );
        blocked_b.status = BranchStatus::Blocked;

        let mut gap_a = Observation::new(
            project_id(),
            run_id(),
            "Branch blocked: no available solver capability".to_string(),
        );
        gap_a.observation_type = ObservationType::EvidenceGap;
        let mut gap_b = Observation::new(
            project_id(),
            run_id(),
            "Branch blocked: no available solver capability".to_string(),
        );
        gap_b.observation_type = ObservationType::EvidenceGap;

        let assessment = evaluator.evaluate(&TerminationInput {
            mission: Some(&mission),
            branches: &[blocked_a, blocked_b],
            observations: &[gap_a, gap_b],
            ..TerminationInput::new(&project_id(), &mission_run)
        });

        assert_eq!(assessment.status, TerminationStatus::Pause);
        assert_eq!(assessment.evidence_gap_count, 2);
        assert!(
            assessment
                .reasons
                .iter()
                .any(|reason| reason == "evidence gaps remain")
        );
        assert!(
            assessment
                .unmet_goal_requirements
                .iter()
                .any(|item| item.contains("no persisted execution task"))
        );
    }

    /// 对照：仍有可运行分支时，证据缺口不得阻断继续执行。
    #[test]
    fn evidence_gaps_with_runnable_branches_still_continue() {
        let evaluator = TerminationEvaluator;
        let mut mission = Mission::new(project_id(), "audit".to_string());
        mission.id = mission_id();
        let mut mission_run = run(10, 1);
        mission_run.mission_id = Some(mission_id());

        let mut active = Branch::new(
            project_id(),
            mission_id(),
            "Follow-up surface mapping".to_string(),
            "hypothesis".to_string(),
        );
        active.status = BranchStatus::Active;
        active.budget_steps = 4;

        let mut gap = Observation::new(
            project_id(),
            run_id(),
            "Branch blocked: no available solver capability".to_string(),
        );
        gap.observation_type = ObservationType::EvidenceGap;

        let assessment = evaluator.evaluate(&TerminationInput {
            mission: Some(&mission),
            branches: std::slice::from_ref(&active),
            observations: &[gap],
            ..TerminationInput::new(&project_id(), &mission_run)
        });

        assert_eq!(assessment.status, TerminationStatus::Continue);
        assert!(
            assessment
                .reasons
                .iter()
                .any(|reason| reason == "runnable Mission branches remain")
        );
    }
}
