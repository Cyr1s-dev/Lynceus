//! META：元认知发散 —— `server/core/agents/closure.py`
//! `MetacognitionAgent` 的移植。
//!
//! 五个发散框架在确定性回退中恒被施加——provider 缺席降级的是覆盖
//! 质量，绝不是收口链本身。任何 provider 失败、畸形响应或断言式
//! （非猜想式）方向都回退到结构化生成器：元认知绝不在 provider 宕机
//! 时阻塞收口链，也绝不编造确定性。

use std::collections::HashSet;
use std::sync::OnceLock;

use models::agent::TerminationAssessment;
use models::closure::CoverageAssessment;
use models::closure::MetacognitionAssessment;
use models::closure::MetacognitionDirection;
use models::closure::MetacognitionFramework;
use models::closure::MetacognitionMode;
use models::closure::MetacognitionTrigger;
use models::domain::AuditDomain;
use models::finding::Finding;
use models::mission::Branch;
use models::mission::Mission;
use models::run::AuditRun;

use crate::llm::LlmMessage;
use crate::llm::ProviderRuntime;
use crate::llm::StructuredGenerationRequest;

/// 模型调用的用途标签（provider 路由表按它匹配绑定）。
pub const METACOGNITION_PURPOSE: &str = "metacognition_divergence";

/// 元认知协议提示词（Python `METACOGNITION_PROTOCOL` 逐字节镜像）。
/// 元认知协议提示词（Python `METACOGNITION_PROTOCOL` 逐字节镜像）。
/// 内置默认以文件存储（resources/prompts/metacognition_divergence.md，include_str! 逐字节）。
pub const METACOGNITION_PROTOCOL: &str = include_str!("../../resources/prompts/metacognition_divergence.md");

/// 会把方向变成断言式结论的措辞（Python `_ASSERTIVE_DIRECTION_PATTERNS`）。
///
/// 含此类措辞的 LLM 方向被整体丢弃而非修复——确定性回退已经诚实地
/// 覆盖了素材。
const ASSERTIVE_DIRECTION_PATTERN_TEXTS: [&str; 6] = [
    r"\bis (?:definitely |certainly |clearly )?vulnerable\b",
    r"\bis exploitable\b",
    r"\bwill (?:definitely |certainly )?(?:succeed|work|bypass)\b",
    r"\b(?:definitely|certainly|obviously|undoubtedly|guaranteed)\b",
    r"\bproves?\b",
    r"\bconfirmed\b",
];

fn assertive_direction_patterns() -> &'static [regex::Regex] {
    static PATTERNS: OnceLock<Vec<regex::Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        ASSERTIVE_DIRECTION_PATTERN_TEXTS
            .iter()
            .map(|text| {
                regex::Regex::new(text).unwrap_or_else(|error| {
                    panic!("内置正则 {text} 必须可编译（静态字面量）: {error}")
                })
            })
            .collect()
    })
}

/// 文本是否含断言式措辞（Python `_is_assertive`：小写化后逐一匹配）。
#[must_use]
pub fn is_assertive_direction_text(text: &str) -> bool {
    let lowered = text.to_lowercase();
    assertive_direction_patterns()
        .iter()
        .any(|pattern| pattern.is_match(&lowered))
}

/// LLM 方向 payload 校验（Python `_LLMDirectionsPayload` /
/// `_LLMDirectionPayload`；模型为 `extra="forbid"`，故未知
/// 字段使整体校验失败 → 调用方降级）。
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LlmDirection {
    title: String,
    hypothesis: String,
    #[serde(default)]
    rationale: String,
    #[serde(default = "default_llm_framework")]
    framework: MetacognitionFramework,
    #[serde(default)]
    related_blind_spots: Vec<AuditDomain>,
}

