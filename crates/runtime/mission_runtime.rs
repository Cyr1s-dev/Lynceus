//! Mission runtime —— `manager.py` 的 run 容器创建、复杂度策略应用与
//! runtime 提交/安全执行。
//!
//! 启动-恢复的 epoch 语义（Python `start_mission` 文档的镜像）：
//! Run/Mission/Branch 状态**先持久化**，再提交分离任务——分离的 runtime
//! 拿到的永远是已落盘的世界。同一 epoch 处于 PENDING/RUNNING 时重复
//! start 复用该 epoch，不制造重复 run。
//!
//! 与 Python 的两处并发差异（沿用 `task_backend` 的既定决策）：
//! - `asyncio` 取消在协程内抛 `CancelledError` 让运行时自己落盘"中断"
//!   状态；tokio `abort` 直接 drop future——"中断 → PAUSED"的落盘由取消
//!   发起方（`pause_mission`）负责，故 `run_mission_runtime_safely`
//!   只保留失败分支；
//! - runtime 令牌用 [`crate::manager::RuntimeToken`]（`Arc<()>` 指针身份）
//!   镜像 Python `object()` 身份比较。

// Runtime methods deliberately retain the async/public shape of the Python
// manager while the Axum/Tauri adapters migrate to Rust incrementally.
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::redundant_closure_for_method_calls)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::unused_async)]
#![allow(clippy::unused_async_trait_impl)]
#![allow(clippy::unused_self)]

use std::sync::Arc;
use std::time::Instant;

use agents::branch_generator::BranchGenerationInput;
use agents::critique::CritiqueInput;
use agents::retrieval::KnowledgeRetriever;
use agents::strategy_board::{ApplyOpsInput, PromptInput};
use models::ApprovalMode;
use models::AuditEvent;
use models::AuditEventType;
use models::AuditRun;
use models::Branch;
use models::Mission;
use models::MissionId;
use models::MissionStartResult;
use models::MissionStatus;
use models::Project;
use models::RetrievalChunkId;
use models::RetrievalInvocation;
use models::RetrievalInvocationId;
use models::RetrievalSourceKind;
use models::RetrievalStatus;
use models::RetrievedEvidence;
use models::RunId;
use models::RunStatus;
use models::StrategyBoardDomain;
use models::StrategyBoardSnapshot;
use models::Timestamp;
use models::utcnow;
use serde_json::Map;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;

use storage::Repository;
use storage::StorageError;

use crate::errors::EngineError;
use crate::events::EventDraft;
use crate::manager::AuditManager;
use crate::manager::RuntimeToken;
use crate::narratives::NarrativeDraft;

struct RepositoryKnowledgeRetriever<'a> {
    repository: &'a dyn Repository,
}

impl KnowledgeRetriever for RepositoryKnowledgeRetriever<'_> {
    type Error = StorageError;

    fn retrieve(
        &self,
        query: &models::KnowledgeRetrievalQuery,
    ) -> Result<Vec<models::KnowledgeRetrievalResult>, Self::Error> {
        self.repository.search_knowledge_cards(query)
    }
}

#[derive(Default)]
struct BranchKnowledge {
    results: Vec<models::KnowledgeRetrievalResult>,
    invocation_id: Option<RetrievalInvocationId>,
}

impl AuditManager {
    /// 启动 Mission 并可选分离其长运行 Branch runtime（Python
    /// `start_mission`）。
    ///
    /// # Errors
    /// Mission/Project/Run 不存在、run 配置非法（422 族）或仓储写入失败。
    pub async fn start_mission(
        self: &Arc<Self>,
        mission_id: &MissionId,
        config: Option<Map<String, Value>>,
        auto_start_runtime: bool,
        background_runtime: bool,
        max_concurrent_branches: Option<i64>,
        max_total_steps: Option<i64>,
    ) -> Result<MissionStartResult, EngineError> {
        let start_lock = self.mission_start_lock(mission_id).await;
        let _guard = start_lock.lock().await;
        let mut mission = self.require_mission(mission_id.as_str())?;
        let project = self.require_project(mission.project_id.as_str())?;
        // target 可能在 mission 创建之后才被更新（intake 的计划落地就是
        // 创建后写 target），创建时的资产投影拿到的还是空 target。这里按
        // 当前 target 补投影一次：upsert 按 (mission, type, value) 去重，
        // 重复投影幂等；值嗅探见 `assets_from_mission_target`。
        self.project_mission_assets_safe(&mission, &[], &[], &[], &[], false)
            .await;
        let mut run = match &mission.active_run_id {
            Some(active_run_id) => self.repository().get_run(active_run_id.as_str())?,
            None => None,
        };
        let reusable = run
            .as_ref()
            .is_some_and(|run| matches!(run.status, RunStatus::Pending | RunStatus::Running));
        let running_without_handle = match run.as_ref() {
            Some(run) if run.status == RunStatus::Running => {
                !self.has_runtime_handle(&run.id).await
            }
            _ => false,
        };
        let should_schedule_runtime = !reusable
            || run
                .as_ref()
                .is_some_and(|run| run.status == RunStatus::Pending)
            || running_without_handle;
        if !reusable {
            let run_status = if auto_start_runtime {
                RunStatus::Running
            } else {
                RunStatus::Pending
            };
            let created = self
                .create_mission_run(&mission, config.clone().unwrap_or_default(), run_status)
                .await?;
            mission.active_run_id = Some(created.id.clone());
            run = Some(created);
        } else if config.is_some() {
            if let Some(run) = run.as_mut() {
                for (key, value) in config.unwrap_or_default() {
                    run.config.insert(key, value);
                }
                run.updated_at = utcnow();
                self.repository().update_run(run)?;
            }
        }
        let Some(run) = run.as_mut() else {
            // 防御性收窄：active_run_id 指向已删除的 run 时 get_run 返回
            // None——reusable 为假走新建路径，理论上不可达。
            return Err(EngineError::RunNotFound(format!(
                "mission run could not be resolved: {}",
                mission_id.as_str()
            )));
        };

        if auto_start_runtime && run.status == RunStatus::Pending {
            run.status = RunStatus::Running;
            if run.started_at.is_none() {
                run.started_at = Some(utcnow());
            }
            run.updated_at = utcnow();
            self.repository().update_run(run)?;
        }
        mission.status = if auto_start_runtime {
            MissionStatus::Running
        } else {
            MissionStatus::Draft
        };
        mission.updated_at = utcnow();
        self.persist_mission_notifying(&mission)?;

        let branches = self
            .ensure_mission_run_branches(&mission, &project, run)
            .await?;
        if !reusable {
            self.record_event_safe(EventDraft {
                run_id: Some(&run.id),
                status: Some(mission.status.as_str()),
                data: Some(Map::from_iter([
                    (
                        "mission_id".to_string(),
                        Value::String(mission.id.as_str().to_string()),
                    ),
                    (
                        "branch_count".to_string(),
                        Value::from(branches.len() as i64),
                    ),
                    (
                        "auto_start_runtime".to_string(),
                        Value::Bool(auto_start_runtime),
                    ),
                    (
                        "background_runtime".to_string(),
                        Value::Bool(background_runtime),
                    ),
                ])),
                ..EventDraft::new(
                    &project.id,
                    if auto_start_runtime {
                        AuditEventType::RunStarted
                    } else {
                        AuditEventType::UserNote
                    },
                    "mission_control",
                    if auto_start_runtime {
                        "Mission run started"
                    } else {
                        "Mission run prepared"
                    },
                )
            })
            .await;
        }

        let run_id = run.id.clone();
        if auto_start_runtime && background_runtime && should_schedule_runtime {
            self.submit_mission_runtime(
                mission_id.clone(),
                run_id.clone(),
                "mission_started",
                max_concurrent_branches,
                max_total_steps,
            )
            .await;
        }

        let mut mission = mission;
        let branches = if auto_start_runtime && !background_runtime && should_schedule_runtime {
            self.run_mission_runtime(
                mission_id,
                &run_id,
                "mission_started",
                max_concurrent_branches,
                max_total_steps,
            )
            .await?;
            mission = self.require_mission(mission_id.as_str())?;
            self.repository().list_branches(
                None,
                Some(mission.id.as_str()),
                Some(run_id.as_str()),
            )?
        } else {
            branches
        };
        Ok(MissionStartResult {
            mission,
            run_id,
            branches,
        })
    }

