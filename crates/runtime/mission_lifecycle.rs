//! Mission 生命周期 —— `manager.py` 的 project bootstrap、Mission CRUD 与
//! 用户指令应用部分。
//!
//! 单一写入路径红线在此落地：API 与其他控制面**必须**经由本模块方法写
//! Mission/Project/Directive，绝不直接触碰仓储——今天集中校验存在性，
//! 未来 cross-cutting 关注（provenance / 授权 / 审计日志）只有一处可挂。
//!
//! Python 侧 `create_mission` / `update_mission_metadata` /
//! `batch_update_missions` 的关键字参数组以输入结构体镜像（
//! [`CreateMissionInput`] / [`UpdateMissionInput`]）；其余方法参数少，
//! 直接平铺。

// Public async methods retain the existing API shape so Axum/Tauri callers
// can switch implementations without a contract change.
#![allow(clippy::collapsible_if)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::unused_async)]
#![allow(clippy::unused_async_trait_impl)]

use std::collections::HashSet;
use std::sync::Arc;

use models::ApprovalMode;
use models::AuditDomain;
use models::AuditEventType;
use models::BranchId;
use models::Fact;
use models::GraphNodeType;
use models::Mission;
use models::MissionGoalContract;
use models::MissionId;
use models::MissionStartResult;
use models::MissionStatus;
use models::ObservationType;
use models::Project;
use models::ProjectId;
use models::RunId;
use models::StrMap;
use models::UserDirective;
use models::UserDirectiveStatus;
use models::UserDirectiveType;
use models::utcnow;
use serde_json::Map;
use serde_json::Value;

use crate::errors::EngineError;
use crate::events::EventDraft;
use crate::events::ObservationDraft;
use crate::manager::AuditManager;

/// `create_mission` 的关键字参数组（Python kwargs 镜像）。
#[derive(Debug)]
pub struct CreateMissionInput {
    /// 用户目标原文（不翻译、不改写、不归一化）。
    pub user_goal: String,
    /// 展示标题。
    pub title: Option<String>,
    /// 目标键值（`None` 落盘为空映射）。
    pub target: Option<Map<String, Value>>,
    /// 所属 Project（`None` 时新建）。
    pub project_id: Option<ProjectId>,
    /// 结构化约束。
    pub constraints: Vec<String>,
    /// 成功判据（空时取 Python 默认单条）。
    pub success_criteria: Vec<String>,
    /// 目标契约（`None` 取默认契约）。
    pub goal_contract: Option<MissionGoalContract>,
    /// 标签。
    pub tags: Vec<String>,
    /// 分类。
    pub category: Option<String>,
    /// 审批模式（Python 默认 `ask_for_approval`）。
    pub approval_mode: ApprovalMode,
    /// 创建者（Python 默认 `user`）。
    pub created_by: String,
    /// 附加元数据。
    pub metadata: Map<String, Value>,
}

impl CreateMissionInput {
    /// 以最小输入构造，其余取 Python 默认。
    #[must_use]
    pub fn new(user_goal: String) -> Self {
        Self {
            user_goal,
            title: None,
            target: None,
            project_id: None,
            constraints: Vec::new(),
            success_criteria: Vec::new(),
            goal_contract: None,
            tags: Vec::new(),
            category: None,
            approval_mode: ApprovalMode::AskForApproval,
            created_by: "user".to_string(),
            metadata: Map::new(),
        }
    }
}

/// `update_mission_metadata` 的关键字参数组：字段 `None` 即"不更新"
/// （Python `is not None` 判定的镜像——因此无法把可空字段显式写回
/// `None`，与 Python 一致）。
#[derive(Debug, Default)]
pub struct UpdateMissionInput {
    /// 用户目标原文（空白字符串被拒绝）。
    pub user_goal: Option<String>,
    /// 展示标题（归一化后为空被拒绝）。
    pub title: Option<String>,
    /// 标签。
    pub tags: Option<Vec<String>>,
    /// 分类。
    pub category: Option<String>,
    /// 归档位。
    pub archived: Option<bool>,
    /// 审批模式。
    pub approval_mode: Option<ApprovalMode>,
    /// 附加元数据（整体替换）。
    pub metadata: Option<Map<String, Value>>,
    /// 目标契约。
    pub goal_contract: Option<MissionGoalContract>,
}

impl AuditManager {
    // -- project bootstrap ------------------------------------------------

    /// 创建 Project 并落盘起点/目标事实（Python `create_project`）。
    ///
    /// Project 创建的唯一入口：未来的授权 / provenance / 审计日志只有这一
    /// 处可挂。
    ///
    /// # Errors
    /// 仓储写入失败。
    pub async fn create_project(
        &self,
        name: &str,
        audit_domain: AuditDomain,
        description: Option<&str>,
        target: Option<&StrMap>,
        goal: Option<&str>,
    ) -> Result<Project, EngineError> {
        let mut project = Project::new(name.to_string(), audit_domain);
        project.description = description.map(str::to_string);
        project.target = target.cloned().unwrap_or_default();
        let project = self.repository().create_project(&project)?;
        self.seed_project_facts(&project, goal).await?;

        self.record_event_safe(EventDraft {
            data: Some(Map::from_iter([(
                "audit_domain".to_string(),
                Value::String(project.audit_domain.as_str().to_string()),
            )])),
            ..EventDraft::new(
                &project.id,
                AuditEventType::ProjectCreated,
                "manager",
                &format!("项目已创建：{}", project.name),
            )
        })
        .await;
        Ok(project)
    }

    /// 为新建 Project 追加起点/目标事实（Python `seed_project_facts`）。
    ///
    /// # Errors
    /// 仓储写入失败。
    pub async fn seed_project_facts(
        &self,
        project: &Project,
        goal: Option<&str>,
    ) -> Result<Vec<Fact>, EngineError> {
        let mut origin = Fact::new(
            project.id.clone(),
            "origin".to_string(),
            format!(
                "Audit target registered: {}",
                display_target_or_name(&project.target, &project.name)
            ),
        );
        origin.node_type = GraphNodeType::OriginFact;
        origin.data = Map::from_iter([
            (
                "target".to_string(),
                serde_json::to_value(target_json(&project.target)).unwrap_or(Value::Null),
            ),
            (
                "audit_domain".to_string(),
                Value::String(project.audit_domain.as_str().to_string()),
            ),
        ]);

        let mut goal_fact = Fact::new(
            project.id.clone(),
            "goal".to_string(),
            goal.unwrap_or("Find high-confidence vulnerabilities with reproducible evidence")
                .to_string(),
        );
        goal_fact.node_type = GraphNodeType::GoalFact;

        let saved_origin = self.repository().add_fact(&origin)?;
        let saved_goal = self.repository().add_fact(&goal_fact)?;
        Ok(vec![saved_origin, saved_goal])
    }

