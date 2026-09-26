//! BranchGenerator：发散式分支生成 —— `server/core/agents/
//! mission_runtime.py` `BranchGenerator` 的移植。
//!
//! 每个分支种类绑定一份可执行契约（verb + object + artifact path + hit
//! signal）：缺 hit signal 的分支无法失败，无法失败的分支不是假设。
//! 契约只命名能力、绝不命名具体工具——分支保持工具无关，Capability
//! Router 才能自由选择。分支标题出现工具名是编程错误，立即报错。

use std::collections::HashSet;
use std::sync::OnceLock;

use models::closure::MetacognitionDirection;
use models::critique::ExecutableContract;
use models::fact::Fact;
use models::ids::RetrievalInvocationId;
use models::ids::RunId;
use models::ids::StrategyBoardSnapshotId;
use models::knowledge::KnowledgeRetrievalResult;
use models::mission::Branch;
use models::mission::Mission;
use models::project::Project;

use crate::exit_gate::branch_kind_for_domains;

/// 分支生成的调用目的标签（审计字段）。
pub const BRANCH_GENERATION_PURPOSE: &str = "branch_generation";

/// 分支生成协议：模型从目标与证据发散出可证伪的审计分支。
pub const BRANCH_GENERATION_PROTOCOL: &str =
    include_str!("../../resources/prompts/branch_generator.md");

/// 分支标题中禁用的工具名词表（Python `TOOL_NAME_TOKENS`）。
///
/// 命名工具的分支会把"能力"窄化为"某个工具的调用"，剥夺 Router 的
/// 选择权——这是契约层违规，不是风格问题。
pub const TOOL_NAME_TOKENS: [&str; 5] =
    ["semgrep", "nuclei", "ida", "maigret", "browser"];

/// 分支契约静态表（Python `BRANCH_CONTRACTS` 的镜像）：
/// (`branch_kind`, verb, object, `artifact_path`, `hit_signal`)。
const BRANCH_CONTRACT_TABLE: [(&str, &str, &str, &str, &str); 18] = [
    (
        "url.surface_mapping",
        "crawl and enumerate reachable routes",
        "the target URL and its linked client-side assets",
        "artifacts/recon/surface_map.json",
        "at least one previously unknown route or parameter is recorded",
    ),
    (
        "url.auth_session",
        "probe authentication and session transitions",
        "login, session, and authorization-sensitive endpoints",
        "artifacts/recon/auth_session.json",
        "a session or authorization boundary responds inconsistently across roles",
    ),
    (
        "url.input_validation",
        "submit bounded validation probes",
        "reachable endpoints and their parameters",
        "artifacts/validation/input_validation.json",
        "a probe produces a server-side error or reflected marker traceable to input",
    ),
    (
        "url.known_exposure",
        "fingerprint and match against known exposure signatures",
        "the service banner, headers, and technology fingerprints",
        "artifacts/validation/known_exposure.json",
        "a technology or version matches a known vulnerable signature",
    ),
    (
        "source.source_sink",
        "trace dataflow from untrusted sources to dangerous sinks",
        "the source tree under audit",
        "artifacts/sast/source_sink.sarif",
        "a path reaches a dangerous sink without an intervening sanitizer",
    ),
    (
        "source.dependency_config",
        "resolve and audit declared dependencies and configuration",
        "dependency manifests and security-relevant configuration files",
        "artifacts/sast/dependency_config.json",
        "a dependency or setting matches a known-vulnerable or unsafe pattern",
    ),
    (
        "source.framework_route",
        "enumerate framework routes and their handlers",
        "route declarations and controller entrypoints",
        "artifacts/sast/framework_routes.json",
        "a privileged handler is reachable without an authorization check",
    ),
    (
        "source.secret_exposure",
        "scan for credential and key material",
        "the source tree and its generated artifacts",
        "artifacts/sast/secret_exposure.json",
        "a high-entropy or structurally valid credential is found in tracked content",
    ),
    (
        "binary.parser_surface",
        "identify parsing entrypoints and their bounds assumptions",
        "the binary's input-handling routines",
        "artifacts/binary/parser_surface.json",
        "a parser reads attacker-controlled length or offset without validation",
    ),
    (
        "binary.dangerous_api",
        "enumerate calls to unsafe memory APIs",
        "the binary's imported and internal memory routines",
        "artifacts/binary/dangerous_api.json",
        "an unsafe memory call is reachable with attacker-influenced arguments",
    ),
    (
        "binary.heap_stack",
        "assess reachability and controllability of a memory hazard",
        "candidate corruption sites identified earlier",
        "artifacts/binary/memory_hazard.json",
        "a candidate site is reached with controlled data in a test harness",
    ),
    (
        "binary.protocol_surface",
        "map protocol states and malformed-input handling",
        "the binary's network or IPC entrypoints",
        "artifacts/binary/protocol_surface.json",
        "a protocol state accepts malformed input without rejecting it",
    ),
    (
        "traffic.parameter_analysis",
        "extract and classify observed parameters",
        "the imported traffic artifact",
        "artifacts/traffic/parameters.json",
        "a parameter crosses a trust boundary or appears reflected downstream",
    ),
    (
        "traffic.auth_context",
        "reconstruct authentication context from observed exchanges",
        "cookies, headers, and tokens present in the traffic",
        "artifacts/traffic/auth_context.json",
        "an authenticated or privileged session state is recovered",
    ),
    (
        "traffic.interesting_endpoint",
        "extract endpoints not reachable by unauthenticated crawling",
        "the imported traffic artifact",
        "artifacts/traffic/endpoints.json",
        "an endpoint absent from the unauthenticated surface map is recorded",
    ),
    (
        "traffic.replay_validation",
        "replay a bounded, authorized subset of requests",
        "requests classified as side-effect free",
        "artifacts/traffic/replay.json",
        "a replayed request reproduces the observed server-side behavior",
    ),
    (
        "mixed.classification",
        "classify the target and resolve its scope",
        "the submitted goal and any supplied artifacts",
        "artifacts/intake/classification.json",
        "the target resolves to a concrete type with an in-scope boundary",
    ),
    (
        "mixed.initial_surface",
        "identify at least one reachable audit surface",
        "the classified target",
        "artifacts/intake/initial_surface.json",
        "a reachable surface is recorded with a verifiable origin fact",
    ),
];