    /// 提交一代受令牌守护的 Mission runtime，不持有请求所有权（Python
    /// `_submit_mission_runtime`）。
    pub(crate) async fn submit_mission_runtime(
        self: &Arc<Self>,
        mission_id: MissionId,
        run_id: RunId,
        trigger: &str,
        max_concurrent_branches: Option<i64>,
        max_total_steps: Option<i64>,
    ) {
        let runtime_token = RuntimeToken::new(());
        self.register_runtime_token(&run_id, Arc::clone(&runtime_token))
            .await;
        let manager = Arc::clone(self);
        let task_run_id = run_id.clone();
        let task_mission_id = mission_id.clone();
        let task_trigger = trigger.to_string();
        let task_token = Arc::clone(&runtime_token);
        let handle = self
            .task_backend()
            .submit(
                &format!(
                    "mission-runtime:{}:{}",
                    mission_id.as_str(),
                    run_id.as_str()
                ),
                Box::new(move || {
                    Box::pin(async move {
                        manager
                            .run_mission_runtime_safely(
                                &task_mission_id,
                                &task_run_id,
                                task_token,
                                &task_trigger,
                                max_concurrent_branches,
                                max_total_steps,
                            )
                            .await;
                    })
                }),
            )
            .await;
        self.register_runtime_handle_if_current(&run_id, &runtime_token, handle)
            .await;
    }

    /// 驱动一次 Mission runtime：策略板维护 + branch 循环（Python
    /// `_run_mission_runtime`）。
    ///
    /// # Errors
    /// Run 不存在或 branch runtime 失败。
    pub(crate) async fn run_mission_runtime(
        &self,
        mission_id: &MissionId,
        run_id: &RunId,
        trigger: &str,
        max_concurrent_branches: Option<i64>,
        max_total_steps: Option<i64>,
    ) -> Result<(), EngineError> {
        let run = self.require_run(run_id)?;
        self.maybe_auto_maintain_strategy_board(&run, trigger).await;
        self.run_branch_runtime(mission_id, run_id, max_concurrent_branches, max_total_steps)
            .await
    }

    /// 分离 runtime 的失败落盘（Python `_run_mission_runtime_safely`）。
    ///
    /// 异常分支只在令牌仍是当前代时写状态（旧代提交的迟到落盘不得覆盖
    /// 新代）；`finally` 清理同样以令牌为界。取消分支见模块文档：tokio
    /// abort 不给运行时落盘机会，"中断 → PAUSED"由 `pause_mission` 落盘。
    pub(crate) async fn run_mission_runtime_safely(
        &self,
        mission_id: &MissionId,
        run_id: &RunId,
        runtime_token: RuntimeToken,
        trigger: &str,
        max_concurrent_branches: Option<i64>,
        max_total_steps: Option<i64>,
    ) {
        if let Err(exc) = self
            .run_mission_runtime(
                mission_id,
                run_id,
                trigger,
                max_concurrent_branches,
                max_total_steps,
            )
            .await
        {
            if self.is_current_runtime_token(run_id, &runtime_token).await {
                match self.require_run(run_id) {
                    Ok(mut run) => {
                        let note = format!("Mission runtime failed: {exc}");
                        if let Err(persist) = self.fail_run(&mut run, &note).await {
                            tracing::warn!(
                                run = %run_id.as_str(),
                                error = %persist,
                                "分离 runtime 失败态落盘失败"
                            );
                        }
                        match self.require_run(run_id) {
                            Ok(run) => {
                                if let Err(sync) = self.sync_mission_status_from_run(&run) {
                                    tracing::warn!(
                                        run = %run_id.as_str(),
                                        error = %sync,
                                        "分离 runtime 状态同步失败"
                                    );
                                }
                            }
                            Err(reload) => {
                                tracing::warn!(
                                    run = %run_id.as_str(),
                                    error = %reload,
                                    "分离 runtime 重载 run 失败"
                                );
                            }
                        }
                    }
                    Err(load) => {
                        tracing::warn!(
                            run = %run_id.as_str(),
                            error = %load,
                            "分离 runtime 加载 run 失败"
                        );
                    }
                }
            }
        }
        self.clear_runtime_if_current(run_id, &runtime_token).await;
    }