    // -- 存在性校验（单一写入路径的地基） -----------------------------------

    /// 加载 Project，不存在则报错（Python `_require_project`）。
    ///
    /// # Errors
    /// Project 不存在（404 族）或仓储读取失败。
    pub(crate) fn require_project(&self, project_id: &str) -> Result<Project, EngineError> {
        self.repository()
            .get_project(project_id)?
            .ok_or_else(|| EngineError::ProjectNotFound(format!("unknown project: {project_id}")))
    }

    /// 加载 AuditRun，不存在则报错（Python `_require_run`）。
    ///
    /// # Errors
    /// Run 不存在（404 族）或仓储读取失败。
    pub(crate) fn require_run(&self, run_id: &RunId) -> Result<models::AuditRun, EngineError> {
        self.repository().get_run(run_id.as_str())?.ok_or_else(|| {
            EngineError::RunNotFound(format!("unknown audit run: {}", run_id.as_str()))
        })
    }

    /// 加载 Mission，不存在则报错（Python `_require_mission`）。
    ///
    /// # Errors
    /// Mission 不存在（404 族）或仓储读取失败。
    pub(crate) fn require_mission(&self, mission_id: &str) -> Result<Mission, EngineError> {
        self.repository()
            .get_mission(mission_id)?
            .ok_or_else(|| EngineError::MissionNotFound(format!("unknown mission: {mission_id}")))
    }

    /// 加载 Branch，不存在则报错（Python `_require_branch`）。
    ///
    /// # Errors
    /// Branch 不存在（404 族）或仓储读取失败。
    pub(crate) fn require_branch(&self, branch_id: &str) -> Result<models::Branch, EngineError> {
        self.repository()
            .get_branch(branch_id)?
            .ok_or_else(|| EngineError::BranchNotFound(format!("unknown branch: {branch_id}")))
    }

    // -- Mission CRUD --------------------------------------------------------

    /// 创建 Mission 并播种起点/目标事实（Python `create_mission`）。
    ///
    /// # Errors
    /// 仓储写入失败，或 `title` / 枚举参数非法（Python `ValueError` 族，
    /// 映射 422）。
    pub async fn create_mission(&self, input: CreateMissionInput) -> Result<Mission, EngineError> {
        let normalized_target = string_target(input.target.as_ref());
        let target_type = resolve_target_type(&normalized_target);
        let project = match input.project_id {
            None => {
                self.create_project(
                    &mission_project_name(&input.user_goal),
                    audit_domain_for_target_type(target_type),
                    Some(&input.user_goal),
                    Some(&normalized_target),
                    Some(&input.user_goal),
                )
                .await?
            }
            Some(project_id) => self.require_project(project_id.as_str())?,
        };

        let mut mission = Mission::new(project.id.clone(), input.user_goal);
        mission.title = normalize_title(input.title.as_deref());
        mission.target = if normalized_target.is_empty() {
            project.target.clone()
        } else {
            normalized_target
        };
        if !input.constraints.is_empty() {
            mission.constraints = input.constraints;
        }
        if input.success_criteria.is_empty() {
            mission.success_criteria =
                vec!["Produce evidence-backed findings or explicit capability gaps".to_string()];
        } else {
            mission.success_criteria = input.success_criteria;
        }
        mission.goal_contract = input.goal_contract.unwrap_or_default();
        mission.tags = normalize_tags(&input.tags);
        mission.category = normalize_optional_label(input.category.as_deref());
        mission.approval_mode = input.approval_mode;
        mission.created_by = input.created_by;
        mission.metadata = input.metadata;
        let saved = self.repository().create_mission(&mission)?;

        let mut origin = Fact::new(
            saved.project_id.clone(),
            "mission.origin".to_string(),
            format!(
                "Mission target registered: {}",
                display_target(&saved.target, &project.target)
            ),
        );
        origin.mission_id = Some(saved.id.clone());
        origin.node_type = GraphNodeType::OriginFact;
        origin.data = Map::from_iter([
            (
                "mission_id".to_string(),
                Value::String(saved.id.as_str().to_string()),
            ),
            (
                "target".to_string(),
                serde_json::to_value(target_json(&saved.target)).unwrap_or(Value::Null),
            ),
            (
                "target_type".to_string(),
                Value::String(target_type.to_string()),
            ),
        ]);
        self.repository().add_fact(&origin)?;

        let mut goal_fact = Fact::new(
            saved.project_id.clone(),
            "mission.goal".to_string(),
            saved.user_goal.clone(),
        );
        goal_fact.mission_id = Some(saved.id.clone());
        goal_fact.node_type = GraphNodeType::GoalFact;
        goal_fact.data = Map::from_iter([
            (
                "mission_id".to_string(),
                Value::String(saved.id.as_str().to_string()),
            ),
            (
                "constraints".to_string(),
                serde_json::to_value(&saved.constraints).unwrap_or(Value::Null),
            ),
            (
                "success_criteria".to_string(),
                serde_json::to_value(&saved.success_criteria).unwrap_or(Value::Null),
            ),
            (
                "goal_contract".to_string(),
                serde_json::to_value(&saved.goal_contract).unwrap_or(Value::Null),
            ),
        ]);
        self.repository().add_fact(&goal_fact)?;

        self.record_event_safe(EventDraft {
            status: Some(saved.status.as_str()),
            data: Some(Map::from_iter([
                (
                    "mission_id".to_string(),
                    Value::String(saved.id.as_str().to_string()),
                ),
                (
                    "target_type".to_string(),
                    Value::String(target_type.to_string()),
                ),
            ])),
            ..EventDraft::new(
                &saved.project_id,
                AuditEventType::UserNote,
                "mission_control",
                "Mission created",
            )
        })
        .await;
        self.project_mission_assets_safe(&saved, &[], &[], &[], &[], false)
            .await;
        Ok(saved)
    }