/// Python `framework: MetacognitionFramework = MetacognitionFramework.ANALOGY`。
fn default_llm_framework() -> MetacognitionFramework {
    MetacognitionFramework::Analogy
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LlmDirections {
    #[serde(default)]
    directions: Vec<LlmDirection>,
}

/// 一次元认知评估的输入（Python `assess` 的 keyword-only 参数镜像）。
#[derive(Debug)]
pub struct MetacognitionInput<'a> {
    /// 所属 Mission。
    pub mission: &'a Mission,
    /// 所属 Run。
    pub run: &'a AuditRun,
    /// COV 输出。
    pub coverage: &'a CoverageAssessment,
    /// 终止评估（可选）。
    pub termination: Option<&'a TerminationAssessment>,
    /// 现有分支（LLM 路径的去重上下文）。
    pub branches: &'a [Branch],
    /// 全部 Finding。
    pub findings: &'a [Finding],
    /// 触发原因。
    pub trigger: MetacognitionTrigger,
    /// 收口轮次。
    pub round_index: i64,
}

impl<'a> MetacognitionInput<'a> {
    /// 以默认触发（收敛）构造。
    #[must_use]
    pub fn new(mission: &'a Mission, run: &'a AuditRun, coverage: &'a CoverageAssessment) -> Self {
        Self {
            mission,
            run,
            coverage,
            termination: None,
            branches: &[],
            findings: &[],
            trigger: MetacognitionTrigger::Convergence,
            round_index: 0,
        }
    }
}

/// LLM 发散路径的运行时绑定（Python `provider_runtime` + `provider_id`）。
///
/// `dyn ProviderRuntime` 不实现 `Debug`——按值绑定是运行时
/// 依赖，不是需要打印的状态。
#[derive(Clone, Copy)]
pub struct LlmDivergenceContext<'a> {
    /// 结构化生成运行时。
    pub runtime: &'a dyn ProviderRuntime,
    /// Provider 标识（`None` = 默认 provider）。
    pub provider_id: Option<&'a str>,
}

/// LLM 发散路径失败（Python 侧为开放异常集合，统一降级）。
#[derive(Debug, thiserror::Error)]
pub enum LlmDivergenceError {
    /// Provider 调用失败（网络 / 超时 / provider 内部错误）。
    #[error("provider call failed: {message}")]
    ProviderCall {
        /// 面向日志的失败描述。
        message: String,
    },
    /// 响应不符合方向 payload schema（Python `ValidationError`）。
    #[error("malformed provider response: {message}")]
    MalformedResponse {
        /// 面向日志的失败描述。
        message: String,
    },
}

impl LlmDivergenceError {
    /// 降级备注里的"异常类名"（镜像 Python `type(exc).__name__`）。
    ///
    /// Python 侧异常类型是开放集合（provider 实现决定），两侧不保证
    /// 逐字节一致；测试语义只锁定 "falling back" 关键词。
    #[must_use]
    fn kind_name(&self) -> &'static str {
        match self {
            LlmDivergenceError::ProviderCall { .. } => "ProviderRuntimeError",
            LlmDivergenceError::MalformedResponse { .. } => "ValidationError",
        }
    }
}

/// 假设过短即非可行动方向的阈值（Python `MIN_HYPOTHESIS_LENGTH` 语义，
/// 出自 critique 侧同源规则）。
const MIN_HYPOTHESIS_CHARS: usize = 20;

/// META：带 LLM 路径与确定性回退的发散检查器。
#[derive(Debug)]
pub struct MetacognitionAgent {
    max_directions: usize,
}

impl Default for MetacognitionAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl MetacognitionAgent {
    /// 构造器（Python 默认 `max_directions=5`）。
    #[must_use]
    pub fn new() -> Self {
        Self { max_directions: 5 }
    }

    /// 指定方向上限（Python `max(1, max_directions)` 语义）。
    #[must_use]
    pub fn with_max_directions(max_directions: usize) -> Self {
        Self {
            max_directions: max_directions.max(1),
        }
    }

