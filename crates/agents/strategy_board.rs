//! `StrategyBoardMaintainer` 服务 —— `server/core/agents/strategy_board.py`
//! 的移植。
//!
//! 只经由 `ProviderRuntime` 协议感知模型，不知道厂商 SDK、HTTP 客户端、
//! shell 执行或具体工具适配器。板面快照追加只写——`apply_ops` 永远返回
//! 带 `source_snapshot_id` 的新快照，绝不改写旧快照。

use models::agent::ContextPack;
use models::ids::ModelInvocationId;
use models::ids::ProjectId;
use models::ids::ProviderId;
use models::ids::RunId;
use models::knowledge::KnowledgeCard;
use models::project::Project;
use models::run::AuditRun;
use models::strategy_board::StrategyBoardDomain;
use models::strategy_board::StrategyBoardError;
use models::strategy_board::StrategyBoardIdea;
use models::strategy_board::StrategyBoardIdeaStatus;
use models::strategy_board::StrategyBoardKnowledgeCard;
use models::strategy_board::StrategyBoardMemory;
use models::strategy_board::StrategyBoardMemoryKind;
use models::strategy_board::StrategyBoardOpType;
use models::strategy_board::StrategyBoardOperation;
use models::strategy_board::StrategyBoardOps;
use models::strategy_board::StrategyBoardPromptMessage;
use models::strategy_board::StrategyBoardPromptPayload;
use models::strategy_board::StrategyBoardSnapshot;
use serde_json::Map;
use serde_json::Value;

use crate::llm::ProviderCallError;
use crate::llm::ProviderRuntime;
use crate::llm::StructuredGenerationRequest;

/// Python 侧 `StrategyBoardOps` 的 ideas 上限（板面压力契约）。
const MAX_IDEAS: usize = 8;
/// Python 侧 `StrategyBoardOps` 的 memory 上限（板面压力契约）。
const MAX_MEMORY: usize = 12;
/// 效率提醒滚动窗口（`reminders[-4:]`）。
const MAX_REMINDERS: usize = 4;
/// `board_merge` 摘要截断长度。
const SUMMARY_MAX_CHARS: usize = 1000;
/// `efficiency_reminder` 截断长度。
const REMINDER_MAX_CHARS: usize = 300;

/// 维护者系统提示词（Python `STRATEGY_BOARD_PROTOCOL` 逐字节镜像）。
/// 维护者系统提示词（Python `STRATEGY_BOARD_PROTOCOL` 逐字节镜像）。
/// 内置默认以文件存储（resources/prompts/strategy_board_maintainer.md，include_str! 逐字节）。
pub const STRATEGY_BOARD_PROTOCOL: &str = include_str!("../../resources/prompts/strategy_board_maintainer.md");

/// 提示词知识卡视图：Python 侧 `StrategyBoardKnowledgeCard | KnowledgeCard`
/// 结构联合的镜像——两者对提示词只暴露这四个字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptKnowledgeCard {
    /// 卡片标识符。
    pub id: String,
    /// 标题。
    pub title: String,
    /// 内容。
    pub content: String,
    /// 检索标签。
    pub tags: Vec<String>,
}

impl PromptKnowledgeCard {
    /// 从维护者检索卡转换。
    #[must_use]
    pub fn from_board_card(card: &StrategyBoardKnowledgeCard) -> Self {
        Self {
            id: card.id.clone(),
            title: card.title.clone(),
            content: card.content.clone(),
            tags: card.tags.clone(),
        }
    }

    /// 从持久知识卡转换。
    #[must_use]
    pub fn from_knowledge_card(card: &KnowledgeCard) -> Self {
        Self {
            id: card.id.as_str().to_string(),
            title: card.title.clone(),
            content: card.content.clone(),
            tags: card.tags.clone(),
        }
    }
}

/// 确定性内存检索门面，为将来 RAG 化卡片留位
/// （Python `StrategyBoardKnowledgeRetriever`）。
#[derive(Debug, Default)]
pub struct StrategyBoardKnowledgeRetriever {
    cards: Vec<StrategyBoardKnowledgeCard>,
}

impl StrategyBoardKnowledgeRetriever {
    /// 空检索器。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 以初始卡集构造。
    #[must_use]
    pub fn with_cards(cards: Vec<StrategyBoardKnowledgeCard>) -> Self {
        Self { cards }
    }

    /// 取某领域画像的紧凑卡集，无 embedding 模型调用（`retrieve`）。
    ///
    /// 排序键 `(profile_score + tag_score, priority)` 降序；首元素为 0 的
    /// 卡（画像与标签都不命中）被过滤。稳定排序 + Python `sorted(reverse=
    /// True)` 同为保序降序。
    #[must_use]
    pub fn retrieve(
        &self,
        domain_profile: StrategyBoardDomain,
        limit: usize,
        tags: &[String],
    ) -> Vec<&StrategyBoardKnowledgeCard> {
        let tag_set: std::collections::HashSet<String> =
            tags.iter().map(|tag| tag.to_lowercase()).collect();
        let score = |card: &StrategyBoardKnowledgeCard| -> (i64, i64) {
            let profile_score = if card.domain_profiles.contains(&domain_profile) {
                2
            } else {
                i64::from(card.domain_profiles.contains(&StrategyBoardDomain::General))
            };
            let tag_score = card
                .tags
                .iter()
                .map(|tag| tag.to_lowercase())
                .filter(|tag| tag_set.contains(tag))
                .count();
            (
                profile_score + i64::try_from(tag_score).unwrap_or(i64::MAX),
                card.priority,
            )
        };
        let mut ranked: Vec<&StrategyBoardKnowledgeCard> = self.cards.iter().collect();
        ranked.sort_by_key(|card| std::cmp::Reverse(score(card)));
        ranked
            .into_iter()
            .filter(|card| score(card).0 > 0)
            .take(limit)
            .collect()
    }
}