    /// 列出 Mission，可选按 Project 过滤（Python `list_missions`）。
    ///
    /// # Errors
    /// 指定的 Project 不存在或仓储读取失败。
    pub fn list_missions(
        &self,
        project_id: Option<&ProjectId>,
    ) -> Result<Vec<Mission>, EngineError> {
        if let Some(project_id) = project_id {
            self.require_project(project_id.as_str())?;
        }
        Ok(self
            .repository()
            .list_missions(project_id.map(ProjectId::as_str))?)
    }

    /// 更新 workspace UI 使用的非执行字段（Python `update_mission_metadata`）。
    ///
    /// 审批模式变更会同步合并进活跃 run 的 config。
    ///
    /// # Errors
    /// Mission/Run 不存在、`user_goal`/`title` 非法（422 族）或仓储写入失败。
    pub async fn update_mission_metadata(
        &self,
        mission_id: &str,
        input: UpdateMissionInput,
    ) -> Result<Mission, EngineError> {
        let mut mission = self.require_mission(mission_id)?;
        if let Some(user_goal) = &input.user_goal {
            let stripped = user_goal.trim();
            if stripped.is_empty() {
                return Err(EngineError::Value(
                    "user_goal must not be blank".to_string(),
                ));
            }
            mission.user_goal = stripped.to_string();
        }
        if let Some(title) = &input.title {
            let normalized = normalize_title(Some(title));
            if normalized.is_none() {
                return Err(EngineError::Value("title must not be blank".to_string()));
            }
            mission.title = normalized;
        }
        if let Some(tags) = input.tags {
            mission.tags = normalize_tags(&tags);
        }
        if let Some(category) = input.category {
            mission.category = normalize_optional_label(Some(&category));
        }
        if let Some(archived) = input.archived {
            mission.archived = archived;
        }
        if let Some(approval_mode) = input.approval_mode {
            mission.approval_mode = approval_mode;
        }
        if let Some(metadata) = input.metadata {
            mission.metadata = metadata;
        }
        if let Some(goal_contract) = input.goal_contract {
            mission.goal_contract = goal_contract;
        }
        mission.updated_at = utcnow();
        let saved = self.persist_mission_notifying(&mission)?;
        if input.approval_mode.is_some() {
            if let Some(active_run_id) = &mission.active_run_id {
                if let Some(mut run) = self.repository().get_run(active_run_id.as_str())? {
                    run.config.insert(
                        "approval_mode".to_string(),
                        Value::String(mission.approval_mode.as_str().to_string()),
                    );
                    run.updated_at = utcnow();
                    self.repository().update_run(&run)?;
                }
            }
        }
        Ok(saved)
    }

    /// 删除 Mission（Python `delete_mission`）。
    ///
    /// 历史 project 图记录刻意保留：Mission 删除是工作区列表操作，不是
    /// 证据链变更。
    ///
    /// # Errors
    /// Mission 不存在或仓储写入失败。
    pub fn delete_mission(&self, mission_id: &str) -> Result<(), EngineError> {
        let mission = self.require_mission(mission_id)?;
        let project_id = mission.project_id.as_str().to_string();
        self.repository().delete_mission(mission_id)?;
        // 每个 mission 都会自动建一个同名 Project；删掉最后一条 mission 后该项目
        // 即成空壳，还会以目标原名堆积在项目选择器里。连带删掉空项目，避免
        // "删了任务、项目选择器还留一串历史目标"。
        let remaining = self.repository().list_missions(Some(&project_id))?;
        if remaining.is_empty() {
            self.repository().delete_project(&project_id)?;
        }
        Ok(())
    }

    /// 整体持久化一条已加载的 Mission（Python 上传路由直接调
    /// `repository.update_mission` 同步 workspace 元数据；Rust 侧保持
    /// "HTTP 层不直写仓储" 的纪律，收口在 Manager 门面）。
    ///
    /// # Errors
    /// Mission 不存在或仓储写入失败。
    pub fn update_mission_record(&self, mission: &Mission) -> Result<Mission, EngineError> {
        self.require_mission(mission.id.as_str())?;
        self.persist_mission_notifying(mission)
    }

    /// 删除 Project 并级联清理全部关联记录（Python `delete_project`）。
    ///
    /// # Errors
    /// Project 不存在或仓储写入失败。
    pub fn delete_project(&self, project_id: &str) -> Result<(), EngineError> {
        self.require_project(project_id)?;
        Ok(self.repository().delete_project(project_id)?)
    }

    /// 批量应用列表管理动作（Python `batch_update_missions`）。
    ///
    /// # Errors
    /// 任一 Mission 不存在、动作不支持（422 族）或仓储写入失败。
    pub async fn batch_update_missions(
        &self,
        mission_ids: &[MissionId],
        action: &str,
        category: Option<&str>,
        tags: &[String],
    ) -> Result<Vec<Mission>, EngineError> {
        if mission_ids.is_empty() {
            return Err(EngineError::Value(
                "mission_ids must not be empty".to_string(),
            ));
        }
        let normalized_tags = normalize_tags(tags);
        let mut updated: Vec<Mission> = Vec::new();
        for mission_id in mission_ids {
            let mut mission = self.require_mission(mission_id.as_str())?;
            match action {
                "archive" => mission.archived = true,
                "restore" => mission.archived = false,
                "delete" => {
                    self.repository().delete_mission(mission.id.as_str())?;
                    continue;
                }
                "set_category" => {
                    mission.category = normalize_optional_label(category);
                }
                "add_tags" => {
                    let merged: Vec<String> = mission
                        .tags
                        .iter()
                        .cloned()
                        .chain(normalized_tags.iter().cloned())
                        .collect();
                    mission.tags = normalize_tags(&merged);
                }
                "remove_tags" => {
                    let remove: HashSet<&str> =
                        normalized_tags.iter().map(String::as_str).collect();
                    mission.tags.retain(|tag| !remove.contains(tag.as_str()));
                }
                other => {
                    return Err(EngineError::Value(format!(
                        "unsupported mission batch action: {other}"
                    )));
                }
            }
            mission.updated_at = utcnow();
            updated.push(self.repository().update_mission(&mission)?);
        }
        Ok(updated)
    }