/// 分支契约按种类查表（Python `BRANCH_CONTRACTS.get(kind, …)`）。
///
/// 未收录种类返回空契约——Critic 会把缺失要素报告为
/// `NEEDS_REVISION`，而不是让生成器崩溃。
#[must_use]
pub fn branch_contract(kind: &str) -> Option<ExecutableContract> {
    let (_, verb, object, artifact_path, hit_signal) =
        BRANCH_CONTRACT_TABLE.iter().find(|entry| entry.0 == kind)?;
    Some(ExecutableContract {
        verb: verb.to_string(),
        object: object.to_string(),
        artifact_path: artifact_path.to_string(),
        hit_signal: hit_signal.to_string(),
    })
}

/// 模型返回的分支候选（`generate_structured` 的 JSON 形状）。
#[derive(Debug, Clone, serde::Deserialize)]
struct LlmBranchCandidate {
    /// 分支标题（人类可读的假设陈述）。
    title: String,
    /// 可证伪的猜想。
    hypothesis: String,
    /// 为什么这条值得先查。
    rationale: String,
    /// 分支种类（必须是 `BRANCH_CONTRACT_TABLE` 中的 key）。
    branch_kind: String,
    /// 优先级（越大越先查）。
    #[serde(default)]
    priority: i64,
    /// 置信度（0..=1）。
    #[serde(default)]
    confidence: f64,
}

/// 模型返回的分支集合。
#[derive(Debug, Clone, serde::Deserialize)]
struct LlmBranches {
    /// 分支候选列表（至少一条）。
    branches: Vec<LlmBranchCandidate>,
}

/// 分支生成失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BranchGeneratorError {
    /// 分支标题中出现了工具名（Python `ValueError`）。
    #[error("branch title must not be a tool name: {title}")]
    ToolNamedBranch {
        /// 违规标题。
        title: String,
    },
    /// 模型调用失败（网络、provider 不可用、限流等）。
    #[error("branch generation provider call failed: {message}")]
    ProviderCall {
        /// 脱敏后的失败原因。
        message: String,
    },
    /// 模型返回的结构无法解析成可用分支（缺字段、空分支、类型错误）。
    #[error("branch generation response is malformed: {message}")]
    MalformedResponse {
        /// 脱敏后的解析失败原因。
        message: String,
    },
}