/// `build_prompt_payload` 的输入（Python keyword-only 参数镜像）。
pub struct PromptInput<'a> {
    /// 所属 Project。
    pub project: &'a Project,
    /// 关联 Run。
    pub run: Option<&'a AuditRun>,
    /// 基线快照（缺省时按空板构造）。
    pub snapshot: Option<&'a StrategyBoardSnapshot>,
    /// 上下文包。
    pub context_pack: Option<&'a ContextPack>,
    /// 领域画像（默认 `general`）。
    pub domain_profile: StrategyBoardDomain,
    /// 触发来源（默认 `manual`）。
    pub trigger: &'a str,
    /// 最近活动。
    pub recent_activity: &'a [String],
    /// 知识卡上限（默认 6）。
    pub max_knowledge_cards: usize,
    /// 显式卡集（`None` 时走检索器）。
    pub knowledge_cards: Option<&'a [PromptKnowledgeCard]>,
}

impl<'a> PromptInput<'a> {
    /// 以必填参数构造，其余取 Python 默认值。
    #[must_use]
    pub fn new(
        project: &'a Project,
        run: Option<&'a AuditRun>,
        snapshot: Option<&'a StrategyBoardSnapshot>,
    ) -> Self {
        Self {
            project,
            run,
            snapshot,
            context_pack: None,
            domain_profile: StrategyBoardDomain::General,
            trigger: "manual",
            recent_activity: &[],
            max_knowledge_cards: 6,
            knowledge_cards: None,
        }
    }
}

/// `apply_ops` 的输入（Python keyword-only 参数镜像）。
pub struct ApplyOpsInput<'a> {
    /// 基线快照。
    pub base: &'a StrategyBoardSnapshot,
    /// 待应用的操作批。
    pub ops: &'a StrategyBoardOps,
    /// 触发来源（默认 `manual`）。
    pub trigger: &'a str,
    /// Provider 标识符。
    pub provider_id: Option<&'a ProviderId>,
    /// 模型调用标识符。
    pub model_invocation_id: Option<&'a ModelInvocationId>,
    /// 创建者（默认 `strategy_board_maintainer`）。
    pub created_by: &'a str,
}

impl<'a> ApplyOpsInput<'a> {
    /// 以必填参数构造，其余取 Python 默认值。
    #[must_use]
    pub fn new(base: &'a StrategyBoardSnapshot, ops: &'a StrategyBoardOps) -> Self {
        Self {
            base,
            ops,
            trigger: "manual",
            provider_id: None,
            model_invocation_id: None,
            created_by: "strategy_board_maintainer",
        }
    }
}

/// `propose_ops` 失败：provider 调用失败，或响应不合 `StrategyBoardOps`
/// 契约（Python 侧为开放异常集合，调用方按整体降级处理）。
#[derive(Debug, thiserror::Error)]
pub enum StrategyBoardProposeError {
    /// provider 运行时调用失败。
    #[error(transparent)]
    Provider(#[from] ProviderCallError),
    /// 响应载荷不合操作批契约。
    #[error("invalid strategy board ops payload: {0}")]
    InvalidOps(#[from] serde_json::Error),
}

/// 维护模型输出的记忆 kind 归一（边界容错）。
///
/// 真机观测（2026-09，MiniMax）：维护 purpose 的结构化输出反复写出词表外
/// 的 kind（`objective`、`behavior`，两次运行两种拼法），不在
/// [`models::strategy_board::StrategyBoardMemoryKind`] 词表内，导致整批
/// ops 被拒、auto-maintenance 每轮跳过（用户可见报错）。此处做**确定性
/// 降级**：关键词启发映射到最近词表，未知 kind 兜底 `hint`——与
/// intake_fallback 红线（provider 输出畸形必须确定性降级，serde 错误
/// 绝不到客户端）同一哲学，绝不因模型的 kind 漂移丢弃整批板面操作。
///
/// 返回值为 `StrategyBoardMemoryKind` 的 snake_case wire 词表成员。
fn canonical_memory_kind(kind: &str) -> &'static str {
    let lowered = kind.trim().to_ascii_lowercase();
    match lowered.as_str() {
        "fact" => "fact",
        "evidence" | "proof" => "evidence",
        "failure_boundary" | "failure-boundary" | "failure boundary" => "failure_boundary",
        "constraint" | "limitation" => "constraint",
        "tool_behavior" | "tool-behavior" | "tool behavior" | "behavior" | "behaviour" => {
            "tool_behavior"
        }
        "hint" | "objective" | "goal" | "target" | "aim" => "hint",
        "summary" => "summary",
        _ => {
            if lowered.contains("behavio") || lowered.contains("tool") {
                "tool_behavior"
            } else if lowered.contains("fail") || lowered.contains("boundary") {
                "failure_boundary"
            } else if lowered.contains("evidence") || lowered.contains("proof") {
                "evidence"
            } else if lowered.contains("constrain") || lowered.contains("limit") {
                "constraint"
            } else if lowered.contains("summar") {
                "summary"
            } else if lowered.contains("fact") {
                "fact"
            } else {
                // 兜底：一条无法归类的持久记忆按"引导"处理。
                "hint"
            }
        }
    }
}

/// 在反序列化前把操作批里的记忆 kind 归一为合法词表（见
/// [`canonical_memory_kind`]）。
fn normalize_memory_kinds(payload: &mut Value) {
    let Some(ops) = payload.get_mut("ops").and_then(Value::as_array_mut) else {
        return;
    };
    for op in ops {
        if op.get("kind").and_then(Value::as_str).is_none() {
            continue;
        }
        if let Some(slot) = op.get_mut("kind")
            && let Some(kind) = slot.as_str()
        {
            *slot = Value::String(canonical_memory_kind(kind).to_string());
        }
    }
}

/// 构建提示词、校验操作并落板面快照的服务
/// （Python `StrategyBoardMaintainerService`）。
#[derive(Debug, Default)]
pub struct StrategyBoardMaintainerService {
    knowledge: StrategyBoardKnowledgeRetriever,
}

impl StrategyBoardMaintainerService {
    /// 以指定检索器构造（缺省为空检索器）。
    #[must_use]
    pub fn new(knowledge_retriever: StrategyBoardKnowledgeRetriever) -> Self {
        Self {
            knowledge: knowledge_retriever,
        }
    }

