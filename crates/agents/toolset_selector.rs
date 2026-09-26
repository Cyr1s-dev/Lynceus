//! `ToolsetSelector` —— 初始 visible toolset 的唯一选择入口。
//!
//! 输入是 `ToolRetriever` 返回的 compact 候选（不是完整 catalog），输出是
//! 3~8 个工具 ID。两条路径：
//! - 确定性 fast path：显式工具命中 / 候选数不超上限 / 分差显著——直接
//!   截取，零模型调用（Retrieval Fast Path 契约）；
//! - 模型辅助路径：候选多且分差接近时，由 fast model 在候选集内挑选
//!   （purpose `toolset_select`），输出必须回指候选 ID，越界一律忽略。
//!
//! 小模型不是安全边界：选择结果只决定「哪些 compact 候选值得加载完整
//! schema」，执行授权仍由 Harness（allow-list / schema 校验 / 风险策略）
//! 决定。

use std::sync::Arc;

use models::tool_retrieval::ToolCandidate;
use models::tool_retrieval::ToolsetSelection;
use models::tool_retrieval::ToolsetSelectionMethod;
use serde_json::Value;

use crate::llm::LlmMessage;
use crate::llm::ProviderRuntime;
use crate::llm::StructuredGenerationRequest;

/// 选择器配置（上限与阈值集中在此）。
#[derive(Debug, Clone)]
pub struct ToolsetSelectorConfig {
    /// visible toolset 上限（GOAL：3~8，配置化）。
    pub max_tools: usize,
    /// 确定性 fast path 的分差阈值：第 1 名与第 `max_tools+1` 名的分差
    /// 不小于该值时视为「非常明确」，跳过模型选择。
    pub decisive_margin: f64,
    /// 模型辅助选择的 purpose 标签。
    pub select_purpose: &'static str,
}

impl Default for ToolsetSelectorConfig {
    fn default() -> Self {
        Self {
            max_tools: 6,
            decisive_margin: 1.0,
            select_purpose: "toolset_select",
        }
    }
}

/// 初始工具集选择器（候选 → 少量 visible tools）。
#[derive(Debug, Clone)]
pub struct ToolsetSelector {
    config: ToolsetSelectorConfig,
}

