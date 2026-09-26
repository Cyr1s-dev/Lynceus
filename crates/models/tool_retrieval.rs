//! 工具检索的类型层：Compact 描述符 / 候选 / 查询 / 初始选择。
//!
//! 三层工具可见性在类型上显式区分（Full Catalog → Candidates →
//! Visible Tools）：[`CompactToolDescriptor`] 是目录层的紧凑元数据，
//! [`ToolCandidate`] 是检索层候选（含打分与 rationale），而真正进入
//! provider 请求的完整 schema 仍由 Harness 按选择结果加载——候选永远
//! 不等于已授权的可见工具。

use serde::Deserialize;
use serde::Serialize;

/// Solver config 注入键：初始 visible toolset（字符串数组）。缺省 =
/// Harness 保持静态行为（向后兼容）；非空 = 收紧可见工具。
pub const CONFIG_VISIBLE_TOOL_IDS: &str = "visible_tool_ids";
/// Solver config 注入键：知识提示（字符串数组，KnowledgeRetriever 命中
/// 的紧凑摘要；与工具检索平行汇合进 Solver 上下文）。
pub const CONFIG_KNOWLEDGE_HINTS: &str = "knowledge_hints";

/// 工具可用性 wire 值（catalog 检测 + native/remote 扩展）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCandidateAvailability {
    /// local-tools.json 显式配置。
    Configured,
    /// PATH 解析。
    Path,
    /// 本地未检测到可执行文件。
    Missing,
    /// Rust 原生工具（注册 adapter 即可用）。
    Native,
    /// 远程来源（如 IDA MCP；运行时按 harness 绑定）。
    Remote,
}

impl ToolCandidateAvailability {
    /// wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::Path => "path",
            Self::Missing => "missing",
            Self::Native => "native",
            Self::Remote => "remote",
        }
    }

    /// 是否可在本地执行（missing 之外的取值）。
    #[must_use]
    pub const fn locally_executable(self) -> bool {
        !matches!(self, Self::Missing)
    }
}

/// 工具紧凑描述符（Catalog 条目的检索视图，刻意很小）。
///
/// 完整参数 schema / JSON schema / 示例只在工具被选入 visible toolset
/// 后由 Harness 加载，绝不进入检索结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactToolDescriptor {
    /// 工具 ID（catalog id / native id / 远程合成 id）。
    pub tool_id: String,
    /// 展示名。
    pub title: String,
    /// 一句话摘要。
    pub summary: String,
    /// 能力关键词（词法检索索引）。
    pub capabilities: Vec<String>,
    /// 领域提示（weak signal）。
    pub domain: String,
    /// 可用性。
    pub availability: ToolCandidateAvailability,
    /// 风险等级（low/medium/high）。
    pub risk: String,
    /// adapter 状态（implemented / remote）。
    pub adapter_status: String,
}

/// 一个检索候选：描述符 + 相关度 + 确定性 rationale。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCandidate {
    /// 紧凑描述符。
    pub descriptor: CompactToolDescriptor,
    /// 相关度（0.0 起的非负累计分；排序键）。
    pub relevance: f64,
    /// 确定性归因（如 `matched capability: binary debugging`、
    /// `explicitly requested by user`）——绝不用 LLM 生成解释。
    pub rationale: Vec<String>,
}

impl ToolCandidate {
    /// 工具 ID。
    #[must_use]
    pub fn tool_id(&self) -> &str {
        &self.descriptor.tool_id
    }

    /// 是否本地可执行。
    #[must_use]
    pub fn locally_executable(&self) -> bool {
        self.descriptor.availability.locally_executable()
    }
}

/// 工具检索查询（bootstrap 确定性信号或 Solver 的 `search_tools` 动态
/// 查询的检索侧投影）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRetrievalQuery {
    /// 自由文本查询（与 `capability_queries` 合并参与打分）。
    #[serde(default)]
    pub text: String,
    /// 能力查询。
    #[serde(default)]
    pub capability_queries: Vec<String>,
    /// 工件 kinds（elf/pcap/url/source…，兼容性加成）。
    #[serde(default)]
    pub artifact_kinds: Vec<String>,
    /// 显式工具名（强加成，但仍需通过 Catalog/policy）。
    #[serde(default)]
    pub explicit_tools: Vec<String>,
    /// 风险上限（none/low/medium/high；超出者过滤）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_limit: Option<String>,
    /// 返回条数上限。
    pub limit: usize,
}