    /// 为某 Project/Run 范围创建空板快照（`empty_snapshot`）。
    #[must_use]
    pub fn empty_snapshot(
        &self,
        project_id: &ProjectId,
        run_id: Option<&RunId>,
        domain_profile: StrategyBoardDomain,
        trigger: &str,
    ) -> StrategyBoardSnapshot {
        let mut snapshot = StrategyBoardSnapshot::new(project_id.clone());
        snapshot.run_id = run_id.cloned();
        snapshot.domain_profile = domain_profile;
        snapshot.trigger = trigger.to_string();
        snapshot.created_by = "strategy_board_service".to_string();
        snapshot
    }

    /// 为外部 sidecar 模型构建完整提示词载荷（`build_prompt_payload`）。
    #[must_use]
    pub fn build_prompt_payload(&self, input: &PromptInput<'_>) -> StrategyBoardPromptPayload {
        let base = input.snapshot.map_or_else(
            || {
                self.empty_snapshot(
                    &input.project.id,
                    input.run.map(|run| &run.id),
                    input.domain_profile,
                    "bootstrap",
                )
            },
            Clone::clone,
        );

        let selected_cards: Vec<PromptKnowledgeCard> = input.knowledge_cards.map_or_else(
            || {
                self.knowledge
                    .retrieve(input.domain_profile, input.max_knowledge_cards, &[])
                    .into_iter()
                    .map(PromptKnowledgeCard::from_board_card)
                    .collect()
            },
            <[PromptKnowledgeCard]>::to_vec,
        );

        let user_payload = build_user_payload(input, &base, &selected_cards);
        let user_content =
            serde_json::to_string(&Value::Object(user_payload.clone())).unwrap_or_default();
        // WP4：维护者系统提示词接 Agent 预设（DB 覆盖优先，回落内置文件默认）。
        let system_prompt = crate::prompts::system_prompt(
            models::agent_preset::PRESET_STRATEGY_BOARD_MAINTAINER,
            STRATEGY_BOARD_PROTOCOL,
        );
        StrategyBoardPromptPayload {
            project_id: input.project.id.clone(),
            run_id: input.run.map(|run| run.id.clone()),
            domain_profile: input.domain_profile,
            snapshot_id: Some(base.id.clone()),
            system_prompt: system_prompt.clone(),
            user_payload,
            messages: vec![
                StrategyBoardPromptMessage::new("system", system_prompt),
                StrategyBoardPromptMessage::new("user", user_content),
            ],
            knowledge_card_ids: selected_cards.iter().map(|card| card.id.clone()).collect(),
        }
    }

    /// 请配置好的 provider 提议板面操作（`propose_ops`）。
    ///
    /// Python 侧第二个返回槽恒为 `None`（为告警预留但从未使用），此处
    /// 直接省略。
    ///
    /// # Errors
    ///
    /// provider 调用失败或响应不合 `StrategyBoardOps` 契约。
    pub async fn propose_ops(
        &self,
        provider_runtime: &dyn ProviderRuntime,
        provider_id: &str,
        payload: &StrategyBoardPromptPayload,
    ) -> Result<StrategyBoardOps, StrategyBoardProposeError> {
        let messages: Vec<crate::llm::LlmMessage> = payload
            .messages
            .iter()
            .map(|message| crate::llm::LlmMessage::new(&message.role, message.content.clone()))
            .collect();
        let result = provider_runtime
            .generate_structured(StructuredGenerationRequest {
                provider_id,
                messages: &messages,
                purpose: "strategy_board_maintainer",
                project_id: Some(&payload.project_id),
                run_id: payload.run_id.as_ref(),
                task_id: None,
            })
            .await?;
        let mut payload = Value::Object(result);
        normalize_memory_kinds(&mut payload);
        let ops: StrategyBoardOps = serde_json::from_value(payload)?;
        Ok(ops)
    }