impl ToolsetSelector {
    /// 默认配置的选择器。
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: ToolsetSelectorConfig::default(),
        }
    }

    /// 指定配置的选择器。
    #[must_use]
    pub fn with_config(config: ToolsetSelectorConfig) -> Self {
        Self { config }
    }

    /// 选择初始 visible toolset。
    ///
    /// 候选已按 relevance 降序（ToolRetriever 契约）；本方法只做选择，
    /// 不重新打分。`explicit_tools` 是查询文本里点名的工具（确定性
    /// 提取），命中候选时优先。
    pub async fn select(
        &self,
        explicit_tools: &[String],
        candidates: &[ToolCandidate],
        provider: Option<&Arc<dyn ProviderRuntime>>,
        provider_id: Option<&str>,
    ) -> ToolsetSelection {
        if candidates.is_empty() {
            return ToolsetSelection::empty("no tool candidates matched the task");
        }

        // Retrieval Fast Path：候选本身不多于上限 → 无需挑选。
        if candidates.len() <= self.config.max_tools {
            return Self::deterministic(candidates, candidates.len());
        }

        // 显式工具优先命中（用户点名 + 候选在列 → 分差意义已明确）。
        let explicit_hits = candidates
            .iter()
            .filter(|candidate| {
                explicit_tools
                    .iter()
                    .any(|tool| candidate.tool_id().eq_ignore_ascii_case(tool))
            })
            .count();
        if explicit_hits > 0 {
            let ids = candidates
                .iter()
                .filter(|candidate| {
                    explicit_tools
                        .iter()
                        .any(|tool| candidate.tool_id().eq_ignore_ascii_case(tool))
                })
                .take(self.config.max_tools)
                .map(|candidate| candidate.tool_id().to_string())
                .collect::<Vec<_>>();
            let mut rationale = vec!["explicitly requested tool matched a candidate".to_string()];
            rationale.push(format!(
                "kept top remaining deterministic picks up to {}",
                self.config.max_tools
            ));
            // 显式命中后用确定性序补齐剩余名额。
            let mut selected = ids;
            for candidate in candidates {
                if selected.len() >= self.config.max_tools {
                    break;
                }
                if !selected.iter().any(|id| id == candidate.tool_id()) {
                    selected.push(candidate.tool_id().to_string());
                }
            }
            return ToolsetSelection {
                selected_tool_ids: selected,
                method: ToolsetSelectionMethod::Deterministic,
                candidates_considered: candidates.len(),
                rationale,
            };
        }

        // 分差显著 → 确定性 fast path。
        let top = candidates[0].relevance;
        let boundary = candidates
            .get(self.config.max_tools)
            .map_or(0.0, |candidate| candidate.relevance);
        if top - boundary >= self.config.decisive_margin {
            return Self::deterministic(candidates, self.config.max_tools);
        }

        // 模型辅助路径（候选多且接近）。provider 缺失时保守回退确定性。
        if let Some(runtime) = provider
            && let Some(selection) = self
                .model_select(runtime, provider_id.unwrap_or_default(), candidates)
                .await
        {
            return selection;
        }
        Self::deterministic(candidates, self.config.max_tools)
    }

    /// 确定性截取（保序，含 rationale）。
    fn deterministic(candidates: &[ToolCandidate], take: usize) -> ToolsetSelection {
        let selected_tool_ids = candidates
            .iter()
            .take(take)
            .map(|candidate| candidate.tool_id().to_string())
            .collect::<Vec<_>>();
        let rationale = candidates
            .iter()
            .take(take)
            .flat_map(|candidate| candidate.rationale.iter().cloned())
            .take(12)
            .collect::<Vec<_>>();
        ToolsetSelection {
            selected_tool_ids,
            method: ToolsetSelectionMethod::Deterministic,
            candidates_considered: candidates.len(),
            rationale,
        }
    }

    /// 模型辅助挑选：模型只能从给定候选 ID 集合中选择，越界忽略。
    async fn model_select(
        &self,
        runtime: &Arc<dyn ProviderRuntime>,
        provider_id: &str,
        candidates: &[ToolCandidate],
    ) -> Option<ToolsetSelection> {
        let menu = candidates
            .iter()
            .map(|candidate| {
                serde_json::json!({
                    "tool_id": candidate.descriptor.tool_id,
                    "title": candidate.descriptor.title,
                    "summary": candidate.descriptor.summary,
                    "capabilities": candidate.descriptor.capabilities,
                    "relevance": candidate.relevance,
                })
            })
            .collect::<Vec<_>>();
        let user = serde_json::json!({
            "candidates": menu,
            "max_tools": self.config.max_tools,
        });
        let messages = vec![
            LlmMessage::new("system", TOOLSET_SELECT_PROTOCOL.to_string()),
            LlmMessage::new("user", user.to_string()),
        ];
        let result = runtime
            .generate_structured(StructuredGenerationRequest {
                provider_id,
                messages: &messages,
                purpose: self.config.select_purpose,
                project_id: None,
                run_id: None,
                task_id: None,
            })
            .await
            .ok()?;
        let selected = result
            .get("tool_ids")
            .and_then(Value::as_array)?
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .filter(|id| {
                candidates
                    .iter()
                    .any(|candidate| candidate.tool_id() == *id)
            })
            .take(self.config.max_tools)
            .map(str::to_string)
            .collect::<Vec<String>>();
        if selected.is_empty() {
            return None;
        }
        let deduped = dedupe(selected);
        Some(ToolsetSelection {
            selected_tool_ids: deduped,
            method: ToolsetSelectionMethod::ModelAssisted,
            candidates_considered: candidates.len(),
            rationale: vec!["fast model picked from compact candidates".to_string()],
        })
    }
}

impl Default for ToolsetSelector {
    fn default() -> Self {
        Self::new()
    }
}

