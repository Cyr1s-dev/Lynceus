//! 确定性的 run 级上下文装配 —— `server/core/agents/context.py` 的移植。
//!
//! 面向用户的投影（`summary` 与各类文本条目）保持紧凑并按 token 预算
//! 裁剪；面向模型的操作日志窗口从脱敏后的 Mission journal **无损**拷贝进
//! `operation_log_records`，绝不摘要化——摘要绝不替代结构化记录本身。
//!
//! Python 侧 `build_context_pack` 是 async 的，但那只为了让同一协议能被
//! SQL/网络后端实现；Rust 仓储层是同步的（见
//! `storage::repository` 的偏差记录），本模块随之同步，异步边界
//! 留给引擎层。
//!
//! 路径边界校验（`_operation_log_window`）与 journal 内部复用同一
//! [`storage::normalize`] 归一化，保证"解析侧"与"读取侧"看到同一
//! 形态的路径。

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use models::agent::ContextCompressionReport;
use models::agent::ContextPack;
use models::evidence::Evidence;
use models::fact::Fact;
use models::fact::GraphNodeType;
use models::finding::Finding;
use models::hint::Hint;
use models::ids::ProjectId;
use models::ids::RunId;
use models::ids::TaskId;
use models::intent::Intent;
use models::intent::IntentStatus;
use models::lifecycle::FindingStatus;
use models::lifecycle::Severity;
use models::lifecycle::ToolStatus;
use models::tool_invocation::ToolInvocation;
use models::trajectory::TrajectorySummary;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use storage::Repository;
use storage::StorageError;
use storage::SwarmOperationJournal;
use storage::SwarmOperationRecord;

use storage::normalize as normalize_path;

/// Python `_SEVERITY_RANK`：Finding 优先级排序用的严重级别权重。
fn severity_rank(severity: Severity) -> i64 {
    match severity {
        Severity::Critical => 5,
        Severity::High => 4,
        Severity::Medium => 3,
        Severity::Low => 2,
        Severity::Info => 1,
    }
}

/// 一次压缩通过（pass）的运行期返回值（Python `ContextCompressionResult`）。
#[derive(Debug, Clone, PartialEq)]
pub struct ContextCompressionResult {
    /// 构建出的上下文包。
    pub context_pack: ContextPack,
    /// 压缩审计报告。
    pub report: ContextCompressionReport,
}

/// 上下文装配失败的原因。
#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    /// 仓储操作失败。
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// 操作日志读取失败。
    #[error(transparent)]
    Journal(#[from] storage::JournalError),
    /// 操作记录序列化失败（理论不可达：记录字段全部 JSON 可表示）。
    #[error("serializing operation record failed: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// `build_context_pack` 的输入（Python keyword-only 参数组镜像）。
pub struct BuildContextInput<'a> {
    /// 所属 Project。
    pub project_id: &'a str,
    /// 所属 Run。
    pub run_id: &'a str,
    /// 关联 Task（Python 默认 `None`）。
    pub task_id: Option<&'a str>,
    /// 用途（Python 默认 `"solver"`）。
    pub purpose: &'a str,
    /// token 预算（Python 默认 2048）。
    pub token_budget: i64,
    /// 当前 Intent id（Python 默认 `None`）。
    pub current_intent_id: Option<&'a str>,
    /// 压缩策略标签（Python 默认 `"deterministic_v1"`）。
    pub compression_strategy: &'a str,
}

impl<'a> BuildContextInput<'a> {
    /// 以必填参数构造，其余取 Python 默认值。
    #[must_use]
    pub fn new(project_id: &'a str, run_id: &'a str) -> Self {
        Self {
            project_id,
            run_id,
            task_id: None,
            purpose: "solver",
            token_budget: 2048,
            current_intent_id: None,
            compression_strategy: "deterministic_v1",
        }
    }
}

/// 从仓储状态构建工件感知的 ContextPack（Python `ContextCompressor`）。
///
/// 持有仓储与共享的操作日志构件（manager 把自己的 journal 传入，使追加
/// 与读取共享同一序列号缓存）。
pub struct ContextCompressor {
    repo: Arc<dyn Repository>,
    operation_journal: Arc<SwarmOperationJournal>,
}

impl ContextCompressor {
    /// Python `ContextCompressor(repository, operation_journal=None)`：不传
    /// journal 时新建独立实例。
    #[must_use]
    pub fn new(repo: Arc<dyn Repository>) -> Self {
        Self {
            repo,
            operation_journal: Arc::new(SwarmOperationJournal::default()),
        }
    }

    /// 带显式操作日志构件构造（与 manager 共享同一 journal 时使用）。
    #[must_use]
    pub fn with_operation_journal(
        repo: Arc<dyn Repository>,
        operation_journal: Arc<SwarmOperationJournal>,
    ) -> Self {
        Self {
            repo,
            operation_journal,
        }
    }