    // -- 用户指令 ------------------------------------------------------------

    /// 应用结构化用户指令到 Mission/Branch/Board 状态（Python
    /// `apply_user_directive`）。
    ///
    /// # Errors
    /// Mission（或指令指定的 Branch/Run）不存在、指令参数非法或仓储写入
    /// 失败。
    pub async fn apply_user_directive(
        &self,
        mission_id: &MissionId,
        directive_type: UserDirectiveType,
        content: &str,
        branch_id: Option<&BranchId>,
        created_by: &str,
        metadata: Option<Map<String, Value>>,
    ) -> Result<UserDirective, EngineError> {
        let mut mission = self.require_mission(mission_id.as_str())?;
        let mut directive = self
            .create_directive(
                &mission,
                directive_type,
                content,
                branch_id,
                created_by,
                metadata,
            )
            .await?;

        match directive_type {
            UserDirectiveType::Pause => {
                mission.status = MissionStatus::Paused;
                mission.updated_at = utcnow();
                self.repository().update_mission(&mission)?;
                if let Some(active_run_id) = &mission.active_run_id {
                    self.pause_active_run(active_run_id, content).await?;
                }
                directive = self.mark_directive_applied(directive)?;
            }
            UserDirectiveType::Resume => {
                directive = self.mark_directive_applied(directive)?;
                mission.status = MissionStatus::Running;
                mission.updated_at = utcnow();
                self.repository().update_mission(&mission)?;
            }
            UserDirectiveType::PrioritizeBranch if branch_id.is_some() => {
                let mut branch = self.require_branch(branch_id.map_or("", BranchId::as_str))?;
                branch.priority = (branch.priority + 20).clamp(80, 100);
                branch.updated_at = utcnow();
                branch.metadata.insert(
                    "priority_reason".to_string(),
                    Value::String(content.to_string()),
                );
                self.repository().update_branch(&branch)?;
                directive = self.mark_directive_applied(directive)?;
            }
            UserDirectiveType::AbandonBranch if branch_id.is_some() => {
                let branch_id = branch_id.map_or("", BranchId::as_str);
                self.abandon_branch(branch_id, content).await?;
                directive = self.mark_directive_applied(directive)?;
            }
            UserDirectiveType::ReopenBranch if branch_id.is_some() => {
                let branch_id = branch_id.map_or("", BranchId::as_str);
                self.reopen_branch(branch_id, content).await?;
                directive = self.mark_directive_applied(directive)?;
            }
            UserDirectiveType::AddRequirement => {
                // 删除复杂度升档后，用户追加的需求经 mission.constraints 落到
                // 任务上，并由下面的 record_strategy_board_directive 让主 Agent
                // 在下一步拾取；不再有"升一档"这种间接传递。
                mission.constraints.push(content.to_string());
                mission.updated_at = utcnow();
                self.repository().update_mission(&mission)?;
                directive = self.mark_directive_applied(directive)?;
            }
            UserDirectiveType::NarrowScope => {
                mission.constraints.push(content.to_string());
                mission.updated_at = utcnow();
                self.repository().update_mission(&mission)?;
                directive = self.mark_directive_applied(directive)?;
            }
            UserDirectiveType::ExcludeScope => {
                mission.constraints.push(format!("exclude: {content}"));
                mission.updated_at = utcnow();
                self.repository().update_mission(&mission)?;
                directive = self.mark_directive_applied(directive)?;
            }
            UserDirectiveType::RollbackPatch => {
                directive.parsed_intent.insert(
                    "rollback_status".to_string(),
                    Value::String("unsupported".to_string()),
                );
                directive.parsed_intent.insert(
                    "reason".to_string(),
                    Value::String(
                        "Patch editing/rollback execution is not implemented in this MVP."
                            .to_string(),
                    ),
                );
                directive.status = UserDirectiveStatus::Rejected;
                directive.applied_at = Some(utcnow());
                directive = self.repository().update_user_directive(&directive)?;
            }
            UserDirectiveType::AskQuestion
            | UserDirectiveType::PrioritizeBranch
            | UserDirectiveType::AbandonBranch
            | UserDirectiveType::ReopenBranch => {
                directive = self.mark_directive_applied(directive)?;
            }
        }

        let observation_run_id = mission
            .active_run_id
            .clone()
            .unwrap_or_else(|| RunId::new(String::new()));
        let mut data = Map::from_iter([
            (
                "directive_id".to_string(),
                Value::String(directive.id.as_str().to_string()),
            ),
            ("content".to_string(), Value::String(content.to_string())),
        ]);
        self.add_observation(ObservationDraft {
            mission_id: Some(&mission.id),
            branch_id,
            observation_type: ObservationType::UserNote,
            worker_id: Some("mission_control"),
            data: Some(std::mem::take(&mut data)),
            ..ObservationDraft::new(
                &mission.project_id,
                &observation_run_id,
                &format!("User directive applied: {}", directive_type.as_str()),
            )
        })
        .await?;
        if mission.active_run_id.is_some() {
            self.record_strategy_board_directive(&mission, &directive)
                .await;
        }
        Ok(self
            .repository()
            .get_user_directive(directive.id.as_str())?
            .unwrap_or(directive))
    }

    /// 暂停 Mission 及其活跃 AuditRun，不删除历史（Python `pause_mission`）。
    ///
    /// # Errors
    /// Mission/Run 不存在或仓储写入失败。
    pub async fn pause_mission(
        &self,
        mission_id: &MissionId,
        content: Option<&str>,
        created_by: Option<&str>,
    ) -> Result<Mission, EngineError> {
        let content = content.unwrap_or("Mission paused by user");
        let created_by = created_by.unwrap_or("user");
        let mut mission = self.require_mission(mission_id.as_str())?;
        let directive = self
            .create_directive(
                &mission,
                UserDirectiveType::Pause,
                content,
                None,
                created_by,
                None,
            )
            .await?;
        mission.status = MissionStatus::Paused;
        mission.updated_at = utcnow();
        self.repository().update_mission(&mission)?;
        if let Some(active_run_id) = &mission.active_run_id {
            self.pause_active_run(active_run_id, content).await?;
        }
        let directive = self.mark_directive_applied(directive)?;
        self.record_event_safe(EventDraft {
            run_id: mission.active_run_id.as_ref(),
            message: Some(content),
            status: Some(mission.status.as_str()),
            data: Some(Map::from_iter([
                (
                    "mission_id".to_string(),
                    Value::String(mission.id.as_str().to_string()),
                ),
                (
                    "directive_id".to_string(),
                    Value::String(directive.id.as_str().to_string()),
                ),
            ])),
            ..EventDraft::new(
                &mission.project_id,
                AuditEventType::RunPaused,
                "mission_control",
                "Mission paused",
            )
        })
        .await;
        Ok(mission)
    }