    /// 应用已校验的操作并返回新的追加式快照（`apply_ops`）。
    ///
    /// # Errors
    ///
    /// 操作缺 content/id、引用不存在的条目，或超出板面压力上限时返回
    /// 对应的 [`StrategyBoardError`]（Python `ValueError` 的类型化对应）。
    pub fn apply_ops(
        &self,
        input: &ApplyOpsInput<'_>,
    ) -> Result<StrategyBoardSnapshot, StrategyBoardError> {
        let mut ideas: Vec<StrategyBoardIdea> = input.base.ideas.clone();
        let mut memory: Vec<StrategyBoardMemory> = input.base.memory.clone();
        let mut reminders: Vec<String> = input.base.efficiency_reminders.clone();
        let mut summary = input.base.summary.clone();
        let now = models::common::utcnow();

        for op in &input.ops.ops {
            match op.op_type {
                StrategyBoardOpType::IdeaAdd
                | StrategyBoardOpType::IdeaUpdate
                | StrategyBoardOpType::IdeaDelete => apply_idea_op(&mut ideas, op, now)?,
                StrategyBoardOpType::MemoryAdd
                | StrategyBoardOpType::MemoryUpdate
                | StrategyBoardOpType::MemoryDelete => apply_memory_op(&mut memory, op, now)?,
                StrategyBoardOpType::BoardMerge => {
                    summary = truncate_chars(&require_content(op)?, SUMMARY_MAX_CHARS);
                }
                StrategyBoardOpType::EfficiencyReminder => {
                    reminders.push(truncate_chars(&require_content(op)?, REMINDER_MAX_CHARS));
                    let overflow = reminders.len().saturating_sub(MAX_REMINDERS);
                    reminders.drain(..overflow);
                }
            }
        }

        let snapshot = StrategyBoardSnapshot {
            id: models::ids::StrategyBoardSnapshotId::new(models::common::new_id("board")),
            project_id: input.base.project_id.clone(),
            run_id: input.base.run_id.clone(),
            source_snapshot_id: Some(input.base.id.clone()),
            version: input.base.version + 1,
            domain_profile: input.base.domain_profile,
            summary,
            ideas,
            memory,
            efficiency_reminders: reminders,
            applied_ops: input.ops.ops.clone(),
            knowledge_card_ids: input.base.knowledge_card_ids.clone(),
            provider_id: input.provider_id.cloned(),
            model_invocation_id: input.model_invocation_id.cloned(),
            trigger: input.trigger.to_string(),
            created_by: input.created_by.to_string(),
            created_at: models::common::utcnow(),
            metadata: input.base.metadata.clone(),
        };
        Ok(snapshot)
    }
}

/// 应用单条 idea 操作（`apply_ops` 内循环的拆分）。
///
/// # Errors
///
/// 操作缺 content/id、引用不存在的 idea，或超出 idea 上限时返回对应
/// [`StrategyBoardError`]。
fn apply_idea_op(
    ideas: &mut Vec<StrategyBoardIdea>,
    op: &StrategyBoardOperation,
    now: models::common::Timestamp,
) -> Result<(), StrategyBoardError> {
    match op.op_type {
        StrategyBoardOpType::IdeaAdd => {
            let content = require_content(op)?;
            if ideas.len() >= MAX_IDEAS {
                return Err(StrategyBoardError::IdeasLimitExceeded);
            }
            ideas.push(StrategyBoardIdea {
                id: models::common::new_id("idea"),
                status: op.status.unwrap_or(StrategyBoardIdeaStatus::Pending),
                content,
                reason: op.reason.clone(),
                refs: op.refs.clone(),
                confidence: 0.5,
                created_at: models::common::utcnow(),
                updated_at: now,
            });
        }
        StrategyBoardOpType::IdeaUpdate => {
            let item_id = require_id(op)?;
            let Some(idea) = ideas.iter_mut().find(|idea| idea.id == item_id) else {
                return Err(StrategyBoardError::UnknownIdea(item_id));
            };
            if let Some(status) = op.status {
                idea.status = status;
            }
            if let Some(content) = &op.content {
                idea.content.clone_from(content);
            }
            if let Some(reason) = &op.reason {
                idea.reason = Some(reason.clone());
            }
            if !op.refs.is_empty() {
                idea.refs.clone_from(&op.refs);
            }
            idea.updated_at = now;
        }
        StrategyBoardOpType::IdeaDelete => {
            let item_id = require_id(op)?;
            if !ideas.iter().any(|idea| idea.id == item_id) {
                return Err(StrategyBoardError::UnknownIdea(item_id));
            }
            ideas.retain(|idea| idea.id != item_id);
        }
        _ => {}
    }
    Ok(())
}

/// 应用单条 memory 操作（`apply_ops` 内循环的拆分）。
///
/// # Errors
///
/// 操作缺 content/id、引用不存在的 memory，或超出 memory 上限时返回对应
/// [`StrategyBoardError`]。
fn apply_memory_op(
    memory: &mut Vec<StrategyBoardMemory>,
    op: &StrategyBoardOperation,
    now: models::common::Timestamp,
) -> Result<(), StrategyBoardError> {
    match op.op_type {
        StrategyBoardOpType::MemoryAdd => {
            let content = require_content(op)?;
            if memory.len() >= MAX_MEMORY {
                return Err(StrategyBoardError::MemoryLimitExceeded);
            }
            memory.push(StrategyBoardMemory {
                id: models::common::new_id("mem"),
                kind: op.kind.unwrap_or(StrategyBoardMemoryKind::Summary),
                content,
                reason: op.reason.clone(),
                refs: op.refs.clone(),
                confidence: 0.7,
                created_at: models::common::utcnow(),
                updated_at: now,
            });
        }
        StrategyBoardOpType::MemoryUpdate => {
            let item_id = require_id(op)?;
            let Some(item) = memory.iter_mut().find(|item| item.id == item_id) else {
                return Err(StrategyBoardError::UnknownMemory(item_id));
            };
            if let Some(kind) = op.kind {
                item.kind = kind;
            }
            if let Some(content) = &op.content {
                item.content.clone_from(content);
            }
            if let Some(reason) = &op.reason {
                item.reason = Some(reason.clone());
            }
            if !op.refs.is_empty() {
                item.refs.clone_from(&op.refs);
            }
            item.updated_at = now;
        }
        StrategyBoardOpType::MemoryDelete => {
            let item_id = require_id(op)?;
            if !memory.iter().any(|item| item.id == item_id) {
                return Err(StrategyBoardError::UnknownMemory(item_id));
            }
            memory.retain(|item| item.id != item_id);
        }
        _ => {}
    }
    Ok(())
}

/// Python `text[:limit]`：按字符截断。
fn truncate_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// 组装提示词的 user 消息载荷（`build_prompt_payload` 内联组装的拆分）。
fn build_user_payload(
    input: &PromptInput<'_>,
    base: &StrategyBoardSnapshot,
    selected_cards: &[PromptKnowledgeCard],
) -> Map<String, Value> {
    let mut user_payload = Map::new();
    user_payload.insert(
        "trigger".to_string(),
        Value::String(input.trigger.to_string()),
    );
    user_payload.insert("project".to_string(), Value::Object(project_payload(input)));
    user_payload.insert(
        "run".to_string(),
        match input.run {
            Some(run) => Value::Object(run_payload(run)),
            None => Value::Null,
        },
    );
    user_payload.insert(
        "domain_profile".to_string(),
        Value::String(input.domain_profile.as_str().to_string()),
    );
    user_payload.insert(
        "current_board".to_string(),
        Value::Object(snapshot_payload(base)),
    );
    user_payload.insert(
        "context_pack".to_string(),
        match input.context_pack {
            Some(pack) => Value::Object(context_pack_payload(pack)),
            None => Value::Null,
        },
    );
    user_payload.insert(
        "recent_activity".to_string(),
        Value::Array(
            input
                .recent_activity
                .iter()
                .map(|item| Value::String(item.clone()))
                .collect(),
        ),
    );
    user_payload.insert(
        "knowledge_cards".to_string(),
        Value::Array(
            selected_cards
                .iter()
                .map(|card| {
                    let mut entry = Map::new();
                    entry.insert("id".to_string(), Value::String(card.id.clone()));
                    entry.insert("title".to_string(), Value::String(card.title.clone()));
                    entry.insert("content".to_string(), Value::String(card.content.clone()));
                    entry.insert(
                        "tags".to_string(),
                        Value::Array(
                            card.tags
                                .iter()
                                .map(|tag| Value::String(tag.clone()))
                                .collect(),
                        ),
                    );
                    Value::Object(entry)
                })
                .collect(),
        ),
    );
    let mut contract = Map::new();
    contract.insert("format".to_string(), Value::String("json".to_string()));
    contract.insert("root_key".to_string(), Value::String("ops".to_string()));
    contract.insert("max_ops".to_string(), Value::from(4));
    user_payload.insert("response_contract".to_string(), Value::Object(contract));
    user_payload
}

/// Project → 提示词载荷（`build_prompt_payload` 内联组装的拆分）。
fn project_payload(input: &PromptInput<'_>) -> Map<String, Value> {
    let mut payload = Map::new();
    payload.insert(
        "id".to_string(),
        Value::String(input.project.id.as_str().to_string()),
    );
    payload.insert(
        "name".to_string(),
        Value::String(input.project.name.clone()),
    );
    payload.insert(
        "audit_domain".to_string(),
        Value::String(input.project.audit_domain.as_str().to_string()),
    );
    payload.insert(
        "description".to_string(),
        match &input.project.description {
            Some(text) => Value::String(text.clone()),
            None => Value::Null,
        },
    );
    payload.insert(
        "target".to_string(),
        serde_json::to_value(&input.project.target).unwrap_or(Value::Null),
    );
    payload
}

/// 板面快照 → 提示词载荷（Python `_snapshot_payload`）。
fn snapshot_payload(snapshot: &StrategyBoardSnapshot) -> Map<String, Value> {
    let mut payload = Map::new();
    payload.insert(
        "id".to_string(),
        Value::String(snapshot.id.as_str().to_string()),
    );
    payload.insert("version".to_string(), Value::from(snapshot.version));
    payload.insert(
        "summary".to_string(),
        Value::String(snapshot.summary.clone()),
    );
    payload.insert(
        "ideas".to_string(),
        Value::Array(
            snapshot
                .ideas
                .iter()
                .map(|idea| serde_json::to_value(idea).unwrap_or(Value::Null))
                .collect(),
        ),
    );
    payload.insert(
        "memory".to_string(),
        Value::Array(
            snapshot
                .memory
                .iter()
                .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
                .collect(),
        ),
    );
    payload.insert(
        "efficiency_reminders".to_string(),
        Value::Array(
            snapshot
                .efficiency_reminders
                .iter()
                .map(|item| Value::String(item.clone()))
                .collect(),
        ),
    );
    payload
}

/// Run → 提示词载荷（Python `_run_payload`）。
fn run_payload(run: &AuditRun) -> Map<String, Value> {
    let mut payload = Map::new();
    payload.insert("id".to_string(), Value::String(run.id.as_str().to_string()));
    payload.insert(
        "status".to_string(),
        Value::String(run.status.as_str().to_string()),
    );
    payload.insert("steps_used".to_string(), Value::from(run.steps_used));
    payload.insert(
        "max_total_steps".to_string(),
        Value::from(run.max_total_steps),
    );
    payload.insert(
        "task_ids".to_string(),
        Value::Array(
            run.task_ids
                .iter()
                .map(|id| Value::String(id.clone()))
                .collect(),
        ),
    );
    payload.insert(
        "note".to_string(),
        match &run.note {
            Some(note) => Value::String(note.clone()),
            None => Value::Null,
        },
    );
    payload
}

/// `ContextPack` → 提示词载荷（Python `_context_pack_payload`）。
fn context_pack_payload(pack: &ContextPack) -> Map<String, Value> {
    let mut payload = Map::new();
    payload.insert(
        "id".to_string(),
        Value::String(pack.id.as_str().to_string()),
    );
    payload.insert("purpose".to_string(), Value::String(pack.purpose.clone()));
    payload.insert("summary".to_string(), Value::String(pack.summary.clone()));
    payload.insert(
        "facts".to_string(),
        Value::Array(
            pack.facts
                .iter()
                .map(|item| Value::String(item.clone()))
                .collect(),
        ),
    );
    payload.insert(
        "intents".to_string(),
        Value::Array(
            pack.intents
                .iter()
                .map(|item| Value::String(item.clone()))
                .collect(),
        ),
    );
    payload.insert(
        "hints".to_string(),
        Value::Array(
            pack.hints
                .iter()
                .map(|item| Value::String(item.clone()))
                .collect(),
        ),
    );
    payload.insert(
        "evidence_ids".to_string(),
        Value::Array(
            pack.evidence_ids
                .iter()
                .map(|item| Value::String(item.clone()))
                .collect(),
        ),
    );
    payload.insert(
        "finding_ids".to_string(),
        Value::Array(
            pack.finding_ids
                .iter()
                .map(|item| Value::String(item.clone()))
                .collect(),
        ),
    );
    payload.insert(
        "tool_invocation_ids".to_string(),
        Value::Array(
            pack.tool_invocation_ids
                .iter()
                .map(|item| Value::String(item.clone()))
                .collect(),
        ),
    );
    payload.insert(
        "operation_log_records".to_string(),
        Value::Array(
            pack.operation_log_records
                .iter()
                .map(|record| Value::Object(record.clone()))
                .collect(),
        ),
    );
    payload.insert("metadata".to_string(), Value::Object(pack.metadata.clone()));
    payload
}

/// 操作必须有非空 content（Python `_require_content`）。
fn require_content(op: &StrategyBoardOperation) -> Result<String, StrategyBoardError> {
    match &op.content {
        Some(content) if !content.trim().is_empty() => Ok(content.trim().to_string()),
        _ => Err(StrategyBoardError::MissingContent {
            op_type: op.op_type.as_str().to_string(),
        }),
    }
}

/// 操作必须有非空 id（Python `_require_id`）。
fn require_id(op: &StrategyBoardOperation) -> Result<String, StrategyBoardError> {
    match &op.id {
        Some(id) if !id.trim().is_empty() => Ok(id.clone()),
        _ => Err(StrategyBoardError::MissingId {
            op_type: op.op_type.as_str().to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::common::StrMap;
    use models::common::utcnow;
    use models::domain::AuditDomain;
    use models::project::Project;
    use models::run::AuditRun;

    fn service() -> StrategyBoardMaintainerService {
        StrategyBoardMaintainerService::default()
    }

    fn project() -> Project {
        Project::new("board-project".to_string(), AuditDomain::WebSast)
    }

    fn run(project_id: ProjectId) -> AuditRun {
        AuditRun {
            id: RunId::new("run_board".to_string()),
            project_id,
            mission_id: None,
            status: models::lifecycle::RunStatus::Running,
            config: Map::new(),
            max_total_steps: 48,
            steps_used: 3,
            task_ids: vec!["task_1".to_string()],
            note: Some("note".to_string()),
            created_at: utcnow(),
            started_at: Some(utcnow()),
            finished_at: None,
            updated_at: utcnow(),
        }
    }

    fn idea_add(content: &str) -> StrategyBoardOperation {
        StrategyBoardOperation {
            op_type: StrategyBoardOpType::IdeaAdd,
            id: None,
            status: None,
            kind: None,
            content: Some(content.to_string()),
            reason: None,
            refs: Vec::new(),
            metadata: Map::new(),
        }
    }

    #[test]
    fn snapshot_limits_enforce_eight_ideas() {
        let svc = service();
        let base = svc.empty_snapshot(
            &ProjectId::new("proj_1".to_string()),
            None,
            StrategyBoardDomain::General,
            "bootstrap",
        );
        let ops = StrategyBoardOps {
            ops: (0..4).map(|idx| idea_add(&format!("idea {idx}"))).collect(),
        };

        let current = svc
            .apply_ops(&ApplyOpsInput::new(&base, &ops))
            .unwrap_or_else(|error| panic!("首批 4 条 idea_add 必须成功: {error}"));
        let current = svc
            .apply_ops(&ApplyOpsInput::new(&current, &ops))
            .unwrap_or_else(|error| panic!("次批 4 条 idea_add 必须成功: {error}"));
        assert_eq!(current.ideas.len(), 8);

        let overflow = StrategyBoardOps {
            ops: vec![idea_add("overflow")],
        };
        assert_eq!(
            svc.apply_ops(&ApplyOpsInput::new(&current, &overflow)),
            Err(StrategyBoardError::IdeasLimitExceeded)
        );
    }

    #[test]
    fn apply_ops_updates_and_deletes() {
        let svc = service();
        let base = svc.empty_snapshot(
            &ProjectId::new("proj_1".to_string()),
            None,
            StrategyBoardDomain::VulnerabilityResearch,
            "bootstrap",
        );
        let added = svc
            .apply_ops(&ApplyOpsInput::new(
                &base,
                &StrategyBoardOps {
                    ops: vec![
                        idea_add("validate current lead"),
                        StrategyBoardOperation {
                            op_type: StrategyBoardOpType::MemoryAdd,
                            id: None,
                            status: None,
                            kind: Some(StrategyBoardMemoryKind::FailureBoundary),
                            content: Some(
                                "tool timed out once; retry with smaller scope".to_string(),
                            ),
                            reason: None,
                            refs: Vec::new(),
                            metadata: Map::new(),
                        },
                    ],
                },
            ))
            .unwrap_or_else(|error| panic!("add 批必须成功: {error}"));
        let idea_id = added.ideas[0].id.clone();
        let memory_id = added.memory[0].id.clone();

        let updated = svc
            .apply_ops(&ApplyOpsInput::new(
                &added,
                &StrategyBoardOps {
                    ops: vec![
                        StrategyBoardOperation {
                            op_type: StrategyBoardOpType::IdeaUpdate,
                            id: Some(idea_id),
                            status: Some(StrategyBoardIdeaStatus::Testing),
                            kind: None,
                            content: Some("validate narrowed lead".to_string()),
                            reason: None,
                            refs: Vec::new(),
                            metadata: Map::new(),
                        },
                        StrategyBoardOperation {
                            op_type: StrategyBoardOpType::MemoryDelete,
                            id: Some(memory_id),
                            status: None,
                            kind: None,
                            content: None,
                            reason: None,
                            refs: Vec::new(),
                            metadata: Map::new(),
                        },
                    ],
                },
            ))
            .unwrap_or_else(|error| panic!("update/delete 批必须成功: {error}"));

        assert_eq!(updated.version, added.version + 1);
        assert_eq!(updated.source_snapshot_id, Some(added.id.clone()));
        assert_eq!(updated.ideas[0].status, StrategyBoardIdeaStatus::Testing);
        assert_eq!(updated.ideas[0].content, "validate narrowed lead");
        assert!(updated.memory.is_empty());
    }

    #[test]
    fn apply_ops_rejects_unknown_update_id() {
        let svc = service();
        let base = svc.empty_snapshot(
            &ProjectId::new("proj_1".to_string()),
            None,
            StrategyBoardDomain::General,
            "bootstrap",
        );
        let ops = StrategyBoardOps {
            ops: vec![StrategyBoardOperation {
                op_type: StrategyBoardOpType::IdeaUpdate,
                id: Some("idea_missing".to_string()),
                status: None,
                kind: None,
                content: Some("x".to_string()),
                reason: None,
                refs: Vec::new(),
                metadata: Map::new(),
            }],
        };
        assert_eq!(
            svc.apply_ops(&ApplyOpsInput::new(&base, &ops)),
            Err(StrategyBoardError::UnknownIdea("idea_missing".to_string()))
        );
    }

    #[test]
    fn efficiency_reminders_truncate_and_roll_last_four() {
        let svc = service();
        let base = svc.empty_snapshot(
            &ProjectId::new("proj_1".to_string()),
            None,
            StrategyBoardDomain::General,
            "bootstrap",
        );
        let reminder = |content: &str| StrategyBoardOperation {
            op_type: StrategyBoardOpType::EfficiencyReminder,
            id: None,
            status: None,
            kind: None,
            content: Some(content.to_string()),
            reason: None,
            refs: Vec::new(),
            metadata: Map::new(),
        };
        let mut current = base;
        for idx in 0..6 {
            current = svc
                .apply_ops(&ApplyOpsInput::new(
                    &current,
                    &StrategyBoardOps {
                        ops: vec![reminder(&format!("reminder {idx}"))],
                    },
                ))
                .unwrap_or_else(|error| panic!("efficiency_reminder 必须成功: {error}"));
        }
        assert_eq!(current.efficiency_reminders.len(), 4);
        assert_eq!(
            current.efficiency_reminders[0], "reminder 2",
            "滚动窗口保留最后 4 条"
        );
    }

    #[test]
    fn board_merge_truncates_summary_to_thousand_chars() {
        let svc = service();
        let base = svc.empty_snapshot(
            &ProjectId::new("proj_1".to_string()),
            None,
            StrategyBoardDomain::General,
            "bootstrap",
        );
        let long = "x".repeat(1200);
        let merged = svc
            .apply_ops(&ApplyOpsInput::new(
                &base,
                &StrategyBoardOps {
                    ops: vec![StrategyBoardOperation {
                        op_type: StrategyBoardOpType::BoardMerge,
                        id: None,
                        status: None,
                        kind: None,
                        content: Some(long),
                        reason: None,
                        refs: Vec::new(),
                        metadata: Map::new(),
                    }],
                },
            ))
            .unwrap_or_else(|error| panic!("board_merge 必须成功: {error}"));
        assert_eq!(merged.summary.chars().count(), 1000);
    }

    #[test]
    fn prompt_payload_carries_project_run_and_board() {
        let svc = service();
        let project = project();
        let run = run(project.id.clone());
        let input = PromptInput::new(&project, Some(&run), None);
        let payload = svc.build_prompt_payload(&input);

        assert_eq!(payload.domain_profile, StrategyBoardDomain::General);
        assert_eq!(payload.messages.len(), 2);
        assert_eq!(payload.messages[0].role, "system");
        assert_eq!(payload.messages[1].role, "user");
        assert_eq!(
            payload.messages[0].content, STRATEGY_BOARD_PROTOCOL,
            "系统消息即协议文本"
        );
        assert!(payload.snapshot_id.is_some());

        let keys: Vec<&str> = payload.user_payload.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "trigger",
                "project",
                "run",
                "domain_profile",
                "current_board",
                "context_pack",
                "recent_activity",
                "knowledge_cards",
                "response_contract",
            ]
        );
        let project_payload = payload.user_payload["project"]
            .as_object()
            .unwrap_or_else(|| panic!("project 必须是对象"));
        assert_eq!(
            project_payload["audit_domain"],
            Value::String("web_sast".to_string())
        );
        let run_payload = payload.user_payload["run"]
            .as_object()
            .unwrap_or_else(|| panic!("run 必须是对象"));
        assert_eq!(run_payload["status"], Value::String("running".to_string()));
        assert_eq!(run_payload["max_total_steps"], Value::from(48));
        assert_eq!(payload.user_payload["context_pack"], Value::Null);
        assert_eq!(
            payload.user_payload["response_contract"],
            serde_json::json!({"format": "json", "root_key": "ops", "max_ops": 4})
        );
    }

    #[test]
    fn prompt_payload_uses_explicit_knowledge_cards() {
        let card = StrategyBoardKnowledgeCard {
            id: "kcard_x".to_string(),
            title: "SQLi primer".to_string(),
            content: "Use parameterized queries".to_string(),
            domain_profiles: vec![StrategyBoardDomain::VulnerabilityResearch],
            tags: vec!["sqli".to_string()],
            priority: 80,
        };
        let retriever = StrategyBoardKnowledgeRetriever::with_cards(vec![card.clone()]);
        let svc = StrategyBoardMaintainerService::new(retriever);

        let mut project = project();
        let mut target = StrMap::new();
        target.insert("url".to_string(), "https://app.example.test".to_string());
        project.target = target;
        let prompt_cards = [PromptKnowledgeCard::from_board_card(&card)];
        let mut input = PromptInput::new(&project, None, None);
        input.knowledge_cards = Some(&prompt_cards);
        let payload = svc.build_prompt_payload(&input);

        assert_eq!(payload.knowledge_card_ids, ["kcard_x"]);
        let cards = payload.user_payload["knowledge_cards"]
            .as_array()
            .unwrap_or_else(|| panic!("knowledge_cards 必须是数组"));
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0]["title"], Value::String("SQLi primer".to_string()));
    }

