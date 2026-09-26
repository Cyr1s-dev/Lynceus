//! Mission branch runtime（Python `AuditManager.run_branch_runtime`）。
//!
//! 这一层是 Rust 编排器的真正执行闭环：分支先经能力路由，再创建
//! Intent/Task，solver 只返回结构化结果，最后由 manager 一次性校验并
//! 提交图谱。任何“看起来完成”但没有通过 Observer、Termination 与
//! Closure Gate 的路径都会停在 `Paused`，不会误报 `Completed`。

// Runtime method signatures intentionally mirror the Python orchestration
// contract; these allowances are limited to that compatibility boundary.
#![allow(clippy::assigning_clones)]
#![allow(clippy::clone_on_copy)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::uninlined_format_args)]

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use agents::capability_router::RouteInput;
use agents::context::BuildContextInput;
use agents::observer::ReviewInput;
use agents::reflector::ReflectFailureInput;
use agents::solver::{SolverContext, SolverError, SolverExecutionError, SolverResult};
use agents::termination::TerminationInput;
use agents::trajectory::SummarizeInput;
use agents::worker::WorkerMcpBinding;
use evidence::{FindingVerificationService, Sha256Fingerprint};
use futures_util::stream::{self, StreamExt};
use models::agent::{Observation, ObservationType, TerminationAssessment, TerminationStatus};
use models::event::{AuditEvent, AuditEventType};
use models::finding::Finding;
use models::ids::MissionId;
use models::ids::RunId;
use models::intent::{Intent, IntentStatus};
use models::lifecycle::{FindingStatus, RunStatus, TaskStatus};
use models::mission::{Branch, BranchStatus};
use models::run::AgentTask;
use models::tool_invocation::ToolInvocation;
use models::{AgentNarrativeEventKind, AuditRun, Mission, Project, utcnow};
use serde_json::{Map, Value};
use tokio::time::{Duration, MissedTickBehavior, interval, sleep};

use crate::errors::EngineError;
use crate::events::EventDraft;
use crate::manager::AuditManager;
use crate::narratives::NarrativeDraft;

const BRANCH_RUNTIME_MAX_CONCURRENCY: usize = 8;

/// 整个 mission 同时在跑的外部 worker 尝试数上限的默认值。
///
/// 语义 = 运营者口径的"最大并发 Worker"：无论同时有多少 branch / 多少待领
/// intent，外部 worker 进程同时在跑的不超过这么多；其余 attempt 在池外 FIFO
/// 排队，取到许可才真正 spawn。
const DEFAULT_MAX_CONCURRENT_WORKERS: i64 = 4;

/// 并发池上限的硬顶。防止配置笔误把机器打爆。
const WORKER_POOL_MAX_CONCURRENCY: i64 = 32;

/// lease 结算结果：`Settled` = 正常迁移到终态；`Lost` = 结算时 lease 已非
/// 我方持有（过期 / 被回收 / revision 失配）。`Lost` **不是错误**——任务结果
/// 在 settle 之前已由 `commit_solver_*` 落库，这里只表示并发槽簿记未能由本
/// guard 完成，调用方须显式记录（不静默），绝不据此把整个 run 判败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeaseSettlement {
    Settled,
    Lost,
}

struct WorkerLeaseGuard {
    repository: Arc<dyn storage::Repository>,
    lease_id: String,
    worker_run_id: String,
    revision: i64,
    armed: bool,
}

impl WorkerLeaseGuard {
    fn new(repository: Arc<dyn storage::Repository>, lease: &models::WorkerLease) -> Self {
        Self {
            repository,
            lease_id: lease.id.to_string(),
            worker_run_id: lease.worker_run_id.clone().unwrap_or_default(),
            revision: lease.revision,
            armed: true,
        }
    }

    /// solver 长跑期间延长租期（保活）。成功时回写最新 revision 并返回
    /// `true`；lease 已非我方持有（过期/被回收）返回 `false` 并解除 Drop 的
    /// cancel（无可释放）。真实存储错误经 `?` 上抛。
    fn heartbeat(&mut self, lease_seconds: i64) -> Result<bool, EngineError> {
        let Some(lease) = self.repository.heartbeat_worker_lease(
            &self.lease_id,
            &self.worker_run_id,
            self.revision,
            lease_seconds,
        )? else {
            self.armed = false;
            return Ok(false);
        };
        self.revision = lease.revision;
        Ok(true)
    }

    fn settle(&mut self, status: TaskStatus) -> Result<LeaseSettlement, EngineError> {
        let result = match status {
            TaskStatus::Succeeded => self.repository.complete_worker_lease(
                &self.lease_id,
                &self.worker_run_id,
                self.revision,
            )?,
            TaskStatus::Failed => self.repository.fail_worker_lease(
                &self.lease_id,
                &self.worker_run_id,
                self.revision,
            )?,
            TaskStatus::Cancelled | TaskStatus::WaitingForDecision | TaskStatus::Queued => self
                .repository
                .cancel_worker_lease(&self.lease_id, &self.worker_run_id, self.revision)?,
            TaskStatus::Running => None,
        };
        let Some(lease) = result else {
            // lease 已非我方持有：任务结果已落库，不再把整个 run 判败，
            // 解除 Drop 的 cancel（无可释放），交由调用方显式记录。
            self.armed = false;
            return Ok(LeaseSettlement::Lost);
        };
        self.revision = lease.revision;
        self.armed = false;
        Ok(LeaseSettlement::Settled)
    }
}

impl Drop for WorkerLeaseGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.repository.cancel_worker_lease(
                &self.lease_id,
                &self.worker_run_id,
                self.revision,
            );
        }
    }
}

/// run 级 worker 并发池的 RAII 守门。
///
/// `run_branch_runtime` 入口把池登记到 manager 上并持有一个本 guard；函数
/// 返回（含 `?` 提前返回与 panic 展开）时摘除该 run 的池。否则一次 run 结束
/// 后池会残留在 manager 里，下一个 run 复用到半空的池，并发上限就失真了。
struct WorkerPoolGuard<'a> {
    manager: &'a AuditManager,
    run_id: RunId,
}

impl Drop for WorkerPoolGuard<'_> {
    fn drop(&mut self) {
        // 临界区只有一次 remove，绝不在持锁期间 await；std Mutex 在此处
        // （async 上下文的同步 drop）不会像 tokio blocking_lock 那样 panic。
        let mut pools = self
            .manager
            .worker_pools
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pools.remove(&self.run_id);
    }
}

/// 运营者中断的结果（API 层据此回不同的状态码）。
#[derive(Debug, Clone)]
pub enum MissionInterruptOutcome {
    /// 中断成功：取消信号已发，消息已入注入表等派发层取走。
    Interrupted {
        /// 被打断的 worker run id。
        worker_run_id: String,
        /// 该 worker 的 runtime（wire 值）。
        runtime: String,
        /// 续跑是否已确认（派发层异步完成，中断返回时恒为 false）。
        resumed: bool,
    },
    /// 这个 mission 没有正在运行的 worker（可能从未派发或已全部结束）。
    NothingRunning,
    /// 有 Running 骨架行但它不在本进程的 inflight 表里（api.exe 重启后
    /// 进程已死，取消信号无处可送）。
    NotInflight,
}

impl AuditManager {
    /// 取该 run 的全局 worker 并发池；未注册（不在 `run_branch_runtime`
    /// 作用域内派发）时返回 `None`，调用方按"不限并发"处理。
    fn worker_pool_for(&self, run_id: &str) -> Option<Arc<tokio::sync::Semaphore>> {
        let pools = self
            .worker_pools
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pools.iter().find_map(|(id, pool)| {
            (id.as_str() == run_id).then(|| Arc::clone(pool))
        })
    }

    /// 驱动一个 Mission run 的分支执行、评审、终止与收口链。
    ///
    /// `max_concurrent_branches` 控制同一波可运行 Branch 的有界并发；
    /// SQLite 写入仍由仓储事务与连接锁串行化，状态机语义保持不变。
    pub(crate) async fn run_branch_runtime(
        &self,
        mission_id: &MissionId,
        run_id: &RunId,
        max_concurrent_branches: Option<i64>,
        max_total_steps: Option<i64>,
    ) -> Result<(), EngineError> {
        let _mutation_guard = self.run_mutation_guard().await;
        let mission = self.require_mission(mission_id.as_str())?;
        let project = self.require_project(mission.project_id.as_str())?;
        let mut run = self.require_run(run_id)?;
        if run.project_id != project.id || run.mission_id.as_ref() != Some(mission_id) {
            return Err(EngineError::RunNotFound(format!(
                "audit run {} does not belong to mission {}",
                run_id.as_str(),
                mission_id.as_str()
            )));
        }
        if let Some(limit) = max_total_steps {
            if limit < 1 {
                return Err(EngineError::BudgetConfigError(
                    "max_total_steps must be >= 1".to_string(),
                ));
            }
            run.max_total_steps = limit;
            run.updated_at = utcnow();
            self.repository().update_run(&run)?;
        }

        if matches!(run.status, RunStatus::Pending | RunStatus::Paused) {
            run.status = RunStatus::Running;
            run.started_at.get_or_insert_with(utcnow);
            run.updated_at = utcnow();
            self.repository().update_run(&run)?;
        }

        // 全局 worker 并发池：整个 run（跨所有 branch）共享同一个许可池。
        // 必须在这里建——`run_mutation_guard` 已持有，同一 run 不会有第二个
        // 并发派发入口把池建重。guard 保证函数返回时摘除。
        let worker_pool = Arc::new(tokio::sync::Semaphore::new(max_concurrent_workers(&run)));
        {
            let mut pools = self
                .worker_pools
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            pools.insert(run_id.clone(), Arc::clone(&worker_pool));
        }
        let _worker_pool_guard = WorkerPoolGuard {
            manager: self,
            run_id: run_id.clone(),
        };
        tracing::info!(
            run_id = %run_id.as_str(),
            max_concurrent_workers = max_concurrent_workers(&run),
            "worker concurrency pool registered"
        );

        let max_passes = crate::closure::resolve_max_branch_passes(&run);
        for _pass in 0..max_passes {
            let current = self.require_run(run_id)?;
            if current.steps_used >= current.max_total_steps {
                break;
            }
            let branches = self.repository().list_branches(
                Some(project.id.as_str()),
                Some(mission.id.as_str()),
                Some(run.id.as_str()),
            )?;
            let runnable: Vec<_> = branches
                .into_iter()
                .filter(|branch| {
                    matches!(branch.status, BranchStatus::Proposed | BranchStatus::Active)
                        && branch.steps_used < branch.budget_steps
                })
                .collect();
            if runnable.is_empty() {
                break;
            }
            let limit = branch_concurrency_limit(&current, max_concurrent_branches, runnable.len());
            let findings_before = self.repository().list_findings(project.id.as_str())?.len();
            // 寻宝:按吸引子降序派发,高吸引力分支优先拿到步数预算与并发槽。
            let runnable = crate::attractiveness::rank_by_attractiveness(runnable);
            self.run_branch_wave(&mission, &project, run_id, runnable, limit)
                .await?;
            // 螺旋账本:本波有无新发现 → 更新停滞计数(驱动下一波并发扩张)。
            let findings_after = self.repository().list_findings(project.id.as_str())?.len();
            let mut ledger_run = self.require_run(run_id)?;
            let empty = crate::attractiveness::note_wave_productivity(
                &mut ledger_run.config,
                findings_after > findings_before,
            );
            ledger_run.updated_at = utcnow();
            self.repository().update_run(&ledger_run)?;
            tracing::debug!(
                run_id = %run_id.as_str(),
                empty_passes = empty,
                "spiral ledger updated after branch wave"
            );
        }

        self.normalize_exhausted_branches(run_id)?;
        let mut run = self.require_run(run_id)?;
        let mut assessment = self.review_and_evaluate(&mission, &project, &run).await?;
        // 目标驱动的续跑循环（对齐参考实现的 planner 持续性）：
        // 波次干涸不再是终点。目标已满足 → 收口链验证后完成；目标未满足
        // → 同样进收口链重新规划（trigger=EnumerationStall），收容不下时
        // 用确定性兜底分支保证"永远有下一轮"，直到轮次/步数预算耗尽才按
        // 真实终态落定。队列驱动 → 目标驱动的差别全在这一段。
        let max_rounds = crate::closure::resolve_max_closure_rounds(&run);
        for round_index in 0..max_rounds {
            run = self.require_run(run_id)?;
            if run.steps_used >= run.max_total_steps {
                break;
            }
            let trigger = if assessment.status == TerminationStatus::Complete {
                models::MetacognitionTrigger::Convergence
            } else {
                models::MetacognitionTrigger::EnumerationStall
            };
            let outcome = self
                .run_mission_closure_chain(
                    &mission,
                    &project,
                    &run,
                    &assessment,
                    round_index,
                    round_index + 1 < max_rounds,
                    trigger,
                )
                .await?;
            if outcome.completion_approved() {
                self.complete_run(&mission, &run).await?;
                return Ok(());
            }
            let admitted: Vec<Branch> = if outcome.admitted_branch_ids.is_empty() {
                if assessment.status == TerminationStatus::Complete {
                    // 原语义：目标已满足但收口链拒绝完成 → 拒绝注记停机。
                    self.pause_run_with_note(&run, &Self::closure_refusal_message(&outcome))?;
                    return Ok(());
                }
                // 目标未满足且收容不下：确定性兜底分支强制再规划一轮。
                // 没有它，"目标未满足 + MGATE 不升级"会直接停机——那是
                // 队列驱动的旧行为，出洞效率的死角。
                tracing::info!(
                    run_id = %run.id.as_str(),
                    round = round_index,
                    "closure chain admitted nothing while the goal is unsatisfied; \
                     forcing a deterministic re-plan branch"
                );
                let fallback = self.deterministic_goal_branch(&mission, &run);
                vec![self.repository().create_branch(&fallback)?]
            } else {
                self.repository()
                    .list_branches(
                        Some(project.id.as_str()),
                        Some(mission.id.as_str()),
                        Some(run.id.as_str()),
                    )?
                    .into_iter()
                    .filter(|branch| {
                        outcome
                            .admitted_branch_ids
                            .iter()
                            .any(|id| id == branch.id.as_str())
                    })
                    .collect()
            };
            let forced_replan = outcome.admitted_branch_ids.is_empty();
            let mut resumed = run.clone();
            resumed.status = RunStatus::Running;
            resumed.note = Some(if forced_replan {
                "deterministic re-plan branch admitted; goal still unsatisfied".to_string()
            } else {
                "closure chain admitted escalation branches".to_string()
            });
            resumed.updated_at = utcnow();
            self.repository().update_run(&resumed)?;
            self.sync_mission_status_from_run(&resumed)?;
            let limit = branch_concurrency_limit(&resumed, max_concurrent_branches, admitted.len());
            let admitted = crate::attractiveness::rank_by_attractiveness(admitted);
            self.run_branch_wave(&mission, &project, run_id, admitted, limit)
                .await?;
            self.normalize_exhausted_branches(run_id)?;
            run = self.require_run(run_id)?;
            assessment = self.review_and_evaluate(&mission, &project, &run).await?;
        }
        if assessment.status == TerminationStatus::Complete {
            self.pause_run_with_note(
                &run,
                "closure chain refused completion: closure round budget exhausted",
            )?;
        } else {
            // 轮次预算耗尽且目标仍未满足：按状态机落定（Pause /
            // NeedsHumanDecision 等待答门等），理由来自终态评估本身。
            self.apply_termination_status(&run, &assessment)?;
        }
        Ok(())
    }

    async fn run_branch_wave(
        &self,
        mission: &Mission,
        project: &Project,
        run_id: &RunId,
        branches: Vec<Branch>,
        concurrency: usize,
    ) -> Result<(), EngineError> {
        if branches.is_empty() {
            return Ok(());
        }
        let mut scheduled = Vec::new();
        for branch in branches {
            if !self.repository().reserve_run_steps(run_id.as_str(), 1)? {
                break;
            }
            scheduled.push(branch);
        }
        if scheduled.is_empty() {
            return Ok(());
        }
        let active_limit = concurrency.clamp(1, scheduled.len());
        tracing::info!(
            run_id = %run_id.as_str(),
            active_branches = scheduled.len(),
            max_concurrent_branches = active_limit,
            "starting bounded branch wave"
        );

        let run = self.require_run(run_id)?;
        let futures = scheduled.into_iter().map(|branch| {
            let run = run.clone();
            async move { self.run_single_branch(mission, project, &run, branch).await }
        });
        let mut first_error = None;
        let mut stream = stream::iter(futures).buffer_unordered(active_limit);
        while let Some(result) = stream.next().await {
            if let Err(error) = result
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(())
        }
    }