    /// 把 run 状态压缩为有界的 `ContextPack` 加审计报告
    /// （Python `build_context_pack`）。
    ///
    /// # Errors
    ///
    /// 仓储操作失败（[`ContextError::Storage`]）、操作日志读取失败
    /// （[`ContextError::Journal`]）或操作记录序列化失败
    /// （[`ContextError::Serialize`]，实际不可达）。
    // Python 侧是单个 267 行方法，1:1 移植保持同构，不按 lint 拆分。
    #[allow(clippy::too_many_lines)]
    pub fn build_context_pack(
        &self,
        input: &BuildContextInput<'_>,
    ) -> Result<ContextCompressionResult, ContextError> {
        let project_id = input.project_id;
        let run_id = input.run_id;

        let facts = self.repo.list_facts(project_id)?;
        let intents: Vec<Intent> = self
            .repo
            .list_intents(project_id)?
            .into_iter()
            .filter(|intent| run_scope_match(intent.run_id.as_ref(), run_id))
            .collect();
        let hints = self.repo.list_hints(project_id)?;
        let evidence: Vec<Evidence> = self
            .repo
            .list_evidence(project_id)?
            .into_iter()
            .filter(|item| run_scope_match(item.run_id.as_ref(), run_id))
            .collect();
        let findings: Vec<Finding> = self
            .repo
            .list_findings(project_id)?
            .into_iter()
            .filter(|item| run_scope_match(item.run_id.as_ref(), run_id))
            .collect();
        let tools: Vec<ToolInvocation> = self
            .repo
            .list_tool_invocations(Some(project_id))?
            .into_iter()
            .filter(|item| run_scope_match(item.run_id.as_ref(), run_id))
            .collect();
        let trajectory_summaries =
            self.repo
                .list_trajectory_summaries(project_id, Some(run_id), None)?;

        let current_intent =
            resolve_current_intent(&intents, input.current_intent_id, input.task_id);
        let current_source_fact_ids: HashSet<&str> = current_intent
            .map(|intent| intent.source_fact_ids.iter().map(String::as_str).collect())
            .unwrap_or_default();
        let branch_id = current_intent.and_then(|intent| {
            intent
                .branch_id
                .as_ref()
                .map(|branch| branch.as_str().to_string())
        });
        let latest_trajectory = latest_trajectory(&trajectory_summaries, branch_id.as_deref());
        let (operation_records, operation_log_metadata) = self.operation_log_window(
            project_id,
            run_id,
            input.task_id,
            branch_id.as_deref(),
            input.token_budget,
        )?;
        // Python `isinstance(x, int) and not isinstance(x, bool)`：JSON 的
        // 整数 Number 才计入，bool/浮点一律归零。
        let operation_log_total = operation_log_metadata
            .get("matching_total")
            .and_then(Value::as_i64)
            .unwrap_or(0);

        let mut budget_left = input.token_budget;
        let mut warnings: Vec<String> = Vec::new();

        let mut selected_facts: Vec<&Fact> = Vec::new();
        let mandatory_facts: Vec<&Fact> = facts
            .iter()
            .filter(|fact| {
                matches!(
                    fact.node_type,
                    GraphNodeType::OriginFact | GraphNodeType::GoalFact
                ) || current_source_fact_ids.contains(fact.id.as_str())
            })
            .collect();
        for fact in dedupe_by_id(&mandatory_facts) {
            selected_facts.push(fact);
            budget_left -= estimate_tokens(&fact_text(fact));
        }

        if budget_left < 0 {
            warnings
                .push("mandatory origin/goal/current-intent facts exceed token budget".to_string());
        }

        let selected_fact_ids: HashSet<&str> =
            selected_facts.iter().map(|fact| fact.id.as_str()).collect();
        let mut optional_facts: Vec<&Fact> = facts
            .iter()
            .filter(|fact| !selected_fact_ids.contains(fact.id.as_str()))
            .collect();
        // Python `sort(key=(confidence, created_at), reverse=True)`：置信度
        // 降序，并列时创建时间降序，稳定排序保持原序。
        optional_facts.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(Ordering::Equal)
                .then_with(|| {
                    b.created_at
                        .partial_cmp(&a.created_at)
                        .unwrap_or(Ordering::Equal)
                })
        });
        for fact in optional_facts {
            budget_left =
                include_if_budget(&mut selected_facts, fact, budget_left, &fact_text(fact));
        }

        let mut selected_intents: Vec<&Intent> = Vec::new();
        for intent in prioritize_intents(&intents, current_intent) {
            budget_left = include_if_budget(
                &mut selected_intents,
                intent,
                budget_left,
                &intent_text(intent),
            );
        }

        let mut selected_hints: Vec<&Hint> = Vec::new();
        let mut sorted_hints: Vec<&Hint> = hints.iter().collect();
        sorted_hints.sort_by(|a, b| {
            b.weight.cmp(&a.weight).then_with(|| {
                b.created_at
                    .partial_cmp(&a.created_at)
                    .unwrap_or(Ordering::Equal)
            })
        });
        for hint in sorted_hints {
            budget_left =
                include_if_budget(&mut selected_hints, hint, budget_left, &hint_text(hint));
        }

        let selected_findings = select_findings(&findings, input.token_budget);
        let selected_evidence_ids =
            select_evidence_ids(&evidence, &selected_findings, input.token_budget);
        let recent_failures = recent_failed_tools(&tools);
        let artifact_refs = artifact_refs(&evidence, &tools);

        let fact_texts: Vec<String> = selected_facts
            .iter()
            .map(|fact| truncate(&fact_text(fact), 240))
            .collect();
        let intent_texts: Vec<String> = selected_intents
            .iter()
            .map(|intent| truncate(&intent_text(intent), 220))
            .collect();
        let hint_texts: Vec<String> = selected_hints
            .iter()
            .map(|hint| truncate(&hint_text(hint), 180))
            .collect();

        let summary = build_summary(
            input.purpose,
            &selected_findings,
            &recent_failures,
            &artifact_refs,
            latest_trajectory,
        );

        if input.token_budget < 512 {
            warnings.push("small token budget forced aggressive trimming".to_string());
        }

        let mut pack = ContextPack::new(
            ProjectId::new(project_id.to_string()),
            RunId::new(run_id.to_string()),
            input.purpose.to_string(),
            summary.clone(),
        );
        pack.task_id = input.task_id.map(|id| TaskId::new(id.to_string()));
        pack.facts = fact_texts;
        pack.fact_ids = selected_facts
            .iter()
            .map(|fact| fact.id.as_str().to_string())
            .collect();
        pack.intents = intent_texts;
        pack.intent_ids = selected_intents
            .iter()
            .map(|intent| intent.id.as_str().to_string())
            .collect();
        pack.hints = hint_texts;
        pack.hint_ids = selected_hints
            .iter()
            .map(|hint| hint.id.as_str().to_string())
            .collect();
        pack.evidence_ids = dedupe_strings(
            selected_evidence_ids
                .into_iter()
                .chain(
                    latest_trajectory
                        .map(|trajectory| trajectory.evidence_ids.clone())
                        .unwrap_or_default(),
                )
                .collect(),
        );
        pack.finding_ids = dedupe_strings(
            selected_findings
                .iter()
                .map(|finding| finding.id.as_str().to_string())
                .chain(
                    latest_trajectory
                        .map(|trajectory| trajectory.finding_ids.clone())
                        .unwrap_or_default(),
                )
                .collect(),
        );
        pack.tool_invocation_ids = dedupe_strings(
            recent_failures
                .iter()
                .map(|tool| tool.id.as_str().to_string())
                .chain(
                    latest_trajectory
                        .map(|trajectory| trajectory.tool_invocation_ids.clone())
                        .unwrap_or_default(),
                )
                .collect(),
        );
        pack.operation_log_records = operation_records
            .iter()
            .map(|record| match serde_json::to_value(record) {
                Ok(Value::Object(map)) => Ok(map),
                Ok(_) => Ok(Map::new()),
                Err(source) => Err(ContextError::Serialize(source)),
            })
            .collect::<Result<Vec<_>, _>>()?;
        pack.token_budget = input.token_budget;
        pack.compression_strategy = input.compression_strategy.to_string();
        pack.metadata = {
            let mut metadata = Map::new();
            metadata.insert(
                "current_intent_id".to_string(),
                current_intent.map_or(Value::Null, |intent| {
                    Value::String(intent.id.as_str().to_string())
                }),
            );
            metadata.insert(
                "artifact_refs".to_string(),
                Value::Array(
                    artifact_refs
                        .iter()
                        .take(20)
                        .map(|reference| Value::String(reference.clone()))
                        .collect(),
                ),
            );
            metadata.insert(
                "artifact_policy".to_string(),
                Value::String("references_only_no_raw_artifact_content".to_string()),
            );
            metadata.insert(
                "trajectory_summary_id".to_string(),
                latest_trajectory.map_or(Value::Null, |trajectory| {
                    Value::String(trajectory.id.as_str().to_string())
                }),
            );
            metadata.insert(
                "trajectory_pressure".to_string(),
                latest_trajectory.map_or(Value::Null, |trajectory| {
                    Value::String(trajectory.pressure.as_str().to_string())
                }),
            );
            metadata.insert(
                "operation_log".to_string(),
                Value::Object(operation_log_metadata),
            );
            metadata
        };

        let mut report = ContextCompressionReport::new(
            ProjectId::new(project_id.to_string()),
            RunId::new(run_id.to_string()),
            summary,
        );
        report.source_counts = count_map(&[
            ("facts", count(facts.len())),
            ("intents", count(intents.len())),
            ("hints", count(hints.len())),
            ("evidence", count(evidence.len())),
            ("findings", count(findings.len())),
            ("tool_invocations", count(tools.len())),
            ("trajectory_summaries", count(trajectory_summaries.len())),
            ("operation_log_records", operation_log_total),
        ]);
        report.included_counts = count_map(&[
            ("facts", count(pack.fact_ids.len())),
            ("intents", count(pack.intent_ids.len())),
            ("hints", count(pack.hint_ids.len())),
            ("evidence", count(pack.evidence_ids.len())),
            ("findings", count(pack.finding_ids.len())),
            ("tool_invocations", count(pack.tool_invocation_ids.len())),
            (
                "trajectory_summaries",
                i64::from(latest_trajectory.is_some()),
            ),
            ("operation_log_records", count(operation_records.len())),
        ]);
        report.dropped_counts = count_map(&[
            (
                "facts",
                count(facts.len().saturating_sub(pack.fact_ids.len())),
            ),
            (
                "intents",
                count(intents.len().saturating_sub(pack.intent_ids.len())),
            ),
            (
                "hints",
                count(hints.len().saturating_sub(pack.hint_ids.len())),
            ),
            (
                "evidence",
                count(evidence.len().saturating_sub(pack.evidence_ids.len())),
            ),
            (
                "findings",
                count(findings.len().saturating_sub(pack.finding_ids.len())),
            ),
            (
                "tool_invocations",
                count(tools.len().saturating_sub(pack.tool_invocation_ids.len())),
            ),
            (
                "trajectory_summaries",
                count(trajectory_summaries.len())
                    .saturating_sub(i64::from(latest_trajectory.is_some())),
            ),
            (
                "operation_log_records",
                operation_log_total.saturating_sub(count(operation_records.len())),
            ),
        ]);
        report.strategy = input.compression_strategy.to_string();
        report.warnings = warnings;

        Ok(ContextCompressionResult {
            context_pack: pack,
            report,
        })
    }

    /// 读取 run 配置指向的 Mission 操作日志窗口
    /// （Python `_operation_log_window`）。
    ///
    /// run 缺失 / 跨 project / 配置缺失 → `journal_unavailable`；
    /// 日志目录越出 workspace 边界 → `journal_path_rejected`。
    ///
    /// # Errors
    ///
    /// 仓储读取失败（[`ContextError::Storage`]）或 journal 读取失败
    /// （[`ContextError::Journal`]）。
    fn operation_log_window(
        &self,
        project_id: &str,
        run_id: &str,
        task_id: Option<&str>,
        branch_id: Option<&str>,
        token_budget: i64,
    ) -> Result<(Vec<SwarmOperationRecord>, Map<String, Value>), ContextError> {
        let run = match self.repo.get_run(run_id)? {
            Some(run) if run.project_id.as_str() == project_id => run,
            _ => return Ok(unavailable_window()),
        };
        let workspace_raw = run
            .config
            .get("mission_workspace_path")
            .and_then(Value::as_str);
        let log_dir_raw = run.config.get("log_dir").and_then(Value::as_str);
        let (Some(workspace_raw), Some(log_dir_raw)) = (workspace_raw, log_dir_raw) else {
            return Ok(unavailable_window());
        };
        let workspace = normalize_path(Path::new(workspace_raw));
        let log_dir = normalize_path(Path::new(log_dir_raw));
        // Python `log_dir.relative_to(workspace)` 的成败判定（组件级前缀）。
        if !log_dir.starts_with(&workspace) {
            return Ok(rejected_window());
        }

        // Python `max(12_000, min(48_000, token_budget * 12))`。
        let max_chars = 12_000_i64.max((token_budget * 12).min(48_000));
        let max_chars = usize::try_from(max_chars).unwrap_or(12_000);
        let (records, metadata) = self
            .operation_journal
            .read_context_window(&log_dir, run_id, branch_id, task_id, 64, max_chars)?;
        let mut merged = metadata;
        merged.insert(
            "mission_id".to_string(),
            run.mission_id
                .as_ref()
                .map_or(Value::Null, |id| Value::String(id.as_str().to_string())),
        );
        merged.insert("run_id".to_string(), Value::String(run_id.to_string()));
        merged.insert(
            "page_api".to_string(),
            run.mission_id.as_ref().map_or(Value::Null, |id| {
                Value::String(format!("/missions/{}/operation-log", id.as_str()))
            }),
        );
        merged.insert(
            "reader_tool".to_string(),
            run.mission_id.as_ref().map_or(Value::Null, |_| {
                Value::String("mission_operation_log.read".to_string())
            }),
        );
        Ok((records, merged))
    }
}