    #[test]
    fn retriever_ranks_by_profile_tag_and_priority() {
        let matching = StrategyBoardKnowledgeCard {
            id: "kcard_match".to_string(),
            title: "match".to_string(),
            content: "c".to_string(),
            domain_profiles: vec![StrategyBoardDomain::CtfWeb],
            tags: Vec::new(),
            priority: 10,
        };
        let general = StrategyBoardKnowledgeCard {
            id: "kcard_general".to_string(),
            title: "general".to_string(),
            content: "c".to_string(),
            domain_profiles: vec![StrategyBoardDomain::General],
            tags: Vec::new(),
            priority: 99,
        };
        let unmatched = StrategyBoardKnowledgeCard {
            id: "kcard_none".to_string(),
            title: "none".to_string(),
            content: "c".to_string(),
            domain_profiles: vec![StrategyBoardDomain::BinaryStatic],
            tags: Vec::new(),
            priority: 100,
        };
        let retriever =
            StrategyBoardKnowledgeRetriever::with_cards(vec![unmatched, general, matching]);
        let results = retriever.retrieve(StrategyBoardDomain::CtfWeb, 6, &[]);
        let ids: Vec<&str> = results.iter().map(|card| card.id.as_str()).collect();
        assert_eq!(ids, ["kcard_match", "kcard_general"]);
    }