    /// 暂停一次 active run 的统一收尾：置 Paused + 撤 runtime token + 取消
    /// branch-runtime（drop → worker 进程树被杀）+ 收口在飞 worker-run。
    ///
    /// `pause_mission` 与 `apply_user_directive` 的 Pause 分支共用——两条暂停
    /// 路径必须行为一致，漏取消/漏收口会留下"界面显示暂停但 worker 还在跑、
    /// worker-run 卡 running"的幽灵态。
    async fn pause_active_run(&self, run_id: &RunId, content: &str) -> Result<(), EngineError> {
        let mut run = self.require_run(run_id)?;
        run.status = models::RunStatus::Paused;
        run.note = Some(content.to_string());
        run.updated_at = utcnow();
        self.repository().update_run(&run)?;
        self.remove_runtime_token(&run.id).await;
        if let Some(handle) = self.take_runtime_handle(&run.id).await {
            if !handle.is_finished() {
                let _ = handle.cancel();
            }
        }
        // 收口在飞的 worker-run：上面的 handle.cancel() 只 drop 了
        // branch_runtime future，dispatch 内的 finish() 不会随之执行，
        // worker-run 会滞留 running/pending，前端 Workers 泳道据此一直
        // 显示"运行中"转圈。按 active run 批量置 Cancelled（finish 幂等，
        // 已终态/已正常收口的不覆盖）。
        self.finalize_inflight_worker_runs(run_id.as_str(), content)
            .await?;
        Ok(())
    }

    /// 把某次 run 下仍在飞（Pending/Running）的 worker-run 批量收口为 Cancelled。
    ///
    /// 暂停/取消只 drop 了 branch_runtime future，dispatch 内的 `finish()` 不会
    /// 执行，worker-run 会滞留 running/pending，前端 Workers 泳道据此一直显示
    /// "运行中"。`WorkerRun::finish` 幂等（已终态不覆盖），可安全兜底；`run_id`
    /// 由派发时写入（`begin_dispatch`），故按它精确圈定本次 run 的 worker。
    async fn finalize_inflight_worker_runs(
        &self,
        run_id: &str,
        reason: &str,
    ) -> Result<(), EngineError> {
        let runs = self
            .repository()
            .list_worker_runs(None, Some(run_id), None, 1000)?;
        for mut worker_run in runs {
            if worker_run.status.is_terminal() {
                continue;
            }
            worker_run.finish(
                models::WorkerRunStatus::Cancelled,
                Some(format!("mission paused: {reason}")),
            );
            self.repository().upsert_worker_run(&worker_run)?;
        }
        Ok(())
    }

    /// 恢复 Mission 并可选记录新需求（Python `resume_mission`）。
    ///
    /// 活跃 epoch 无可运行 Branch 时开新 epoch，旧 run 的证据与轨迹完整
    /// 保留（审计可重放）。
    ///
    /// # Errors
    /// Mission/Run/Project 不存在或仓储写入失败。
    pub async fn resume_mission(
        self: &Arc<Self>,
        mission_id: &MissionId,
        new_requirement: Option<&str>,
        config: Option<Map<String, Value>>,
        background_runtime: bool,
    ) -> Result<MissionStartResult, EngineError> {
        let mut mission = self.require_mission(mission_id.as_str())?;
        let project = self.require_project(mission.project_id.as_str())?;
        if let Some(new_requirement) = new_requirement {
            self.apply_user_directive(
                mission_id,
                UserDirectiveType::AddRequirement,
                new_requirement,
                None,
                "user",
                None,
            )
            .await?;
        }
        mission.status = MissionStatus::Running;
        mission.finished_at = None;
        mission.updated_at = utcnow();
        self.repository().update_mission(&mission)?;

        let mut started_new_epoch = false;
        let run_id;
        if let Some(active_run_id) = &mission.active_run_id {
            let mut previous_run = self.require_run(active_run_id)?;
            let previous_branches = self.repository().list_branches(
                Some(mission.project_id.as_str()),
                Some(mission.id.as_str()),
                Some(previous_run.id.as_str()),
            )?;
            let has_runnable_branch = previous_run.steps_used < previous_run.max_total_steps
                && previous_branches.iter().any(|branch| {
                    matches!(
                        branch.status,
                        models::BranchStatus::Proposed | models::BranchStatus::Active
                    ) && branch.steps_used < branch.budget_steps
                });
            if has_runnable_branch {
                previous_run.status = models::RunStatus::Running;
                if previous_run.started_at.is_none() {
                    previous_run.started_at = Some(utcnow());
                }
                previous_run.finished_at = None;
                previous_run.updated_at = utcnow();
                if let Some(config) = &config {
                    for (key, value) in config {
                        previous_run.config.insert(key.clone(), value.clone());
                    }
                }
                self.repository().update_run(&previous_run)?;
                run_id = previous_run.id;
            } else {
                // 暂停的终局 epoch 没有真正能跑的工作：复用它会让 Resume 变
                // no-op（或因每个 Branch 都耗尽而立刻失败）。保留旧 run 的
                // 证据与轨迹供审计，开新的有界 epoch。
                let mut next_config = previous_run.config.clone();
                if let Some(config) = &config {
                    for (key, value) in config {
                        next_config.insert(key.clone(), value.clone());
                    }
                }
                let run = self
                    .create_mission_run(&mission, next_config, models::RunStatus::Running)
                    .await?;
                run_id = run.id.clone();
                mission.active_run_id = Some(run.id.clone());
                mission.updated_at = utcnow();
                self.repository().update_mission(&mission)?;
                started_new_epoch = true;
            }
        } else {
            let run = self
                .create_mission_run(
                    &mission,
                    config.unwrap_or_default(),
                    models::RunStatus::Running,
                )
                .await?;
            run_id = run.id.clone();
            mission.active_run_id = Some(run.id.clone());
            self.repository().update_mission(&mission)?;
            started_new_epoch = true;
        }

        if started_new_epoch {
            let run = self.require_run(&run_id)?;
            self.ensure_mission_run_branches(&mission, &project, &run)
                .await?;
        }
        self.record_event_safe(EventDraft {
            run_id: Some(&run_id),
            data: Some(Map::from_iter([
                (
                    "mission_id".to_string(),
                    Value::String(mission.id.as_str().to_string()),
                ),
                (
                    "new_requirement".to_string(),
                    Value::Bool(new_requirement.is_some()),
                ),
                (
                    "started_new_epoch".to_string(),
                    Value::Bool(started_new_epoch),
                ),
            ])),
            ..EventDraft::new(
                &mission.project_id,
                AuditEventType::RunResumed,
                "mission_control",
                "Mission resumed",
            )
        })
        .await;

        if background_runtime {
            self.submit_mission_runtime(
                mission_id.clone(),
                run_id.clone(),
                "mission_resumed",
                None,
                None,
            )
            .await;
        } else {
            self.run_mission_runtime(mission_id, &run_id, "mission_resumed", None, None)
                .await?;
            mission = self.require_mission(mission_id.as_str())?;
        }
        let branches = self.repository().list_branches(
            None,
            Some(mission.id.as_str()),
            Some(run_id.as_str()),
        )?;
        Ok(MissionStartResult {
            mission,
            run_id,
            branches,
        })
    }