    /// 执行一轮发散评估。
    ///
    /// `llm` 为 `None` 时直接走确定性路径；LLM 路径的任何失败都降级
    /// 为确定性路径并在 `notes` 记录原因——收口链不因 provider 宕机
    /// 阻塞。
    ///
    /// # 红线语义
    ///
    /// 深化框架（极端/组合/降维）仅在目标**未满足**时触发；目标已满足
    /// 时触发会使 DONE 结构性不可达——任何审计总能被更深地探查，而
    /// 收口链必须能诚实地收口。
    pub async fn assess(
        &self,
        input: &MetacognitionInput<'_>,
        llm: Option<LlmDivergenceContext<'_>>,
    ) -> MetacognitionAssessment {
        let mut notes: Vec<String> = Vec::new();
        let mut directions: Vec<MetacognitionDirection> = Vec::new();
        let mut mode = MetacognitionMode::Deterministic;

        if let Some(llm) = llm {
            match self.assess_with_llm(input, llm).await {
                Ok(llm_directions) => {
                    if !llm_directions.is_empty() {
                        notes.push(format!(
                            "provider proposed {} direction(s)",
                            llm_directions.len()
                        ));
                        directions = llm_directions;
                        mode = MetacognitionMode::Llm;
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        mission = %input.mission.id,
                        run = %input.run.id,
                        error = %error,
                        "LLM divergence path failed; falling back to deterministic frameworks"
                    );
                    notes.push(format!(
                        "LLM divergence path failed ({}: {}); falling back to deterministic \
                         frameworks",
                        error.kind_name(),
                        error
                    ));
                }
            }
        }

        if directions.is_empty() {
            directions =
                self.deterministic_directions(input.coverage, input.termination, input.findings);
        }

        let mut addressed: Vec<AuditDomain> = Vec::new();
        for direction in &directions {
            for domain in &direction.related_blind_spots {
                if !addressed.contains(domain) {
                    addressed.push(*domain);
                }
            }
        }

        let mut assessment =
            MetacognitionAssessment::new(input.run.project_id.clone(), input.run.id.clone());
        assessment.mission_id = Some(input.mission.id.clone());
        assessment.round_index = input.round_index;
        assessment.trigger = input.trigger;
        assessment.mode = mode;
        assessment.frameworks_applied = directions
            .iter()
            .map(|direction| direction.framework)
            .collect();
        assessment.directions = directions;
        assessment.blind_spots_addressed = addressed;
        assessment.notes = notes;
        assessment.metadata.insert(
            "provider_id".to_string(),
            match llm.and_then(|context| context.provider_id) {
                Some(provider_id) => serde_json::Value::String(provider_id.to_string()),
                None => serde_json::Value::Null,
            },
        );
        assessment
            .metadata
            .insert("branch_count".to_string(), json_count(input.branches.len()));
        assessment.metadata.insert(
            "finding_count".to_string(),
            json_count(input.findings.len()),
        );
        assessment
    }

    /// LLM 发散路径（Python `_assess_with_llm`）。
    async fn assess_with_llm(
        &self,
        input: &MetacognitionInput<'_>,
        llm: LlmDivergenceContext<'_>,
    ) -> Result<Vec<MetacognitionDirection>, LlmDivergenceError> {
        let payload = llm_divergence_payload(input, self.max_directions);
        let payload_json = serde_json::to_string(&payload).map_err(|error| {
            LlmDivergenceError::MalformedResponse {
                message: format!("payload serialization failed: {error}"),
            }
        })?;

        let messages = [
            LlmMessage::new(
            "system",
            crate::prompts::system_prompt(
                models::agent_preset::PRESET_METACOGNITION_DIVERGENCE,
                METACOGNITION_PROTOCOL,
            ),
        ),
            LlmMessage::new("user", payload_json),
        ];
        let result = llm
            .runtime
            .generate_structured(StructuredGenerationRequest {
                provider_id: llm.provider_id.unwrap_or_default(),
                messages: &messages,
                purpose: METACOGNITION_PURPOSE,
                project_id: Some(&input.run.project_id),
                run_id: Some(&input.run.id),
                task_id: None,
            })
            .await
            .map_err(|error| LlmDivergenceError::ProviderCall {
                message: error.message,
            })?;

        let parsed: LlmDirections = serde_json::from_value(serde_json::Value::Object(result))
            .map_err(|error| LlmDivergenceError::MalformedResponse {
                message: error.to_string(),
            })?;

        let mut accepted: Vec<MetacognitionDirection> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for item in parsed.directions {
            if accepted.len() >= self.max_directions {
                break;
            }
            let hypothesis = item.hypothesis.trim().to_string();
            if hypothesis.chars().count() < MIN_HYPOTHESIS_CHARS
                || is_assertive_direction_text(&hypothesis)
            {
                continue;
            }
            let key = normalized_whitespace_lower(&hypothesis);
            if !seen.insert(key) {
                continue;
            }
            accepted.push(MetacognitionDirection {
                title: if item.title.trim().is_empty() {
                    "Unnamed direction".to_string()
                } else {
                    item.title.trim().to_string()
                },
                hypothesis,
                rationale: item.rationale.trim().to_string(),
                framework: item.framework,
                related_blind_spots: item.related_blind_spots,
                related_unmet_requirements: Vec::new(),
            });
        }
        Ok(accepted)
    }

