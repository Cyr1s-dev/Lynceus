//! Agent-facing 工具检索契约（与 [`crate::retrieval::KnowledgeRetriever`]
//! 平行的第二个 Retriever）。
//!
//! 两个 Retriever 职责正交：
//! - `KnowledgeRetriever` 回答「我应该知道/尝试什么？」；
//! - `ToolRetriever` 回答「我能用什么？」。
//!
//! 两者都只做 discovery，绝不执行工具——真正执行仍属于
//! `AgentToolHarness`（allow-list / schema 校验 / 预算 / 风险策略）。
//! 检索基于已装配的 catalog 快照做纯计算，因此契约是同步且不可失败的
//! （catalog 缺失或空目录在装配期失败，检索层收到的是空索引并返回空
//! 候选——空知识库/空目录必须优雅降级，不得阻塞 Mission）。

use models::{ToolRetrievalQuery, ToolRetrievalResult};

/// 从完整 Tool Catalog 中检索与需求相关的候选工具。
///
/// 实现方负责：搜索目录、排序候选、过滤明显不可用工具、返回 compact
/// metadata。不负责 installation / tool call / shell / MCP invocation。
pub trait ToolRetriever: Send + Sync {
    /// 按查询检索候选（结果按 relevance 降序、已限流）。
    fn retrieve(&self, query: &ToolRetrievalQuery) -> ToolRetrievalResult;

    /// 检索索引覆盖的工具名（id + 展示名小写；供 bootstrap 的显式工具
    /// 识别使用，绝不包含参数 schema）。
    fn known_tool_names(&self) -> Vec<String>;
}