    /// 重新审查一个已结束/暂停 Mission。该入口用于恢复历史任务时再次
    /// 验证 flag/finding，不允许仅凭旧的 `completed` 状态短路。
    /// 中断 mission 当前正在跑的外部 worker 并注入运营者消息。
    ///
    /// 语义 = 终端里的 Ctrl+C 再输入：取消信号杀进程，消息留在注入表，
    /// 派发层随 `cancelled` 结局取走并以 resume / fresh start 续跑任务
    /// （见 `engines::worker::dispatch`）。**中断的是一次执行，不是任务。**
    ///
    /// **刻意不取 `run_mutation_guard`**：那把锁被 `run_branch_runtime`
    /// 整程持有（从驱动到全部波次结束），中断端点若也抢它，mission 运行
    /// 期间端点全部堵死——而中断的本质只是发取消信号 + 读库 + best-effort
    /// 记事件，不改写 run 状态机，不需要互斥。
    ///
    /// # Errors
    /// Mission 不存在 / 无 active run / 无 worker runtime 绑定。
    pub async fn interrupt_mission_worker(
        &self,
        mission_id: &MissionId,
        message: &str,
    ) -> Result<MissionInterruptOutcome, EngineError> {
        let mission = self.require_mission(mission_id.as_str())?;
        let run_id = mission.active_run_id.clone().ok_or_else(|| {
            EngineError::Value(format!("mission {mission_id} has no active run"))
        })?;
        let selector = self.worker_runtime().ok_or_else(|| {
            EngineError::Value("no external worker runtime is bound".to_string())
        })?;
        // 候选 = 这个 run 上骨架行还是 Running 的 worker。
        let running: Vec<models::worker::WorkerRun> = self
            .repository()
            .list_worker_runs(Some(mission.project_id.as_str()), Some(run_id.as_str()), None, 50)?
            .into_iter()
            .filter(|run| run.status == models::worker::WorkerRunStatus::Running)
            .collect();
        if running.is_empty() {
            return Ok(MissionInterruptOutcome::NothingRunning);
        }
        // 优先 inflight 表里真实在跑的：重启前留下的僵尸骨架行不在表里，
        // 对它 cancel 只会得到 false，交给下面统一报 NotInflight。
        let inflight = selector.inflight_worker_run_ids().await;
        let Some(target) = running
            .iter()
            .find(|run| inflight.contains(&run.id))
            .or_else(|| running.first())
        else {
            return Ok(MissionInterruptOutcome::NothingRunning);
        };
        if !selector.interrupt_worker(&target.id, message).await {
            return Ok(MissionInterruptOutcome::NotInflight);
        }
        self.record_event_safe(EventDraft {
            run_id: Some(&run_id),
            task_id: target.task_id.as_ref(),
            status: Some("interrupted"),
            message: Some("operator interrupt; worker cancelled, continuation pending"),
            data: Some(Map::from_iter([
                (
                    "worker_run_id".to_string(),
                    Value::String(target.id.clone()),
                ),
                (
                    "runtime".to_string(),
                    Value::String(target.runtime.as_str().to_string()),
                ),
                (
                    "message_chars".to_string(),
                    Value::from(i64::try_from(message.chars().count()).unwrap_or(i64::MAX)),
                ),
            ])),
            ..EventDraft::new(
                &mission.project_id,
                AuditEventType::UserNote,
                "operator",
                "Operator interrupted the running worker",
            )
        })
        .await;
        Ok(MissionInterruptOutcome::Interrupted {
            worker_run_id: target.id.clone(),
            runtime: target.runtime.as_str().to_string(),
            resumed: false,
        })
    }

    pub async fn reassess_mission_completion(
        &self,
        mission_id: &MissionId,
    ) -> Result<TerminationAssessment, EngineError> {
        let _mutation_guard = self.run_mutation_guard().await;
        let mission = self.require_mission(mission_id.as_str())?;
        let project = self.require_project(mission.project_id.as_str())?;
        let run_id = mission
            .active_run_id
            .clone()
            .or_else(|| {
                self.repository()
                    .list_runs(project.id.as_str())
                    .ok()
                    .and_then(|runs| {
                        runs.into_iter()
                            .filter(|run| run.mission_id.as_ref() == Some(&mission.id))
                            .max_by_key(|run| run.updated_at)
                    })
                    .map(|run| run.id)
            })
            .ok_or_else(|| {
                EngineError::RunNotFound(format!("mission {} has no run", mission_id))
            })?;
        self.normalize_exhausted_branches(&run_id)?;
        let run = self.require_run(&run_id)?;
        let assessment = self.review_and_evaluate(&mission, &project, &run).await?;
        if assessment.status == TerminationStatus::Complete {
            let outcome = self
                .run_mission_closure_chain(
                    &mission,
                    &project,
                    &run,
                    &assessment,
                    0,
                    true,
                    models::MetacognitionTrigger::Manual,
                )
                .await?;
            if outcome.completion_approved() {
                self.complete_run(&mission, &run).await?;
            } else {
                self.pause_run_with_note(&run, &Self::closure_refusal_message(&outcome))?;
            }
        } else {
            self.apply_termination_status(&run, &assessment)?;
        }
        Ok(assessment)
    }

    async fn run_single_branch(
        &self,
        mission: &Mission,
        project: &Project,
        run: &AuditRun,
        mut branch: Branch,
    ) -> Result<(), EngineError> {
        branch.status = BranchStatus::Active;
        branch.updated_at = utcnow();
        self.repository().update_branch(&branch)?;
        self.record_branch_observation(
            project,
            run,
            &branch,
            ObservationType::Hypothesis,
            format!("Executing branch: {}", branch.title),
            Map::new(),
        )?;

        // 分类定界分支：Direct API 分类（不走 Harness）——任务类型识别由
        // `natural_language_intake` 路由的模型直接完成，结果入白板。
        if branch
            .metadata
            .get("branch_kind")
            .and_then(Value::as_str)
            == Some("mixed.classification")
        {
            return self
                .run_classification_branch(mission, project, run, branch)
                .await;
        }

        let facts = self.repository().list_facts(project.id.as_str())?;
        let modules = self.repository().list_modules()?;
        let module_ids: Vec<String> = modules
            .iter()
            .filter(|module| module.enabled)
            .map(|module| module.id.as_str().to_string())
            .collect();
        let solver_names = self.solvers().names();
        let dispatch = self.capability_router.route(&RouteInput {
            branch: &branch,
            project,
            available_solvers: &solver_names,
            available_modules: &module_ids,
            run_config: Some(&run.config),
            facts: &facts,
        });
        let Some(solver_name) = dispatch.solver.clone().filter(|_| !dispatch.capability_gap) else {
            branch.status = BranchStatus::Blocked;
            branch.metadata.insert(
                "capability_gap".to_string(),
                Value::String(dispatch.gap_summary.unwrap_or(dispatch.rationale)),
            );
            branch.updated_at = utcnow();
            self.repository().update_branch(&branch)?;
            self.record_branch_observation(
                project,
                run,
                &branch,
                ObservationType::EvidenceGap,
                "Branch blocked: no available solver capability".to_string(),
                Map::new(),
            )?;
            return Ok(());
        };

        let mut intent = Intent::new(project.id.clone(), branch.title.clone());
        intent.mission_id = Some(mission.id.clone());
        intent.branch_id = Some(branch.id.clone());
        intent.run_id = Some(run.id.clone());
        intent.description = Some(branch.hypothesis.clone());
        intent.source_fact_ids.clone_from(&branch.related_fact_ids);
        intent.solver = Some(solver_name.clone());
        intent.priority = branch.priority;
        intent.max_steps = branch.budget_steps.max(1);
        intent.created_by = "manager".to_string();
        self.repository().add_intent(&intent)?;
        self.record_branch_observation(
            project,
            run,
            &branch,
            ObservationType::Hypothesis,
            format!("Intent dispatched to {solver_name}"),
            Map::from_iter([(
                String::from("intent_id"),
                Value::String(intent.id.to_string()),
            )]),
        )?;

        let task = self
            .dispatch_intent(
                project,
                mission,
                run,
                &branch,
                intent,
                dispatch.config,
                solver_name,
            )
            .await?;
        self.update_branch_after_task(project, run, branch, &task)?;
        Ok(())
    }

    /// 分类定界分支的 Direct API 实现：`natural_language_intake` 路由的
    /// 模型直接对目标定界分类（不再起一个 Harness worker 专门做分类）。
    /// 结果写入白板事实与分支 metadata；失败阻塞该分支（绝不静默放行）。
    async fn run_classification_branch(
        &self,
        mission: &Mission,
        project: &Project,
        run: &AuditRun,
        mut branch: Branch,
    ) -> Result<(), EngineError> {
        let Some(runtime) = self.provider_runtime() else {
            branch.metadata.insert(
                "classification_error".to_string(),
                Value::String("provider runtime unavailable".to_string()),
            );
            branch.status = BranchStatus::Blocked;
            branch.updated_at = utcnow();
            self.repository().update_branch(&branch)?;
            return self.record_branch_observation(
                project,
                run,
                &branch,
                ObservationType::Blockage,
                "目标分类被阻塞：Provider 运行时不可用".to_string(),
                Map::new(),
            );
        };
        let payload = serde_json::json!({
            "goal": mission.user_goal,
            "target": project.target,
            "hypothesis": branch.hypothesis,
        });
        let messages = [
            agents::llm::LlmMessage::new(
                "system",
                "你是任务定界分类器。只输出一个 JSON 对象，不要输出其他文本：{\"target_type\": \"url|source|binary|traffic|cloud|unknown\", \"scope_summary\": \"一句中文范围概括\", \"constraints\": [\"短句约束\"], \"reason\": \"一句分类依据\"}".to_string(),
            ),
            agents::llm::LlmMessage::new(
                "user",
                serde_json::to_string_pretty(&payload).unwrap_or_default(),
            ),
        ];
        match agents::llm::ProviderRuntime::generate_structured(
            runtime.as_ref(),
            agents::llm::StructuredGenerationRequest {
                provider_id: "",
                messages: &messages,
                purpose: "natural_language_intake",
                project_id: Some(&project.id),
                run_id: Some(&run.id),
                task_id: None,
            },
        )
        .await
        {
            Ok(map) => {
                let target_type = map
                    .get("target_type")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let scope_summary = map
                    .get("scope_summary")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let reason = map
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.add_user_fact(
                    project.id.as_str(),
                    "classification",
                    &format!("目标分类定界：{target_type} — {scope_summary}"),
                    Map::from_iter([
                        (
                            String::from("target_type"),
                            Value::String(target_type.clone()),
                        ),
                        (
                            String::from("branch_id"),
                            Value::String(branch.id.as_str().to_string()),
                        ),
                    ]),
                    Vec::new(),
                    0.7,
                )?;
                branch.metadata.insert(
                    "classification".to_string(),
                    Value::Object(map),
                );
                branch.status = BranchStatus::Succeeded;
                branch.updated_at = utcnow();
                self.repository().update_branch(&branch)?;
                self.record_branch_observation(
                    project,
                    run,
                    &branch,
                    ObservationType::HypothesisUpdate,
                    format!(
                        "目标分类完成（Direct API）：{target_type} — {scope_summary}（{reason}）"
                    ),
                    Map::new(),
                )?;
                Ok(())
            }
            Err(error) => {
                branch.metadata.insert(
                    "classification_error".to_string(),
                    Value::String(error.to_string()),
                );
                branch.status = BranchStatus::Blocked;
                branch.updated_at = utcnow();
                self.repository().update_branch(&branch)?;
                self.record_branch_observation(
                    project,
                    run,
                    &branch,
                    ObservationType::Blockage,
                    format!("目标分类 Direct API 失败：{error}"),
                    Map::new(),
                )?;
                Ok(())
            }
        }
    }