    /// 放弃 Branch，保留全部证据/历史（Python `abandon_branch`）。
    ///
    /// # Errors
    /// Branch 不存在或仓储写入失败。
    pub async fn abandon_branch(
        &self,
        branch_id: &str,
        reason: &str,
    ) -> Result<models::Branch, EngineError> {
        let mut branch = self.require_branch(branch_id)?;
        branch.status = models::BranchStatus::Abandoned;
        branch.updated_at = utcnow();
        branch.metadata.insert(
            "abandoned_reason".to_string(),
            Value::String(reason.to_string()),
        );
        let saved = self.repository().update_branch(&branch)?;
        self.record_event_safe(EventDraft {
            run_id: saved.run_id.as_ref(),
            status: Some(saved.status.as_str()),
            data: Some(Map::from_iter([
                (
                    "mission_id".to_string(),
                    Value::String(saved.mission_id.as_str().to_string()),
                ),
                (
                    "branch_id".to_string(),
                    Value::String(saved.id.as_str().to_string()),
                ),
                ("reason".to_string(), Value::String(reason.to_string())),
            ])),
            ..EventDraft::new(
                &saved.project_id,
                AuditEventType::UserNote,
                "mission_control",
                "Branch abandoned",
            )
        })
        .await;
        if let Some(run_id) = &saved.run_id {
            self.add_observation(ObservationDraft {
                mission_id: Some(&saved.mission_id),
                branch_id: Some(&saved.id),
                observation_type: ObservationType::Decision,
                worker_id: Some("mission_control"),
                data: Some(Map::from_iter([(
                    "reason".to_string(),
                    Value::String(reason.to_string()),
                )])),
                ..ObservationDraft::new(
                    &saved.project_id,
                    run_id,
                    &format!(
                        "Branch abandoned: {}",
                        if reason.is_empty() {
                            &saved.title
                        } else {
                            reason
                        }
                    ),
                )
            })
            .await?;
        }
        Ok(saved)
    }

    /// 重开已放弃/阻塞的 Branch（Python `reopen_branch`）。
    ///
    /// # Errors
    /// Branch 不存在或仓储写入失败。
    pub async fn reopen_branch(
        &self,
        branch_id: &str,
        reason: &str,
    ) -> Result<models::Branch, EngineError> {
        let mut branch = self.require_branch(branch_id)?;
        branch.status = models::BranchStatus::Active;
        branch.updated_at = utcnow();
        branch.metadata.insert(
            "reopen_reason".to_string(),
            Value::String(reason.to_string()),
        );
        let saved = self.repository().update_branch(&branch)?;
        self.record_event_safe(EventDraft {
            run_id: saved.run_id.as_ref(),
            status: Some(saved.status.as_str()),
            data: Some(Map::from_iter([
                (
                    "mission_id".to_string(),
                    Value::String(saved.mission_id.as_str().to_string()),
                ),
                (
                    "branch_id".to_string(),
                    Value::String(saved.id.as_str().to_string()),
                ),
                ("reason".to_string(), Value::String(reason.to_string())),
            ])),
            ..EventDraft::new(
                &saved.project_id,
                AuditEventType::UserNote,
                "mission_control",
                "Branch reopened",
            )
        })
        .await;
        Ok(saved)
    }

    /// 记录一次诚实的"不支持回滚"指令（Python `rollback_patch`）。
    ///
    /// # Errors
    /// Mission 不存在或指令写入失败。
    pub async fn rollback_patch(
        &self,
        mission_id: &MissionId,
        checkpoint_id: Option<&str>,
        reason: &str,
    ) -> Result<UserDirective, EngineError> {
        let mission = self.require_mission(mission_id.as_str())?;
        let content = if reason.is_empty() {
            format!(
                "Rollback requested for checkpoint {}",
                checkpoint_id.unwrap_or("<none>")
            )
        } else {
            reason.to_string()
        };
        let mut metadata = Map::new();
        metadata.insert(
            "checkpoint_id".to_string(),
            checkpoint_id.map_or(Value::Null, |id| Value::String(id.to_string())),
        );
        metadata.insert(
            "rollback_status".to_string(),
            Value::String("unsupported".to_string()),
        );
        self.apply_user_directive(
            &mission.id,
            UserDirectiveType::RollbackPatch,
            &content,
            None,
            "user",
            Some(metadata),
        )
        .await
    }

    // -- 指令辅助 ------------------------------------------------------------