    /// 为一个 Mission run epoch 创建可执行分支集（Python
    /// `_ensure_mission_run_branches`）。
    ///
    /// # Errors
    /// 策略板初始化、分支生成或批判评审失败。
    pub(crate) async fn ensure_mission_run_branches(
        &self,
        mission: &Mission,
        project: &Project,
        run: &AuditRun,
    ) -> Result<Vec<Branch>, EngineError> {
        let existing = self.repository().list_branches(
            None,
            Some(mission.id.as_str()),
            Some(run.id.as_str()),
        )?;
        if !existing.is_empty() {
            return Ok(existing);
        }
        let facts = self.repository().list_facts(project.id.as_str())?;
        let snapshot = self
            .get_or_create_strategy_board(
                project.id.as_str(),
                Some(&run.id),
                StrategyBoardDomain::General,
            )
            .await?;
        let hints: Vec<String> = self
            .repository()
            .list_hints(project.id.as_str())?
            .into_iter()
            .map(|hint| hint.text)
            .collect();
        // Retrieval Substrate：mission 语境 → 检索 → context packing，
        // 只把相关、多样、预算内的知识注入分支生成（不再是全量 IDs）。
        let knowledge = self.retrieve_branch_knowledge(mission, project, run);
        let runtime = self.provider_runtime().ok_or_else(|| {
            EngineError::ProviderConfigError(
                "provider runtime is not configured; branch generation needs a model"
                    .to_string(),
            )
        })?;
        let generated = match self
            .branch_generator
            .generate(
                runtime.as_ref(),
                "",
                &BranchGenerationInput {
                    mission,
                    project,
                    facts: &facts,
                    hints: &hints,
                    strategy_board_id: Some(&snapshot.id),
                    knowledge_results: &knowledge.results,
                    retrieval_invocation_id: knowledge.invocation_id.as_ref(),
                    run_id: Some(&run.id),
                },
            )
            .await
        {
            Ok(branches) => branches,
            Err(error) => {
                // 兜底：模型/校验失败**绝不**让 mission 僵死——错误只降级，
                // 原始目标随后成为唯一分支。
                tracing::warn!(
                    error = %error,
                    "branch generation failed; degrading to the raw mission goal as the sole branch"
                );
                Vec::new()
            }
        };
        let known_fact_ids: Vec<String> = facts
            .iter()
            .map(|fact| fact.id.as_str().to_string())
            .collect();
        let selected = self.apply_initial_branch_limit(generated, run);
        let admitted = self
            .critique_and_admit_branches(selected, &known_fact_ids, run)
            .await?;
        if admitted.is_empty() {
            // 生成失败 / 空集 / 批判全拒：三条路都通向同一个死局——mission
            // 处于 running 但没有任何可执行分支（实测过的僵尸态）。兜底
            // 分支保证"永远有活儿干"。
            let fallback = self.deterministic_goal_branch(mission, run);
            let created = self.repository().create_branch(&fallback)?;
            self.record_event_safe(EventDraft {
                run_id: Some(&run.id),
                status: Some("fallback"),
                data: Some(Map::from_iter([
                    (
                        "branch_id".to_string(),
                        Value::String(created.id.as_str().to_string()),
                    ),
                    (
                        "branch_kind".to_string(),
                        Value::String(
                            created
                                .metadata
                                .get("branch_kind")
                                .and_then(Value::as_str)
                                .unwrap_or("mixed.classification")
                                .to_string(),
                        ),
                    ),
                ])),
                ..EventDraft::new(
                    &run.project_id,
                    AuditEventType::UserNote,
                    "branch_generator",
                    "Branch generation yielded nothing admissible; the raw mission goal became the sole branch",
                )
            })
            .await;
            return Ok(vec![created]);
        }
        Ok(admitted)
    }

    /// 确定性兜底分支：原始 mission 目标原样成为唯一可执行分支。
    ///
    /// kind 按目标形态选：有 url/domain/host → `mixed.initial_surface`
    /// （capability_router 路由到 asset_recon / web_recon，直接干真活）；
    /// 纯模糊输入 → `mixed.classification`（Direct API 分类路径，不依赖
    /// 外部 worker、不要求目标形态，让模型先把输入定性，结果入白板供
    /// 下一轮收口链使用）。
    ///
    /// 兜底分支**绕过批判**：批判再拒就回到零分支僵死态，而一个不完美的
    /// 假设远好于没有假设；`fallback` / `fallback_reason` 元数据与审计
    /// 事件如实记录来源，绝不伪装成模型产出。
    pub(crate) fn deterministic_goal_branch(&self, mission: &Mission, run: &AuditRun) -> Branch {
        let branch_kind = if target_has_real_locator(&mission.target) {
            "mixed.initial_surface"
        } else {
            "mixed.classification"
        };
        let title = {
            let compact = mission.user_goal.split_whitespace().collect::<Vec<_>>().join(" ");
            if compact.is_empty() {
                format!("{} fallback", branch_kind)
            } else {
                compact.chars().take(80).collect()
            }
        };
        let mut branch = Branch::new(
            mission.project_id.clone(),
            mission.id.clone(),
            title,
            format!(
                "It is unverified whether the mission goal is satisfiable as stated: {}",
                mission.user_goal
            ),
        );
        branch.run_id = Some(run.id.clone());
        branch.created_by = "deterministic_fallback".to_string();
        branch.metadata = Map::from_iter([
            ("branch_kind".to_string(), Value::String(branch_kind.to_string())),
            ("fallback".to_string(), Value::Bool(true)),
            (
                "fallback_reason".to_string(),
                Value::String(
                    "model branch generation failed, returned an empty set, or every \
                     candidate was rejected by critique"
                        .to_string(),
                ),
            ),
        ]);
        branch
    }

