//! Intelligence Hub：外部 / 被动 / OSINT 数据源的查询、归一化与
//! 攻击面关系展开。
//!
//! 架构红线：
//!
//! - **Intelligence ≠ Evidence**：这里产生的一切都只有 provenance
//!   （`IntelRawRecord`），永远不能成为 Finding 的证据；Finding 的
//!   证据链仍然是 `Finding → Evidence → ToolInvocation`。
//! - **Seed ≠ Asset**：资产发现是关系展开（seed → 多源线索 → 实体
//!   归一去重 → 关系建图 → 新 seed），不是点枚举。
//! - **Source 抽象**：业务代码只见 [`IntelligenceSource`] trait，绝不
//!   出现 `if fofa / if shodan` 式分支。
//! - **故障隔离**：任何 source 失败只记录在其
//!   [`models::IntelSourceResult::errors`]，绝不中断其他 source。
//! - **确定性 confidence**：置信度由 pipeline 的确定性规则推导，
//!   不由 LLM 生成。

pub mod normalize;
pub mod pipeline;
pub mod source;
pub mod sources;

pub use pipeline::{IntelExpansionReport, IntelligencePipeline, PromoteOutcome};
pub use source::{IntelError, IntelSourceInfo, IntelligenceSource, NormalizedRecord};
pub use sources::{CrtShSource, WaybackSource, default_sources};