    /// 落盘一条 UserDirective 并记录事件（Python `_create_directive`）。
    ///
    /// # Errors
    /// 仓储写入失败。
    async fn create_directive(
        &self,
        mission: &Mission,
        directive_type: UserDirectiveType,
        content: &str,
        branch_id: Option<&BranchId>,
        created_by: &str,
        metadata: Option<Map<String, Value>>,
    ) -> Result<UserDirective, EngineError> {
        let mut directive = UserDirective::new(
            mission.project_id.clone(),
            mission.id.clone(),
            directive_type,
            content.to_string(),
        );
        directive.run_id.clone_from(&mission.active_run_id);
        directive.branch_id = branch_id.cloned();
        directive.parsed_intent = Map::from_iter([
            (
                "directive_type".to_string(),
                Value::String(directive_type.as_str().to_string()),
            ),
            (
                "branch_id".to_string(),
                branch_id.map_or(Value::Null, |id| Value::String(id.as_str().to_string())),
            ),
        ]);
        directive.created_by = created_by.to_string();
        directive.metadata = metadata.unwrap_or_default();
        let saved = self.repository().add_user_directive(&directive)?;

        self.record_event_safe(EventDraft {
            run_id: mission.active_run_id.as_ref(),
            status: Some(saved.status.as_str()),
            data: Some(Map::from_iter([
                (
                    "mission_id".to_string(),
                    Value::String(mission.id.as_str().to_string()),
                ),
                (
                    "branch_id".to_string(),
                    branch_id.map_or(Value::Null, |id| Value::String(id.as_str().to_string())),
                ),
                (
                    "directive_id".to_string(),
                    Value::String(saved.id.as_str().to_string()),
                ),
                (
                    "directive_type".to_string(),
                    Value::String(directive_type.as_str().to_string()),
                ),
            ])),
            ..EventDraft::new(
                &mission.project_id,
                AuditEventType::UserNote,
                created_by,
                &format!("User directive recorded: {}", directive_type.as_str()),
            )
        })
        .await;
        Ok(saved)
    }

    /// 标记指令已应用（Python `_mark_directive_applied`）。
    ///
    /// # Errors
    /// 仓储写入失败。
    fn mark_directive_applied(
        &self,
        directive: UserDirective,
    ) -> Result<UserDirective, EngineError> {
        let mut directive = directive;
        directive.status = UserDirectiveStatus::Applied;
        directive.applied_at = Some(utcnow());
        Ok(self.repository().update_user_directive(&directive)?)
    }

    /// 把结构化用户控制输入写入策略板（Python
    /// `_record_strategy_board_directive`）。
    ///
    /// 板面 sidecar 绝不阻断指令应用：`apply_strategy_board_ops` 失败（含
    /// 未移植路径）时降级记录 "skipped" 事件。
    async fn record_strategy_board_directive(&self, mission: &Mission, directive: &UserDirective) {
        let Some(run_id) = &mission.active_run_id else {
            return;
        };
        let content = format!(
            "User directive {}: {}",
            directive.directive_type.as_str(),
            directive.content
        );
        if self
            .apply_strategy_board_ops(mission.project_id.as_str(), run_id, &content)
            .await
            .is_err()
        {
            self.record_event_safe(EventDraft {
                run_id: Some(run_id),
                status: Some("skipped"),
                data: Some(Map::from_iter([
                    (
                        "mission_id".to_string(),
                        Value::String(mission.id.as_str().to_string()),
                    ),
                    (
                        "directive_id".to_string(),
                        Value::String(directive.id.as_str().to_string()),
                    ),
                ])),
                ..EventDraft::new(
                    &mission.project_id,
                    AuditEventType::UserNote,
                    "strategy_board",
                    "Strategy Board directive update skipped",
                )
            })
            .await;
        }
    }
}

// -- 归一化辅助（Python 静态方法） -------------------------------------------

/// 目标键值 → 字符串映射（Python `_string_target`）：只保留
/// str/int/float/bool 值（`None` 丢弃），值转 Python `str()` 形式。
#[must_use]
pub(crate) fn string_target(target: Option<&Map<String, Value>>) -> StrMap {
    let mut result = StrMap::new();
    let Some(target) = target else {
        return result;
    };
    for (key, value) in target {
        let Some(string_value) = scalar_to_python_string(value) else {
            continue;
        };
        result.insert(key.clone(), string_value);
    }
    result
}

/// JSON 标量 → Python `str()` 形式（bool 是 `True`/`False`，非标量返回
/// `None` 对应 Python `isinstance` 过滤失败）。
fn scalar_to_python_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Bool(flag) => Some(if *flag { "True" } else { "False" }.to_string()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// 从目标键值推断目标类型字符串（Python `_resolve_target_type`）。
///
/// 只看用户自己填的目标键名（`url` / `repo` / `binary` / `har` ...）——
/// 这是用户给的硬信号，不是对模糊输入的猜测。目标分类枚举已删，这里
/// 只返回可审计的字符串。
#[must_use]
pub(crate) fn resolve_target_type(target: &StrMap) -> &'static str {
    let keys: HashSet<String> = target.iter().map(|(key, _)| key.to_lowercase()).collect();
    let values = target
        .iter()
        .map(|(_, value)| value.to_string())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if ["url", "base_url", "target"]
        .iter()
        .any(|k| keys.contains(*k))
        && values.contains("http")
    {
        return "url";
    }
    if ["repo", "repo_path", "source_root", "source"]
        .iter()
        .any(|k| keys.contains(*k))
    {
        return "source";
    }
    if ["binary", "ida_database"].iter().any(|k| keys.contains(*k)) {
        return "binary";
    }
    if ["artifact_path", "traffic_artifact", "har"]
        .iter()
        .any(|k| keys.contains(*k))
    {
        return "traffic";
    }
    if ["cloud", "account", "cluster"]
        .iter()
        .any(|k| keys.contains(*k))
    {
        return "cloud";
    }
    if keys.len() > 1 {
        "mixed"
    } else {
        "unknown"
    }
}

// 目标类型字符串 → 默认审计域：统一复用 engines::upload_intake 的实现，
// 避免两处各存一份（两者的映射本就一致）。
use engines::upload_intake::audit_domain_for_target_type;

/// Mission 隐式 Project 名（Python `_mission_project_name`）：目标压缩到
/// 单行截 80，空目标回退 `audit mission`。
#[must_use]
pub(crate) fn mission_project_name(user_goal: &str) -> String {
    let compact = user_goal.split_whitespace().collect::<Vec<_>>().join(" ");
    if !compact.is_empty() {
        return compact.chars().take(80).collect();
    }
    "audit mission".to_string()
}