/// 一次首轮分支生成的输入（Python `generate` 的 keyword-only 参数镜像）。
///
/// 知识输入保留检索分数、命中词和来源，避免在 Agent 边界退化成不透明
/// ID 或无依据的卡片正文。
#[derive(Debug)]
pub struct BranchGenerationInput<'a> {
    /// 所属 Mission。
    pub mission: &'a Mission,
    /// 所属 Project。
    pub project: &'a Project,
    /// 起点事实（kind 为 `origin` / `goal` 的进入 `related_fact_ids`）。
    pub facts: &'a [Fact],
    /// 上下文提示。
    pub hints: &'a [String],
    /// 关联的策略板快照。
    pub strategy_board_id: Option<&'a StrategyBoardSnapshotId>,
    /// 已由检索与预算打包选出的语义结果。
    pub knowledge_results: &'a [KnowledgeRetrievalResult],
    /// 产生该结果集的检索审计记录。
    pub retrieval_invocation_id: Option<&'a RetrievalInvocationId>,
    /// 关联 Run。
    pub run_id: Option<&'a RunId>,
}

impl<'a> BranchGenerationInput<'a> {
    /// 以最小输入构造（无事实 / 提示 / 知识卡片，未绑定 Run）。
    #[must_use]
    pub fn new(mission: &'a Mission, project: &'a Project) -> Self {
        Self {
            mission,
            project,
            facts: &[],
            hints: &[],
            strategy_board_id: None,
            knowledge_results: &[],
            retrieval_invocation_id: None,
            run_id: None,
        }
    }
}

/// 一次方向物化的输入（Python `generate_from_directions` 的镜像）。
#[derive(Debug)]
pub struct DirectionMaterializationInput<'a> {
    /// 所属 Mission。
    pub mission: &'a Mission,
    /// 所属 Project。
    pub project: &'a Project,
    /// EGUARD 放行的方向。
    pub directions: &'a [MetacognitionDirection],
    /// 起点事实。
    pub facts: &'a [Fact],
    /// 关联 Run。
    pub run_id: Option<&'a RunId>,
    /// 收口轮次。
    pub round_index: i64,
}

impl<'a> DirectionMaterializationInput<'a> {
    /// 以最小输入构造（无事实，未绑定 Run，round 0）。
    #[must_use]
    pub fn new(
        mission: &'a Mission,
        project: &'a Project,
        directions: &'a [MetacognitionDirection],
    ) -> Self {
        Self {
            mission,
            project,
            directions,
            facts: &[],
            run_id: None,
            round_index: 0,
        }
    }
}

/// 确定性首轮可证伪分支生成器。
#[derive(Debug, Default)]
pub struct BranchGenerator;

fn knowledge_context(results: &[KnowledgeRetrievalResult]) -> serde_json::Value {
    serde_json::Value::Array(
        results
            .iter()
            .map(|result| {
                let card = &result.card;
                serde_json::json!({
                    "id": card.id.as_str(),
                    "title": card.title,
                    "summary": card.effective_summary(),
                    "kind": card.kind,
                    "tags": card.tags,
                    "tool": card.tool,
                    "technique": card.technique,
                    "platform": card.platform,
                    "protocol": card.protocol,
                    "prerequisites": card.prerequisites,
                    "score": result.score,
                    "matched_terms": result.matched_terms,
                    "retrieval_reason": result.retrieval_reason,
                    "source_id": card.source,
                    "source_locator": card.source_locator,
                    "content_hash": card.content_hash,
                })
            })
            .collect(),
    )
}

fn branch_context(input: &BranchGenerationInput<'_>) -> serde_json::Map<String, serde_json::Value> {
    let mut context = serde_json::Map::new();
    context.insert(
        "hints".to_string(),
        serde_json::Value::Array(
            input
                .hints
                .iter()
                .map(|hint| serde_json::Value::String(hint.clone()))
                .collect(),
        ),
    );
    context.insert(
        "strategy_board_id".to_string(),
        input
            .strategy_board_id
            .map_or(serde_json::Value::Null, |id| {
                serde_json::Value::String(id.as_str().to_string())
            }),
    );
    context.insert(
        "knowledge_card_ids".to_string(),
        serde_json::Value::Array(
            input
                .knowledge_results
                .iter()
                .map(|result| serde_json::Value::String(result.card.id.as_str().to_string()))
                .collect(),
        ),
    );
    context.insert(
        "retrieval_invocation_id".to_string(),
        input
            .retrieval_invocation_id
            .map_or(serde_json::Value::Null, |id| {
                serde_json::Value::String(id.as_str().to_string())
            }),
    );
    context.insert(
        "knowledge_cards".to_string(),
        knowledge_context(input.knowledge_results),
    );
    context.insert(
        "valid_branch_kinds".to_string(),
        serde_json::Value::Array(
            BRANCH_CONTRACT_TABLE
                .iter()
                .map(|(kind, ..)| serde_json::Value::String((*kind).to_string()))
                .collect(),
        ),
    );
    context.insert(
        "target_metadata".to_string(),
        serde_json::Value::Object(
            input
                .project
                .target
                .iter()
                .map(|(key, value)| {
                    (
                        key.to_string(),
                        serde_json::Value::String(value.to_string()),
                    )
                })
                .collect(),
        ),
    );
    context
}