    /// 确定性发散路径（Python `_deterministic_directions`）。
    fn deterministic_directions(
        &self,
        coverage: &CoverageAssessment,
        termination: Option<&TerminationAssessment>,
        findings: &[Finding],
    ) -> Vec<MetacognitionDirection> {
        let blind_spots = &coverage.blind_spots;
        let relevant: HashSet<AuditDomain> = coverage.relevant_domains.iter().copied().collect();
        let relevant_covered: Vec<&AuditDomain> = coverage
            .covered_domains
            .iter()
            .filter(|domain| relevant.contains(domain))
            .collect();
        let unmet: Vec<String> = termination
            .map(|assessment| assessment.unmet_goal_requirements.clone())
            .unwrap_or_default();
        let open_questions: Vec<String> = termination
            .map(|assessment| assessment.high_value_open_questions.clone())
            .unwrap_or_default();
        let goal_met =
            termination.is_some_and(|assessment| assessment.goal_satisfied == Some(true));
        let mut directions: Vec<MetacognitionDirection> = Vec::new();
        let mut taken: HashSet<String> = HashSet::new();

        let push = |direction: MetacognitionDirection,
                    directions: &mut Vec<MetacognitionDirection>,
                    taken: &mut HashSet<String>| {
            let key = direction.normalized_hypothesis();
            if !taken.contains(&key) && directions.len() < self.max_directions {
                taken.insert(key);
                directions.push(direction);
            }
        };

        // 类比：每个盲区都是一个未行使面，已产出证据的技术可能迁移。
        let anchor = relevant_covered.first().map_or_else(
            || "this target type".to_string(),
            |domain| domain.as_str().to_string(),
        );
        for spot in blind_spots {
            push(
                analogy_direction(*spot, &anchor),
                &mut directions,
                &mut taken,
            );
        }

        // 反向：目标当前未满足时，一条未审查的路径可能解释它。
        let inversion_material: Option<&String> = unmet.first().or_else(|| open_questions.first());
        if let Some(requirement) = inversion_material {
            push(
                inversion_direction(requirement),
                &mut directions,
                &mut taken,
            );
        }

        if goal_met {
            // 已验证目标满足且无报告缺口：下方的深化框架是为收口**未满足**
            // 目标而存在的，在此触发会使 DONE 结构性不可达——任何审计总
            // 能被更深地探查，而收口链必须能诚实地收口。
            return directions;
        }

        // 极端：已行使域的边界条件可能行为不同。
        if let Some(&domain) = relevant_covered.first() {
            push(extremes_direction(*domain), &mut directions, &mut taken);
        }

        // 组合：合并两个已覆盖域的观察可能暴露单域不可见的路径。
        if relevant_covered.len() >= 2 {
            let (first, second) = (*relevant_covered[0], *relevant_covered[1]);
            push(
                combination_direction(first, second),
                &mut directions,
                &mut taken,
            );
        }

        // 降维：剥离假设重新推导最小路径。
        if !findings.is_empty() || !open_questions.is_empty() {
            push(dimension_reduction_direction(), &mut directions, &mut taken);
        }

        directions
    }
}