/// Python `item.run_id in {None, run_id}`：project 级（`None`）或本 run。
fn run_scope_match(item_run: Option<&RunId>, run_id: &str) -> bool {
    item_run.is_none_or(|id| id.as_str() == run_id)
}

/// Python `_resolve_current_intent`。
fn resolve_current_intent<'a>(
    intents: &'a [Intent],
    current_intent_id: Option<&str>,
    task_id: Option<&str>,
) -> Option<&'a Intent> {
    if let Some(current_intent_id) = current_intent_id {
        return intents
            .iter()
            .find(|intent| intent.id.as_str() == current_intent_id);
    }
    if let Some(task_id) = task_id {
        return intents.iter().find(|intent| {
            intent
                .claimed_by_task_id
                .as_ref()
                .is_some_and(|claimed| claimed.as_str() == task_id)
        });
    }
    None
}

/// Python `_dedupe_by_id`（Fact）。
fn dedupe_by_id<'a>(facts: &[&'a Fact]) -> Vec<&'a Fact> {
    let mut seen: HashSet<&str> = HashSet::new();
    facts
        .iter()
        .copied()
        .filter(|fact| seen.insert(fact.id.as_str()))
        .collect()
}

/// Python `_prioritize_intents`：当前 intent > pending > priority，全序倒排。
fn prioritize_intents<'a>(
    intents: &'a [Intent],
    current_intent: Option<&Intent>,
) -> Vec<&'a Intent> {
    let mut sorted: Vec<&Intent> = intents.iter().collect();
    sorted.sort_by(|a, b| {
        let key = |intent: &Intent| {
            (
                i64::from(current_intent.is_some_and(|current| current.id == intent.id)),
                i64::from(intent.status == IntentStatus::Pending),
                intent.priority,
            )
        };
        // Python `sorted(..., reverse=True)`：整元组倒序 + 稳定排序。
        key(b).cmp(&key(a))
    });
    sorted
}