impl BranchGenerator {
    /// 生成首轮分支（模型驱动）。
    ///
    /// 分支由模型从 mission 目标、起点事实与检索到的知识卡中发散产出，
    /// 不按目标类型查表——任何输入都得到回答，输入不足以支撑审计时模型
    /// 应当给出"向用户澄清目标"这类分支，而不是返回空。
    ///
    /// # Errors
    /// provider 调用失败返回 [`BranchGeneratorError::ProviderCall`]；响应
    /// 无法解析成至少一条合法分支返回
    /// [`BranchGeneratorError::MalformedResponse`]；标题含工具名返回
    /// [`BranchGeneratorError::ToolNamedBranch`]。
    pub async fn generate(
        &self,
        runtime: &dyn crate::llm::ProviderRuntime,
        provider_id: &str,
        input: &BranchGenerationInput<'_>,
    ) -> Result<Vec<Branch>, BranchGeneratorError> {
        let related_fact_ids: Vec<String> = input
            .facts
            .iter()
            .filter(|fact| fact.kind == "origin" || fact.kind == "goal")
            .map(|fact| fact.id.as_str().to_string())
            .collect();
        let context = branch_context(input);
        let payload = serde_json::to_string(&context).map_err(|error| {
            BranchGeneratorError::MalformedResponse {
                message: format!("payload serialization failed: {error}"),
            }
        })?;

        let messages = [
            crate::llm::LlmMessage::new("system", BRANCH_GENERATION_PROTOCOL.to_string()),
            crate::llm::LlmMessage::new("user", payload),
        ];
        let result = runtime
            .generate_structured(crate::llm::StructuredGenerationRequest {
                provider_id,
                messages: &messages,
                purpose: BRANCH_GENERATION_PURPOSE,
                project_id: Some(&input.project.id),
                run_id: input.run_id,
                task_id: None,
            })
            .await
            .map_err(|error| BranchGeneratorError::ProviderCall {
                message: error.message,
            })?;

        let parsed: LlmBranches = serde_json::from_value(serde_json::Value::Object(result))
            .map_err(|error| BranchGeneratorError::MalformedResponse {
                message: error.to_string(),
            })?;
        if parsed.branches.is_empty() {
            return Err(BranchGeneratorError::MalformedResponse {
                message: "model returned no branches".to_string(),
            });
        }

        let mut branches: Vec<Branch> = Vec::new();
        for candidate in &parsed.branches {
            let branch_kind = candidate.branch_kind.trim();
            let contract = branch_contract(branch_kind).ok_or_else(|| {
                BranchGeneratorError::MalformedResponse {
                    message: format!("unknown branch_kind: {branch_kind}"),
                }
            })?;
            let title = candidate.title.trim();
            if title.is_empty() {
                return Err(BranchGeneratorError::MalformedResponse {
                    message: "branch title must be non-empty".to_string(),
                });
            }
            let mut branch = Branch::new(
                input.project.id.clone(),
                input.mission.id.clone(),
                title.to_string(),
                candidate.hypothesis.trim().to_string(),
            );
            branch.run_id = input.run_id.cloned();
            branch.rationale = candidate.rationale.trim().to_string();
            branch.priority = candidate.priority;
            branch.confidence = candidate.confidence;
            branch.related_fact_ids.clone_from(&related_fact_ids);
            branch.metadata.clone_from(&context);
            branch.metadata.insert(
                "branch_kind".to_string(),
                serde_json::Value::String(branch_kind.to_string()),
            );
            branch.metadata.insert(
                "contract".to_string(),
                serde_json::to_value(&contract)
                    .unwrap_or_else(|error| panic!("契约序列化不会失败: {error}")),
            );
            assert_not_tool_named(&branch)?;
            branches.push(branch);
        }
        tracing::info!(
            mission = %input.mission.id,
            project = %input.project.id,
            count = branches.len(),
            "branch generation"
        );
        Ok(branches)
    }