    #[tokio::test]
    async fn propose_ops_validates_provider_response() {
        use crate::llm::LlmResponse;
        use crate::llm::ProviderCallError;

        struct FakeRuntime;

        #[async_trait::async_trait]
        impl ProviderRuntime for FakeRuntime {
            async fn list_providers(
                &self,
            ) -> Result<Vec<models::provider::ProviderConfig>, ProviderCallError> {
                Ok(Vec::new())
            }
            async fn get_provider(
                &self,
                _provider_id: &str,
            ) -> Result<Option<models::provider::ProviderConfig>, ProviderCallError> {
                Ok(None)
            }
            async fn resolve_default_provider(
                &self,
            ) -> Result<Option<models::provider::ProviderConfig>, ProviderCallError> {
                Ok(None)
            }
            async fn health_check(
                &self,
                _provider_id: &str,
            ) -> Result<models::provider::ProviderHealthResult, ProviderCallError> {
                Ok(models::provider::ProviderHealthResult::new(
                    models::ids::ProviderId::new("fake".to_string()),
                    models::provider::ModelInvocationStatus::Ok,
                    "ok".to_string(),
                ))
            }
            async fn generate_text(
                &self,
                _request: crate::llm::TextGenerationRequest<'_>,
            ) -> Result<LlmResponse, ProviderCallError> {
                Ok(LlmResponse::new(String::new()))
            }
            async fn generate_structured(
                &self,
                _request: crate::llm::StructuredGenerationRequest<'_>,
            ) -> Result<Map<String, Value>, ProviderCallError> {
                Ok(serde_json::json!({
                    "ops": [
                        {
                            "type": "memory_add",
                            "kind": "constraint",
                            "content": "keep board compact",
                            "reason": "test"
                        }
                    ]
                })
                .as_object()
                .unwrap_or_else(|| panic!("字面量是对象"))
                .clone())
            }
        }

        let svc = service();
        let project = project();
        let payload = svc.build_prompt_payload(&PromptInput::new(&project, None, None));
        let ops = svc
            .propose_ops(&FakeRuntime, "provider_1", &payload)
            .await
            .unwrap_or_else(|error| panic!("合法 ops 响应必须通过校验: {error}"));
        assert_eq!(ops.ops.len(), 1);
        assert_eq!(ops.ops[0].op_type, StrategyBoardOpType::MemoryAdd);
        assert_eq!(ops.ops[0].kind, Some(StrategyBoardMemoryKind::Constraint));
    }