    /// 检索并打包分支生成所需的知识（Retrieval Substrate 主路）。
    ///
    /// 查询文本 = user_goal + success_criteria + tags（mission 语境的
    /// 确定性投影）；检索失败不阻塞 mission（§41）：任何仓储错误都
    /// 降级为"无知识注入"，并记录 warn trace。
    fn retrieve_branch_knowledge(
        &self,
        mission: &Mission,
        project: &Project,
        run: &AuditRun,
    ) -> BranchKnowledge {
        let mut query_text = mission.user_goal.clone();
        for criterion in &mission.success_criteria {
            query_text.push(' ');
            query_text.push_str(criterion);
        }
        for tag in &mission.tags {
            query_text.push(' ');
            query_text.push_str(tag);
        }
        let mut query = models::KnowledgeRetrievalQuery::new();
        query.text = query_text;
        query.limit = 24;
        let started = Instant::now();
        let retriever = RepositoryKnowledgeRetriever {
            repository: self.repository().as_ref(),
        };
        let retrieval = retriever.retrieve(&query);

        let mut invocation = RetrievalInvocation::new(project.id.clone(), query.text.clone());
        invocation.run_id = Some(run.id.clone());
        invocation.purpose = "mission_branch_generation".to_string();
        invocation.query_hash = Some(format!("{:x}", Sha256::digest(query.text.as_bytes())));
        invocation.top_k = query.limit;
        invocation.fetch_multiplier = 4;

        let results = match retrieval {
            Ok(results) => results,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "knowledge retrieval unavailable; branch generation continues without knowledge"
                );
                invocation.status = RetrievalStatus::Unavailable;
                invocation.reason = "knowledge_retrieval_unavailable".to_string();
                invocation.error = Some(error.to_string());
                invocation.duration_ms = i64::try_from(started.elapsed().as_millis()).ok();
                let invocation_id = self.persist_knowledge_retrieval(&invocation);
                return BranchKnowledge {
                    results: Vec::new(),
                    invocation_id,
                };
            }
        };

        let options = storage::PackOptions::default();
        let packed = storage::pack_knowledge(&results, &options);
        let selected: Vec<models::KnowledgeRetrievalResult> = packed
            .into_iter()
            .map(|packed| {
                let mut result = packed.item.clone();
                result.card.summary.clone_from(&packed.injection_text);
                result.card.content = packed.injection_text;
                if let Some(note) = packed.pack_note {
                    result.retrieval_reason = Some(
                        result
                            .retrieval_reason
                            .map_or(note.clone(), |reason| format!("{reason}; {note}")),
                    );
                }
                result
            })
            .collect();

        invocation.status = if selected.is_empty() {
            RetrievalStatus::Empty
        } else {
            RetrievalStatus::Hit
        };
        invocation.reason = if results.is_empty() {
            "no_matching_knowledge"
        } else if results
            .iter()
            .any(|result| result.retrieval_reason.as_deref() == Some("fallback_linear_scan"))
        {
            "fallback_linear_scan"
        } else if selected.is_empty() {
            "no_injectable_knowledge"
        } else {
            "fts5_bm25"
        }
        .to_string();
        invocation.token_budget = i64::try_from(options.char_budget / 4).unwrap_or(i64::MAX);
        invocation.candidate_count = i64::try_from(results.len()).unwrap_or(i64::MAX);
        invocation.filtered_count = i64::try_from(selected.len()).unwrap_or(i64::MAX);
        invocation.max_score = results.first().map_or(0.0, |result| result.score);
        invocation.retrieved = results
            .iter()
            .enumerate()
            .map(|(index, result)| {
                let selected_result = selected
                    .iter()
                    .find(|selected| selected.card.id == result.card.id);
                let mut location = Map::new();
                if let Some(locator) = &result.card.source_locator {
                    location.insert("locator".to_string(), Value::String(locator.clone()));
                }
                let mut metadata = Map::new();
                metadata.insert(
                    "matched_terms".to_string(),
                    serde_json::to_value(&result.matched_terms).unwrap_or(Value::Null),
                );
                metadata.insert(
                    "retrieval_reason".to_string(),
                    result
                        .retrieval_reason
                        .clone()
                        .map_or(Value::Null, Value::String),
                );
                metadata.insert(
                    "selected_for_context".to_string(),
                    Value::Bool(selected_result.is_some()),
                );
                if let Some(source) = &result.card.source {
                    metadata.insert("source_id".to_string(), Value::String(source.clone()));
                }
                if let Some(hash) = &result.card.content_hash {
                    metadata.insert("content_hash".to_string(), Value::String(hash.clone()));
                }
                if let Some(selected_result) = selected_result {
                    if selected_result.retrieval_reason != result.retrieval_reason {
                        metadata.insert(
                            "packing_reason".to_string(),
                            selected_result
                                .retrieval_reason
                                .clone()
                                .map_or(Value::Null, Value::String),
                        );
                    }
                }
                RetrievedEvidence {
                    chunk_id: RetrievalChunkId::new(format!(
                        "knowledge:{}",
                        result.card.id.as_str()
                    )),
                    source_kind: RetrievalSourceKind::KnowledgeCard,
                    source_id: result.card.id.as_str().to_string(),
                    score: result.score,
                    rank: i64::try_from(index).unwrap_or(i64::MAX).saturating_add(1),
                    snippet: result.card.effective_summary().chars().take(600).collect(),
                    title: result.card.title.clone(),
                    artifact_record_id: None,
                    artifact_uri: None,
                    artifact_sha256: None,
                    evidence_id: None,
                    finding_id: None,
                    tool_invocation_id: None,
                    model_invocation_id: None,
                    location,
                    metadata,
                }
            })
            .collect();
        invocation.duration_ms = i64::try_from(started.elapsed().as_millis()).ok();
        let invocation_id = self.persist_knowledge_retrieval(&invocation);
        BranchKnowledge {
            results: selected,
            invocation_id,
        }
    }

    fn persist_knowledge_retrieval(
        &self,
        invocation: &RetrievalInvocation,
    ) -> Option<RetrievalInvocationId> {
        match self.repository().add_retrieval_invocation(invocation) {
            Ok(saved) => Some(saved.id),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    retrieval_invocation = %invocation.id,
                    "knowledge retrieval telemetry could not be persisted"
                );
                None
            }
        }
    }

    /// 落盘批判裁决，只收容可执行假设（Python
    /// `_critique_and_admit_branches`）。
    ///
    /// 需修订的分支用改写假设重审一轮；裁决与分支元数据合并落盘。
    ///
    /// # Errors
    /// 仓储写入失败。
    pub(crate) async fn critique_and_admit_branches(
        &self,
        branches: Vec<Branch>,
        known_fact_ids: &[String],
        run: &AuditRun,
    ) -> Result<Vec<Branch>, EngineError> {
        let mut admitted: Vec<Branch> = Vec::new();
        let mut report_ids: Vec<String> = Vec::new();
        let mut rejected_branch_ids: Vec<String> = Vec::new();
        let generated_count = branches.len();

        for mut branch in branches {
            let report = self.critique_agent.review(&CritiqueInput {
                branch: &branch,
                known_fact_ids,
                contract: None,
            });
            let mut report = self.repository().add_critique_report(&report)?;
            report_ids.push(report.id.as_str().to_string());

            if !report.admitted() {
                if let Some(restated) = report.restated_hypothesis.clone() {
                    let original_hypothesis = branch.hypothesis.clone();
                    branch.hypothesis = restated;
                    branch.metadata.insert(
                        "original_hypothesis".to_string(),
                        Value::String(original_hypothesis),
                    );
                    branch.metadata.insert(
                        "revised_by_critique_id".to_string(),
                        Value::String(report.id.as_str().to_string()),
                    );
                    let revised = self.critique_agent.review(&CritiqueInput {
                        branch: &branch,
                        known_fact_ids,
                        contract: None,
                    });
                    let revised = self.repository().add_critique_report(&revised)?;
                    report_ids.push(revised.id.as_str().to_string());
                    report = revised;
                }
            }

            if report.admitted() {
                branch.metadata.insert(
                    "critique_report_id".to_string(),
                    Value::String(report.id.as_str().to_string()),
                );
                branch.metadata.insert(
                    "critique_verdict".to_string(),
                    Value::String(report.verdict.as_str().to_string()),
                );
                admitted.push(self.repository().create_branch(&branch)?);
            } else {
                rejected_branch_ids.push(branch.id.as_str().to_string());
            }
        }

        self.record_event_safe(EventDraft {
            run_id: Some(&run.id),
            status: Some("completed"),
            data: Some(Map::from_iter([
                (
                    "generated_count".to_string(),
                    Value::from(generated_count as i64),
                ),
                (
                    "admitted_count".to_string(),
                    Value::from(admitted.len() as i64),
                ),
                (
                    "rejected_count".to_string(),
                    Value::from(rejected_branch_ids.len() as i64),
                ),
                (
                    "critique_report_ids".to_string(),
                    serde_json::to_value(&report_ids).unwrap_or(Value::Null),
                ),
                (
                    "rejected_branch_ids".to_string(),
                    serde_json::to_value(&rejected_branch_ids).unwrap_or(Value::Null),
                ),
            ])),
            ..EventDraft::new(
                &run.project_id,
                AuditEventType::UserNote,
                "critique_agent",
                "Generated hypotheses critiqued",
            )
        })
        .await;
        Ok(admitted)
    }

    // -- run 容器与复杂度策略 -------------------------------------------------

    /// 创建 Mission/Branch runtime 的 run 容器（Python
    /// `_create_mission_run`）。
    ///
    /// 复杂度策略在 Manager 内重推导：调用方给的限额只能收紧不能放宽。
    ///
    /// # Errors
    /// run 配置非法（复杂度/审批模式/步数预算/provider 引用，422 族）或
    /// 仓储写入失败。
    pub(crate) async fn create_mission_run(
        &self,
        mission: &Mission,
        config: Map<String, Value>,
        status: RunStatus,
    ) -> Result<AuditRun, EngineError> {
        let mut run_config = Map::from_iter([
            (
                "mission_id".to_string(),
                Value::String(mission.id.as_str().to_string()),
            ),
            (
                "strategy_board".to_string(),
                serde_json::json!({"auto_maintain": true}),
            ),
        ]);
        for (key, value) in config {
            run_config.insert(key, value);
        }
        let raw_approval_mode = run_config
            .get("approval_mode")
            .cloned()
            .unwrap_or(Value::String(mission.approval_mode.as_str().to_string()));
        let approval_mode = match &raw_approval_mode {
            Value::String(text) => ApprovalMode::parse(text).map_err(|_| {
                EngineError::Value(format!(
                    "invalid approval_mode: {}",
                    crate::mission_lifecycle::py_str_repr(text)
                ))
            })?,
            other => {
                return Err(EngineError::Value(format!(
                    "invalid approval_mode: {other}"
                )));
            }
        };
        run_config.insert(
            "approval_mode".to_string(),
            Value::String(approval_mode.as_str().to_string()),
        );
        let max_total_steps = Self::resolve_max_total_steps(&run_config)?;
        let resolved_provider_id = self.resolve_provider_id(&run_config)?;
        if let Some(resolved_provider_id) = resolved_provider_id {
            run_config.insert(
                "resolved_provider_id".to_string(),
                Value::String(resolved_provider_id),
            );
        }
        let mut run = AuditRun::new(mission.project_id.clone());
        run.mission_id = Some(mission.id.clone());
        run.status = status;
        run.config = run_config;
        run.max_total_steps = max_total_steps;
        if run.status == RunStatus::Running {
            run.started_at = Some(utcnow());
        }
        Ok(self.repository().create_run(&run)?)
    }

    /// 约束初始扇出（Python `_apply_initial_complexity_policy` 的排序/截断
    /// 部分）：按 (priority, confidence, created_at) 降序取前 `limit` 个。
    ///
    /// 删除复杂度分档后，`limit` 只来自 run config 的 `max_initial_branches`：
    /// Planner 拆出多少意图就扇出多少，缺省不截断。分支自身的
    /// `budget_steps` 保持模型默认，运行时不覆盖。
    #[must_use]
    pub(crate) fn apply_initial_branch_limit(&self, branches: Vec<Branch>, run: &AuditRun) -> Vec<Branch> {
        let mut selected = branches;
        // Python `sorted(key=(priority, confidence, created_at), reverse=True)`：
        // 三键降序。confidence 是 f64（无全序），NaN 不可能出现（模型侧
        // 有界），比较退化按相等处理。
        selected.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| {
                    b.confidence
                        .partial_cmp(&a.confidence)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        if let Some(limit) = config_int(&run.config, "max_initial_branches") {
            selected.truncate(limit.max(1) as usize);
        }
        selected
    }

    /// 校验并返回 run 总步数预算（Python `_resolve_max_total_steps`）：缺省
    /// 取模型默认（64）。
    ///
    /// # Errors
    /// 非 int 或非正数（Python `BudgetConfigError`，映射 422）。
    pub(crate) fn resolve_max_total_steps(config: &Map<String, Value>) -> Result<i64, EngineError> {
        let Some(raw) = config.get("max_total_steps") else {
            return Ok(64);
        };
        match raw {
            Value::Number(number) if number.is_i64() || number.is_u64() => {
                let value = number
                    .as_i64()
                    .unwrap_or_else(|| number.as_u64().map_or(0, |v| v as i64));
                if value < 1 {
                    Err(EngineError::BudgetConfigError(
                        "config['max_total_steps'] must be >= 1".to_string(),
                    ))
                } else {
                    Ok(value)
                }
            }
            _ => Err(EngineError::BudgetConfigError(
                "config['max_total_steps'] must be a positive integer".to_string(),
            )),
        }
    }

    /// 校验/解析 provider 引用（Python `_resolve_provider_id`）：显式
    /// provider_id 视作用户意图必须指向启用的配置；缺省回落到唯一启用的
    /// default provider；纯工具扫描无 provider 也能跑。
    ///
    /// # Errors
    /// provider 引用非法/不存在/未启用（Python `ProviderConfigError`）。
    pub(crate) fn resolve_provider_id(
        &self,
        config: &Map<String, Value>,
    ) -> Result<Option<String>, EngineError> {
        if let Some(raw) = config.get("provider_id") {
            let Value::String(provider_id) = raw else {
                return Err(EngineError::ProviderConfigError(
                    "config['provider_id'] must be a non-empty string".to_string(),
                ));
            };
            if provider_id.is_empty() {
                return Err(EngineError::ProviderConfigError(
                    "config['provider_id'] must be a non-empty string".to_string(),
                ));
            }
            let Some(provider) = self.repository().get_provider(provider_id)? else {
                return Err(EngineError::ProviderConfigError(format!(
                    "unknown provider: {provider_id}"
                )));
            };
            if !provider.enabled {
                return Err(EngineError::ProviderConfigError(format!(
                    "provider disabled: {provider_id}"
                )));
            }
            return Ok(Some(provider.id.as_str().to_string()));
        }
        let defaults: Vec<_> = self
            .repository()
            .list_providers()?
            .into_iter()
            .filter(|provider| provider.is_default && provider.enabled)
            .collect();
        if defaults.is_empty() {
            return Ok(None);
        }
        Ok(Some(defaults[0].id.as_str().to_string()))
    }

    // -- runtime 持久化 ------------------------------------------------------

    /// 把 run 落盘为 FAILED 并附说明（Python `_fail_run`）。
    ///
    /// 事件记录 best-effort：失败落盘本身绝不再抛。
    ///
    /// # Errors
    /// run 持久化失败。
    pub(crate) async fn fail_run(&self, run: &mut AuditRun, note: &str) -> Result<(), EngineError> {
        run.status = RunStatus::Failed;
        run.note = Some(note.to_string());
        run.finished_at = Some(utcnow());
        run.updated_at = utcnow();
        // run 可能尚未持久化（config 错误发生在 create 之前）；update_run
        // 是 upsert，两种情形都安全。
        self.repository().update_run(run)?;
        self.record_event_safe(EventDraft {
            run_id: Some(&run.id),
            message: Some(note),
            status: Some("failed"),
            ..EventDraft::new(
                &run.project_id,
                AuditEventType::RunFailed,
                "manager",
                "审计运行失败",
            )
        })
        .await;
        self.record_narrative_safe(NarrativeDraft {
            run_id: Some(&run.id),
            mission_id: run.mission_id.as_ref(),
            ..NarrativeDraft::new(
                &run.project_id,
                "manager",
                models::AgentNarrativeEventKind::FailureAnalysis,
                note,
            )
        });
        Ok(())
    }

    /// 把 run 终态同步进 Mission 状态（Python `_sync_mission_status_from_run`）。
    ///
    /// # Errors
    /// 仓储写入失败。
    pub(crate) fn sync_mission_status_from_run(&self, run: &AuditRun) -> Result<(), EngineError> {
        let Some(mission_id) = &run.mission_id else {
            return Ok(());
        };
        let Some(mut mission) = self.repository().get_mission(mission_id.as_str())? else {
            return Ok(());
        };
        let next_status = match run.status {
            RunStatus::Running => Some(MissionStatus::Running),
            RunStatus::Paused => Some(MissionStatus::Paused),
            RunStatus::WaitingForDecision => Some(MissionStatus::WaitingForDecision),
            RunStatus::Completed => Some(MissionStatus::Completed),
            RunStatus::Failed => Some(MissionStatus::Failed),
            RunStatus::Cancelled => Some(MissionStatus::Cancelled),
            RunStatus::Pending | RunStatus::Reviewing | RunStatus::Reporting => None,
        };
        let Some(next_status) = next_status else {
            return Ok(());
        };
        mission.status = next_status;
        mission.finished_at = if matches!(
            run.status,
            RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
        ) {
            run.finished_at
        } else {
            None
        };
        mission.updated_at = utcnow();
        self.persist_mission_notifying(&mission)?;
        Ok(())
    }

    // -- 策略板 ------------------------------------------------------------

    /// 取最新策略板快照，缺则建空板（Python `get_or_create_strategy_board`）。
    ///
    /// # Errors
    /// Project/Run 不存在、run 不属于该 Project 或仓储写入失败。
    pub async fn get_or_create_strategy_board(
        &self,
        project_id: &str,
        run_id: Option<&RunId>,
        domain_profile: StrategyBoardDomain,
    ) -> Result<StrategyBoardSnapshot, EngineError> {
        self.require_project(project_id)?;
        if let Some(run_id) = run_id {
            let run = self.require_run(run_id)?;
            if run.project_id.as_str() != project_id {
                return Err(EngineError::RunNotFound(format!(
                    "audit run {} does not belong to {project_id}",
                    run_id.as_str()
                )));
            }
        }
        if let Some(latest) = self.latest_strategy_board(project_id, run_id)? {
            return Ok(latest);
        }
        let snapshot = self.strategy_board.empty_snapshot(
            &models::ProjectId::new(project_id.to_string()),
            run_id,
            domain_profile,
            "bootstrap",
        );
        let saved = self.repository().add_strategy_board_snapshot(&snapshot)?;
        self.record_event_safe(EventDraft {
            run_id,
            data: Some(Map::from_iter([(
                "strategy_board_snapshot_id".to_string(),
                Value::String(saved.id.as_str().to_string()),
            )])),
            ..EventDraft::new(
                &saved.project_id,
                AuditEventType::UserNote,
                "strategy_board",
                "Strategy Board initialized",
            )
        })
        .await;
        Ok(saved)
    }

    /// 最新策略板快照（按 (version, created_at) 取末位）（Python
    /// `_latest_strategy_board`）。
    ///
    /// # Errors
    /// 仓储读取失败。
    fn latest_strategy_board(
        &self,
        project_id: &str,
        run_id: Option<&RunId>,
    ) -> Result<Option<StrategyBoardSnapshot>, EngineError> {
        let mut snapshots = self
            .repository()
            .list_strategy_board_snapshots(project_id, run_id.map(RunId::as_str))?;
        if snapshots.is_empty() {
            return Ok(None);
        }
        snapshots.sort_by_key(|item| (item.version, item.created_at));
        Ok(snapshots.pop())
    }

    /// 可选刷新策略板状态而不阻塞审计进度（Python
    /// `_maybe_auto_maintain_strategy_board`）：sidecar 失败只记事件。
    pub(crate) async fn maybe_auto_maintain_strategy_board(
        &self,
        run: &AuditRun,
        trigger: &str,
    ) -> Option<StrategyBoardSnapshot> {
        let settings = strategy_board_auto_settings(&run.config)?;
        let mut data = Map::from_iter([
            ("trigger".to_string(), Value::String(trigger.to_string())),
            ("auto".to_string(), Value::Bool(true)),
        ]);

        if self.provider_runtime().is_none() {
            self.record_strategy_board_auto_event(
                &run.project_id,
                &run.id,
                "skipped",
                "Strategy Board auto-maintenance skipped",
                Some("provider runtime is not configured"),
                Some(data),
            )
            .await;
            return None;
        }

        let provider_id = settings.get("provider_id").and_then(Value::as_str);
        let provider_id = provider_id.filter(|id| !id.is_empty()).map(str::to_string);
        let token_budget = config_int(&run.config, "strategy_board.token_budget").unwrap_or(2048);
        let domain_profile = settings
            .get("domain_profile")
            .and_then(Value::as_str)
            .and_then(|raw| StrategyBoardDomain::parse(raw).ok())
            .unwrap_or(StrategyBoardDomain::General);

        match self
            .run_strategy_board_maintainer(
                run.project_id.as_str(),
                run.id.as_str(),
                provider_id.as_deref(),
                domain_profile,
                trigger,
                token_budget,
            )
            .await
        {
            Ok(snapshot) => {
                data.insert(
                    "strategy_board_snapshot_id".to_string(),
                    Value::String(snapshot.id.as_str().to_string()),
                );
                self.record_strategy_board_auto_event(
                    &run.project_id,
                    &run.id,
                    "succeeded",
                    "Strategy Board auto-maintenance completed",
                    None,
                    Some(data),
                )
                .await;
                Some(snapshot)
            }
            Err(exc) => {
                let status = if matches!(exc, EngineError::ProviderConfigError(_)) {
                    "skipped"
                } else {
                    "failed"
                };
                let title = if status == "skipped" {
                    "Strategy Board auto-maintenance skipped"
                } else {
                    "Strategy Board auto-maintenance failed"
                };
                self.record_strategy_board_auto_event(
                    &run.project_id,
                    &run.id,
                    status,
                    title,
                    Some(&exc.to_string()),
                    Some(data),
                )
                .await;
                None
            }
        }
    }

    /// 记录策略板自动维护事件（Python `_record_strategy_board_auto_event`）。
    async fn record_strategy_board_auto_event(
        &self,
        project_id: &models::ProjectId,
        run_id: &RunId,
        status: &str,
        title: &str,
        message: Option<&str>,
        data: Option<Map<String, Value>>,
    ) {
        self.record_event_safe(EventDraft {
            run_id: Some(run_id),
            message,
            status: Some(status),
            data: data.or_else(|| Some(Map::new())),
            ..EventDraft::new(
                project_id,
                AuditEventType::UserNote,
                "strategy_board",
                title,
            )
        })
        .await;
    }

    /// 调用配置的 provider 作为策略板维护者并应用其操作（Python
    /// `run_strategy_board_maintainer`）。
    ///
    /// # Errors
    /// provider 运行时未配置（422 族）或尚未移植。
    async fn run_strategy_board_maintainer(
        &self,
        project_id: &str,
        run_id: &str,
        provider_id: Option<&str>,
        domain_profile: StrategyBoardDomain,
        trigger: &str,
        _token_budget: i64,
    ) -> Result<StrategyBoardSnapshot, EngineError> {
        let provider_runtime = self.provider_runtime().ok_or_else(|| {
            EngineError::ProviderConfigError("provider runtime is not configured".to_string())
        })?;
        let project = self.require_project(project_id)?;
        let run = self.require_run(&RunId::new(run_id.to_string()))?;
        if run.project_id.as_str() != project_id {
            return Err(EngineError::RunNotFound(format!(
                "audit run {run_id} does not belong to {project_id}"
            )));
        }
        let snapshot = self
            .get_or_create_strategy_board(project_id, Some(&run.id), domain_profile)
            .await?;
        let context_pack = self
            .repository()
            .list_context_packs(project_id, Some(run_id))?
            .into_iter()
            .last();
        let recent_activity = self
            .repository()
            .list_events(project_id, Some(run_id), 20, None)?
            .into_iter()
            .map(|event| format!("{}: {}", event.event_type.as_str(), event.title))
            .collect::<Vec<_>>();
        let payload = self.strategy_board.build_prompt_payload(&PromptInput {
            project: &project,
            run: Some(&run),
            snapshot: Some(&snapshot),
            context_pack: context_pack.as_ref(),
            domain_profile,
            trigger,
            recent_activity: &recent_activity,
            max_knowledge_cards: 6,
            knowledge_cards: None,
        });
        // 显式钉定（settings / config.provider_id）才解析校验；未钉定传空
        // 串，由网关按 `strategy_board_maintainer` 用途路由（路由表优先，
        // 回落默认 provider）。
        let provider_id = if provider_id.is_some() || run.config.get("provider_id").is_some() {
            provider_id
                .map(str::to_string)
                .or(self.resolve_provider_id(&run.config)?)
        } else {
            None
        }
        .unwrap_or_default();
        let ops = self
            .strategy_board
            .propose_ops(provider_runtime.as_ref(), &provider_id, &payload)
            .await
            .map_err(|error| EngineError::ProviderConfigError(error.to_string()))?;
        let applied = self
            .strategy_board
            .apply_ops(&ApplyOpsInput {
                base: &snapshot,
                ops: &ops,
                trigger,
                provider_id: Some(&models::ProviderId::new(provider_id)),
                model_invocation_id: None,
                created_by: "strategy_board_maintainer",
            })
            .map_err(|error| EngineError::Value(error.to_string()))?;
        let saved = self.repository().add_strategy_board_snapshot(&applied)?;
        if !ops.ops.is_empty() {
            let text = ops
                .ops
                .iter()
                .map(|op| {
                    let detail = op
                        .reason
                        .as_deref()
                        .or(op.content.as_deref())
                        .unwrap_or("no detail");
                    format!("{}: {detail}", strategy_board_op_label(op.op_type))
                })
                .collect::<Vec<_>>()
                .join(" | ");
            self.record_narrative_safe(NarrativeDraft {
                run_id: Some(&run.id),
                mission_id: run.mission_id.as_ref(),
                metadata: Some(Map::from_iter([(
                    "trigger".to_string(),
                    Value::String(trigger.to_string()),
                )])),
                ..NarrativeDraft::new(
                    &project.id,
                    "strategy_board_maintainer",
                    models::AgentNarrativeEventKind::AdvisorNote,
                    &text,
                )
            });
        }
        Ok(saved)
    }

    /// 应用策略板操作序列（Python `apply_strategy_board_ops`）。
    ///
    /// # Errors
    /// 未移植（`_record_strategy_board_directive` 依赖它降级记 skipped
    /// 事件，与 Python 失败路径同形）。
    pub(crate) async fn apply_strategy_board_ops(
        &self,
        project_id: &str,
        run_id: &RunId,
        content: &str,
    ) -> Result<StrategyBoardSnapshot, EngineError> {
        let project = self.require_project(project_id)?;
        let run = self.require_run(run_id)?;
        if run.project_id.as_str() != project_id {
            return Err(EngineError::RunNotFound(format!(
                "audit run {} does not belong to {project_id}",
                run_id.as_str()
            )));
        }
        let snapshot = self
            .get_or_create_strategy_board(project_id, Some(run_id), StrategyBoardDomain::General)
            .await?;
        let value: Value = serde_json::from_str(content)
            .map_err(|error| EngineError::Value(format!("invalid strategy board ops: {error}")))?;
        let ops: models::strategy_board::StrategyBoardOps = serde_json::from_value(value)
            .map_err(|error| EngineError::Value(format!("invalid strategy board ops: {error}")))?;
        let applied = self
            .strategy_board
            .apply_ops(&ApplyOpsInput::new(&snapshot, &ops))
            .map_err(|error| EngineError::Value(error.to_string()))?;
        let _ = project;
        Ok(self.repository().add_strategy_board_snapshot(&applied)?)
    }
}