/// 展示标题归一化（Python `_normalize_title`）：折叠空白、剥首尾引号与
/// 空白、截 120；空返回 `None`。
#[must_use]
pub(crate) fn normalize_title(title: Option<&str>) -> Option<String> {
    let title = title?;
    let compact = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = compact
        .trim_matches(|c: char| {
            matches!(
                c,
                '"' | '\'' | '\u{201C}' | '\u{201D}' | '\u{2018}' | '\u{2019}' | ' '
            )
        })
        .to_string();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(120).collect())
}

/// 可选标签归一化（Python `_normalize_optional_label`）：strip 后为空返回
/// `None`。
#[must_use]
pub(crate) fn normalize_optional_label(value: Option<&str>) -> Option<String> {
    let value = value?;
    let stripped = value.trim();
    if stripped.is_empty() {
        None
    } else {
        Some(stripped.to_string())
    }
}

/// 标签去重归一化（Python `_normalize_tags`）：strip→lower→去重→保序，
/// 最多 16 条。
#[must_use]
pub(crate) fn normalize_tags(tags: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut normalized: Vec<String> = Vec::new();
    for item in tags {
        let Some(tag) = normalize_optional_label(Some(item)) else {
            continue;
        };
        let tag = tag.to_lowercase();
        if seen.contains(&tag) {
            continue;
        }
        seen.insert(tag.clone());
        normalized.push(tag);
        if normalized.len() >= 16 {
            break;
        }
    }
    normalized
}

/// `StrMap` → JSON 对象（Python `dict(target)`）。
fn target_json(target: &StrMap) -> Map<String, Value> {
    target
        .iter()
        .map(|(key, value)| (key.to_string(), Value::String(value.to_string())))
        .collect()
}

/// `target or fallback` 的展示值（Python 空字典为假值）：空映射回落
/// fallback，非空取 Python dict repr。
fn display_target(target: &StrMap, fallback_target: &StrMap) -> String {
    if target.is_empty() {
        if fallback_target.is_empty() {
            return "{}".to_string();
        }
        return py_dict_repr(fallback_target);
    }
    py_dict_repr(target)
}

/// `target or name` 的展示值（Python `project.target or project.name`）：
/// 空映射回落 name 字符串，非空取 Python dict repr。
fn display_target_or_name(target: &StrMap, fallback_name: &str) -> String {
    if target.is_empty() {
        fallback_name.to_string()
    } else {
        py_dict_repr(target)
    }
}

/// Python dict repr（`{'url': 'http://x'}`）——Fact statement 文本拼接的
/// 逐字节镜像。
#[must_use]
pub(crate) fn py_dict_repr(target: &StrMap) -> String {
    let inner = target
        .iter()
        .map(|(key, value)| format!("{}: {}", py_str_repr(key), py_str_repr(value)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{inner}}}")
}

/// Python str repr：默认单引号；含单引号且不含双引号时切换双引号；
/// 两者都含时转义单引号。
#[must_use]
pub(crate) fn py_str_repr(value: &str) -> String {
    let has_single = value.contains('\'');
    let has_double = value.contains('"');
    if has_single && !has_double {
        format!("\"{}\"", value.replace('\\', "\\\\"))
    } else if has_single {
        format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
    } else {
        format!("'{}'", value.replace('\\', "\\\\"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use storage::Repository;

    fn manager_with_repo(
        dir: &tempfile::TempDir,
    ) -> (Arc<dyn storage::Repository>, AuditManager) {
        let repo: Arc<dyn storage::Repository> = Arc::new(
            storage::SqliteRepository::open(dir.path().join("lifecycle.sqlite3"))
                .expect("库必须可打开"),
        );
        let manager = AuditManager::new(
            Arc::clone(&repo),
            agents::solver::SolverRegistry::new(),
            Arc::new(crate::task_backend::InMemoryTaskBackend::default()),
        );
        (repo, manager)
    }

    #[test]
    fn deleting_last_mission_also_removes_its_auto_created_project() {
        let dir = tempfile::tempdir().expect("临时目录必须可创建");
        let (repo, manager) = manager_with_repo(&dir);
        let project = models::project::Project::new(
            "solo".to_string(),
            models::domain::AuditDomain::WebRecon,
        );
        let project_id = project.id.as_str().to_string();
        repo.create_project(&project).expect("项目必须可创建");
        let mission = Mission::new(
            models::ids::ProjectId::new(project_id.clone()),
            "goal".to_string(),
        );
        let mission_id = mission.id.as_str().to_string();
        repo.create_mission(&mission).expect("Mission 必须可创建");

        manager.delete_mission(&mission_id).expect("删除必须成功");

        assert!(repo.get_mission(&mission_id).expect("查询").is_none());
        assert!(
            repo.get_project(&project_id).expect("查询").is_none(),
            "删掉最后一条 mission 后，其自动建的空项目必须被级联删除"
        );
    }

    #[test]
    fn deleting_one_mission_keeps_project_with_remaining_missions() {
        let dir = tempfile::tempdir().expect("临时目录必须可创建");
        let (repo, manager) = manager_with_repo(&dir);
        let project = models::project::Project::new(
            "shared".to_string(),
            models::domain::AuditDomain::WebRecon,
        );
        let project_id = project.id.as_str().to_string();
        repo.create_project(&project).expect("项目必须可创建");
        let pid = models::ids::ProjectId::new(project_id.clone());
        let m1 = Mission::new(pid.clone(), "g1".to_string());
        let m1_id = m1.id.as_str().to_string();
        let m2 = Mission::new(pid.clone(), "g2".to_string());
        let m2_id = m2.id.as_str().to_string();
        repo.create_mission(&m1).expect("m1 必须可创建");
        repo.create_mission(&m2).expect("m2 必须可创建");

        manager.delete_mission(&m1_id).expect("删除 m1 必须成功");

        assert!(repo.get_mission(&m1_id).expect("查询").is_none());
        assert!(repo.get_mission(&m2_id).expect("查询").is_some());
        assert!(
            repo.get_project(&project_id).expect("查询").is_some(),
            "仍 有 mission 的项目不得被级联删除"
        );
    }
}