fn dedupe(ids: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in ids {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// 模型辅助选择系统提示词（只含 ID 契约，不含任何工具参数 schema）。
pub const TOOLSET_SELECT_PROTOCOL: &str = "You are the ToolsetSelector for Lynceus.

You receive a compact list of tool candidates (id, title, summary,
capabilities, relevance). Pick the small subset most useful for the task.

Return JSON only: {\"tool_ids\": [\"candidate id\", ...]}

Rules:
- Only use tool_id values from the candidates list; anything else is ignored.
- Select at most the given max_tools; fewer is fine when the task is narrow.
- Prefer higher relevance unless a summary/capability clearly fits better.";

#[cfg(test)]
mod tests {
    #![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

    use super::*;
    use models::tool_retrieval::CompactToolDescriptor;
    use models::tool_retrieval::ToolCandidateAvailability;

    fn candidate(tool_id: &str, relevance: f64, summary: &str) -> ToolCandidate {
        ToolCandidate {
            descriptor: CompactToolDescriptor {
                tool_id: tool_id.to_string(),
                title: tool_id.to_string(),
                summary: summary.to_string(),
                capabilities: vec!["scanning".to_string()],
                domain: "web_dast".to_string(),
                availability: ToolCandidateAvailability::Path,
                risk: "medium".to_string(),
                adapter_status: "implemented".to_string(),
            },
            relevance,
            rationale: vec!["matched capability: scanning".to_string()],
        }
    }

    #[tokio::test]
    async fn no_candidates_select_no_tools() {
        let selection = ToolsetSelector::new().select(&[], &[], None, None).await;
        assert!(selection.selected_tool_ids.is_empty());
        assert_eq!(
            selection.rationale.first().map(String::as_str),
            Some("no tool candidates matched the task")
        );
    }

    #[tokio::test]
    async fn few_candidates_take_deterministic_fast_path() {
        let candidates = vec![
            candidate("nuclei", 3.0, "scan"),
            candidate("ffuf", 2.0, "disc"),
        ];
        let selection = ToolsetSelector::new()
            .select(&[], &candidates, None, None)
            .await;
        assert_eq!(selection.selected_tool_ids, ["nuclei", "ffuf"]);
        assert_eq!(selection.method, ToolsetSelectionMethod::Deterministic);
    }

    #[tokio::test]
    async fn explicit_tool_wins_without_model_call() {
        let explicit = vec!["ffuf".to_string()];
        let candidates: Vec<ToolCandidate> = (1..=10)
            .map(|index| candidate(&format!("tool{index}"), f64::from(index), "generic"))
            .chain(std::iter::once(candidate("ffuf", 0.1, "content discovery")))
            .collect();
        let selection = ToolsetSelector::new()
            .select(&explicit, &candidates, None, None)
            .await;
        assert_eq!(
            selection.selected_tool_ids.first().map(String::as_str),
            Some("ffuf")
        );
        assert_eq!(selection.method, ToolsetSelectionMethod::Deterministic);
    }

    #[tokio::test]
    async fn decisive_margin_skips_model_and_keeps_order() {
        let candidates: Vec<ToolCandidate> = (1..=10)
            .map(|index| candidate(&format!("tool{index}"), 10.0 - f64::from(index), "generic"))
            .collect();
        let selection = ToolsetSelector::new()
            .select(&[], &candidates, None, None)
            .await;
        assert_eq!(selection.selected_tool_ids.len(), 6);
        assert_eq!(selection.selected_tool_ids[0], "tool1");
        assert_eq!(selection.method, ToolsetSelectionMethod::Deterministic);
    }

    #[tokio::test]
    async fn no_provider_falls_back_to_deterministic_on_close_scores() {
        let candidates: Vec<ToolCandidate> = (1..=10)
            .map(|index| candidate(&format!("tool{index}"), 1.0, "generic"))
            .collect();
        let selection = ToolsetSelector::new()
            .select(&[], &candidates, None, None)
            .await;
        assert_eq!(selection.selected_tool_ids.len(), 6);
        assert_eq!(selection.method, ToolsetSelectionMethod::Deterministic);
    }
}
