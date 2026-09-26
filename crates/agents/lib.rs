//! Lynceus agent 层 —— `server/core/agents/` 的阶段 1 移植。
//!
//! 已移植（顺序即收口链的数据流顺序，详见 `docs/REFACTOR_PROGRESS.md`）：
//! - [`llm`]：结构化 provider 运行时（META 的 LLM 路径依赖）；
//! - [`coverage`]：COV —— 行使面覆盖核算；
//! - [`metacognition`]：META —— 元认知发散（LLM 路径 + 确定性回退）；
//! - [`exit_gate`]：MGATE —— 出口判定（含结果导向权威规则）；
//! - [`escalation`]：EGUARD —— 不可绕过的预算闸；
//! - [`branch_generator`]：收口链之后的发散式分支生成；
//! - [`critique`]：CRITIC —— 假设入池前的对抗评审；
//! - [`termination`]：终止判定（目标契约评估 + 产品验证回执）；
//! - [`observer`]：Finding/证据链的旁路监督（结构化评审报告）；
//! - [`reflector`]：失败复盘器（确定性分类 + 结构化教训）；
//! - [`trajectory`]：带 token 压力监控的滚动轨迹摘要器；
//! - [`strategy_board`]：策略板维护者服务（提示词/操作校验/快照应用）；
//! - [`solver`]：Solver 契约（上下文/结构化结果/注册表）；
//! - [`capability_router`]：Branch 假设 → solver 派遣配置路由（execution
//!   dispatch compatibility layer——开放世界 discovery 已移交
//!   [`tool_retrieval`]）；
//! - [`context`]：工件感知的 `ContextPack` 构建与压缩报告；
//! - [`tool_retrieval`]：`ToolRetriever` 契约（与 `KnowledgeRetriever` 平行）；
//! - [`toolset_selector`]：初始 visible toolset 选择（确定性 fast path +
//!   模型辅助路径）。
//!
//! 收口链一条线：COV → META → MGATE → EGUARD → `BranchGenerator` →
//! `CritiqueAgent`。MGATE 绝不自行创建分支——升级必须流经 EGUARD，
//! 预算闸因此不可绕过。

// 测试代码大量使用 unwrap/expect 直陈前置条件（与 storage 同一惯例）。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod branch_generator;
pub mod capability_router;
pub mod context;
pub mod coverage;
pub mod critique;
pub mod escalation;
pub mod exit_gate;
pub mod llm;
pub mod metacognition;
pub mod observer;
pub mod prompts;
pub mod reflector;
pub mod retrieval;
pub mod solver;
pub mod strategy_board;
pub mod termination;
pub mod tool_retrieval;
pub mod toolset_selector;
pub mod trajectory;
pub mod worker;