/// LLM 发散请求 payload 组装（Python `_assess_with_llm` 的 payload 段）。
///
/// 现有分支假设一并送达——LLM 路径的去重上下文（协议规则 4）。
fn llm_divergence_payload(
    input: &MetacognitionInput<'_>,
    max_directions: usize,
) -> serde_json::Value {
    let termination = input.termination;
    serde_json::json!({
        "mission": {
            "id": input.mission.id.as_str(),
            "goal": input.mission.user_goal,
            "target": input.mission.target,
        },
        "run": {
            "id": input.run.id.as_str(),
            "steps_used": input.run.steps_used,
        },
        "coverage": {
            "relevant_domains": input
                .coverage
                .relevant_domains
                .iter()
                .map(|domain| domain.as_str())
                .collect::<Vec<_>>(),
            "covered_domains": input
                .coverage
                .covered_domains
                .iter()
                .map(|domain| domain.as_str())
                .collect::<Vec<_>>(),
            "blind_spots": input
                .coverage
                .blind_spots
                .iter()
                .map(|domain| domain.as_str())
                .collect::<Vec<_>>(),
        },
        "unmet_goal_requirements": termination
            .map(|assessment| assessment.unmet_goal_requirements.clone())
            .unwrap_or_default(),
        "high_value_open_questions": termination
            .map(|assessment| assessment.high_value_open_questions.clone())
            .unwrap_or_default(),
        "existing_branch_hypotheses": input
            .branches
            .iter()
            .map(|branch| {
                serde_json::json!({
                    "title": branch.title,
                    "status": branch.status.as_str(),
                })
            })
            .collect::<Vec<_>>(),
        "response_contract": {
            "format": "json",
            "root_key": "directions",
            "frameworks": MetacognitionFramework::ALL
                .iter()
                .map(|framework| framework.as_str())
                .collect::<Vec<_>>(),
            "max_directions": max_directions,
        },
    })
}

/// 类比方向：每个盲区都是未行使面，已产出证据的技术可能迁移。
fn analogy_direction(spot: AuditDomain, anchor: &str) -> MetacognitionDirection {
    MetacognitionDirection {
        title: format!("Transfer proven techniques to {}", spot.as_str()),
        hypothesis: format!(
            "The {} surface has not been exercised; techniques that produced \
             evidence in {} may transfer and reveal an exposure there.",
            spot.as_str(),
            anchor
        ),
        rationale: "Analogy framework: the cheapest unexplored direction is the \
                    one whose technique already worked on this Mission."
            .to_string(),
        framework: MetacognitionFramework::Analogy,
        related_blind_spots: vec![spot],
        related_unmet_requirements: Vec::new(),
    }
}

/// 反向方向：假设已覆盖路径无法满足目标，问哪条未审查路径能解释它。
fn inversion_direction(requirement: &str) -> MetacognitionDirection {
    MetacognitionDirection {
        title: "Explain the unsatisfied goal from an unexamined path".to_string(),
        hypothesis: format!(
            "The goal requirement '{requirement}' may remain unsatisfied because \
             the true path lies on a surface or assumption the current branches \
             never examined."
        ),
        rationale: "Inversion framework: assume the covered paths cannot satisfy \
                    the goal, then ask what unexamined path would explain that."
            .to_string(),
        framework: MetacognitionFramework::Inversion,
        related_blind_spots: Vec::new(),
        related_unmet_requirements: vec![requirement.to_string()],
    }
}

/// 极端方向：已行使域的边界条件可能行为不同。
fn extremes_direction(domain: AuditDomain) -> MetacognitionDirection {
    MetacognitionDirection {
        title: format!("Probe boundary conditions of {}", domain.as_str()),
        hypothesis: format!(
            "Boundary conditions of the exercised {} surface (unusual encodings, \
             maximal inputs, edge parameters) may expose handling that ordinary \
             probes did not reach.",
            domain.as_str()
        ),
        rationale: "Extremes framework: normal-range probing confirms typical \
                    behavior, not the behavior at the edges."
            .to_string(),
        framework: MetacognitionFramework::Extremes,
        related_blind_spots: Vec::new(),
        related_unmet_requirements: Vec::new(),
    }
}

/// 组合方向：合并两个已覆盖域的观察可能暴露单域不可见的路径。
fn combination_direction(first: AuditDomain, second: AuditDomain) -> MetacognitionDirection {
    MetacognitionDirection {
        title: format!(
            "Combine {} and {} observations",
            first.as_str(),
            second.as_str()
        ),
        hypothesis: format!(
            "Combining observations from {} and {} may surface a candidate path \
             that neither domain revealed on its own.",
            first.as_str(),
            second.as_str()
        ),
        rationale: "Combination framework: cross-domain joins often expose \
                    trust-boundary violations invisible within one domain."
            .to_string(),
        framework: MetacognitionFramework::Combination,
        related_blind_spots: Vec::new(),
        related_unmet_requirements: Vec::new(),
    }
}