/// 策略板操作类型的 snake_case 标签（serde 重命名是唯一事实源）。
fn strategy_board_op_label(op_type: models::strategy_board::StrategyBoardOpType) -> String {
    serde_json::to_value(op_type)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{op_type:?}"))
}

/// run config 里的 int 值（Python `isinstance(value, int) and not
/// isinstance(value, bool)` 的镜像：bool 不算 int，float 不算 int）。
fn config_int(config: &Map<String, Value>, key: &str) -> Option<i64> {
    match config.get(key) {
        Some(Value::Number(number)) if number.is_i64() || number.is_u64() => number.as_i64(),
        _ => None,
    }
}

/// 策略板自动维护配置（Python `_strategy_board_auto_settings`）：
/// `strategy_board` 对象且 `auto_maintain`/`auto_update` 恰为 `true`。
fn strategy_board_auto_settings(config: &Map<String, Value>) -> Option<Map<String, Value>> {
    let raw = config.get("strategy_board")?;
    let Value::Object(settings) = raw else {
        return None;
    };
    let enabled = settings
        .get("auto_maintain")
        .or_else(|| settings.get("auto_update"))
        .cloned()
        .unwrap_or(Value::Bool(false));
    if enabled != Value::Bool(true) {
        return None;
    }
    Some(settings.clone())
}