    /// 把 EGUARD 放行的元认知方向物化为分支（纯计算，无 I/O）。
    ///
    /// 方向以猜想形式到达；每条变成携带全新可执行契约与升级溯源的
    /// 分支——Critic 仍可拒绝不可证伪或不可执行的方向。分支种类映射
    /// 自方向的盲区域，Capability Router 无须平行路由方案即可派发。
    ///
    /// # Errors
    /// 任一分支标题含工具名词表中的词时返回
    /// [`BranchGeneratorError::ToolNamedBranch`]。
    ///
    /// # Panics
    /// 契约为纯字符串结构，`serde_json::to_value` 的 Err 分支在类型上
    /// 不可达；若到达属编程错误，立即失败优于静默落盘残缺契约。
    pub fn generate_from_directions(
        &self,
        input: &DirectionMaterializationInput<'_>,
    ) -> Result<Vec<Branch>, BranchGeneratorError> {
        let related_fact_ids: Vec<String> = input
            .facts
            .iter()
            .filter(|fact| fact.kind == "origin" || fact.kind == "goal")
            .map(|fact| fact.id.as_str().to_string())
            .collect();
        let mut branches: Vec<Branch> = Vec::new();
        for (index, direction) in input.directions.iter().enumerate() {
            let domains = &direction.related_blind_spots;
            let branch_kind = branch_kind_for_domains(domains);
            let slug = slug_from_title(&direction.title);
            let artifact_name = if slug.is_empty() {
                index.to_string()
            } else {
                slug
            };
            let contract = ExecutableContract {
                verb: "exercise the proposed direction and record observations".to_string(),
                object: direction.title.clone(),
                artifact_path: format!(
                    "artifacts/escalation/round_{}/{}.json",
                    input.round_index, artifact_name
                ),
                hit_signal: "the recorded observation contains a verifiable signal (route, \
                             reflection, mismatch, disclosure, or failure) attributable to this \
                             direction; the absence of any signal falsifies it"
                    .to_string(),
            };
            let mut branch = Branch::new(
                input.project.id.clone(),
                input.mission.id.clone(),
                direction.title.clone(),
                direction.hypothesis.clone(),
            );
            branch.run_id = input.run_id.cloned();
            branch.rationale.clone_from(&direction.rationale);
            branch.priority = 60;
            branch.confidence = 0.4;
            branch.related_fact_ids.clone_from(&related_fact_ids);
            branch.metadata.insert(
                "branch_kind".to_string(),
                serde_json::Value::String(branch_kind),
            );
            branch.metadata.insert(
                "contract".to_string(),
                serde_json::to_value(&contract)
                    .unwrap_or_else(|error| panic!("契约序列化不会失败: {error}")),
            );
            branch.metadata.insert(
                "escalation".to_string(),
                serde_json::json!({
                    "round_index": input.round_index,
                    "framework": direction.framework.as_str(),
                    "related_blind_spots": domains.iter().map(|domain| domain.as_str()).collect::<Vec<_>>(),
                    "related_unmet_requirements": direction.related_unmet_requirements,
                }),
            );
            assert_not_tool_named(&branch)?;
            branches.push(branch);
        }
        if !branches.is_empty() {
            tracing::info!(
                mission = %input.mission.id,
                project = %input.project.id,
                count = branches.len(),
                round_index = input.round_index,
                "escalation branches materialized from directions"
            );
        }
        Ok(branches)
    }
}

/// Python `_assert_not_tool_named`：标题小写化后按 `[a-z0-9_]+` 分词，
/// 任一 token 命中工具名词表即报错。
///
/// # Errors
/// 标题含工具名时返回 [`BranchGeneratorError::ToolNamedBranch`]。
pub fn assert_not_tool_named(branch: &Branch) -> Result<(), BranchGeneratorError> {
    let word_re = word_regex();
    let lowered = branch.title.to_lowercase();
    let words: HashSet<&str> = word_re
        .find_iter(&lowered)
        .map(|matched| matched.as_str())
        .collect();
    if TOOL_NAME_TOKENS.iter().any(|token| words.contains(token)) {
        return Err(BranchGeneratorError::ToolNamedBranch {
            title: branch.title.clone(),
        });
    }
    Ok(())
}

fn word_regex() -> &'static regex::Regex {
    static WORD_RE: OnceLock<regex::Regex> = OnceLock::new();
    WORD_RE.get_or_init(|| {
        regex::Regex::new(r"[a-z0-9_]+")
            .unwrap_or_else(|error| panic!("内置正则必须可编译（静态字面量）: {error}"))
    })
}

/// Python `re.sub(r"[^a-z0-9]+", "-", title.lower()).strip("-")[:48]`。
fn slug_from_title(title: &str) -> String {
    let slug_re = slug_regex();
    let lowered = title.to_lowercase();
    let substituted = slug_re.replace_all(&lowered, "-");
    substituted.trim_matches('-').chars().take(48).collect()
}