/// Python `_select_findings`：CONFIRMED > 严重级别 > 新近，取预算决定的上限。
fn select_findings(findings: &[Finding], token_budget: i64) -> Vec<&Finding> {
    let limit = if token_budget >= 1024 { 8 } else { 3 };
    let mut prioritized: Vec<&Finding> = findings.iter().collect();
    prioritized.sort_by(|a, b| {
        let key = |finding: &Finding| {
            (
                i64::from(finding.status == FindingStatus::Confirmed),
                severity_rank(finding.severity),
            )
        };
        let (key_a, key_b) = (key(a), key(b));
        key_b
            .0
            .cmp(&key_a.0)
            .then_with(|| key_b.1.cmp(&key_a.1))
            .then_with(|| {
                b.created_at
                    .partial_cmp(&a.created_at)
                    .unwrap_or(Ordering::Equal)
            })
    });
    prioritized.truncate(limit);
    prioritized
}

/// Python `_select_evidence_ids`：选中 Finding 的证据 + 大预算时的近因证据。
fn select_evidence_ids(
    evidence: &[Evidence],
    selected_findings: &[&Finding],
    token_budget: i64,
) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for finding in selected_findings {
        ids.extend(finding.evidence_ids.iter().cloned());
    }
    if token_budget >= 1024 {
        let mut recent: Vec<&Evidence> = evidence.iter().collect();
        recent.sort_by(|a, b| {
            b.created_at
                .partial_cmp(&a.created_at)
                .unwrap_or(Ordering::Equal)
        });
        ids.extend(
            recent
                .iter()
                .take(5)
                .map(|item| item.id.as_str().to_string()),
        );
    }
    dedupe_strings(ids)
}