// 抑制未使用告警：`Timestamp` 供后续 M5d-7/8 的收口与终止评估使用，
// `AuditEvent` 保持导入面完整以便后续模块增量扩展时不重排 use 块。
#[allow(dead_code)]
type _Reserved = (Timestamp, AuditEvent);

/// 目标里有没有**真实的**定位符（键名 + 值形态双重校验）。
///
/// 只看键名会被模型的占位值骗过——实测 intake 模型对"你好"回过
/// `{"domain": "未指定"}`：键名像定位符，值什么都不是。占位值按
/// 无定位符处理（兜底走分类定性，而不是派 asset_recon 去解析
/// "未指定"）。
fn target_has_real_locator(target: &models::StrMap) -> bool {
    const PLACEHOLDER_VALUES: &[&str] = &[
        "", "未指定", "待定", "待补充", "无", "unspecified", "unknown", "n/a", "na", "none", "null",
        "tbd", "todo",
    ];
    target.iter().any(|(key, value)| {
        if !matches!(
            key,
            "url" | "domain" | "host" | "target_url" | "base_url" | "target_domain" | "hostname"
        ) {
            return false;
        }
        let value = value.trim();
        if PLACEHOLDER_VALUES
            .iter()
            .any(|placeholder| value.eq_ignore_ascii_case(placeholder))
        {
            return false;
        }
        value.contains("://") || value.contains('.') || value.parse::<std::net::IpAddr>().is_ok()
    })
}