    #[test]
    fn memory_kind_synonyms_are_normalized_case_insensitively() {
        for (raw, expected) in [
            ("objective", StrategyBoardMemoryKind::Hint),
            ("Objective", StrategyBoardMemoryKind::Hint),
            ("goal", StrategyBoardMemoryKind::Hint),
            ("target", StrategyBoardMemoryKind::Hint),
            ("aim", StrategyBoardMemoryKind::Hint),
            ("hint", StrategyBoardMemoryKind::Hint),
            ("fact", StrategyBoardMemoryKind::Fact),
            // 真机观测（2026-09-03）：kind 漂移为 behavior。
            ("behavior", StrategyBoardMemoryKind::ToolBehavior),
            ("Tool Behavior", StrategyBoardMemoryKind::ToolBehavior),
            ("failure boundary", StrategyBoardMemoryKind::FailureBoundary),
            ("evidence", StrategyBoardMemoryKind::Evidence),
            ("summary", StrategyBoardMemoryKind::Summary),
        ] {
            let mut payload = serde_json::json!({
                "ops": [{ "type": "memory_add", "kind": raw, "content": "x" }]
            });
            normalize_memory_kinds(&mut payload);
            let ops: StrategyBoardOps = serde_json::from_value(payload)
                .unwrap_or_else(|error| panic!("kind {raw} 必须可解析: {error}"));
            assert_eq!(ops.ops[0].kind, Some(expected), "kind {raw}");
        }
    }