/// Python `_recent_failed_tools`：最近 5 条失败/超时/被拒的工具调用。
fn recent_failed_tools(tools: &[ToolInvocation]) -> Vec<&ToolInvocation> {
    let mut failures: Vec<&ToolInvocation> = tools
        .iter()
        .filter(|tool| {
            matches!(
                tool.status,
                ToolStatus::Error | ToolStatus::Timeout | ToolStatus::Denied
            )
        })
        .collect();
    failures.sort_by(|a, b| {
        b.started_at
            .partial_cmp(&a.started_at)
            .unwrap_or(Ordering::Equal)
    });
    failures.truncate(5);
    failures
}

/// Python `_artifact_refs`：Evidence 位置与工具工件路径的去重引用列表。
fn artifact_refs(evidence: &[Evidence], tools: &[ToolInvocation]) -> Vec<String> {
    let mut refs: Vec<String> = Vec::new();
    for item in evidence {
        for location in &item.locations {
            // Python `if location.artifact`：空字符串与 None 同为假值。
            if !location.artifact.is_empty() {
                refs.push(location.artifact.clone());
            }
        }
    }
    for tool in tools {
        refs.extend(tool.artifact_paths.iter().cloned());
    }
    dedupe_strings(refs)
}

/// Python `_build_summary`。
fn build_summary(
    purpose: &str,
    selected_findings: &[&Finding],
    recent_failures: &[&ToolInvocation],
    artifact_refs: &[String],
    trajectory: Option<&TrajectorySummary>,
) -> String {
    let mut parts = vec![format!("Context purpose: {purpose}.")];
    if let Some(trajectory) = trajectory {
        parts.push(format!(
            "Latest branch trajectory: {}",
            truncate(&trajectory.summary, 1000)
        ));
    }
    if !selected_findings.is_empty() {
        let finding_bits: Vec<String> = selected_findings
            .iter()
            .take(5)
            .map(|finding| {
                format!(
                    "{}/{}: {}",
                    finding.severity.as_str(),
                    finding.status.as_str(),
                    finding.title
                )
            })
            .collect();
        parts.push(format!(
            "Prioritized findings: {}.",
            finding_bits.join("; ")
        ));
    }
    if !recent_failures.is_empty() {
        let failure_bits: Vec<String> = recent_failures
            .iter()
            .take(3)
            .map(|tool| {
                // Python `tool.error or tool.output_summary`：error 为空值时
                // 回退 output_summary。
                let detail = match tool.error.as_deref() {
                    Some(error) if !error.is_empty() => error,
                    _ => tool.output_summary.as_str(),
                };
                format!("{} {}: {}", tool.tool_name, tool.status.as_str(), detail)
            })
            .collect();
        parts.push(format!(
            "Recent tool failures: {}.",
            failure_bits.join("; ")
        ));
    }
    if !artifact_refs.is_empty() {
        let joined = artifact_refs
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!("Artifacts referenced by path only: {joined}."));
    }
    parts.join(" ")
}