impl ToolRetrievalQuery {
    /// 仅文本的动态查询（`search_tools` 场景）。
    #[must_use]
    pub fn from_text(text: impl Into<String>, limit: usize) -> Self {
        Self {
            text: text.into(),
            capability_queries: Vec::new(),
            artifact_kinds: Vec::new(),
            explicit_tools: Vec::new(),
            risk_limit: None,
            limit,
        }
    }

    /// 检索语义文本：text + capability queries + artifact kinds。
    #[must_use]
    pub fn retrieval_text(&self) -> String {
        let mut parts: Vec<String> = vec![self.text.clone()];
        parts.extend(self.capability_queries.iter().cloned());
        parts.extend(self.artifact_kinds.iter().cloned());
        parts.join(" ")
    }
}

/// 检索结果（候选按 relevance 降序）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolRetrievalResult {
    /// 候选（已排序、已限流）。
    pub candidates: Vec<ToolCandidate>,
}

/// 初始工具集选择方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolsetSelectionMethod {
    /// 确定性快速路径（显式工具/分差显著/无候选）。
    Deterministic,
    /// 模型辅助路径（候选接近时由 fast model 在候选集内挑选）。
    ModelAssisted,
}

impl ToolsetSelectionMethod {
    /// wire 值。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Deterministic => "deterministic",
            Self::ModelAssisted => "model_assisted",
        }
    }
}

/// 初始 visible toolset 选择结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolsetSelection {
    /// 选中的工具 ID（3~8，配置化上下限）。
    pub selected_tool_ids: Vec<String>,
    /// 选择方式。
    pub method: ToolsetSelectionMethod,
    /// 参与选择的候选数。
    pub candidates_considered: usize,
    /// 确定性归因。
    pub rationale: Vec<String>,
}

impl ToolsetSelection {
    /// 空选择（会话任务 / 无候选）。
    #[must_use]
    pub fn empty(reason: &str) -> Self {
        Self {
            selected_tool_ids: Vec::new(),
            method: ToolsetSelectionMethod::Deterministic,
            candidates_considered: 0,
            rationale: vec![reason.to_string()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(tool_id: &str, availability: ToolCandidateAvailability) -> CompactToolDescriptor {
        CompactToolDescriptor {
            tool_id: tool_id.to_string(),
            title: tool_id.to_string(),
            summary: format!("{tool_id} summary"),
            capabilities: vec![format!("{tool_id} capability")],
            domain: "web_recon".to_string(),
            availability,
            risk: "low".to_string(),
            adapter_status: "implemented".to_string(),
        }
    }

    #[test]
    fn availability_wire_values() {
        assert_eq!(ToolCandidateAvailability::Configured.as_str(), "configured");
        assert_eq!(ToolCandidateAvailability::Remote.as_str(), "remote");
        assert!(ToolCandidateAvailability::Native.locally_executable());
        assert!(!ToolCandidateAvailability::Missing.locally_executable());
    }

    #[test]
    fn query_retrieval_text_merges_parts() {
        let query = ToolRetrievalQuery {
            text: "analyze".to_string(),
            capability_queries: vec!["binary analysis".to_string()],
            artifact_kinds: vec!["elf".to_string()],
            explicit_tools: Vec::new(),
            risk_limit: None,
            limit: 10,
        };
        assert_eq!(query.retrieval_text(), "analyze binary analysis elf");
    }

    #[test]
    fn candidate_exposes_tool_id_and_executability() {
        let candidate = ToolCandidate {
            descriptor: descriptor("nuclei", ToolCandidateAvailability::Path),
            relevance: 2.0,
            rationale: vec!["matched capability: scanning".to_string()],
        };
        assert_eq!(candidate.tool_id(), "nuclei");
        assert!(candidate.locally_executable());
    }

    #[test]
    fn empty_selection_carries_reason() {
        let selection = ToolsetSelection::empty("conversation task requires no tools");
        assert!(selection.selected_tool_ids.is_empty());
        assert_eq!(selection.method, ToolsetSelectionMethod::Deterministic);
        assert_eq!(
            selection.rationale,
            ["conversation task requires no tools".to_string()]
        );
    }
}