/// 降维方向：剥离假设重新推导最小信任边界路径。
fn dimension_reduction_direction() -> MetacognitionDirection {
    MetacognitionDirection {
        title: "Re-derive the minimal trust-boundary path".to_string(),
        hypothesis: "Stripping the current assumptions about the target down to \
                     its minimal trust boundary may show whether an unexamined \
                     data-flow path exists beneath the recorded observations."
            .to_string(),
        rationale: "Dimension-reduction framework: accumulated assumptions can \
                    hide a simpler explanation of the observed evidence."
            .to_string(),
        framework: MetacognitionFramework::DimensionReduction,
        related_blind_spots: Vec::new(),
        related_unmet_requirements: Vec::new(),
    }
}

/// Python `" ".join(text.lower().split())`：小写化 + 空白规范化。
fn normalized_whitespace_lower(text: &str) -> String {
    text.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `usize` → JSON 数值（集合尺寸远小于 `i64::MAX`，饱和转换即精确转换）。
fn json_count(len: usize) -> serde_json::Value {
    serde_json::Value::from(i64::try_from(len).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::agent::TerminationStatus;
    use models::ids::MissionId;
    use models::ids::ProjectId;
    use models::ids::RunId;

    const PROJECT_ID_VALUE: &str = "proj_test";

    fn project_id() -> ProjectId {
        ProjectId::new(PROJECT_ID_VALUE.to_string())
    }

    /// Python `_mission()`。
    fn mission() -> Mission {
        let mut mission = Mission::new(project_id(), "Assess the target".to_string());
        mission
            .target
            .insert("url".to_string(), "https://example.test".to_string());
        mission
    }

    /// Python `_run(mission_id)`：`max_total_steps=64`。
    fn run() -> AuditRun {
        let mut audit_run = AuditRun::new(project_id());
        audit_run.mission_id = Some(MissionId::new("mission_x".to_string()));
        audit_run.max_total_steps = 64;
        audit_run
    }

    /// Python `_termination(goal_satisfied=…, unmet=…, open_questions=…)`。
    fn termination(
        goal_satisfied: Option<bool>,
        unmet: Vec<String>,
        open_questions: Vec<String>,
    ) -> TerminationAssessment {
        let mut assessment =
            TerminationAssessment::new(project_id(), RunId::new("run_x".to_string()));
        assessment.status = TerminationStatus::Complete;
        assessment.goal_satisfied = goal_satisfied;
        assessment.unmet_goal_requirements = unmet;
        assessment.high_value_open_questions = open_questions;
        assessment
    }

    /// Python `_coverage_for_meta(...)`。
    fn coverage_for_meta(
        blind_spots: Vec<AuditDomain>,
        covered: Vec<AuditDomain>,
        relevant: Vec<AuditDomain>,
    ) -> CoverageAssessment {
        let mut coverage = CoverageAssessment::new(project_id(), RunId::new("run_x".to_string()));
        coverage.mission_id = Some(MissionId::new("mission_x".to_string()));
        coverage.blind_spots = blind_spots;
        coverage.covered_domains = covered;
        coverage.relevant_domains = relevant;
        coverage
    }

    /// Python `StubProviderRuntime`：固定响应。
    struct StubProviderRuntime {
        response: serde_json::Value,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl ProviderRuntime for StubProviderRuntime {
        async fn list_providers(
            &self,
        ) -> Result<Vec<models::provider::ProviderConfig>, crate::llm::ProviderCallError> {
            Ok(Vec::new())
        }

        async fn get_provider(
            &self,
            _provider_id: &str,
        ) -> Result<Option<models::provider::ProviderConfig>, crate::llm::ProviderCallError>
        {
            Ok(None)
        }

        async fn resolve_default_provider(
            &self,
        ) -> Result<Option<models::provider::ProviderConfig>, crate::llm::ProviderCallError>
        {
            Ok(None)
        }

        async fn health_check(
            &self,
            _provider_id: &str,
        ) -> Result<models::provider::ProviderHealthResult, crate::llm::ProviderCallError> {
            Err(crate::llm::ProviderCallError::new(
                "health check is unused in metacognition tests",
            ))
        }

        async fn generate_text(
            &self,
            _request: crate::llm::TextGenerationRequest<'_>,
        ) -> Result<crate::llm::LlmResponse, crate::llm::ProviderCallError> {
            Err(crate::llm::ProviderCallError::new(
                "text generation is unused in metacognition tests",
            ))
        }

        async fn generate_structured(
            &self,
            _request: StructuredGenerationRequest<'_>,
        ) -> Result<serde_json::Map<String, serde_json::Value>, crate::llm::ProviderCallError>
        {
            if self.fail {
                return Err(crate::llm::ProviderCallError::new("provider outage"));
            }
            self.response.as_object().cloned().ok_or_else(|| {
                crate::llm::ProviderCallError::new("stub response must be a JSON object")
            })
        }
    }

    /// Python `test_metacognition_goal_met_without_gaps_yields_no_direction`。
    ///
    /// DONE 必须可达：目标满足 + 全覆盖 + 无待解问题 → 无方向。
    #[tokio::test]
    async fn goal_met_without_gaps_yields_no_direction() {
        let coverage = coverage_for_meta(
            Vec::new(),
            vec![AuditDomain::WebSast, AuditDomain::CodeDeepSast],
            vec![AuditDomain::WebSast, AuditDomain::CodeDeepSast],
        );
        let source_mission = mission();
        let audit_run = run();
        let satisfied = termination(Some(true), Vec::new(), Vec::new());

        let assessment = MetacognitionAgent::new()
            .assess(
                &MetacognitionInput {
                    termination: Some(&satisfied),
                    ..MetacognitionInput::new(&source_mission, &audit_run, &coverage)
                },
                None,
            )
            .await;

        assert_eq!(assessment.mode, MetacognitionMode::Deterministic);
        assert!(assessment.directions.is_empty());
        assert!(assessment.blind_spots_addressed.is_empty());
    }

    /// Python `test_metacognition_blind_spot_veto_survives_satisfied_goal`。
    ///
    /// 目标满足不豁免未行使的相关面——盲区否决存活。
    #[tokio::test]
    async fn blind_spot_veto_survives_satisfied_goal() {
        let coverage = coverage_for_meta(
            vec![AuditDomain::CodeDeepSast],
            vec![AuditDomain::WebSast],
            vec![AuditDomain::WebSast, AuditDomain::CodeDeepSast],
        );
        let source_mission = mission();
        let audit_run = run();
        let satisfied = termination(Some(true), Vec::new(), Vec::new());

        let assessment = MetacognitionAgent::new()
            .assess(
                &MetacognitionInput {
                    termination: Some(&satisfied),
                    ..MetacognitionInput::new(&source_mission, &audit_run, &coverage)
                },
                None,
            )
            .await;

        assert!(!assessment.directions.is_empty(), "盲区必须产出方向");
        assert!(
            assessment
                .directions
                .iter()
                .all(|direction| direction.framework == MetacognitionFramework::Analogy)
        );
        assert_eq!(
            assessment.blind_spots_addressed,
            vec![AuditDomain::CodeDeepSast]
        );
    }

    /// Python `test_metacognition_deepening_frameworks_need_unsatisfied_goal`。
    ///
    /// # 红线语义
    ///
    /// 深化框架（反向/极端/组合）只为收口**未满足**的目标而存在：
    /// 目标已满足时触发会使 DONE 结构性不可达。
    #[tokio::test]
    async fn deepening_frameworks_need_unsatisfied_goal() {
        let coverage = coverage_for_meta(
            Vec::new(),
            vec![AuditDomain::AssetRecon, AuditDomain::WebRecon],
            vec![AuditDomain::AssetRecon, AuditDomain::WebRecon],
        );
        let url_mission = mission();
        let audit_run = run();

        let unsatisfied = MetacognitionAgent::new()
            .assess(
                &MetacognitionInput {
                    termination: Some(&termination(
                        Some(false),
                        vec!["recover the flag".to_string()],
                        Vec::new(),
                    )),
                    ..MetacognitionInput::new(&url_mission, &audit_run, &coverage)
                },
                None,
            )
            .await;
        let frameworks: HashSet<MetacognitionFramework> = unsatisfied
            .directions
            .iter()
            .map(|direction| direction.framework)
            .collect();
        assert!(frameworks.contains(&MetacognitionFramework::Inversion));
        assert!(frameworks.contains(&MetacognitionFramework::Extremes));
        assert!(frameworks.contains(&MetacognitionFramework::Combination));

        let satisfied = MetacognitionAgent::new()
            .assess(
                &MetacognitionInput {
                    termination: Some(&termination(Some(true), Vec::new(), Vec::new())),
                    ..MetacognitionInput::new(&url_mission, &audit_run, &coverage)
                },
                None,
            )
            .await;
        assert!(satisfied.directions.is_empty());
    }

    /// Python `test_metacognition_llm_path_filters_assertive_and_thin_directions`。
    #[tokio::test]
    async fn llm_path_filters_assertive_and_thin_directions() {
        let runtime = StubProviderRuntime {
            response: serde_json::json!({
                "directions": [
                    {
                        "title": "Conjectural direction",
                        "hypothesis": "an untested admin surface may exist behind the \
                                       alternate virtual host and would explain the redirect",
                        "rationale": "analogy with a prior case",
                        "framework": "analogy",
                        "related_blind_spots": ["web_recon"],
                    },
                    {
                        "title": "Assertive direction",
                        "hypothesis": "the login endpoint is definitely vulnerable to \
                                       authentication bypass via the reset flow",
                        "framework": "inversion",
                    },
                    {
                        "title": "Too short",
                        "hypothesis": "maybe something",
                        "framework": "extremes",
                    },
                    {
                        "title": "Duplicate of first",
                        "hypothesis": "an untested admin surface may exist behind the \
                                       alternate virtual host and would explain the redirect",
                        "framework": "combination",
                    },
                ]
            }),
            fail: false,
        };
        let coverage = coverage_for_meta(
            Vec::new(),
            vec![AuditDomain::WebRecon],
            vec![AuditDomain::WebRecon],
        );
        let url_mission = mission();
        let audit_run = run();
        let satisfied = termination(Some(true), Vec::new(), Vec::new());

        let assessment = MetacognitionAgent::new()
            .assess(
                &MetacognitionInput {
                    termination: Some(&satisfied),
                    ..MetacognitionInput::new(&url_mission, &audit_run, &coverage)
                },
                Some(LlmDivergenceContext {
                    runtime: &runtime,
                    provider_id: Some("prov_x"),
                }),
            )
            .await;

        assert_eq!(assessment.mode, MetacognitionMode::Llm);
        assert_eq!(assessment.directions.len(), 1);
        assert_eq!(assessment.directions[0].title, "Conjectural direction");
        assert_eq!(
            assessment.directions[0].related_blind_spots,
            vec![AuditDomain::WebRecon]
        );
    }

    /// Python `test_metacognition_provider_failure_falls_back_deterministic`。
    #[tokio::test]
    async fn provider_failure_falls_back_deterministic() {
        let runtime = StubProviderRuntime {
            response: serde_json::Value::Null,
            fail: true,
        };
        let coverage = coverage_for_meta(
            vec![AuditDomain::ContentDiscovery],
            Vec::new(),
            vec![AuditDomain::ContentDiscovery],
        );
        let url_mission = mission();
        let audit_run = run();
        let satisfied = termination(Some(true), Vec::new(), Vec::new());

        let assessment = MetacognitionAgent::new()
            .assess(
                &MetacognitionInput {
                    termination: Some(&satisfied),
                    ..MetacognitionInput::new(&url_mission, &audit_run, &coverage)
                },
                Some(LlmDivergenceContext {
                    runtime: &runtime,
                    provider_id: Some("prov_x"),
                }),
            )
            .await;

        assert_eq!(assessment.mode, MetacognitionMode::Deterministic);
        assert!(!assessment.directions.is_empty());
        assert!(
            assessment
                .notes
                .iter()
                .any(|note| note.contains("falling back"))
        );
    }
}