/// Python `_latest_trajectory`：分支过滤后取 `(segment_index, created_at)`
/// 最大者（并列取首个——Python `max` 语义）。
fn latest_trajectory<'a>(
    summaries: &'a [TrajectorySummary],
    branch_id: Option<&str>,
) -> Option<&'a TrajectorySummary> {
    let candidates: Vec<&TrajectorySummary> = match branch_id {
        Some(branch_id) => summaries
            .iter()
            .filter(|summary| {
                summary
                    .branch_id
                    .as_ref()
                    .is_some_and(|branch| branch.as_str() == branch_id)
            })
            .collect(),
        None => summaries.iter().collect(),
    };
    let mut best: Option<&TrajectorySummary> = None;
    for summary in candidates {
        let strictly_greater = best.is_some_and(|current| {
            summary.segment_index > current.segment_index
                || (summary.segment_index == current.segment_index
                    && summary.created_at > current.created_at)
        });
        if best.is_none() || strictly_greater {
            best = Some(summary);
        }
    }
    best
}

/// Python `_dedupe_strings`（`dict.fromkeys`）：保序去重。
fn dedupe_strings(values: Vec<String>) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

/// Python `_fact_text`。
fn fact_text(fact: &Fact) -> String {
    format!(
        "{}:{}: {}",
        fact.node_type.as_str(),
        fact.kind,
        fact.statement
    )
}

/// Python `_intent_text`。
fn intent_text(intent: &Intent) -> String {
    let description = intent
        .description
        .as_ref()
        .filter(|text| !text.is_empty())
        .map(|text| format!(" - {text}"))
        .unwrap_or_default();
    format!(
        "{}:{}: {}{}",
        intent.status.as_str(),
        intent.priority,
        intent.title,
        description
    )
}

/// Python `_hint_text`。
fn hint_text(hint: &Hint) -> String {
    let category = hint
        .category
        .as_ref()
        .filter(|text| !text.is_empty())
        .map(|text| format!("{text}: "))
        .unwrap_or_default();
    format!("{category}{}", hint.text)
}

/// Python `_estimate_tokens`：字符数启发式（`len(text) // 4`，至少 1）。
fn estimate_tokens(text: &str) -> i64 {
    let quarter = i64::try_from(text.chars().count() / 4).unwrap_or(i64::MAX);
    quarter.max(1)
}

/// Python `_include_if_budget`：预算内纳入条目并扣减，预算外跳过。
fn include_if_budget<'a, T>(
    selected: &mut Vec<&'a T>,
    item: &'a T,
    budget_left: i64,
    text: &str,
) -> i64 {
    let cost = estimate_tokens(text);
    if budget_left - cost < 0 {
        return budget_left;
    }
    selected.push(item);
    budget_left - cost
}

/// Python `_truncate`：按字符数截断并追加省略号。
fn truncate(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let prefix: String = value.chars().take(limit.saturating_sub(3)).collect();
    format!("{prefix}...")
}

/// `usize` 长度转计数（溢出饱和到 `i64::MAX`，实践中不可达）。
fn count(len: usize) -> i64 {
    i64::try_from(len).unwrap_or(i64::MAX)
}

/// 按给定序构建计数字段（Python dict 字面量键序 = 插入序）。
fn count_map(pairs: &[(&str, i64)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), Value::from(*value)))
        .collect()
}

/// Python `([], {"policy": "journal_unavailable", "matching_total": 0})`。
fn unavailable_window() -> (Vec<SwarmOperationRecord>, Map<String, Value>) {
    let mut metadata = Map::new();
    metadata.insert(
        "policy".to_string(),
        Value::String("journal_unavailable".to_string()),
    );
    metadata.insert("matching_total".to_string(), Value::from(0));
    (Vec::new(), metadata)
}