    #[test]
    fn unknown_memory_kind_degrades_deterministically_to_hint() {
        for raw in ["mystery_kind", "平台的特殊记忆", "kind-xyz"] {
            let mut payload = serde_json::json!({
                "ops": [{ "type": "memory_add", "kind": raw, "content": "x" }]
            });
            normalize_memory_kinds(&mut payload);
            let ops: StrategyBoardOps = serde_json::from_value(payload)
                .unwrap_or_else(|error| panic!("kind {raw} 必须降级而非拒绝: {error}"));
            assert_eq!(ops.ops[0].kind, Some(StrategyBoardMemoryKind::Hint));
        }
    }

    #[tokio::test]
    async fn propose_ops_normalizes_objective_kind_from_model_output() {
        use crate::llm::LlmResponse;

        struct ObjectiveRuntime;
        #[async_trait::async_trait]
        impl ProviderRuntime for ObjectiveRuntime {
            async fn list_providers(
                &self,
            ) -> Result<Vec<models::provider::ProviderConfig>, ProviderCallError> {
                Ok(Vec::new())
            }
            async fn get_provider(
                &self,
                _provider_id: &str,
            ) -> Result<Option<models::provider::ProviderConfig>, ProviderCallError> {
                Ok(None)
            }
            async fn resolve_default_provider(
                &self,
            ) -> Result<Option<models::provider::ProviderConfig>, ProviderCallError> {
                Ok(None)
            }
            async fn health_check(
                &self,
                _provider_id: &str,
            ) -> Result<models::provider::ProviderHealthResult, ProviderCallError> {
                Ok(models::provider::ProviderHealthResult::new(
                    models::ids::ProviderId::new("fake".to_string()),
                    models::provider::ModelInvocationStatus::Ok,
                    "ok".to_string(),
                ))
            }
            async fn generate_text(
                &self,
                _request: crate::llm::TextGenerationRequest<'_>,
            ) -> Result<LlmResponse, ProviderCallError> {
                Err(ProviderCallError::new("unused"))
            }
            async fn generate_structured(
                &self,
                _request: crate::llm::StructuredGenerationRequest<'_>,
            ) -> Result<Map<String, Value>, ProviderCallError> {
                // 真机观测形态：模型把记忆 kind 写成 "objective"。
                Ok(serde_json::json!({
                    "ops": [
                        {
                            "type": "memory_add",
                            "kind": "objective",
                            "content": "capture the flag within budget",
                            "reason": "test"
                        }
                    ]
                })
                .as_object()
                .unwrap_or_else(|| panic!("字面量是对象"))
                .clone())
            }
        }

        let svc = service();
        let project = project();
        let payload = svc.build_prompt_payload(&PromptInput::new(&project, None, None));
        let ops = svc
            .propose_ops(&ObjectiveRuntime, "provider_1", &payload)
            .await
            .unwrap_or_else(|error| panic!("objective kind 必须被归一化而非整批拒绝: {error}"));
        assert_eq!(ops.ops.len(), 1);
        assert_eq!(ops.ops[0].kind, Some(StrategyBoardMemoryKind::Hint));
    }
}