fn slug_regex() -> &'static regex::Regex {
    static SLUG_RE: OnceLock<regex::Regex> = OnceLock::new();
    SLUG_RE.get_or_init(|| {
        regex::Regex::new(r"[^a-z0-9]+")
            .unwrap_or_else(|error| panic!("内置正则必须可编译（静态字面量）: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::closure::MetacognitionFramework;
    use models::domain::AuditDomain;
    use models::ids::ProjectId;
    use models::{KnowledgeCard, KnowledgeCardId};

    fn project() -> Project {
        let mut project = Project::new("source".to_string(), AuditDomain::WebSast);
        project.id = ProjectId::new("proj_test".to_string());
        project
            .target
            .insert("repo_path".to_string(), "D:/src/app".to_string());
        project
    }

    fn mission() -> Mission {
        Mission::new(
            ProjectId::new("proj_test".to_string()),
            "Audit source".to_string(),
        )
    }

    /// 返回固定分支集合的测试用 provider。
    struct StubProvider {
        branches: serde_json::Value,
    }

    #[async_trait::async_trait]
    impl crate::llm::ProviderRuntime for StubProvider {
        async fn list_providers(
            &self,
        ) -> Result<Vec<models::provider::ProviderConfig>, crate::llm::ProviderCallError> {
            Ok(Vec::new())
        }

        async fn get_provider(
            &self,
            _provider_id: &str,
        ) -> Result<Option<models::provider::ProviderConfig>, crate::llm::ProviderCallError> {
            Ok(None)
        }

        async fn resolve_default_provider(
            &self,
        ) -> Result<Option<models::provider::ProviderConfig>, crate::llm::ProviderCallError> {
            Ok(None)
        }

        async fn health_check(
            &self,
            _provider_id: &str,
        ) -> Result<models::provider::ProviderHealthResult, crate::llm::ProviderCallError> {
            unreachable!("branch generation tests never health-check")
        }

        async fn generate_text(
            &self,
            _request: crate::llm::TextGenerationRequest<'_>,
        ) -> Result<crate::llm::LlmResponse, crate::llm::ProviderCallError> {
            unreachable!("branch generation uses generate_structured")
        }

        async fn generate_structured(
            &self,
            _request: crate::llm::StructuredGenerationRequest<'_>,
        ) -> Result<serde_json::Map<String, serde_json::Value>, crate::llm::ProviderCallError> {
            let object = self
                .branches
                .as_object()
                .cloned()
                .unwrap_or_else(|| panic!("stub branches must be a JSON object"));
            Ok(object)
        }
    }

    /// 一条合法分支的 JSON 形状。
    fn stub_branch(title: &str, kind: &str) -> serde_json::Value {
        serde_json::json!({
            "title": title,
            "hypothesis": "The target exposes at least one untested assumption.",
            "rationale": "Grounding later branches.",
            "branch_kind": kind,
            "priority": 80,
            "confidence": 0.6
        })
    }

    /// Python `test_branch_generator_does_not_name_tools_as_branches`。
    #[tokio::test]
    async fn does_not_name_tools_as_branches() {
        let project = project();
        let source_mission = mission();
        let provider = StubProvider {
            branches: serde_json::json!({ "branches": [stub_branch(
                "Trace untrusted input to sinks",
                "source.source_sink"
            )] }),
        };
        let branches = BranchGenerator
            .generate(&provider, "", &BranchGenerationInput::new(&source_mission, &project))
            .await
            .unwrap_or_else(|error| panic!("生成不得失败: {error}"));

        assert_eq!(branches.len(), 1);
        let titles: Vec<String> = branches
            .iter()
            .map(|branch| branch.title.to_lowercase())
            .collect();
        for title in &titles {
            for tool in ["semgrep", "nuclei", "ida"] {
                assert!(!title.contains(tool), "标题 {title} 不得含工具名 {tool}");
            }
        }
    }

    /// 模型返回的字段必须落到 Branch 上；未知 branch_kind 必须被拒。
    #[tokio::test]
    async fn model_branches_are_parsed_onto_the_branch() {
        let project = project();
        let mission = mission();
        let provider = StubProvider {
            branches: serde_json::json!({ "branches": [stub_branch(
                "Map externally reachable routes",
                "url.surface_mapping"
            )] }),
        };
        let branches = BranchGenerator
            .generate(&provider, "", &BranchGenerationInput::new(&mission, &project))
            .await
            .unwrap_or_else(|error| panic!("生成不得失败: {error}"));
        let first = &branches[0];
        assert_eq!(first.title, "Map externally reachable routes");
        assert_eq!(first.priority, 80);
        assert!((first.confidence - 0.6).abs() < f64::EPSILON);
        assert_eq!(
            first
                .metadata
                .get("branch_kind")
                .and_then(serde_json::Value::as_str),
            Some("url.surface_mapping")
        );
        // 不再写入 target_type：分类机制已删除。
        assert!(first.metadata.get("target_type").is_none());
        let contract = first
            .metadata
            .get("contract")
            .cloned()
            .unwrap_or_else(|| panic!("契约必须存在"));
        assert_eq!(
            contract
                .get("artifact_path")
                .and_then(serde_json::Value::as_str),
            Some("artifacts/recon/surface_map.json")
        );
    }

    /// 未知 branch_kind 是契约违规，必须失败而不是落一条空契约。
    #[tokio::test]
    async fn unknown_branch_kind_is_rejected() {
        let project = project();
        let mission = mission();
        let provider = StubProvider {
            branches: serde_json::json!({ "branches": [stub_branch(
                "Some capability-shaped title",
                "not.a.real.kind"
            )] }),
        };
        let error = BranchGenerator
            .generate(&provider, "", &BranchGenerationInput::new(&mission, &project))
            .await
            .expect_err("未知 branch_kind 必须失败");
        assert!(
            matches!(error, BranchGeneratorError::MalformedResponse { .. }),
            "unexpected error: {error:?}"
        );
    }

    /// 模型返回空分支集 = 没有回答，必须失败（调用方据此降级）。
    #[tokio::test]
    async fn empty_branch_set_is_rejected() {
        let project = project();
        let mission = mission();
        let provider = StubProvider {
            branches: serde_json::json!({ "branches": [] }),
        };
        let error = BranchGenerator
            .generate(&provider, "", &BranchGenerationInput::new(&mission, &project))
            .await
            .expect_err("空分支集必须失败");
        assert!(
            matches!(error, BranchGeneratorError::MalformedResponse { .. }),
            "unexpected error: {error:?}"
        );
    }

    /// 标题含工具名必须被拒（模型也可能违规，不只是硬编码表）。
    #[tokio::test]
    async fn tool_named_model_branch_is_rejected() {
        let project = project();
        let mission = mission();
        let provider = StubProvider {
            branches: serde_json::json!({ "branches": [stub_branch(
                "Run semgrep over the tree",
                "source.source_sink"
            )] }),
        };
        let error = BranchGenerator
            .generate(&provider, "", &BranchGenerationInput::new(&mission, &project))
            .await
            .expect_err("工具名标题必须失败");
        assert!(
            matches!(error, BranchGeneratorError::ToolNamedBranch { .. }),
            "unexpected error: {error:?}"
        );
    }

    #[tokio::test]
    async fn knowledge_context_preserves_retrieval_semantics() {
        let project = project();
        let source_mission = mission();
        let mut card: KnowledgeCard = serde_json::from_value(serde_json::json!({
            "kind": "vulnerability_pattern",
            "title": "SQL injection data flow",
            "summary": "Trace untrusted input to database sinks.",
            "source": "security-wiki",
            "source_locator": "SQL注入.md#flow",
            "content_hash": "abc123"
        }))
        .unwrap_or_else(|error| panic!("测试知识卡必须有效: {error}"));
        card.id = KnowledgeCardId::new("kcard_sql_flow".to_string());
        let results = [KnowledgeRetrievalResult {
            card,
            score: 8.5,
            matched_terms: vec!["sql injection".to_string()],
            retrieval_reason: Some("title".to_string()),
        }];
        let invocation_id = RetrievalInvocationId::new("retr_test".to_string());
        let mut input = BranchGenerationInput::new(&source_mission, &project);
        input.knowledge_results = &results;
        input.retrieval_invocation_id = Some(&invocation_id);
        let provider = StubProvider {
            branches: serde_json::json!({ "branches": [stub_branch(
                "Trace untrusted input to sinks",
                "source.source_sink"
            )] }),
        };

        let branches = BranchGenerator
            .generate(&provider, "", &input)
            .await
            .unwrap_or_else(|error| panic!("生成不得失败: {error}"));
        let metadata = &branches[0].metadata;
        assert_eq!(
            metadata
                .get("retrieval_invocation_id")
                .and_then(serde_json::Value::as_str),
            Some("retr_test")
        );
        let knowledge = metadata["knowledge_cards"]
            .as_array()
            .and_then(|items| items.first())
            .unwrap_or_else(|| panic!("必须注入一条知识结果"));
        assert_eq!(knowledge["score"], serde_json::json!(8.5));
        assert_eq!(knowledge["retrieval_reason"], serde_json::json!("title"));
        assert_eq!(
            knowledge["matched_terms"],
            serde_json::json!(["sql injection"])
        );
        assert_eq!(knowledge["source_id"], serde_json::json!("security-wiki"));
        assert_eq!(
            knowledge["source_locator"],
            serde_json::json!("SQL注入.md#flow")
        );
    }

    #[test]
    fn directions_materialize_with_escalation_provenance() {
        let project = project();
        let url_mission = mission();
        let directions = vec![MetacognitionDirection {
            title: "Transfer proven techniques to web_dast".to_string(),
            hypothesis: "The web_dast surface has not been exercised.".to_string(),
            rationale: "analogy".to_string(),
            framework: MetacognitionFramework::Analogy,
            related_blind_spots: vec![models::domain::AuditDomain::WebDast],
            related_unmet_requirements: vec!["flag".to_string()],
        }];
        let mut input = DirectionMaterializationInput::new(&url_mission, &project, &directions);
        input.round_index = 2;
        let branches = BranchGenerator
            .generate_from_directions(&input)
            .unwrap_or_else(|error| panic!("物化不得失败: {error}"));
        assert_eq!(branches.len(), 1);
        let branch = &branches[0];
        assert_eq!(branch.title, "Transfer proven techniques to web_dast");
        assert_eq!(branch.priority, 60);
        assert!((branch.confidence - 0.4).abs() < f64::EPSILON);
        assert_eq!(
            branch
                .metadata
                .get("branch_kind")
                .and_then(serde_json::Value::as_str),
            Some("url.input_validation")
        );
        let escalation = branch
            .metadata
            .get("escalation")
            .cloned()
            .unwrap_or_else(|| panic!("升级溯源必须存在"));
        assert_eq!(
            escalation
                .get("round_index")
                .and_then(serde_json::Value::as_i64),
            Some(2)
        );
        assert_eq!(
            escalation
                .get("framework")
                .and_then(serde_json::Value::as_str),
            Some("analogy")
        );
        assert_eq!(
            escalation
                .get("related_blind_spots")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(1)
        );
        let artifact_path = branch
            .metadata
            .get("contract")
            .and_then(|contract| contract.get("artifact_path"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            artifact_path,
            "artifacts/escalation/round_2/transfer-proven-techniques-to-web-dast.json"
        );
    }

    #[test]
    fn empty_slug_falls_back_to_direction_index() {
        let project = project();
        let url_mission = mission();
        // 标题全为非 [a-z0-9] 字符 → slug 为空 → 落到 index。
        let directions = vec![MetacognitionDirection {
            title: "!!!---###".to_string(),
            hypothesis: "Some falsifiable hypothesis of decent length here".to_string(),
            rationale: String::new(),
            framework: MetacognitionFramework::Inversion,
            related_blind_spots: vec![],
            related_unmet_requirements: vec![],
        }];
        let branches = BranchGenerator
            .generate_from_directions(&DirectionMaterializationInput::new(
                &url_mission,
                &project,
                &directions,
            ))
            .unwrap_or_else(|error| panic!("物化不得失败: {error}"));
        let artifact_path = branches[0]
            .metadata
            .get("contract")
            .and_then(|contract| contract.get("artifact_path"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        assert_eq!(artifact_path, "artifacts/escalation/round_0/0.json");
    }

    #[test]
    fn tool_named_branch_is_rejected() {
        let mut branch = Branch::new(
            ProjectId::new("p".to_string()),
            models::ids::MissionId::new("m".to_string()),
            "Run Semgrep scan".to_string(),
            "hypothesis".to_string(),
        );
        assert_eq!(
            assert_not_tool_named(&branch),
            Err(BranchGeneratorError::ToolNamedBranch {
                title: "Run Semgrep scan".to_string()
            })
        );
        branch.title = "Trace dataflow from sources to sinks".to_string();
        assert_eq!(assert_not_tool_named(&branch), Ok(()));
    }

    #[test]
    fn slug_truncates_to_48_chars() {
        let long_title = "a".repeat(80);
        assert_eq!(slug_from_title(&long_title).len(), 48);
        assert_eq!(slug_from_title("--hello world--"), "hello-world");
    }
}