    async fn dispatch_intent(
        &self,
        project: &Project,
        mission: &Mission,
        run: &AuditRun,
        branch: &Branch,
        mut intent: Intent,
        mut config: Map<String, Value>,
        solver_name: String,
    ) -> Result<AgentTask, EngineError> {
        let solver = self.solvers().get(&solver_name).ok_or_else(|| {
            EngineError::SolverConfigError(format!("unknown solver: {solver_name}"))
        })?;
        solver
            .validate_config(&config, Some(project))
            .map_err(|error| EngineError::SolverConfigError(error.to_string()))?;

        // Solver bootstrap（唯一工具集选择入口）：确定性信号 → 平行
        // Knowledge/Tool 检索 → visible_tool_ids / knowledge_hints 注入
        // config。失败一律降级为无注入（Harness 保持静态行为）。
        if let Some(summary) = self
            .solver_bootstrap(project, run, branch, &mut config)
            .await
        {
            self.record_branch_observation(
                project,
                run,
                branch,
                ObservationType::Hypothesis,
                summary.text,
                summary.data,
            )?;
        }

        let mut task = AgentTask::new(project.id.clone(), run.id.clone(), solver_name.clone());
        task.mission_id = Some(mission.id.clone());
        task.branch_id = Some(branch.id.clone());
        task.intent_id = Some(intent.id.to_string());
        task.payload = config.clone();
        task.budget_steps = branch.budget_steps.max(1);
        self.repository().create_task(&task)?;
        self.repository()
            .append_run_task(run.id.as_str(), task.id.as_str())?;
        let worker_id = config
            .get("worker_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("coordinator-worker")
            .to_string();
        let worker_run_id = format!("worker-run-{}", task.id.as_str());
        let lease_seconds = run
            .config
            .get("worker_lease_seconds")
            .and_then(Value::as_i64)
            .unwrap_or(300)
            .clamp(1, 3600);
        let lease = self
            .repository()
            .claim_worker_lease(
                project.id.as_str(),
                mission.id.as_str(),
                run.id.as_str(),
                task.id.as_str(),
                &worker_id,
                &worker_run_id,
                lease_seconds,
            )?
            .ok_or_else(|| {
                EngineError::Value(format!("worker lease claim failed for task {}", task.id))
            })?;
        let mut lease_guard = WorkerLeaseGuard::new(Arc::clone(self.repository()), &lease);
        intent.status = IntentStatus::Claimed;
        intent.claimed_by_task_id = Some(task.id.clone());
        intent.updated_at = utcnow();
        self.repository().update_intent(&intent)?;

        task.status = TaskStatus::Running;
        task.started_at = Some(lease.acquired_at);
        self.repository().update_task(&task)?;
        self.record_event_safe(EventDraft {
            run_id: Some(&run.id),
            task_id: Some(&task.id),
            status: Some(TaskStatus::Running.as_str()),
            ..EventDraft::new(
                &project.id,
                AuditEventType::TaskStarted,
                "manager",
                "求解器任务已启动",
            )
        })
        .await;
        self.record_narrative_safe(NarrativeDraft {
            run_id: Some(&run.id),
            mission_id: Some(&mission.id),
            branch_id: Some(&branch.id),
            task_id: Some(&task.id),
            ..NarrativeDraft::new(
                &project.id,
                "manager",
                AgentNarrativeEventKind::Progress,
                &format!(
                    "求解器任务已启动：{solver_name}（分支\"{}\"）",
                    branch.title
                ),
            )
        });

        let facts = self.repository().list_facts(project.id.as_str())?;
        let evidence = self
            .repository()
            .list_evidence(project.id.as_str())?
            .into_iter()
            .filter(|item| item.run_id.as_ref() == Some(&run.id) || item.run_id.is_none())
            .collect();
        let findings = self
            .repository()
            .list_findings(project.id.as_str())?
            .into_iter()
            .filter(|item| item.run_id.as_ref() == Some(&run.id) || item.run_id.is_none())
            .collect();
        let tools = self
            .repository()
            .list_tool_invocations(Some(project.id.as_str()))?
            .into_iter()
            .filter(|item| item.run_id.as_ref() == Some(&run.id) || item.run_id.is_none())
            .collect();
        let hints = self.repository().list_hints(project.id.as_str())?;
        let context_pack = self
            .context_compressor
            .build_context_pack(&BuildContextInput {
                project_id: project.id.as_str(),
                run_id: run.id.as_str(),
                task_id: Some(task.id.as_str()),
                purpose: "solver",
                token_budget: 2048,
                current_intent_id: Some(intent.id.as_str()),
                compression_strategy: "deterministic_v1",
            })
            .ok()
            .and_then(|result| {
                let pack = self
                    .repository()
                    .add_context_pack(&result.context_pack)
                    .ok()?;
                let _ = self
                    .repository()
                    .add_context_compression_report(&result.report);
                Some(pack)
            });
        let mut context = SolverContext::new(project.id.clone(), run.id.clone(), task.id.clone());
        context.mission_id = Some(mission.id.clone());
        context.branch_id = Some(branch.id.clone());
        context.intent = Some(intent.clone());
        context.target = project.target.clone();
        context.facts = facts;
        context.evidence = evidence;
        context.findings = findings;
        context.tool_invocations = tools;
        context.hints = hints;
        context.config = config;
        // mission 目标与操作约束必须到达 worker 指令：前者让 worker 知道
        // 自己这条意图服务什么（此前 mission_goal 从未注入，worker 只看
        // 得到分支意图）；后者是 OPSEC 红线——约束以最高优先级注入，
        // 凌驾于一切探索启发式（对齐参考实现的 constraintBlock）。
        context.config.insert(
            "mission_goal".to_string(),
            Value::String(mission.user_goal.clone()),
        );
        if !mission.constraints.is_empty() {
            context.config.insert(
                "constraints".to_string(),
                serde_json::to_value(&mission.constraints).unwrap_or(Value::Null),
            );
        }
        // 显式钉定（config.provider_id）才解析校验；未钉定留空，由网关
        // 按 `agent_tool_harness` 用途路由（路由表优先，回落默认 provider）。
        context.provider_id = if run.config.get("provider_id").is_some() {
            self.resolve_provider_id(&run.config)?
        } else {
            None
        };
        context.provider_runtime = self.provider_runtime();
        context.worker_runtime = self.worker_runtime();
        // WP4：Agent 预设来源（mission config 指定 / profile 绑定 / 内置默认
        // 的解析在 dispatch 派发时进行）。
        context.agent_preset_source = Some(Arc::new(RepositoryAgentPresetSource {
            repository: Arc::clone(self.repository()),
        }));
        context.context_pack = context_pack;
        context.budget_steps = task.budget_steps;

        // —— Intent 级 worker 并发（swarm）：同一意图同时派 N 个不同引擎的
        // worker，各自独立 MCP grant、共挂同一块 mission 白板；第一个产出可用
        // 结果的胜出，其余取消并 settle。width=1 或唯一可用引擎时退化为原单
        // worker 路径（grant 用 task 派生的 worker_run_id，行为不变）。
        let timeout_seconds = run
            .config
            .get("timeout_seconds")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0);
        let heartbeat_seconds = resolve_lease_heartbeat_seconds(&run.config, lease_seconds);
        let available_engines: Vec<models::worker::WorkerRuntimeType> = match self.worker_runtime() {
            Some(runtime) => runtime
                .probes()
                .await
                .into_iter()
                .filter(|probe| probe.availability.is_available())
                .map(|probe| probe.runtime)
                .collect(),
            None => Vec::new(),
        };
        // 停滞扩张:螺旋账本空转越多,swarm 多派引擎(仅加在用户配置之上,
        // 不覆盖 intent_worker_concurrency;.min 仍受可用引擎数封顶)。
        let empty_passes = crate::attractiveness::empty_passes(&run.config);
        let width = (intent_worker_concurrency(run) + crate::attractiveness::escalation_bump(empty_passes))
            .min(available_engines.len().max(1));
        // 全局并发池削顶：一个 attempt 就要一个许可，所以 width 绝不允许超过
        // 池大小——否则 `acquire_many(width)` 在池小于 width 时永远等不到
        // 齐，整个 run 死锁。
        let worker_pool = self.worker_pool_for(run.id.as_str());
        let width = clamp_width_to_pool(width, worker_pool.as_ref().map(|_| max_concurrent_workers(run)));
        // 池外 FIFO 排队：拿不到许可就**不 spawn**。待领 intent 队列为空时
        // 压根走不到这里，因此不会为"没任务"产生任何一次 LLM 调用。许可持有
        // 到本函数返回（任务落库之后）才释放，槽位因此真实反映"在跑的 worker"。
        let _worker_permits = match worker_pool.as_ref() {
            Some(pool) => Some(
                Arc::clone(pool)
                    .acquire_many_owned(u32::try_from(width).unwrap_or(1))
                    .await
                    .map_err(|error| {
                        EngineError::Value(format!(
                            "worker concurrency pool for run {} is closed: {error}",
                            run.id.as_str()
                        ))
                    })?,
            ),
            None => None,
        };
        let solver_result = if width <= 1 || available_engines.len() <= 1 {
            context.worker_mcp =
                self.issue_worker_mcp_for(&context, &format!("worker-run-{}", task.id.as_str()))?;
            let grant = context.worker_mcp.as_ref().map(|b| b.grant_id.clone());
            let result = Self::solve_with_heartbeat(
                &solver,
                context,
                &mut lease_guard,
                lease_seconds,
                heartbeat_seconds,
                timeout_seconds,
            )
            .await;
            if let Some(grant) = grant
                && let Some(mcp) = self.worker_mcp()
            {
                let _ = mcp.revoke_worker_grant(&grant);
            }
            result
        } else {
            self.run_intent_swarm(
                &solver,
                &context,
                &available_engines,
                width,
                &mut lease_guard,
                lease_seconds,
                heartbeat_seconds,
                timeout_seconds,
            )
            .await
        };
        match solver_result {
            Ok(result) => {
                if solver_result_is_failure(&result) {
                    // 区分"空转"与"全败"：两者的排查动作完全不同——前者要去看
                    // worker 的运行时环境（shell / CLI 起没起来），后者要去看
                    // 具体哪个工具报了什么。
                    let message = result.notes.clone().unwrap_or_else(|| {
                        if result.tool_invocations.is_empty() {
                            "solver produced no output and invoked no tools (idle worker run)"
                                .to_string()
                        } else {
                            "all solver tool invocations failed".to_string()
                        }
                    });
                    self.commit_solver_failure(
                        project,
                        run,
                        &mut task,
                        &mut intent,
                        SolverError::Execution(
                            SolverExecutionError::new(message)
                                .with_tool_invocations(result.tool_invocations),
                        ),
                    )?;
                } else {
                    self.commit_solver_success(
                        project,
                        mission,
                        run,
                        &mut task,
                        &mut intent,
                        result,
                    )?;
                }
            }
            Err(error) => {
                self.commit_solver_failure(project, run, &mut task, &mut intent, error)?;
            }
        }
        if lease_guard.settle(task.status)? == LeaseSettlement::Lost {
            // lease 在结算时已非我方持有（过期 / 被回收 / revision 失配）。
            // 任务结果已由 commit_solver_* 落库，这里显式记录（不静默）后让
            // 分支与 run 正常继续——绝不因并发槽簿记失败把整个 run 判败。
            tracing::warn!(
                lease = %lease.id,
                task = %task.id.as_str(),
                status = %task.status.as_str(),
                "worker lease lost before settlement; task result already committed, run continues"
            );
            let _ = self.record_branch_observation(
                project,
                run,
                branch,
                ObservationType::FailureBoundary,
                format!(
                    "Worker lease lost before settlement; task result already committed (task {}).",
                    task.id.as_str()
                ),
                Map::new(),
            );
        }
        Ok(task)
    }