/// Python `([], {"policy": "journal_path_rejected", "matching_total": 0})`。
fn rejected_window() -> (Vec<SwarmOperationRecord>, Map<String, Value>) {
    let mut metadata = Map::new();
    metadata.insert(
        "policy".to_string(),
        Value::String("journal_path_rejected".to_string()),
    );
    metadata.insert("matching_total".to_string(), Value::from(0));
    (Vec::new(), metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::EvidenceKind;
    use models::domain::AuditDomain;
    use models::evidence::CodeLocation;
    use models::ids::MissionId;
    use models::lifecycle::FindingStatus;
    use models::lifecycle::Severity;
    use models::project::Project;
    use models::run::AuditRun;
    use serde_json::json;
    use storage::SqliteRepository;

    fn temp_repo() -> (tempfile::TempDir, Arc<SqliteRepository>) {
        let dir = tempfile::tempdir().expect("临时目录必须可创建");
        let repo = SqliteRepository::open(dir.path().join("ctx.sqlite3")).expect("库必须可打开");
        (dir, Arc::new(repo))
    }

    fn compressor(repo: Arc<SqliteRepository>) -> ContextCompressor {
        ContextCompressor::new(repo)
    }

    // Python `_seed_project`。
    fn seed_project(repo: &SqliteRepository) -> (Project, AuditRun) {
        let mut project = Project::new("p".to_string(), AuditDomain::WebSast);
        project = repo.create_project(&project).expect("project 必须可创建");
        let mut run = AuditRun::new(project.id.clone());
        run = repo.create_run(&run).expect("run 必须可创建");
        for (node_type, kind, statement) in [
            (GraphNodeType::OriginFact, "origin", "origin target"),
            (GraphNodeType::GoalFact, "goal", "goal evidence"),
        ] {
            let mut fact = Fact::new(project.id.clone(), kind.to_string(), statement.to_string());
            fact.node_type = node_type;
            repo.add_fact(&fact).expect("fact 必须可追加");
        }
        (project, run)
    }

    // test_context_compressor_keeps_must_facts_and_trims_low_priority
    #[test]
    fn keeps_must_facts_and_trims_low_priority() {
        let (_dir, repo) = temp_repo();
        let (project, run) = seed_project(&repo);
        let mut huge = Fact::new(project.id.clone(), "low".to_string(), "x".repeat(2000));
        huge.confidence = 0.1;
        let huge = repo.add_fact(&huge).expect("fact 必须可追加");

        let mut hint = Hint::new(project.id.clone(), "focus auth".to_string());
        hint.weight = 90;
        repo.add_hint(&hint).expect("hint 必须可追加");

        let mut tool = ToolInvocation::new("semgrep".to_string(), "scan".to_string());
        tool.project_id = Some(project.id.clone());
        tool.run_id = Some(run.id.clone());
        tool.output_summary = "failed".to_string();
        tool.status = ToolStatus::Error;
        tool.error = Some("rule config failed".to_string());
        tool.artifact_paths = vec!["/tmp/semgrep.json".to_string()];
        repo.add_tool_invocation(&tool)
            .expect("tool invocation 必须可追加");

        let mut location = CodeLocation::new("/repo/app.py".to_string());
        location.start_line = Some(1);
        let mut evidence = Evidence::new(
            project.id.clone(),
            EvidenceKind::ToolOutput,
            "artifact summary only".to_string(),
        );
        evidence.run_id = Some(run.id.clone());
        evidence.content.insert(
            "raw".to_string(),
            json!("SECRET_ARTIFACT_CONTENT".repeat(100)),
        );
        evidence.locations = vec![location];
        let evidence = repo.add_evidence(&evidence).expect("evidence 必须可追加");

        let mut finding = Finding::new(project.id.clone(), "high finding".to_string());
        finding.run_id = Some(run.id.clone());
        finding.severity = Severity::High;
        finding.status = FindingStatus::Candidate;
        finding.evidence_ids = vec![evidence.id.as_str().to_string()];
        let finding = finding.validated().expect("candidate finding 必须可校验");
        repo.add_finding(&finding).expect("finding 必须可追加");

        let mut input = BuildContextInput::new(project.id.as_str(), run.id.as_str());
        input.purpose = "test";
        input.token_budget = 128;
        let result = compressor(repo)
            .build_context_pack(&input)
            .expect("压缩必须成功");
        let pack = result.context_pack;

        assert!(
            pack.facts.iter().any(|item| item.contains("origin target")),
            "origin fact 必须保留"
        );
        assert!(
            pack.facts.iter().any(|item| item.contains("goal evidence")),
            "goal fact 必须保留"
        );
        assert!(
            !pack.fact_ids.contains(&huge.id.as_str().to_string()),
            "低置信度巨块 fact 必须被预算裁剪"
        );
        assert!(pack.summary.contains("Recent tool failures"));
        assert!(pack.summary.contains("semgrep"));
        let dumped = serde_json::to_string(&pack).expect("序列化不会失败");
        assert!(
            !dumped.contains("SECRET_ARTIFACT_CONTENT"),
            "原始工件内容绝不可进入上下文包"
        );
        assert!(dumped.contains("/repo/app.py"), "工件路径以引用形式保留");
    }

    #[test]
    fn operation_log_window_reports_unavailable_for_missing_run() {
        let (_dir, repo) = temp_repo();
        let mut project = Project::new("p".to_string(), AuditDomain::WebSast);
        project = repo.create_project(&project).expect("project 必须可创建");
        let input = BuildContextInput::new(project.id.as_str(), "run_absent");
        let result = compressor(repo)
            .build_context_pack(&input)
            .expect("缺失 run 必须优雅降级而非报错");
        let metadata = result.context_pack.metadata;
        let operation_log = metadata
            .get("operation_log")
            .and_then(Value::as_object)
            .expect("operation_log 元数据必须存在");
        assert_eq!(
            operation_log.get("policy"),
            Some(&json!("journal_unavailable")),
            "缺失 run 的策略标记"
        );
        assert_eq!(
            operation_log.get("matching_total"),
            Some(&json!(0)),
            "缺失 run 的匹配计数"
        );
        assert!(
            result.context_pack.operation_log_records.is_empty(),
            "无日志可读"
        );
    }

    #[test]
    fn operation_log_window_reads_run_scoped_records() {
        let (dir, repo) = temp_repo();
        let mut project = Project::new("p".to_string(), AuditDomain::WebSast);
        project = repo.create_project(&project).expect("project 必须可创建");
        let mission_id = MissionId::new("mission_1".to_string());
        let workspace = dir.path().join("workspace").join("m1");
        let log_dir = workspace.join("logs");
        let mut run = AuditRun::new(project.id.clone());
        run.mission_id = Some(mission_id.clone());
        run.config.insert(
            "mission_workspace_path".to_string(),
            json!(workspace.to_string_lossy()),
        );
        run.config
            .insert("log_dir".to_string(), json!(log_dir.to_string_lossy()));
        let run = repo.create_run(&run).expect("run 必须可创建");

        let journal = SwarmOperationJournal::default();
        let record = SwarmOperationRecord::new(
            project.id.clone(),
            run.id.clone(),
            "solver".to_string(),
            "tool.call".to_string(),
            "ran semgrep".to_string(),
        );
        journal.append(&log_dir, record).expect("日志必须可追加");

        let compressor = ContextCompressor::with_operation_journal(repo.clone(), Arc::new(journal));
        let input = BuildContextInput::new(project.id.as_str(), run.id.as_str());
        let result = compressor.build_context_pack(&input).expect("压缩必须成功");
        let pack = result.context_pack;
        assert_eq!(pack.operation_log_records.len(), 1, "日志窗口必须取到记录");
        assert_eq!(
            pack.operation_log_records[0].get("entry"),
            Some(&json!("ran semgrep")),
            "记录内容无损进入上下文包"
        );
        let operation_log = pack
            .metadata
            .get("operation_log")
            .and_then(Value::as_object)
            .expect("operation_log 元数据必须存在");
        assert_eq!(
            operation_log.get("mission_id"),
            Some(&json!(mission_id.as_str())),
            "元数据携带 mission id"
        );
        assert_eq!(
            operation_log.get("page_api"),
            Some(&json!("/missions/mission_1/operation-log")),
            "元数据携带分页 API"
        );
    }

    #[test]
    fn operation_log_window_rejects_dir_outside_workspace() {
        let (dir, repo) = temp_repo();
        let mut project = Project::new("p".to_string(), AuditDomain::WebSast);
        project = repo.create_project(&project).expect("project 必须可创建");
        let workspace = dir.path().join("workspace").join("m1");
        let outside = dir.path().join("elsewhere").join("logs");
        let mut run = AuditRun::new(project.id.clone());
        run.config.insert(
            "mission_workspace_path".to_string(),
            json!(workspace.to_string_lossy()),
        );
        run.config
            .insert("log_dir".to_string(), json!(outside.to_string_lossy()));
        let run = repo.create_run(&run).expect("run 必须可创建");

        let input = BuildContextInput::new(project.id.as_str(), run.id.as_str());
        let result = compressor(repo)
            .build_context_pack(&input)
            .expect("越界路径必须优雅降级而非报错");
        let operation_log = result
            .context_pack
            .metadata
            .get("operation_log")
            .and_then(Value::as_object)
            .expect("operation_log 元数据必须存在");
        assert_eq!(
            operation_log.get("policy"),
            Some(&json!("journal_path_rejected")),
            "越界日志目录的策略标记"
        );
    }
}