#[cfg(test)]
mod fallback_tests {    use models::AuditRun;
    use models::Mission;
    use models::mission::BranchStatus;
    use serde_json::json;
    use models::ProjectId;

    use super::*;

    fn fallback_manager() -> (AuditManager, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("临时目录必须可创建");
        let repo = storage::SqliteRepository::open(dir.path().join("fb.sqlite3"))
            .expect("库必须可打开");
        let manager = AuditManager::new(
            std::sync::Arc::new(repo),
            agents::solver::SolverRegistry::new(),
            std::sync::Arc::new(crate::task_backend::InMemoryTaskBackend::default()),
        );
        (manager, dir)
    }

    fn mission_with_target(goal: &str, target: serde_json::Value) -> Mission {
        let mut mission = Mission::new(ProjectId::new("probe_1".to_string()), goal.to_string());
        mission.target = target
            .as_object()
            .cloned()
            .map(|map| {
                map.into_iter()
                    .filter_map(|(key, value)| value.as_str().map(|text| (key, text.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        mission
    }

    #[test]
    fn vague_goal_falls_back_to_classification_branch() {
        let (manager, _dir) = fallback_manager();
        let mission = mission_with_target("你好", json!({"raw_prompt": "你好"}));
        let run = AuditRun::new(mission.project_id.clone());
        let branch = manager.deterministic_goal_branch(&mission, &run);
        assert_eq!(
            branch.metadata.get("branch_kind").and_then(Value::as_str),
            Some("mixed.classification"),
            "无定位符的模糊目标必须走分类定性，而不是猜一个洞"
        );
        assert_eq!(
            branch.metadata.get("fallback").and_then(Value::as_bool),
            Some(true)
        );
        assert!(
            branch.metadata.get("fallback_reason").is_some(),
            "来源必须如实记录"
        );
        assert_eq!(branch.created_by, "deterministic_fallback");
        assert_eq!(branch.status, BranchStatus::Proposed);
        assert!(branch.budget_steps >= 1);
        assert!(
            branch.hypothesis.contains("你好"),
            "假设必须引用原始目标"
        );
    }

    #[test]
    fn locator_goal_falls_back_to_initial_surface_branch() {
        let (manager, _dir) = fallback_manager();
        let mission = mission_with_target(
            "审计 https://shop.example.test",
            json!({"url": "https://shop.example.test"}),
        );
        let run = AuditRun::new(mission.project_id.clone());
        let branch = manager.deterministic_goal_branch(&mission, &run);
        assert_eq!(
            branch.metadata.get("branch_kind").and_then(Value::as_str),
            Some("mixed.initial_surface"),
            "有真实定位符必须直接干真活（router → asset_recon/web_recon）"
        );
    }

    #[test]
    fn placeholder_domain_value_is_not_a_locator() {
        let (manager, _dir) = fallback_manager();
        // 实测形态：intake 模型对"你好"回过 {"domain": "未指定"}。
        let mission = mission_with_target("你好", json!({"domain": "未指定"}));
        let run = AuditRun::new(mission.project_id.clone());
        let branch = manager.deterministic_goal_branch(&mission, &run);
        assert_eq!(
            branch.metadata.get("branch_kind").and_then(Value::as_str),
            Some("mixed.classification"),
            "占位值不是定位符：必须走分类定性，而不是派 asset_recon 去解析'未指定'"
        );
    }
}