    /// Coordinator 为当前 task 发放 scope-bound MCP grant，并把 bearer 以
    /// 内存 binding 传给 WorkerRuntime。MCP server 不从 worker 输入推断
    /// mission/run/task/worker 身份。
    /// 单 worker 求解：与心跳、超时共驱（心跳周期性延长 lease 租期，防止合法
    /// 长跑 worker 的 lease 被 expire 而丢；超时保留原报文与取消语义；solve
    /// 完成即收敛）。
    async fn solve_with_heartbeat(
        solver: &Arc<dyn agents::solver::BaseSolver>,
        context: SolverContext,
        lease_guard: &mut WorkerLeaseGuard,
        lease_seconds: i64,
        heartbeat_seconds: u64,
        timeout_seconds: Option<i64>,
    ) -> Result<SolverResult, SolverError> {
        let solve_fut = solver.solve(context);
        tokio::pin!(solve_fut);
        let timeout_fut = async {
            match timeout_seconds {
                Some(seconds) => sleep(Duration::from_secs(u64::try_from(seconds).unwrap_or(1))).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(timeout_fut);
        let mut ticker = interval(Duration::from_secs(heartbeat_seconds));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        ticker.tick().await;
        let mut lease_lost_midflight = false;
        loop {
            tokio::select! {
                result = &mut solve_fut => break result,
                _ = &mut timeout_fut => {
                    let seconds = timeout_seconds.unwrap_or(0);
                    break Err(SolverError::Other(format!(
                        "solver timed out after {seconds} second(s)"
                    )));
                }
                _ = ticker.tick(), if !lease_lost_midflight => {
                    match lease_guard.heartbeat(lease_seconds) {
                        Ok(true) => {}
                        Ok(false) => {
                            lease_lost_midflight = true;
                            tracing::warn!(
                                "worker lease lost mid-flight; heartbeat stopped, solver continues"
                            );
                        }
                        Err(error) => {
                            lease_lost_midflight = true;
                            tracing::warn!(
                                error = %error,
                                "worker lease heartbeat failed; heartbeat stopped, solver continues"
                            );
                        }
                    }
                }
            }
        }
    }

    /// Intent 级 swarm：同一意图并发派 `width` 个不同引擎的 worker（各自独立
    /// MCP grant + worker_run_id，共挂同一块 mission 白板）。第一个产出「可用
    /// 结果」的胜出，其余经 `runtime.runtime(engine).cancel(worker_run_id)`
    /// 取消；随后继续 poll 到全部 settle（取消的 worker 以 Cancelled 收尾），
    /// 避免遗留 running。全部 settle 后：有胜出用胜出，否则回退错误。
    #[allow(clippy::too_many_arguments)]
    async fn run_intent_swarm(
        &self,
        solver: &Arc<dyn agents::solver::BaseSolver>,
        base: &SolverContext,
        engines: &[models::worker::WorkerRuntimeType],
        width: usize,
        lease_guard: &mut WorkerLeaseGuard,
        lease_seconds: i64,
        heartbeat_seconds: u64,
        timeout_seconds: Option<i64>,
    ) -> Result<SolverResult, SolverError> {
        let runtime = self.worker_runtime();
        let cancel_all = |spawned: &[(String, models::worker::WorkerRuntimeType)], keep: &str| {
            let Some(runtime) = runtime.as_ref() else {
                return;
            };
            for (worker_run_id, engine) in spawned {
                if worker_run_id.as_str() == keep {
                    continue;
                }
                if let Some(adapter) = runtime.runtime(*engine) {
                    let rid = worker_run_id.clone();
                    tokio::spawn(async move {
                        let _ = adapter.cancel(&rid).await;
                    });
                }
            }
        };

        let mut futures = stream::FuturesUnordered::new();
        let mut spawned: Vec<(String, models::worker::WorkerRuntimeType)> = Vec::new();
        for engine in engines.iter().take(width) {
            let worker_run_id = format!("worker-run-{}-{}", base.task_id.as_str(), engine.as_str());
            spawned.push((worker_run_id.clone(), *engine));
            futures.push(self.swarm_attempt(base, *engine, worker_run_id, solver));
        }

        let timeout_fut = async {
            match timeout_seconds {
                Some(seconds) => sleep(Duration::from_secs(u64::try_from(seconds).unwrap_or(1))).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(timeout_fut);
        let mut ticker = interval(Duration::from_secs(heartbeat_seconds));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        ticker.tick().await;
        let mut lease_lost = false;
        let mut timed_out = false;

        let mut winner: Option<SolverResult> = None;
        let mut winner_run_id = String::new();
        let mut last_error: Option<SolverError> = None;
        loop {
            tokio::select! {
                next = futures.next() => {
                    let Some((worker_run_id, result)) = next else { break; };
                    match result {
                        Ok(result) if winner.is_none() && !solver_result_is_failure(&result) => {
                            winner_run_id = worker_run_id;
                            winner = Some(result);
                            cancel_all(&spawned, &winner_run_id);
                            // 第一个可用结果即胜出：取消并 settle 其余，随即返回，
                            // 不再干等（取消信号未必能立刻杀灭 CLI 进程；干等会把
                            // 分支卡到所有 loser 自然结束）。未胜出的 future 在
                            // 函数返回时被 drop，kill_on_drop 兜底杀进程。
                            self.settle_swarm_losers(&spawned, &winner_run_id);
                            break;
                        }
                        Ok(_) => {}
                        Err(error) => {
                            if last_error.is_none() {
                                last_error = Some(error);
                            }
                        }
                    }
                }
                _ = &mut timeout_fut, if !timed_out => {
                    timed_out = true;
                    cancel_all(&spawned, "");
                    self.settle_swarm_losers(&spawned, "");
                    break;
                }
                _ = ticker.tick(), if !lease_lost => {
                    match lease_guard.heartbeat(lease_seconds) {
                        Ok(true) => {}
                        _ => lease_lost = true,
                    }
                }
            }
        }

        if let Some(winner) = winner {
            Ok(winner)
        } else if let Some(error) = last_error {
            Err(error)
        } else if timed_out {
            let seconds = timeout_seconds.unwrap_or(0);
            Err(SolverError::Other(format!(
                "solver timed out after {seconds} second(s)"
            )))
        } else {
            Err(SolverError::Other("swarm produced no result".to_string()))
        }
    }

    /// swarm 收口。
    ///
    /// **有胜者**（`keep` 非空）：删掉竞速落败的 attempt，只留获胜者。
    ///
    /// **无胜者**（超时 / 全部 attempt 失败）：不删，只把还在跑的行标记
    /// cancelled。否则一次失败的任务在 UI 里会"没有任何 worker 记录"，
    /// 排查时无从下手。
    ///
    /// 删除时序上安全：本函数在胜者出现的同一轮循环里调用，落败 attempt 的
    /// future 尚未被 poll 到完成，也就还没走到各自的 commit；随后函数 break、
    /// future 被 drop，`kill_on_drop` 兜底杀进程，不会有事后再把删掉的行
    /// 写回来。`worker_runs` 无外键指向它，删除不牵连别表。
    fn settle_swarm_losers(
        &self,
        spawned: &[(String, models::worker::WorkerRuntimeType)],
        keep: &str,
    ) {
        if keep.is_empty() {
            self.settle_swarm_losers_as_cancelled(spawned);
            return;
        }
        let mut dropped = 0_usize;
        for (worker_run_id, _engine) in spawned {
            if worker_run_id == keep {
                continue;
            }
            match self.repository().delete_worker_run(worker_run_id) {
                Ok(true) => dropped += 1,
                Ok(false) => {}
                // 删失败不能静默：那会在 UI 里留下一个永远不会结束的幽灵行。
                Err(error) => tracing::error!(
                    %error,
                    worker_run_id = %worker_run_id,
                    "failed to drop swarm loser; a stale row may remain"
                ),
            }
        }
        if dropped > 0 {
            tracing::info!(
                dropped_attempts = dropped,
                winner = %keep,
                "swarm settled: losing attempts dropped, winner kept"
            );
        }
    }

    /// 无胜者时的收口：保留现场，只把仍在跑/待跑的 attempt 标记 cancelled。
    fn settle_swarm_losers_as_cancelled(
        &self,
        spawned: &[(String, models::worker::WorkerRuntimeType)],
    ) {
        for (worker_run_id, _engine) in spawned {
            if let Ok(Some(mut run)) = self.repository().get_worker_run(worker_run_id)
                && matches!(
                    run.status,
                    models::worker::WorkerRunStatus::Running
                        | models::worker::WorkerRunStatus::Pending
                )
            {
                run.finish(
                    models::worker::WorkerRunStatus::Cancelled,
                    Some("swarm attempt did not win; settled as cancelled".to_string()),
                );
                // 落库失败不能静默：那会留下本函数要消除的幽灵 "running" 行。
                if let Err(error) = self.repository().upsert_worker_run(&run) {
                    tracing::error!(
                        %error,
                        worker_run_id = %worker_run_id,
                        "failed to settle swarm loser; a stale running row may remain"
                    );
                }
            }
        }
    }

    /// 单个 swarm attempt：钉定引擎（config.worker_runtime）+ 独立 MCP grant
    /// （distinct worker_run_id）+ solve；grant 用完即撤。返回 (worker_run_id, result)。
    async fn swarm_attempt(
        &self,
        base: &SolverContext,
        engine: models::worker::WorkerRuntimeType,
        worker_run_id: String,
        solver: &Arc<dyn agents::solver::BaseSolver>,
    ) -> (String, Result<SolverResult, SolverError>) {
        let mut context = base.clone();
        context.config.insert(
            "worker_runtime".to_string(),
            Value::String(engine.as_str().to_string()),
        );
        let grant = match self.issue_worker_mcp_for(&context, &worker_run_id) {
            Ok(Some(binding)) => {
                let grant_id = binding.grant_id.clone();
                context.worker_mcp = Some(binding);
                Some(grant_id)
            }
            Ok(None) => None,
            Err(error) => {
                return (
                    worker_run_id,
                    Err(SolverError::Other(format!("worker grant failed: {error}"))),
                );
            }
        };
        let result = solver.solve(context).await;
        if let Some(grant) = grant
            && let Some(mcp) = self.worker_mcp()
        {
            let _ = mcp.revoke_worker_grant(&grant);
        }
        (worker_run_id, result)
    }

    fn issue_worker_mcp_for(
        &self,
        context: &SolverContext,
        worker_run_id: &str,
    ) -> Result<Option<WorkerMcpBinding>, EngineError> {
        let Some(mcp) = self.worker_mcp() else {
            return Ok(None);
        };
        let worker_id = context
            .config
            .get("worker_id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("coordinator-worker")
            .to_string();
        let artifact_dir = engines::domains::artifact_dir_for(context)
            .join("worker")
            .join(&worker_run_id)
            .join("artifacts");
        // 外部 CLI 的运行时状态（CODEX_HOME）必须在 worker 的 cwd **之外**。
        // worker 的 cwd 就是 mission 的 `artifacts/`，而 CODEX_HOME 一次能铺
        // 五千多个文件（skills / sessions / sqlite 状态）。写进 cwd 里面，
        // worker 一列举工作目录就遍历到自己的运行时状态——实测把步预算全烧
        // 在"这个巨大的 worker/ 目录是什么"上，最终没产出任何结果。
        let config_dir = Self::mission_worker_home_dir(context).join(&worker_run_id);
        let allowlist = context
            .config
            .get("worker_tool_allowlist")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .filter(|value| !value.trim().is_empty())
                    .collect::<Vec<_>>()
            });
        // Bootstrap 预选工具集（visible_tool_ids）作为**默认**收窄（fail-closed）：
        // 仅当 operator 没有显式设 `worker_tool_allowlist` 时生效——worker 这一轮
        // 只能执行意图相关的 catalog 工具，从根上减少误调用与上下文占用。operator
        // 显式 allowlist 优先、不被覆盖；skill_load 解锁的模块仍可在此基础上
        // expand。bootstrap 未注入（catalog 探测失败）时保持原 allowlist 不变。
        let allowlist = if allowlist.is_none() {
            context
                .config
                .get(models::CONFIG_VISIBLE_TOOL_IDS)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .filter(|value| !value.trim().is_empty())
                        .collect::<Vec<_>>()
                })
                .filter(|tools| !tools.is_empty())
                .or(allowlist)
        } else {
            allowlist
        };
        let scope = engines::broker::ExecutionScope {
            project_id: Some(context.project_id.clone()),
            mission_id: context.mission_id.clone(),
            run_id: Some(context.run_id.clone()),
            task_id: Some(context.task_id.clone()),
            branch_id: context.branch_id.clone(),
            intent_id: context.intent.as_ref().map(|intent| intent.id.to_string()),
            worker_id: Some(worker_id),
            worker_run_id: Some(worker_run_id.to_string()),
            artifact_dir: Some(artifact_dir),
        };
        let credentials = mcp
            .issue_worker_grant(engines::broker::mcp::WorkerGrantSpec::new(scope, allowlist))
            .map_err(EngineError::Value)?;
        Ok(Some(WorkerMcpBinding {
            endpoint: mcp.endpoint().to_string(),
            grant_id: credentials.grant_id,
            bearer_token: credentials.bearer_token,
            worker_run_id: worker_run_id.to_string(),
            config_dir,
        }))
    }

    /// worker runtime 的状态根目录：`<mission>/worker-home`。
    ///
    /// 优先用 workspace 自己声明的 `mission_workspace_path`；取不到时退回
    /// `artifact_dir` 的上一级（约定 `artifact_dir` 就叫 `artifacts`）。
    /// 这里只做路径推导，真正建目录由 `mission_workspace::ensure_*` 和 adapter
    /// 侧负责，避免在派发热路径上引入额外 IO。
    fn mission_worker_home_dir(context: &SolverContext) -> std::path::PathBuf {
        let from_workspace = context
            .config
            .get("mission_workspace_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from);
        let from_artifact_parent = engines::domains::artifact_dir_for(context)
            .parent()
            .map(std::path::Path::to_path_buf);
        let resolved = from_workspace
            .or(from_artifact_parent)
            .unwrap_or_else(|| std::path::PathBuf::from("data").join("worker-home"))
            .join("worker-home");
        // 绝对化：worker 子进程的 cwd 未必等于 api 进程 cwd。若这里是相对路径，
        // codex 等 CLI 拿 CODEX_HOME 去解析会落到"不存在的路径"而起不来（实测
        // codex failed：CODEX_HOME points to "data\worker-home\..." does not
        // exist）。绝对路径与 cwd 无关，建目录与读取才一致。
        std::path::absolute(&resolved).unwrap_or(resolved)
    }

    fn commit_solver_success(
        &self,
        project: &Project,
        mission: &Mission,
        run: &AuditRun,
        task: &mut AgentTask,
        intent: &mut Intent,
        mut result: SolverResult,
    ) -> Result<(), EngineError> {
        let task_id = task.id.clone();
        let branch_id = task.branch_id.clone();
        for invocation in &mut result.tool_invocations {
            invocation.project_id = Some(project.id.clone());
            invocation.mission_id = Some(mission.id.clone());
            invocation.branch_id = branch_id.clone();
            invocation.run_id = Some(run.id.clone());
            invocation.task_id = Some(task_id.clone());
        }
        for fact in &mut result.new_facts {
            fact.project_id = project.id.clone();
            fact.mission_id = Some(mission.id.clone());
            fact.branch_id = branch_id.clone();
            fact.run_id = Some(run.id.clone());
            fact.produced_by_task_id = Some(task_id.clone());
        }
        for evidence in &mut result.new_evidence {
            evidence.project_id = project.id.clone();
            evidence.mission_id = Some(mission.id.clone());
            evidence.branch_id = branch_id.clone();
            evidence.run_id = Some(run.id.clone());
            evidence.produced_by_task_id = Some(task_id.clone());
        }
        for finding in &mut result.new_findings {
            finding.project_id = project.id.clone();
            finding.mission_id = Some(mission.id.clone());
            finding.branch_id = branch_id.clone();
            finding.run_id = Some(run.id.clone());
            finding.produced_by_task_id = Some(task_id.clone());
        }
        for proposed in &mut result.proposed_intents {
            proposed.project_id = project.id.clone();
            proposed.mission_id = Some(mission.id.clone());
            proposed.branch_id = branch_id.clone();
            proposed.run_id = Some(run.id.clone());
        }
        // 外部 Worker 审计记录（独立概念，非 ToolInvocation）：先落
        // worker 记录再提交 solver 结果，保证 fact 引用的 worker_run_id
        // 先于引用者存在。
        self.commit_worker_records(
            project,
            run,
            task_id.as_str(),
            branch_id.as_ref(),
            Some(&mission.id),
            &result.worker_runs,
            &result.worker_invocations,
        )?;
        enrich_solver_result_provenance(&mut result);
        let known_fact_ids: HashSet<String> = self
            .repository()
            .list_facts(project.id.as_str())?
            .into_iter()
            .map(|item| item.id.to_string())
            .collect();
        let known_evidence_ids: HashSet<String> = self
            .repository()
            .list_evidence(project.id.as_str())?
            .into_iter()
            .map(|item| item.id.to_string())
            .collect();
        validate_solver_references(&result, &known_fact_ids, &known_evidence_ids)?;
        task.status = TaskStatus::Succeeded;
        task.finished_at = Some(utcnow());
        task.produced_fact_ids = result
            .new_facts
            .iter()
            .map(|item| item.id.to_string())
            .collect();
        task.produced_evidence_ids = result
            .new_evidence
            .iter()
            .map(|item| item.id.to_string())
            .collect();
        task.produced_finding_ids = result
            .new_findings
            .iter()
            .map(|item| item.id.to_string())
            .collect();
        task.tool_invocation_ids = result
            .tool_invocations
            .iter()
            .map(|item| item.id.to_string())
            .collect();
        intent.status = IntentStatus::Resolved;
        intent.updated_at = utcnow();
        let event = AuditEvent::new(
            project.id.clone(),
            AuditEventType::SolverCompleted,
            "manager".to_string(),
            "求解器已完成".to_string(),
        );
        self.repository().commit_solver_result(
            task,
            intent,
            &result.tool_invocations,
            &result.new_facts,
            &result.new_evidence,
            &result.new_findings,
            &result.proposed_intents,
            std::slice::from_ref(&event),
        )?;
        self.announce_solver_commit(
            task.mission_id.as_ref().map(models::MissionId::as_str),
            std::slice::from_ref(&event),
            &result.new_findings,
        );
        self.repository().increment_run_steps(
            run.id.as_str(),
            i64::try_from(task.tool_invocation_ids.len()).unwrap_or(i64::MAX),
        )?;

        // 终稿回给用户（settlement 契约）：外部 worker 本轮最后一句
        // 结论必须作为 WorkerSummary 叙事事件落库。缺了这一步，worker 跑得
        // 再成功，用户眼前也只有一堆 Progress 痕迹、看不到任何产出——
        // "输入你好却什么都没回来"正是这么来的。
        // 泳道归位由前端按 task_id 判定，这里不必猜 agent 名。
        if let Some(summary) = result
            .notes
            .as_deref()
            .filter(|text| !text.trim().is_empty())
        {
            self.record_narrative_safe(NarrativeDraft {
                run_id: Some(&run.id),
                mission_id: Some(&mission.id),
                branch_id: branch_id.as_ref(),
                task_id: Some(&task_id),
                ..NarrativeDraft::new(
                    &project.id,
                    "worker",
                    AgentNarrativeEventKind::WorkerSummary,
                    summary,
                )
            });
        }
        Ok(())
    }

    /// 持久化外部 Worker 审计记录（独立于 `ToolInvocation` 的审计概念）：
    /// provenance 强制归位（mission/run/branch/task），幂等 upsert。
    fn commit_worker_records(
        &self,
        project: &Project,
        run: &AuditRun,
        task_id: &str,
        branch_id: Option<&models::BranchId>,
        mission_id: Option<&models::MissionId>,
        worker_runs: &[models::worker::WorkerRun],
        worker_invocations: &[models::worker::WorkerInvocation],
    ) -> Result<(), EngineError> {
        for worker_run in worker_runs {
            let mut record = worker_run.clone();
            record.project_id = project.id.clone();
            record.mission_id = mission_id.cloned();
            record.branch_id = branch_id.cloned();
            record.run_id = Some(run.id.clone());
            record.task_id = Some(models::TaskId::new(task_id.to_string()));
            self.repository().upsert_worker_run(&record)?;
        }
        for worker_invocation in worker_invocations {
            let mut record = worker_invocation.clone();
            record.project_id = Some(project.id.clone());
            self.repository().upsert_worker_invocation(&record)?;
        }
        Ok(())
    }

    fn commit_solver_failure(
        &self,
        project: &Project,
        run: &AuditRun,
        task: &mut AgentTask,
        intent: &mut Intent,
        error: SolverError,
    ) -> Result<(), EngineError> {
        let message = error.to_string();
        let (mut invocations, worker_runs, worker_invocations) = match error {
            SolverError::Execution(error) => (
                error.tool_invocations,
                error.worker_runs,
                error.worker_invocations,
            ),
            SolverError::Other(_) => (Vec::new(), Vec::new(), Vec::new()),
        };
        // 失败路径同样保留外部 Worker 审计链（"我们派发了 worker 且它
        // 失败了"的事实不丢）。
        self.commit_worker_records(
            project,
            run,
            task.id.as_str(),
            task.branch_id.as_ref(),
            task.mission_id.as_ref(),
            &worker_runs,
            &worker_invocations,
        )?;
        for invocation in &mut invocations {
            invocation.project_id = Some(project.id.clone());
            invocation.mission_id = task.mission_id.clone();
            invocation.branch_id = task.branch_id.clone();
            invocation.run_id = Some(run.id.clone());
            invocation.task_id = Some(task.id.clone());
        }
        task.status = TaskStatus::Failed;
        task.error = Some(message.clone());
        task.finished_at = Some(utcnow());
        task.tool_invocation_ids = invocations.iter().map(|item| item.id.to_string()).collect();
        intent.status = IntentStatus::Dismissed;
        intent.updated_at = utcnow();
        let event = AuditEvent::new(
            project.id.clone(),
            AuditEventType::SolverFailed,
            "manager".to_string(),
            "求解器失败".to_string(),
        );
        self.repository().commit_solver_result(
            task,
            intent,
            &invocations,
            &[],
            &[],
            &[],
            &[],
            std::slice::from_ref(&event),
        )?;
        self.announce_solver_commit(
            task.mission_id.as_ref().map(models::MissionId::as_str),
            std::slice::from_ref(&event),
            &[],
        );
        self.repository().increment_run_steps(
            run.id.as_str(),
            i64::try_from(invocations.len()).unwrap_or(i64::MAX),
        )?;

        // 收尾（settlement）：失败/超时的 run 也必须回给用户一句结论。
        // wrapup 契约——"这句话会作为本次运行的结果展示"。这里
        // 没有 SDK 帮我们注入收尾提示词（外部 CLI 已被杀），所以用 adapter
        // 在进程死前抢到的终稿；抢不到才退化成一句确定性的事实陈述，
        // 绝不静默：用户眼前可以是一句"没做完"，不能是空白。
        let settlement = worker_invocations
            .iter()
            .rev()
            .find_map(|invocation| invocation.summary.as_deref())
            .filter(|text| !text.trim().is_empty())
            .map_or_else(
                || format!("Worker 未产出结论即终止：{message}"),
                |text| text.to_string(),
            );
        self.record_narrative_safe(NarrativeDraft {
            run_id: Some(&run.id),
            mission_id: task.mission_id.as_ref(),
            branch_id: task.branch_id.as_ref(),
            task_id: Some(&task.id),
            ..NarrativeDraft::new(
                &project.id,
                "worker",
                AgentNarrativeEventKind::WorkerSummary,
                &settlement,
            )
        });

        // Reflector 复盘：确定性失败分类持久化供知识沉淀，并留下人读的
        // 失败分析与教训注记（best-effort,绝不打断失败落盘路径）。
        let report = self.reflector.reflect_failure(&ReflectFailureInput {
            project_id: project.id.as_str(),
            run_id: run.id.as_str(),
            task: Some(task),
            tool_invocations: &invocations,
            observations: &[],
            error: Some(&message),
        });
        let _ = self.repository().add_reflector_report(&report);
        self.record_narrative_safe(NarrativeDraft {
            run_id: Some(&run.id),
            mission_id: task.mission_id.as_ref(),
            branch_id: task.branch_id.as_ref(),
            task_id: Some(&task.id),
            metadata: Some(Map::from_iter([(
                "failure_type".to_string(),
                Value::String(report.failure_type.as_str().to_string()),
            )])),
            ..NarrativeDraft::new(
                &project.id,
                "reflector",
                AgentNarrativeEventKind::FailureAnalysis,
                &report.root_cause_summary,
            )
        });
        if !report.lessons.is_empty() {
            self.record_narrative_safe(NarrativeDraft {
                run_id: Some(&run.id),
                mission_id: task.mission_id.as_ref(),
                branch_id: task.branch_id.as_ref(),
                task_id: Some(&task.id),
                ..NarrativeDraft::new(
                    &project.id,
                    "reflector",
                    AgentNarrativeEventKind::ReflectorNote,
                    &report.lessons.join(" "),
                )
            });
        }
        Ok(())
    }

    fn update_branch_after_task(
        &self,
        project: &Project,
        run: &AuditRun,
        mut branch: Branch,
        task: &AgentTask,
    ) -> Result<(), EngineError> {
        branch.steps_used = branch
            .steps_used
            .saturating_add(1)
            .saturating_add(i64::try_from(task.tool_invocation_ids.len()).unwrap_or(i64::MAX));
        branch
            .related_fact_ids
            .extend(task.produced_fact_ids.iter().cloned());
        branch
            .related_evidence_ids
            .extend(task.produced_evidence_ids.iter().cloned());
        branch
            .related_finding_ids
            .extend(task.produced_finding_ids.iter().cloned());
        branch
            .related_tool_invocation_ids
            .extend(task.tool_invocation_ids.iter().cloned());
        branch.related_fact_ids.sort();
        branch.related_fact_ids.dedup();
        branch.related_evidence_ids.sort();
        branch.related_evidence_ids.dedup();
        branch.related_finding_ids.sort();
        branch.related_finding_ids.dedup();
        branch.related_tool_invocation_ids.sort();
        branch.related_tool_invocation_ids.dedup();
        if task.status == TaskStatus::Failed {
            branch.status = BranchStatus::Blocked;
        } else if !task.produced_finding_ids.is_empty() {
            branch.status = BranchStatus::Succeeded;
        } else if branch.steps_used >= branch.budget_steps {
            branch.status = BranchStatus::Blocked;
        } else {
            branch.status = BranchStatus::Active;
        }
        branch.updated_at = utcnow();
        self.repository().update_branch(&branch)?;
        let summary = if task.status == TaskStatus::Succeeded {
            format!(
                "求解器产出 {} 条发现、{} 条证据",
                task.produced_finding_ids.len(),
                task.produced_evidence_ids.len()
            )
        } else {
            task.error
                .clone()
                .unwrap_or_else(|| "求解器失败".to_string())
        };
        if task.status == TaskStatus::Succeeded {
            self.record_narrative_safe(NarrativeDraft {
                run_id: Some(&run.id),
                mission_id: Some(&branch.mission_id),
                branch_id: Some(&branch.id),
                task_id: Some(&task.id),
                ..NarrativeDraft::new(
                    &project.id,
                    "manager",
                    AgentNarrativeEventKind::Progress,
                    &summary,
                )
            });
        }
        self.record_branch_observation(
            project,
            run,
            &branch,
            ObservationType::ToolResult,
            summary,
            Map::new(),
        )?;
        Ok(())
    }

    async fn review_and_evaluate(
        &self,
        mission: &Mission,
        project: &Project,
        run: &AuditRun,
    ) -> Result<TerminationAssessment, EngineError> {
        let mut findings: Vec<Finding> = self
            .repository()
            .list_findings(project.id.as_str())?
            .into_iter()
            .filter(|finding| finding.run_id.as_ref() == Some(&run.id))
            .collect();
        let evidence = self.repository().list_evidence(project.id.as_str())?;
        let facts = self.repository().list_facts(project.id.as_str())?;
        let report = self.observer.review(&ReviewInput {
            run_id: &run.id,
            findings: &findings,
            evidence: &evidence,
            facts: &facts,
        });
        for review in &report.reviews {
            if let Some(finding) = findings
                .iter_mut()
                .find(|item| item.id.as_str() == review.finding_id)
            {
                let previous_status = finding.status;
                let previous_severity = finding.severity;
                let mut status = agents::observer::Observer::verdict_to_status(review.verdict);
                if status == FindingStatus::Confirmed {
                    let decision = FindingVerificationService::check_confirmation(
                        finding,
                        &evidence,
                        &self
                            .repository()
                            .list_tool_invocations(Some(project.id.as_str()))?,
                    );
                    if !decision.allowed() {
                        status = FindingStatus::NeedsReview;
                    } else if let Some(product) = decision.product_verification() {
                        finding.review.insert(
                            "product_verification".to_string(),
                            serde_json::to_value(product).unwrap_or(Value::Null),
                        );
                    }
                }
                finding.status = status;
                finding.review.insert(
                    "observer_reasons".to_string(),
                    Value::Array(review.reasons.iter().cloned().map(Value::String).collect()),
                );
                finding.review.insert(
                    "observer_missing".to_string(),
                    Value::Array(review.missing.iter().cloned().map(Value::String).collect()),
                );
                finding.updated_at = utcnow();
                let stored = self.repository().update_finding(finding)?;
                // 仅真实状态/严重级迁移才值得打断用户（Python
                // NotifyingRepository.update_finding）。
                if stored.status != previous_status || stored.severity != previous_severity {
                    let notification = crate::notifications::notification_for_finding(&stored);
                    self.publish_notification(&notification);
                }
            }
        }
        self.record_event_safe(EventDraft {
            run_id: Some(&run.id),
            status: Some(if report.ready_to_report {
                "ready"
            } else {
                "needs_review"
            }),
            data: Some(Map::from_iter([(
                String::from("summary"),
                Value::String(report.summary.clone()),
            )])),
            ..EventDraft::new(
                &project.id,
                AuditEventType::ObserverReviewed,
                "observer",
                "观察者复检完成",
            )
        })
        .await;
        self.record_narrative_safe(NarrativeDraft {
            run_id: Some(&run.id),
            mission_id: Some(&mission.id),
            metadata: Some(Map::from_iter([(
                "ready_to_report".to_string(),
                Value::Bool(report.ready_to_report),
            )])),
            ..NarrativeDraft::new(
                &project.id,
                "observer",
                AgentNarrativeEventKind::ObserverNote,
                &report.summary,
            )
        });

        let tasks = self.repository().list_tasks(run.id.as_str())?;
        let branches = self.repository().list_branches(
            Some(project.id.as_str()),
            Some(mission.id.as_str()),
            Some(run.id.as_str()),
        )?;
        let intents = self.repository().list_intents(project.id.as_str())?;
        let observations = self
            .repository()
            .list_observations(project.id.as_str(), Some(run.id.as_str()))?;
        let leases = self
            .repository()
            .list_worker_leases(project.id.as_str(), Some(run.id.as_str()))?;
        let gates = self
            .repository()
            .list_decision_gates(Some(project.id.as_str()), Some(run.id.as_str()))?;
        let assessment = self.termination_evaluator.evaluate(&TerminationInput {
            project_id: &project.id,
            run,
            intents: &intents,
            findings: &findings,
            worker_leases: &leases,
            decision_gates: &gates,
            observations: &observations,
            mission: Some(mission),
            tasks: &tasks,
            branches: &branches,
            evidence: &evidence,
        });
        let saved = self.repository().add_termination_assessment(&assessment)?;
        Ok(saved)
    }

    fn normalize_exhausted_branches(&self, run_id: &RunId) -> Result<(), EngineError> {
        let branches = self
            .repository()
            .list_branches(None, None, Some(run_id.as_str()))?;
        for mut branch in branches {
            if matches!(branch.status, BranchStatus::Proposed | BranchStatus::Active)
                && branch.steps_used >= branch.budget_steps
            {
                branch.status = BranchStatus::Blocked;
                branch
                    .metadata
                    .insert("budget_exhausted".to_string(), Value::Bool(true));
                branch.updated_at = utcnow();
                self.repository().update_branch(&branch)?;
            }
        }
        Ok(())
    }

    fn apply_termination_status(
        &self,
        run: &AuditRun,
        assessment: &TerminationAssessment,
    ) -> Result<(), EngineError> {
        let mut next = run.clone();
        next.status = match assessment.status {
            TerminationStatus::Continue => RunStatus::Running,
            TerminationStatus::Pause => RunStatus::Paused,
            TerminationStatus::NeedsHumanDecision => RunStatus::WaitingForDecision,
            TerminationStatus::Complete => RunStatus::Reviewing,
        };
        next.note = assessment.reasons.first().cloned();
        next.updated_at = utcnow();
        self.repository().update_run(&next)?;
        self.sync_mission_status_from_run(&next)?;
        let reason = assessment
            .reasons
            .first()
            .map_or("no reason recorded", String::as_str);
        self.record_narrative_safe(NarrativeDraft {
            run_id: Some(&run.id),
            mission_id: run.mission_id.as_ref(),
            ..NarrativeDraft::new(
                &run.project_id,
                "termination_evaluator",
                AgentNarrativeEventKind::NextAction,
                &format!("Run {}: {reason}", next.status.as_str()),
            )
        });
        Ok(())
    }

    async fn complete_run(&self, mission: &Mission, run: &AuditRun) -> Result<(), EngineError> {
        let mut next = run.clone();
        next.status = RunStatus::Completed;
        next.finished_at = Some(utcnow());
        next.note = Some("closure gate approved completion".to_string());
        next.updated_at = utcnow();
        self.repository().update_run(&next)?;
        self.sync_mission_status_from_run(&next)?;

        // 收口时产出终段轨迹摘要（确定性渲染）并留一条人读推理摘要。
        let observations = self
            .repository()
            .list_observations(mission.project_id.as_str(), Some(run.id.as_str()))?;
        let tool_invocations: Vec<ToolInvocation> = self
            .repository()
            .list_tool_invocations(Some(mission.project_id.as_str()))?
            .into_iter()
            .filter(|item| item.run_id.as_ref() == Some(&run.id) || item.run_id.is_none())
            .collect();
        let previous = self
            .repository()
            .list_trajectory_summaries(mission.project_id.as_str(), Some(run.id.as_str()), None)?
            .into_iter()
            .last();
        let summary = self.trajectory_summarizer.summarize(&SummarizeInput {
            project_id: &mission.project_id,
            run_id: &run.id,
            observations: &observations,
            tool_invocations: &tool_invocations,
            previous: previous.as_ref(),
            mission_id: Some(&mission.id),
            branch_id: None,
            task_id: None,
            token_budget: None,
        });
        let summary = self.repository().add_trajectory_summary(&summary)?;
        self.record_narrative_safe(NarrativeDraft {
            run_id: Some(&run.id),
            mission_id: Some(&mission.id),
            metadata: Some(Map::from_iter([
                (
                    "trajectory_summary_id".to_string(),
                    Value::String(summary.id.as_str().to_string()),
                ),
                (
                    "segment_index".to_string(),
                    Value::from(summary.segment_index),
                ),
            ])),
            ..NarrativeDraft::new(
                &mission.project_id,
                "trajectory_summarizer",
                AgentNarrativeEventKind::ReasoningSummary,
                &summary.summary,
            )
        });

        self.record_event_safe(EventDraft {
            run_id: Some(&next.id),
            status: Some(RunStatus::Completed.as_str()),
            ..EventDraft::new(
                &mission.project_id,
                AuditEventType::RunCompleted,
                "manager",
                "任务已完成",
            )
        })
        .await;
        Ok(())
    }

    fn pause_run_with_note(&self, run: &AuditRun, note: &str) -> Result<(), EngineError> {
        let mut next = run.clone();
        next.status = RunStatus::Paused;
        next.finished_at = None;
        next.note = Some(note.to_string());
        next.updated_at = utcnow();
        self.repository().update_run(&next)?;
        self.sync_mission_status_from_run(&next)?;
        Ok(())
    }

    fn closure_refusal_message(outcome: &crate::closure::ClosureChainOutcome) -> String {
        Self::closure_refusal_note(outcome)
    }

    fn record_branch_observation(
        &self,
        project: &Project,
        run: &AuditRun,
        branch: &Branch,
        observation_type: ObservationType,
        summary: String,
        data: Map<String, Value>,
    ) -> Result<(), EngineError> {
        let mut observation = Observation::new(project.id.clone(), run.id.clone(), summary);
        observation.mission_id = Some(branch.mission_id.clone());
        observation.branch_id = Some(branch.id.clone());
        observation.observation_type = observation_type;
        observation.source = "manager".to_string();
        observation.actor = Some("manager".to_string());
        observation.data = data;
        self.repository().add_observation(&observation)?;
        Ok(())
    }
}

/// A solver that produced no domain output did not complete useful work.
/// Persist it through the failure path so the task/branch/run state cannot be
/// presented as a successful result merely because the Rust future returned
/// `Ok(SolverResult)`.
///
/// Two shapes count as "no domain output":
/// 1. **空转**——零 fact / 零证据 / 零发现 / 零后续意图，**且一次工具都没调**。
///    实测成因：pi 的 bash 工具不可用（没装 Git for Windows，PATH 里只有 WSL
///    占位 shim），它什么都执行不了，却仍然以 exit 0 收场。少了这一条，这种
///    attempt 会被记成 `succeeded`，甚至在竞速里靠"没报错"胜出。
/// 2. **全败**——有工具调用，但整条 trace 没有一个成功。
///
/// 注意：只要产出过任何一项（fact / evidence / finding / 后续意图）或有过一次
/// 成功调用，就不是失败——"查过了，这里没有"是合法的审计结论，不能一概判败。
fn solver_result_is_failure(result: &SolverResult) -> bool {
    let produced_nothing = result.new_facts.is_empty()
        && result.new_evidence.is_empty()
        && result.new_findings.is_empty()
        && result.proposed_intents.is_empty();
    if !produced_nothing {
        return false;
    }
    if result.tool_invocations.is_empty() {
        return true;
    }
    result
        .tool_invocations
        .iter()
        .all(|invocation| invocation.status != models::ToolStatus::Ok)
}

/// solver 长跑期间的 lease 心跳周期（秒）。config `worker_lease_heartbeat_seconds`
/// 可钉定（clamp 到 `1..=lease_seconds`，便于测试）；否则取 lease 的 1/3
/// （clamp `1..=120`），给约 3 次保活机会。
fn resolve_lease_heartbeat_seconds(run_config: &Map<String, Value>, lease_seconds: i64) -> u64 {
    let seconds = run_config
        .get("worker_lease_heartbeat_seconds")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .map_or_else(
            || (lease_seconds / 3).clamp(1, 120),
            |configured| configured.clamp(1, lease_seconds.max(1)),
        );
    u64::try_from(seconds).unwrap_or(1)
}

fn branch_concurrency_limit(run: &AuditRun, requested: Option<i64>, branch_count: usize) -> usize {
    if branch_count == 0 {
        return 0;
    }
    let configured = requested
        .or_else(|| run_config_i64(&run.config, "max_concurrent_branches"))
        .unwrap_or(1)
        .max(1);
    usize::try_from(configured)
        .unwrap_or(BRANCH_RUNTIME_MAX_CONCURRENCY)
        .min(BRANCH_RUNTIME_MAX_CONCURRENCY)
        .min(branch_count)
        .max(1)
}

/// Intent 级 worker 并发（swarm 宽度）：同一意图同时派几个不同引擎的 worker。
/// 缺省 4，钳制到 [1, 8]；派发处再按当前可用引擎数收窄。
fn intent_worker_concurrency(run: &AuditRun) -> usize {
    let configured = run_config_i64(&run.config, "intent_worker_concurrency")
        .unwrap_or(4)
        .clamp(1, 8);
    usize::try_from(configured).unwrap_or(4)
}

/// swarm 宽度按池大小削顶（纯函数，便于单测"不死锁"不变式）。
///
/// 一个 attempt 就要一个许可，所以 width 绝不允许超过池大小：否则
/// `acquire_many(width)` 在池小于 width 时永远等不到齐，整个 run 死锁。
/// `pool_limit = None` 表示该 run 没注册池（不在 `run_branch_runtime`
/// 作用域内派发），此时不限。
fn clamp_width_to_pool(width: usize, pool_limit: Option<usize>) -> usize {
    match pool_limit {
        Some(limit) => width.min(limit.max(1)),
        None => width,
    }
}

/// 整个 mission 同时在跑的外部 worker 尝试数上限（跨 branch 共享的池大小）。
///
/// 与 `intent_worker_concurrency` 是两个正交的旋钮：后者决定**同一个 intent
/// 派几个 runtime 竞速**，前者决定**整个 run 同时允许几个 attempt 在跑**。
/// 缺省 4，钳制到 [1, [`WORKER_POOL_MAX_CONCURRENCY`]]。
fn max_concurrent_workers(run: &AuditRun) -> usize {
    let configured = run_config_i64(&run.config, "max_concurrent_workers")
        .unwrap_or(DEFAULT_MAX_CONCURRENT_WORKERS)
        .clamp(1, WORKER_POOL_MAX_CONCURRENCY);
    usize::try_from(configured).unwrap_or_else(|_| {
        usize::try_from(DEFAULT_MAX_CONCURRENT_WORKERS).unwrap_or(1)
    })
}

fn run_config_i64(config: &Map<String, Value>, key: &str) -> Option<i64> {
    match config.get(key) {
        Some(Value::Number(number)) if number.is_i64() || number.is_u64() => number
            .as_i64()
            .or_else(|| number.as_u64().and_then(|value| i64::try_from(value).ok())),
        _ => None,
    }
}

/// Fill provenance links only when they are unambiguous within one solver
/// result, mirroring Python `_enrich_solver_result_provenance`. A missing or
/// unreadable artifact deliberately remains unbound so the evidence gate can
/// downgrade the finding instead of manufacturing a digest.
fn enrich_solver_result_provenance(result: &mut SolverResult) {
    let successful: Vec<&ToolInvocation> = result
        .tool_invocations
        .iter()
        .filter(|invocation| invocation.status == models::ToolStatus::Ok)
        .collect();
    for evidence in &mut result.new_evidence {
        let invocation = evidence
            .produced_by_tool_invocation_id
            .as_ref()
            .and_then(|id| result.tool_invocations.iter().find(|item| &item.id == id))
            .or_else(|| {
                if evidence.produced_by_tool_invocation_id.is_none() && successful.len() == 1 {
                    successful.first().copied()
                } else {
                    None
                }
            });
        if evidence.produced_by_tool_invocation_id.is_none()
            && let Some(invocation) = invocation
        {
            evidence.produced_by_tool_invocation_id = Some(invocation.id.clone());
        }
        if let Some(invocation) = invocation
            && evidence.evidence_path.is_none()
        {
            evidence.evidence_path = invocation.artifact_paths.first().cloned();
        }
        if evidence.fingerprint.is_none()
            && let Some(path) = evidence.evidence_path.as_deref()
        {
            evidence.fingerprint = std::fs::read(Path::new(path))
                .ok()
                .map(|bytes| Sha256Fingerprint::compute(&bytes).as_hex().to_string());
        }
    }
}

fn validate_solver_references(
    result: &SolverResult,
    known_fact_ids: &HashSet<String>,
    known_evidence_ids: &HashSet<String>,
) -> Result<(), EngineError> {
    let mut all_fact_ids = known_fact_ids.clone();
    all_fact_ids.extend(result.new_facts.iter().map(|item| item.id.to_string()));
    let mut evidence_ids = known_evidence_ids.clone();
    evidence_ids.extend(result.new_evidence.iter().map(|item| item.id.to_string()));
    for evidence in &result.new_evidence {
        if evidence
            .supports_fact_ids
            .iter()
            .any(|id| !all_fact_ids.contains(id))
            && !evidence.supports_fact_ids.is_empty()
        {
            return Err(EngineError::ReferenceValidationError(format!(
                "evidence {} references unknown fact",
                evidence.id
            )));
        }
    }
    for finding in &result.new_findings {
        if finding
            .evidence_ids
            .iter()
            .any(|id| !evidence_ids.contains(id))
            && !finding.evidence_ids.is_empty()
        {
            return Err(EngineError::ReferenceValidationError(format!(
                "finding {} references unknown evidence",
                finding.id
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use agents::solver::{BaseSolver, SolverRegistry};
    use models::domain::AuditDomain;
    use storage::Repository;

    use super::*;

    #[derive(Default)]
    struct ProbeCounters {
        active: AtomicUsize,
        peak: AtomicUsize,
    }

    struct ProbeActiveGuard {
        counters: Arc<ProbeCounters>,
    }

    impl Drop for ProbeActiveGuard {
        fn drop(&mut self) {
            self.counters.active.fetch_sub(1, Ordering::SeqCst);
        }
    }

    struct ProbeSolver {
        counters: Arc<ProbeCounters>,
        delay: Duration,
        failures: HashSet<String>,
        produce_evidence: bool,
        /// 完成前等待的并发进入数（确定性重叠证明：等待 active 达到该值，
        /// 若调度实际串行则有界超时后继续，不会挂死测试）。
        wait_for_active: Option<usize>,
    }

    impl ProbeSolver {
        fn new(counters: Arc<ProbeCounters>, delay: Duration) -> Self {
            Self {
                counters,
                delay,
                failures: HashSet::new(),
                produce_evidence: false,
                wait_for_active: None,
            }
        }

        fn failing_on(mut self, title: &str) -> Self {
            self.failures.insert(title.to_string());
            self
        }

        fn with_evidence(mut self) -> Self {
            self.produce_evidence = true;
            self
        }

        fn waiting_for_overlap(mut self, expected: usize) -> Self {
            self.wait_for_active = Some(expected);
            self
        }
    }

    #[async_trait::async_trait]
    impl BaseSolver for ProbeSolver {
        fn name(&self) -> &str {
            "web_recon"
        }

        fn audit_domains(&self) -> HashSet<AuditDomain> {
            [AuditDomain::WebRecon].into()
        }

        async fn solve(&self, context: SolverContext) -> Result<SolverResult, SolverError> {
            let current = self.counters.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.counters.peak.fetch_max(current, Ordering::SeqCst);
            let _guard = ProbeActiveGuard {
                counters: Arc::clone(&self.counters),
            };
            if let Some(expected) = self.wait_for_active {
                // 确定性重叠证明：等待第 expected 个分支同时进入 solve。
                // 有界等待，实际串行时超时后继续（由 peak 断言裁决）。
                for _ in 0..500 {
                    if self.counters.active.load(Ordering::SeqCst) >= expected {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
            tokio::time::sleep(self.delay).await;

            let title = context
                .intent
                .as_ref()
                .map_or_else(String::new, |intent| intent.title.clone());
            if self.failures.contains(&title) {
                return Err(SolverError::Other(format!("probe failure: {title}")));
            }

            let mut result = SolverResult::default();
            if self.produce_evidence {
                let mut invocation =
                    ToolInvocation::new("probe_tool".to_string(), format!("probe {title}"));
                invocation.output_summary = format!("probe output for {title}");
                let mut evidence = models::Evidence::new(
                    context.project_id.clone(),
                    models::EvidenceKind::ToolOutput,
                    format!("probe evidence for {title}"),
                );
                evidence.produced_by_tool_invocation_id = Some(invocation.id.clone());
                result.tool_invocations.push(invocation);
                result.new_evidence.push(evidence);
            }
            Ok(result)
        }
    }

    fn test_manager() -> (AuditManager, tempfile::TempDir, Project, Mission, AuditRun) {
        test_manager_with_registry(SolverRegistry::new())
    }

    fn test_manager_with_registry(
        registry: SolverRegistry,
    ) -> (AuditManager, tempfile::TempDir, Project, Mission, AuditRun) {
        let dir = tempfile::tempdir().expect("临时目录必须可创建");
        let repo =
            storage::SqliteRepository::open(dir.path().join("ev.sqlite3")).expect("库必须可打开");
        let project = Project::new(
            "branch-narratives-probe".to_string(),
            models::domain::AuditDomain::WebRecon,
        );
        repo.create_project(&project).expect("项目必须可创建");
        let mission = Mission::new(project.id.clone(), "probe goal".to_string());
        repo.create_mission(&mission).expect("Mission 必须可创建");
        let mut run = AuditRun::new(project.id.clone());
        run.mission_id = Some(mission.id.clone());
        repo.create_run(&run).expect("Run 必须可创建");

        let manager = AuditManager::new(
            std::sync::Arc::new(repo),
            registry,
            std::sync::Arc::new(crate::task_backend::InMemoryTaskBackend::default()),
        );
        (manager, dir, project, mission, run)
    }

    fn manager_with_probe(
        delay: Duration,
        configure: impl FnOnce(ProbeSolver) -> ProbeSolver,
    ) -> (
        AuditManager,
        tempfile::TempDir,
        Project,
        Mission,
        AuditRun,
        Arc<ProbeCounters>,
    ) {
        let counters = Arc::new(ProbeCounters::default());
        let mut registry = SolverRegistry::new();
        registry.register(Arc::new(configure(ProbeSolver::new(
            Arc::clone(&counters),
            delay,
        ))));
        let (manager, dir, mut project, mission, run) = test_manager_with_registry(registry);
        project
            .target
            .insert("url".to_string(), "https://example.test".to_string());
        manager
            .repository()
            .update_project(&project)
            .expect("项目目标必须可更新");
        (manager, dir, project, mission, run, counters)
    }

    fn probe_branches(
        manager: &AuditManager,
        project: &Project,
        mission: &Mission,
        run: &AuditRun,
        titles: &[&str],
    ) -> Vec<Branch> {
        titles
            .iter()
            .map(|title| {
                let mut branch = Branch::new(
                    project.id.clone(),
                    mission.id.clone(),
                    (*title).to_string(),
                    format!("probe hypothesis {title}"),
                );
                branch.run_id = Some(run.id.clone());
                branch.budget_steps = 8;
                branch.metadata.insert(
                    "branch_kind".to_string(),
                    Value::String("url.surface_mapping".to_string()),
                );
                manager
                    .repository()
                    .create_branch(&branch)
                    .expect("分支必须可创建")
            })
            .collect()
    }

    #[tokio::test]
    async fn branch_wave_respects_limit_and_runs_parallel() {
        // 确定性重叠证明：两个分支都等待 active==2 才完成——只有真实
        // 并发调度才能让双方同时进入 solve，串行执行会在有界等待后由
        // peak 断言失败。不依赖墙钟，避免环境抖动。
        let (manager, _dir, project, mission, run, counters) =
            manager_with_probe(Duration::from_millis(50), |solver| {
                solver.waiting_for_overlap(2)
            });
        let branches = probe_branches(&manager, &project, &mission, &run, &["a", "b", "c", "d"]);

        manager
            .run_branch_wave(&mission, &project, &run.id, branches, 2)
            .await
            .expect("并发分支波次必须完成");

        assert_eq!(counters.peak.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn branch_wave_limit_one_is_serial() {
        let (manager, _dir, project, mission, run, counters) =
            manager_with_probe(Duration::from_millis(50), |solver| solver);
        let branches = probe_branches(&manager, &project, &mission, &run, &["a", "b", "c"]);

        let started = Instant::now();
        manager
            .run_branch_wave(&mission, &project, &run.id, branches, 1)
            .await
            .expect("串行退化必须完成");

        assert_eq!(counters.peak.load(Ordering::SeqCst), 1);
        assert!(
            started.elapsed() >= Duration::from_millis(140),
            "max_concurrent_branches=1 必须退化为串行执行"
        );
    }

    #[tokio::test]
    async fn branch_wave_allows_branch_count_below_limit() {
        let (manager, _dir, project, mission, run, counters) =
            manager_with_probe(Duration::from_millis(30), |solver| solver);
        let branches = probe_branches(&manager, &project, &mission, &run, &["a", "b"]);

        manager
            .run_branch_wave(&mission, &project, &run.id, branches, 8)
            .await
            .expect("分支少于并发上限时必须正常完成");

        assert_eq!(counters.peak.load(Ordering::SeqCst), 2);
        assert_eq!(
            manager
                .repository()
                .list_tasks(run.id.as_str())
                .expect("任务必须可列出")
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn branch_wave_keeps_siblings_running_after_solver_failure() {
        let (manager, _dir, project, mission, run, counters) = manager_with_probe(
            Duration::from_millis(30),
            // b 显式失败；a/c 需要一条真实产出才算成功——否则空结果会被
            // `solver_result_is_failure` 判败，"只失败 b" 这个断言就失去意义。
            |solver| solver.failing_on("b").with_evidence(),
        );
        let branches = probe_branches(&manager, &project, &mission, &run, &["a", "b", "c"]);

        manager
            .run_branch_wave(&mission, &project, &run.id, branches, 3)
            .await
            .expect("solver 失败应落为 task 失败，不应取消同波其它分支");

        assert_eq!(counters.peak.load(Ordering::SeqCst), 3);
        let tasks = manager
            .repository()
            .list_tasks(run.id.as_str())
            .expect("任务必须可列出");
        assert_eq!(
            tasks
                .iter()
                .filter(|task| task.status == TaskStatus::Succeeded)
                .count(),
            2
        );
        assert_eq!(
            tasks
                .iter()
                .filter(|task| task.status == TaskStatus::Failed)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn branch_timeout_fails_only_that_branch_and_releases_active_slot() {
        let (manager, _dir, project, mission, mut run, counters) =
            manager_with_probe(Duration::from_secs(5), |solver| solver);
        run.config
            .insert("timeout_seconds".to_string(), Value::from(1));
        manager
            .repository()
            .update_run(&run)
            .expect("run 配置必须可更新");
        let branches = probe_branches(&manager, &project, &mission, &run, &["timeout"]);

        manager
            .run_branch_wave(&mission, &project, &run.id, branches, 1)
            .await
            .expect("timeout 走 solver failure 落盘路径");

        assert_eq!(counters.active.load(Ordering::SeqCst), 0);
        let task = manager
            .repository()
            .list_tasks(run.id.as_str())
            .expect("任务必须可列出")
            .pop()
            .expect("必须创建一个 task");
        assert_eq!(task.status, TaskStatus::Failed);
    }

    #[tokio::test]
    async fn lease_lost_before_settlement_is_nonfatal_and_recorded() {
        // Fix 1：settle 时 lease 已被外部取消（模拟 api.exe 重启 / 并发回收
        // 越过 expiry 边界）。任务结果此刻已由 commit_solver_* 落库，
        // run_branch_wave 必须返回 Ok、task 落 Succeeded，并显式记录一条
        // FailureBoundary 观察（不静默），绝不因并发槽簿记失败把整 run 判败。
        let (manager, _dir, project, mission, run, _counters) =
            manager_with_probe(Duration::from_millis(800), |solver| {
                // 本测试关心 lease 簿记，不是成败分类：给 solver 一条真实产出，
                // 免得它的空结果被 `solver_result_is_failure` 判败而跑题。
                solver.with_evidence()
            });
        let branches = probe_branches(&manager, &project, &mission, &run, &["lease-lost"]);
        let manager = Arc::new(manager);
        let run_id = run.id.clone();
        let handle = {
            let manager = Arc::clone(&manager);
            let project = project.clone();
            let mission = mission.clone();
            tokio::spawn(async move {
                manager
                    .run_branch_wave(&mission, &project, &run_id, branches, 1)
                    .await
            })
        };
        // 轮询直到 dispatch claim 出 Active lease，再用当前 revision 外部取消。
        // probe 延迟 800ms 提供充足窗口；默认 lease=300s ⇒ 心跳周期 100s，
        // solve 期间不会触发心跳，revision 稳定，取消确定成功。
        let repo = Arc::clone(manager.repository());
        let mut cancelled = false;
        for _ in 0..200 {
            if let Some(lease) = repo
                .list_worker_leases(project.id.as_str(), Some(run.id.as_str()))
                .expect("lease 必须可列出")
                .into_iter()
                .find(|lease| lease.status == models::agent::WorkerLeaseStatus::Active)
            {
                let worker_run_id = lease.worker_run_id.clone().unwrap_or_default();
                repo.cancel_worker_lease(&lease.id.to_string(), &worker_run_id, lease.revision)
                    .expect("外部取消必须成功");
                cancelled = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(cancelled, "必须在 settle 之前观察到 Active lease 并取消");

        handle
            .await
            .expect("wave 任务不得 panic")
            .expect("lease 丢失不得让 run_branch_wave 判败");

        let task = repo
            .list_tasks(run.id.as_str())
            .expect("任务必须可列出")
            .pop()
            .expect("必须创建一个 task");
        assert_eq!(task.status, TaskStatus::Succeeded);

        let stored_run = repo
            .get_run(run.id.as_str())
            .expect("run 必须可读取")
            .expect("run 必须存在");
        assert_ne!(stored_run.status, RunStatus::Failed);

        let observations = repo
            .list_observations(project.id.as_str(), Some(run.id.as_str()))
            .expect("观察必须可列出");
        assert!(
            observations.iter().any(|observation| {
                observation.observation_type == ObservationType::FailureBoundary
                    && observation.summary.contains("lease lost")
            }),
            "必须显式记录一条 lease-lost FailureBoundary 观察（不静默）"
        );
    }

    #[tokio::test]
    async fn lease_heartbeat_keeps_long_running_solver_alive() {
        // Fix 2：solver 长跑（5s）超过 lease 时长（3s），但心跳每 1s 把 expiry
        // 推到 now+3s，lease 全程保持 Active，settle 正常迁移到 Completed。
        // 若无心跳，lease 会在 3s 处 Expired、settle 落 Lost（停在 Expired）。
        let (manager, _dir, project, mission, mut run, _counters) =
            manager_with_probe(Duration::from_secs(5), |solver| {
                // 本测试关心心跳保活，不是成败分类：给 solver 一条真实产出，
                // 免得它的空结果被 `solver_result_is_failure` 判败而跑题。
                solver.with_evidence()
            });
        run.config
            .insert("worker_lease_seconds".to_string(), Value::from(3));
        run.config
            .insert("worker_lease_heartbeat_seconds".to_string(), Value::from(1));
        manager
            .repository()
            .update_run(&run)
            .expect("run 配置必须可更新");
        let branches = probe_branches(&manager, &project, &mission, &run, &["heartbeat"]);

        manager
            .run_branch_wave(&mission, &project, &run.id, branches, 1)
            .await
            .expect("心跳保活下波次必须完成");

        let task = manager
            .repository()
            .list_tasks(run.id.as_str())
            .expect("任务必须可列出")
            .pop()
            .expect("必须创建一个 task");
        assert_eq!(task.status, TaskStatus::Succeeded);

        let stored_run = manager
            .repository()
            .get_run(run.id.as_str())
            .expect("run 必须可读取")
            .expect("run 必须存在");
        assert_ne!(stored_run.status, RunStatus::Failed);

        let lease = manager
            .repository()
            .list_worker_leases(project.id.as_str(), Some(run.id.as_str()))
            .expect("lease 必须可列出")
            .pop()
            .expect("必须存在一个 lease");
        assert_eq!(
            lease.status,
            models::agent::WorkerLeaseStatus::Completed,
            "心跳保活后 settle 必须正常完成 lease，而非 Expired/Lost"
        );
    }

    #[test]
    fn worker_lease_guard_heartbeat_syncs_revision_and_settles() {
        // Fix 1 单元：heartbeat 成功必须回写 revision，否则随后的 settle 会因
        // CAS 失配自己触发假 Lost。这里 claim→heartbeat→settle 全链路验证。
        let (manager, _dir, project, mission, run) = test_manager();
        let repo = Arc::clone(manager.repository());
        let mut task = AgentTask::new(project.id.clone(), run.id.clone(), "web_recon".to_string());
        task.mission_id = Some(mission.id.clone());
        repo.create_task(&task).expect("Task 必须可创建");

        let lease = repo
            .claim_worker_lease(
                project.id.as_str(),
                mission.id.as_str(),
                run.id.as_str(),
                task.id.as_str(),
                "coordinator-worker",
                &format!("worker-run-{}", task.id.as_str()),
                5,
            )
            .expect("claim 必须成功")
            .expect("claim 必须返回 lease");

        let mut guard = WorkerLeaseGuard::new(Arc::clone(&repo), &lease);
        // 心跳用更长的租期，确定性地推动 expiry 前移。
        assert!(
            guard.heartbeat(60).expect("心跳不得报错"),
            "活跃 lease 的心跳必须成功"
        );
        let after = repo
            .list_worker_leases(project.id.as_str(), Some(run.id.as_str()))
            .expect("lease 必须可列出")
            .into_iter()
            .find(|item| item.id == lease.id)
            .expect("心跳后 lease 必须存在");
        assert!(after.revision > lease.revision, "心跳必须递增 revision");
        assert!(
            after.lease_expires_at > lease.lease_expires_at,
            "心跳必须把 expiry 前移"
        );

        // revision 已被 guard 回写：settle 必须 Settled，而非假 Lost。
        assert_eq!(
            guard.settle(TaskStatus::Succeeded).expect("settle 不得报错"),
            LeaseSettlement::Settled
        );
        let completed = repo
            .list_worker_leases(project.id.as_str(), Some(run.id.as_str()))
            .expect("lease 必须可列出")
            .into_iter()
            .find(|item| item.id == lease.id)
            .expect("settle 后 lease 必须存在");
        assert_eq!(completed.status, models::agent::WorkerLeaseStatus::Completed);
        assert_eq!(
            completed.worker_run_id.as_deref(),
            lease.worker_run_id.as_deref()
        );
    }

    #[test]
    fn worker_lease_guard_reports_lost_after_external_cancel() {
        // Fix 1 单元：外部取消（模拟 api.exe 重启 / 并发回收）后，lease 已非
        // 我方持有：heartbeat→false、settle→Lost，且两者都不是 Err（非致命）。
        let (manager, _dir, project, mission, run) = test_manager();
        let repo = Arc::clone(manager.repository());
        let mut task = AgentTask::new(project.id.clone(), run.id.clone(), "web_recon".to_string());
        task.mission_id = Some(mission.id.clone());
        repo.create_task(&task).expect("Task 必须可创建");
        let lease = repo
            .claim_worker_lease(
                project.id.as_str(),
                mission.id.as_str(),
                run.id.as_str(),
                task.id.as_str(),
                "coordinator-worker",
                &format!("worker-run-{}", task.id.as_str()),
                30,
            )
            .expect("claim 必须成功")
            .expect("claim 必须返回 lease");
        let worker_run_id = lease.worker_run_id.clone().expect("lease 必须有 owner");
        let mut guard = WorkerLeaseGuard::new(Arc::clone(&repo), &lease);

        repo.cancel_worker_lease(&lease.id.to_string(), &worker_run_id, lease.revision)
            .expect("外部取消必须成功")
            .expect("取消必须返回 lease");
        assert!(
            !guard.heartbeat(30).expect("心跳不得报错"),
            "lease 已非我方持有，心跳必须返回 false"
        );
        assert_eq!(
            guard.settle(TaskStatus::Succeeded).expect("settle 不得报错"),
            LeaseSettlement::Lost,
            "lease 已丢失，settle 必须返回 Lost 而非 Err"
        );
    }

    #[tokio::test]
    async fn cancelling_branch_wave_drops_all_active_branch_futures() {
        let (manager, _dir, project, mission, run, counters) =
            manager_with_probe(Duration::from_secs(60), |solver| solver);
        let branches = probe_branches(&manager, &project, &mission, &run, &["a", "b"]);
        let manager = Arc::new(manager);
        let run_id = run.id.clone();
        let handle = {
            let manager = Arc::clone(&manager);
            let project = project.clone();
            let mission = mission.clone();
            tokio::spawn(async move {
                manager
                    .run_branch_wave(&mission, &project, &run_id, branches, 2)
                    .await
            })
        };
        for _ in 0..100 {
            if counters.active.load(Ordering::SeqCst) == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(counters.active.load(Ordering::SeqCst), 2);

        handle.abort();
        let _ = handle.await;

        assert_eq!(counters.active.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn parallel_branches_keep_evidence_provenance_on_their_branch() {
        let (manager, _dir, project, mission, run, _counters) =
            manager_with_probe(Duration::from_millis(20), ProbeSolver::with_evidence);
        let branches = probe_branches(&manager, &project, &mission, &run, &["a", "b"]);
        let branch_ids: HashSet<_> = branches.iter().map(|branch| branch.id.clone()).collect();

        manager
            .run_branch_wave(&mission, &project, &run.id, branches, 2)
            .await
            .expect("并行 evidence 提交必须完成");

        let evidence = manager
            .repository()
            .list_evidence(project.id.as_str())
            .expect("evidence 必须可列出");
        assert_eq!(evidence.len(), 2);
        let invocations = manager
            .repository()
            .list_tool_invocations(Some(project.id.as_str()))
            .expect("工具调用必须可列出");
        for item in evidence {
            let branch_id = item.branch_id.expect("evidence 必须绑定 branch");
            assert!(branch_ids.contains(&branch_id));
            let invocation_id = item
                .produced_by_tool_invocation_id
                .expect("evidence 必须保留工具 provenance");
            let invocation = invocations
                .iter()
                .find(|invocation| invocation.id == invocation_id)
                .expect("provenance 指向的工具调用必须存在");
            assert_eq!(invocation.branch_id.as_ref(), Some(&branch_id));
            assert_eq!(invocation.run_id.as_ref(), Some(&run.id));
        }
    }

    #[tokio::test]
    async fn branch_wave_does_not_schedule_past_remaining_run_budget() {
        let (manager, _dir, project, mission, mut run, _counters) =
            manager_with_probe(Duration::from_millis(20), |solver| solver);
        run.max_total_steps = 2;
        manager
            .repository()
            .update_run(&run)
            .expect("run 预算必须可更新");
        let branches = probe_branches(&manager, &project, &mission, &run, &["a", "b", "c", "d"]);

        manager
            .run_branch_wave(&mission, &project, &run.id, branches, 4)
            .await
            .expect("预算内波次必须完成");

        let stored = manager
            .repository()
            .get_run(run.id.as_str())
            .expect("run 必须可读取")
            .expect("run 必须存在");
        assert_eq!(stored.steps_used, 2);
        assert_eq!(
            manager
                .repository()
                .list_tasks(run.id.as_str())
                .expect("任务必须可列出")
                .len(),
            2
        );
    }

    #[test]
    fn branch_concurrency_limit_uses_run_config_and_runtime_cap() {
        let (_manager, _dir, _project, _mission, mut run) = test_manager();
        run.config
            .insert("max_concurrent_branches".to_string(), Value::from(4));
        assert_eq!(branch_concurrency_limit(&run, None, 6), 4);
        assert_eq!(
            branch_concurrency_limit(&run, Some(99), 99),
            BRANCH_RUNTIME_MAX_CONCURRENCY
        );
        assert_eq!(branch_concurrency_limit(&run, Some(0), 3), 1);
    }

    #[test]
    fn max_concurrent_workers_defaults_to_four_and_clamps() {
        let (_manager, _dir, _project, _mission, run) = test_manager();
        // 未配置 → 运营者口径的默认 4。
        assert_eq!(max_concurrent_workers(&run), 4);

        let mut configured = run.clone();
        configured
            .config
            .insert("max_concurrent_workers".to_string(), Value::from(2));
        assert_eq!(max_concurrent_workers(&configured), 2);

        // 上限硬顶：配置笔误不能把机器打爆。
        let mut huge = run.clone();
        huge.config
            .insert("max_concurrent_workers".to_string(), Value::from(9999));
        assert_eq!(
            max_concurrent_workers(&huge),
            usize::try_from(WORKER_POOL_MAX_CONCURRENCY).unwrap_or(1)
        );

        // 0 / 负值钳到 1：池至少要有 1 个许可，否则谁都派不出去。
        let mut zero = run.clone();
        zero.config
            .insert("max_concurrent_workers".to_string(), Value::from(0));
        assert_eq!(max_concurrent_workers(&zero), 1);
    }

    #[test]
    fn clamp_width_to_pool_prevents_acquire_deadlock() {
        // 池小于 width 时必须削到池大小，否则 acquire_many(width) 永远等不齐。
        assert_eq!(clamp_width_to_pool(4, Some(4)), 4);
        assert_eq!(clamp_width_to_pool(8, Some(4)), 4);
        assert_eq!(clamp_width_to_pool(4, Some(1)), 1);
        // 池边界值也必须是合法下限。
        assert_eq!(clamp_width_to_pool(4, Some(0)), 1);
        // 没注册池 → 原样返回，不限。
        assert_eq!(clamp_width_to_pool(8, None), 8);
    }

    #[test]
    fn worker_pool_guard_removes_the_pool_on_drop() {
        let (manager, _dir, _project, _mission, _run) = test_manager();
        let run_id = RunId::from("run_pool_guard".to_string());
        let pool = Arc::new(tokio::sync::Semaphore::new(4));

        {
            let mut pools = manager
                .worker_pools
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            pools.insert(run_id.clone(), Arc::clone(&pool));
        }
        assert!(
            manager.worker_pool_for(run_id.as_str()).is_some(),
            "注册后必须能按 run 取到池"
        );

        {
            let _guard = WorkerPoolGuard {
                manager: &manager,
                run_id: run_id.clone(),
            };
        }
        assert!(
            manager.worker_pool_for(run_id.as_str()).is_none(),
            "guard drop 后池必须被摘除，否则下一个 run 会复用到半空的池"
        );
    }

    #[test]
    fn worker_pool_for_unknown_run_returns_none() {
        let (manager, _dir, _project, _mission, _run) = test_manager();
        assert!(manager.worker_pool_for("run_does_not_exist").is_none());
    }

    #[test]
    fn solver_failure_persists_reflector_report_and_narratives() {
        let (manager, _dir, project, _mission, run) = test_manager();
        let mut task = AgentTask::new(project.id.clone(), run.id.clone(), "web_recon".to_string());
        manager
            .repository()
            .create_task(&task)
            .expect("Task 必须可创建");
        let mut intent = Intent::new(project.id.clone(), "probe branch".to_string());
        manager
            .repository()
            .add_intent(&intent)
            .expect("Intent 必须可创建");

        manager
            .commit_solver_failure(
                &project,
                &run,
                &mut task,
                &mut intent,
                SolverError::Other("nuclei timed out after 30s".to_string()),
            )
            .expect("失败落盘必须成功");

        let reports = manager
            .repository()
            .list_reflector_reports(project.id.as_str(), Some(run.id.as_str()))
            .expect("复盘报告必须可列出");
        assert_eq!(reports.len(), 1);
        assert_eq!(
            reports[0].failure_type,
            models::ReflectorFailureType::Timeout
        );

        let events = manager
            .repository()
            .list_agent_narrative_events(
                project.id.as_str(),
                Some(run.id.as_str()),
                None,
                None,
                None,
            )
            .expect("叙事事件必须可列出");
        assert_eq!(events.len(), 3);
        // 收尾（settlement）：失败 run 也必须有一条用户可见的结论。这是
        // wrapup 契约的回归防线——缺了它，worker 挂了用户在会话里
        // 只看到 reflector 的复盘，看不到"这次到底有没有产出"。
        let settlement = events
            .iter()
            .find(|event| event.event_kind == AgentNarrativeEventKind::WorkerSummary)
            .expect("失败路径必须有 WorkerSummary 收尾注记");
        assert_eq!(settlement.source_agent, "worker");
        assert_eq!(settlement.task_id.as_ref(), Some(&task.id));
        assert!(
            settlement.original_text.contains("nuclei timed out after 30s"),
            "拿不到 worker 终稿时必须退化成确定性事实陈述，实际: {}",
            settlement.original_text
        );
        let failure = events
            .iter()
            .find(|event| event.event_kind == AgentNarrativeEventKind::FailureAnalysis)
            .expect("必须有失败分析注记");
        assert_eq!(failure.source_agent, "reflector");
        assert!(failure.original_text.contains("timed out"));
        assert_eq!(failure.task_id.as_ref(), Some(&task.id));
        let lesson = events
            .iter()
            .find(|event| event.event_kind == AgentNarrativeEventKind::ReflectorNote)
            .expect("必须有反思者注记");
        assert!(lesson.original_text.contains("timeout budgets"));
    }

    #[tokio::test]
    async fn complete_run_persists_trajectory_summary_and_reasoning_narrative() {
        let (manager, _dir, project, mission, run) = test_manager();
        manager
            .repository()
            .add_observation(&Observation::new(
                project.id.clone(),
                run.id.clone(),
                "branch probe finished".to_string(),
            ))
            .expect("观察必须可持久化");

        manager
            .complete_run(&mission, &run)
            .await
            .expect("run 完成必须成功");

        let summaries = manager
            .repository()
            .list_trajectory_summaries(project.id.as_str(), Some(run.id.as_str()), None)
            .expect("轨迹摘要必须可列出");
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].mission_id.as_ref(), Some(&mission.id));

        let events = manager
            .repository()
            .list_agent_narrative_events(
                project.id.as_str(),
                Some(run.id.as_str()),
                None,
                None,
                None,
            )
            .expect("叙事事件必须可列出");
        let reasoning = events
            .iter()
            .find(|event| event.event_kind == AgentNarrativeEventKind::ReasoningSummary)
            .expect("必须有推理摘要注记");
        assert_eq!(reasoning.source_agent, "trajectory_summarizer");
        assert_eq!(reasoning.original_text, summaries[0].summary);
        assert_eq!(reasoning.mission_id.as_ref(), Some(&mission.id));
    }

    #[test]
    fn all_failed_tool_trace_cannot_be_committed_as_success() {
        let mut invocation =
            ToolInvocation::new("web_exploit_campaign".to_string(), "test".to_string());
        invocation.status = models::ToolStatus::Error;
        let result = SolverResult {
            tool_invocations: vec![invocation],
            ..SolverResult::default()
        };
        assert!(solver_result_is_failure(&result));
    }

    #[test]
    fn partial_success_trace_remains_auditable() {
        let invocation =
            ToolInvocation::new("web_exploit_campaign".to_string(), "test".to_string());
        let result = SolverResult {
            tool_invocations: vec![invocation],
            ..SolverResult::default()
        };
        assert!(!solver_result_is_failure(&result));
    }

    /// 回归：零工具调用 + 零产出必须是失败。实测成因是 pi 的 bash 工具不可用
    /// （没装 Git for Windows，PATH 里只有 WSL 占位 shim），它什么都执行不了
    /// 却仍以 exit 0 收场——旧逻辑的 `!tool_invocations.is_empty()` 让这种
    /// 空转被记成 succeeded，甚至在竞速里靠"没报错"胜出。
    #[test]
    fn idle_run_with_no_tool_calls_is_a_failure() {
        let result = SolverResult::default();
        assert!(
            result.tool_invocations.is_empty(),
            "fixture must actually exercise the zero-call shape"
        );
        assert!(
            solver_result_is_failure(&result),
            "什么都没产出且什么都没调，不能算成功"
        );
    }

    /// "查过了，这里没有"是合法结论：只要产出过任意一项就不是失败。没有这条，
    /// 修复会把诚实的空结论一并判败，audit 语义就废了。
    #[test]
    fn honest_negative_conclusion_is_not_a_failure() {
        let result = SolverResult {
            proposed_intents: vec![models::intent::Intent::new(
                models::ids::ProjectId::new("proj_1".to_string()),
                "try the next direction".to_string(),
            )],
            ..SolverResult::default()
        };
        assert!(
            !solver_result_is_failure(&result),
            "提出了后续意图 = 有贡献，即便没有 finding"
        );
    }

    /// 有过一次成功调用就不是失败，哪怕其余全败。
    #[test]
    fn any_successful_tool_call_is_not_a_failure() {
        let mut ok = ToolInvocation::new("web_excon_campaign".to_string(), "ok".to_string());
        ok.status = models::ToolStatus::Ok;
        let mut bad = ToolInvocation::new("ffuf".to_string(), "bad".to_string());
        bad.status = models::ToolStatus::Error;
        let result = SolverResult {
            tool_invocations: vec![bad, ok],
            ..SolverResult::default()
        };
        assert!(!solver_result_is_failure(&result));
    }

    #[test]
    fn solver_success_surfaces_worker_summary_to_the_user() {
        // 回归防线：外部 worker 成功后，终稿必须作为 WorkerSummary 叙事事件
        // 回给用户。这是"输入你好却什么都没回来"的直接修复点——worker 跑完了、
        // facts/evidence 都落了，但用户眼前只有 Progress 痕迹、看不到结论。
        let (manager, _dir, project, mission, run) = test_manager();
        let mut task = AgentTask::new(project.id.clone(), run.id.clone(), "web_recon".to_string());
        manager
            .repository()
            .create_task(&task)
            .expect("Task 必须可创建");
        let mut intent = Intent::new(project.id.clone(), "probe branch".to_string());
        manager
            .repository()
            .add_intent(&intent)
            .expect("Intent 必须可创建");

        let result = SolverResult {
            notes: Some("你好，目标可达，未发现开放高危服务".to_string()),
            ..SolverResult::default()
        };
        manager
            .commit_solver_success(&project, &mission, &run, &mut task, &mut intent, result)
            .expect("成功落盘必须成功");

        let events = manager
            .repository()
            .list_agent_narrative_events(
                project.id.as_str(),
                Some(run.id.as_str()),
                None,
                None,
                None,
            )
            .expect("叙事事件必须可列出");
        let summary = events
            .iter()
            .find(|event| event.event_kind == AgentNarrativeEventKind::WorkerSummary)
            .expect("成功路径必须有 WorkerSummary 注记");
        assert_eq!(summary.source_agent, "worker");
        assert_eq!(summary.task_id.as_ref(), Some(&task.id));
        assert_eq!(summary.mission_id.as_ref(), Some(&mission.id));
        assert_eq!(summary.original_text, "你好，目标可达，未发现开放高危服务");
    }

    #[test]
    fn solver_success_without_notes_emits_no_worker_summary() {
        // 没终稿就不编一条：WorkerSummary 是可选的，空文本不得落库。
        let (manager, _dir, project, mission, run) = test_manager();
        let mut task = AgentTask::new(project.id.clone(), run.id.clone(), "web_recon".to_string());
        manager
            .repository()
            .create_task(&task)
            .expect("Task 必须可创建");
        let mut intent = Intent::new(project.id.clone(), "probe branch".to_string());
        manager
            .repository()
            .add_intent(&intent)
            .expect("Intent 必须可创建");

        manager
            .commit_solver_success(
                &project,
                &mission,
                &run,
                &mut task,
                &mut intent,
                SolverResult::default(),
            )
            .expect("成功落盘必须成功");

        let events = manager
            .repository()
            .list_agent_narrative_events(
                project.id.as_str(),
                Some(run.id.as_str()),
                None,
                None,
                None,
            )
            .expect("叙事事件必须可列出");
        assert!(
            !events
                .iter()
                .any(|event| event.event_kind == AgentNarrativeEventKind::WorkerSummary),
            "无终稿时不应出现空的 WorkerSummary"
        );
    }

    // ------------------------------------------------------------------
    // 外部 Worker Runtime 派发（acceptance：provenance / fail-closed /
    // 不伪造 Finding / unavailable 显式失败）
    // ------------------------------------------------------------------

    struct FakeWorkerRuntime {
        status: models::WorkerRunStatus,
        sleep: Duration,
    }

    #[async_trait::async_trait]
    impl agents::worker::WorkerRuntime for FakeWorkerRuntime {
        fn runtime_type(&self) -> models::WorkerRuntimeType {
            models::WorkerRuntimeType::ClaudeCode
        }

        async fn probe(&self) -> models::WorkerProbe {
            models::WorkerProbe::new(
                models::WorkerRuntimeType::ClaudeCode,
                models::WorkerAvailability::Available,
                Vec::new(),
            )
        }

        fn capabilities(&self) -> Vec<String> {
            Vec::new()
        }

        async fn start(
            &self,
            request: agents::worker::WorkerExecutionRequest,
        ) -> Result<agents::worker::WorkerExecutionOutcome, agents::worker::WorkerRuntimeError>
        {
            tokio::time::sleep(self.sleep).await;
            let mut run = models::WorkerRun::new(
                models::ProjectId::new("proj_fake_worker".to_string()),
                models::WorkerRuntimeType::ClaudeCode,
                request.instruction,
            );
            run.mark_started();
            let mut invocation = models::WorkerInvocation::new(
                models::WorkerRuntimeType::ClaudeCode,
                models::WorkerInvocationPurpose::Start,
                Some(run.id.clone()),
            );
            let transcript = if self.status == models::WorkerRunStatus::Succeeded {
                b"fake worker transcript output".to_vec()
            } else {
                Vec::new()
            };
            run.finish(self.status, Some("fake worker finished".to_string()));
            // 真实 adapter 的成功/失败路径都会写 invocation.summary（终稿），
            // fake 也要写，否则测不到 dispatch 的终稿优先分支。
            invocation.summary = Some("fake worker finished".to_string());
            invocation.finish(self.status);
            Ok(agents::worker::WorkerExecutionOutcome {
                run,
                invocation,
                transcript,
            })
        }

        async fn resume(
            &self,
            _request: agents::worker::WorkerExecutionRequest,
        ) -> Result<agents::worker::WorkerExecutionOutcome, agents::worker::WorkerRuntimeError>
        {
            Err(agents::worker::WorkerRuntimeError::new(
                agents::worker::WorkerRuntimeErrorKind::Unsupported,
                "fake worker does not support resume",
            ))
        }

        async fn events(
            &self,
            _session_ref: &str,
        ) -> Result<Vec<models::worker::WorkerEvent>, agents::worker::WorkerRuntimeError> {
            Ok(Vec::new())
        }

        async fn cancel(
            &self,
            _session_ref: &str,
        ) -> Result<bool, agents::worker::WorkerRuntimeError> {
            Ok(false)
        }
    }

    struct FakeWorkerSelector {
        runtime: Option<FakeWorkerRuntime>,
    }

    #[async_trait::async_trait]
    impl agents::worker::WorkerRuntimeSelector for FakeWorkerSelector {
        async fn select(
            &self,
            _preferred: Option<&str>,
        ) -> Result<Arc<dyn agents::worker::WorkerRuntime>, agents::worker::WorkerRuntimeError>
        {
            match self.runtime.as_ref() {
                Some(runtime) => Ok(Arc::new(FakeWorkerRuntime {
                    status: runtime.status,
                    sleep: runtime.sleep,
                })),
                None => Err(agents::worker::WorkerRuntimeError::new(
                    agents::worker::WorkerRuntimeErrorKind::NotReady,
                    "no external worker runtime is available (fake)",
                )),
            }
        }

        async fn probes(&self) -> Vec<models::WorkerProbe> {
            Vec::new()
        }

        fn runtime(
            &self,
            _runtime_type: models::WorkerRuntimeType,
        ) -> Option<Arc<dyn agents::worker::WorkerRuntime>> {
            None
        }
    }

    fn manager_with_worker(
        status: models::WorkerRunStatus,
        sleep: Duration,
        runtime_present: bool,
    ) -> (AuditManager, tempfile::TempDir, Project, Mission, AuditRun) {
        let (manager, dir, mut project, mission, mut run) =
            test_manager_with_registry(engines::solvers::default_solver_registry());
        project
            .target
            .insert("url".to_string(), "https://example.test".to_string());
        manager
            .repository()
            .update_project(&project)
            .expect("项目目标必须可更新");
        let artifact_dir = dir.path().join("artifacts");
        run.config.insert(
            "artifact_dir".to_string(),
            Value::String(artifact_dir.to_string_lossy().into_owned()),
        );
        manager
            .repository()
            .update_run(&run)
            .expect("run 配置必须可更新");
        if runtime_present {
            manager.set_worker_runtime(Some(Arc::new(FakeWorkerSelector {
                runtime: Some(FakeWorkerRuntime { status, sleep }),
            })));
        } else {
            manager.set_worker_runtime(Some(Arc::new(FakeWorkerSelector { runtime: None })));
        }
        (manager, dir, project, mission, run)
    }

    #[tokio::test]
    async fn worker_success_commits_provenance_and_controlled_observation() {
        let (manager, dir, project, mission, run) =
            manager_with_worker(models::WorkerRunStatus::Succeeded, Duration::ZERO, true);
        let branches = probe_branches(&manager, &project, &mission, &run, &["a"]);

        manager
            .run_branch_wave(&mission, &project, &run.id, branches, 1)
            .await
            .expect("worker 分支波次必须完成");

        let tasks = manager
            .repository()
            .list_tasks(run.id.as_str())
            .expect("任务必须可列出");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, TaskStatus::Succeeded);

        let worker_runs = manager
            .repository()
            .list_worker_runs(Some(project.id.as_str()), None, None, 10)
            .expect("worker runs 必须可列出");
        assert_eq!(worker_runs.len(), 1);
        let worker_run = &worker_runs[0];
        assert_eq!(worker_run.status, models::WorkerRunStatus::Succeeded);
        assert_eq!(
            worker_run.run_id.as_ref().map(models::RunId::as_str),
            Some(run.id.as_str())
        );
        assert_eq!(
            worker_run.task_id.as_ref().map(models::TaskId::as_str),
            Some(tasks[0].id.as_str())
        );
        assert!(worker_run.branch_id.is_some());
        assert!(
            worker_run.transcript_path.is_some(),
            "成功的 worker 输出必须密封为 transcript 工件"
        );
        let transcript_path = worker_run.transcript_path.clone().expect("transcript path");
        assert!(std::path::Path::new(&transcript_path).exists());

        let facts = manager
            .repository()
            .list_facts(project.id.as_str())
            .expect("facts 必须可列出");
        assert!(
            facts.iter().any(|fact| fact.kind == "worker.run.completed"),
            "worker 完成事实必须落库: {facts:?}"
        );
        // Fact 的 statement 是用户和下游 agent 唯一能看见的一行：必须带上
        // worker 终稿，而不能只是一句 "completed a run (status succeeded)"
        // ——那等于把外部输出全藏起来，前端 fact 列表看不到任何实质内容。
        let completed = facts
            .iter()
            .find(|fact| fact.kind == "worker.run.completed")
            .expect("worker.run.completed fact");
        assert!(
            completed.statement.contains("fake worker finished"),
            "Fact statement 必须包含 worker 终稿: {:?}",
            completed.statement
        );
        // 终稿打头、harness/状态收尾成尾标签。早先 statement 以
        // "external worker 'x' completed a run (status y): " 开头，把产出压在
        // 套话后面；现在约定反过来——先读到的必须是 worker 真正说了什么。
        assert!(
            completed.statement.starts_with("fake worker finished"),
            "Fact statement 必须由 worker 终稿打头: {:?}",
            completed.statement
        );
        assert!(
            completed
                .statement
                .contains("[worker 'claude_code' status="),
            "Fact statement 必须保留 harness/状态尾标签: {:?}",
            completed.statement
        );
        assert_eq!(
            completed.data.get("summary").and_then(Value::as_str),
            Some("fake worker finished"),
            "终稿原文必须单独落在 data.summary，供程序化消费"
        );
        // 外部输出绝不直接变成 Finding。
        let findings = manager
            .repository()
            .list_findings(project.id.as_str())
            .expect("findings 必须可列出");
        assert!(findings.is_empty(), "受控 observation 不得伪造 Finding");
        let _ = dir;
    }

    #[tokio::test]
    async fn worker_failure_preserves_audit_and_never_fabricates_findings() {
        let (manager, _dir, project, mission, run) =
            manager_with_worker(models::WorkerRunStatus::Failed, Duration::ZERO, true);
        let branches = probe_branches(&manager, &project, &mission, &run, &["a"]);

        manager
            .run_branch_wave(&mission, &project, &run.id, branches, 1)
            .await
            .expect("worker 失败必须落为 task 失败而非编排错误");

        let tasks = manager
            .repository()
            .list_tasks(run.id.as_str())
            .expect("任务必须可列出");
        assert_eq!(tasks[0].status, TaskStatus::Failed);
        assert!(
            tasks[0]
                .error
                .as_deref()
                .is_some_and(|error| error.contains("external worker runtime")),
            "失败消息必须显式提及外部 worker: {:?}",
            tasks[0].error
        );

        let worker_runs = manager
            .repository()
            .list_worker_runs(Some(project.id.as_str()), None, None, 10)
            .expect("worker runs 必须可列出");
        assert_eq!(worker_runs.len(), 1, "失败也要保留 worker 审计链");
        assert_eq!(worker_runs[0].status, models::WorkerRunStatus::Failed);

        let findings = manager
            .repository()
            .list_findings(project.id.as_str())
            .expect("findings 必须可列出");
        assert!(findings.is_empty(), "失败的 worker 不得伪造 Finding");
    }

    #[tokio::test]
    async fn selector_unavailable_fails_task_with_explicit_error() {
        let (manager, _dir, project, mission, run) =
            manager_with_worker(models::WorkerRunStatus::Succeeded, Duration::ZERO, false);
        let branches = probe_branches(&manager, &project, &mission, &run, &["a"]);

        manager
            .run_branch_wave(&mission, &project, &run.id, branches, 1)
            .await
            .expect("选择器不可用必须落为显式 task 失败");

        let tasks = manager
            .repository()
            .list_tasks(run.id.as_str())
            .expect("任务必须可列出");
        assert_eq!(tasks[0].status, TaskStatus::Failed);
        let error = tasks[0].error.clone().unwrap_or_default();
        assert!(
            error.contains("external worker runtime unavailable")
                && error.contains("configuration required"),
            "必须显式报 unavailable/configuration required: {error}"
        );
        let worker_runs = manager
            .repository()
            .list_worker_runs(Some(project.id.as_str()), None, None, 10)
            .expect("worker runs 必须可列出");
        assert!(worker_runs.is_empty(), "未派发的执行不得有 worker 记录");
    }
}

/// 仓储适配的 Agent 预设来源（[`models::agent_preset::AgentPresetSource`]）。
struct RepositoryAgentPresetSource {
    repository: Arc<dyn storage::Repository>,
}

impl models::agent_preset::AgentPresetSource for RepositoryAgentPresetSource {
    fn enabled_preset(&self, key: &str) -> Option<models::AgentPreset> {
        let preset = self.repository.get_agent_preset(key).ok()??;
        preset.enabled.then_some(preset)
    }
}
