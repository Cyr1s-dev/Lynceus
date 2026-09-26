//! Trajectory Summarizer：带 token 压力监控的滚动上下文压缩 ——
//! `server/core/agents/trajectory.py` 的移植。
//!
//! `HackSynth` 把规划器与摘要器配对，让长探索保持有限上下文内。这就是那个
//! 摘要器，并且与 agent 层其余部分一样是确定性的——模型写的轨迹摘要只是
//! 又一个幻觉可能进入证据路径的入口。
//!
//! 压缩刻意有损，但不均匀。进度闲聊被折叠成计数；而后来的 worker 为不
//! 重犯错过的错所需要的东西——失败边界、证据缺口、矛盾、开放问题——逐字
//! 前传。Finding 与 Evidence 按 id 引用而非复述，因此摘要永远不会变成
//! 证据链的第二份分叉副本。

use models::agent::Observation;
use models::agent::ObservationType;
use models::ids::BranchId;
use models::ids::MissionId;
use models::ids::ProjectId;
use models::ids::RunId;
use models::ids::TaskId;
use models::tool_invocation::ToolInvocation;
use models::trajectory::TokenPressure;
use models::trajectory::TrajectorySummary;
use serde_json::Value;

/// 每 token 字符数（Python 模块常量）。
///
/// 粗略的英文/代码平均值，只用于决定何时升高压力——绝不用于计费或硬截断。
pub const CHARS_PER_TOKEN: usize = 4;

/// 逐字前传的观察类型上限默认值（Python 模块常量）。
pub const DEFAULT_MAX_VERBATIM: usize = 8;

/// 逐字前传文本的观察类型（Python `VERBATIM_TYPES`）。
///
/// 丢失它们就是 agent 重付一次已付过的失败学费的方式。
const VERBATIM_TYPES: [ObservationType; 4] = [
    ObservationType::FailureBoundary,
    ObservationType::Blockage,
    ObservationType::Contradiction,
    ObservationType::ToolFailure,
];

/// 记为开放问题的观察类型（Python `OPEN_QUESTION_TYPES`）。
const OPEN_QUESTION_TYPES: [ObservationType; 2] =
    [ObservationType::EvidenceGap, ObservationType::Hypothesis];

/// 从文本长度估计 token 数（`estimate_tokens`）。
///
/// 刻意的近似：精确分词需要 provider 专属分词器，而 core 不得依赖它。
/// Python `len(text)` 按码点计数，Rust 侧以 `chars().count()` 镜像
/// （`str::len()` 是字节数，中文输入下两者不等）。
#[must_use]
pub fn estimate_tokens(text: &str) -> i64 {
    if text.is_empty() {
        return 0;
    }
    let char_count = text.chars().count();
    i64::try_from(char_count / CHARS_PER_TOKEN)
        .unwrap_or(i64::MAX)
        .max(1)
}

/// 一次摘要的输入（Python `summarize` 的 keyword-only 参数镜像）。
pub struct SummarizeInput<'a> {
    /// 所属 Project。
    pub project_id: &'a ProjectId,
    /// 所属 Run。
    pub run_id: &'a RunId,
    /// 自上次摘要以来的新观察。
    pub observations: &'a [Observation],
    /// 自上次摘要以来的新工具调用。
    pub tool_invocations: &'a [ToolInvocation],
    /// 前一段摘要（存在时其叙述被折叠而非重算）。
    pub previous: Option<&'a TrajectorySummary>,
    /// 所属 Mission。
    pub mission_id: Option<&'a MissionId>,
    /// 所属 Branch。
    pub branch_id: Option<&'a BranchId>,
    /// 关联 Task。
    pub task_id: Option<&'a TaskId>,
    /// 本段 token 预算覆盖（`None` 或 0 = 用构造期预算，Python falsy 语义）。
    pub token_budget: Option<i64>,
}

impl<'a> SummarizeInput<'a> {
    /// 以必填参数构造，其余取 Python 默认值。
    #[must_use]
    pub fn new(
        project_id: &'a ProjectId,
        run_id: &'a RunId,
        observations: &'a [Observation],
    ) -> Self {
        Self {
            project_id,
            run_id,
            observations,
            tool_invocations: &[],
            previous: None,
            mission_id: None,
            branch_id: None,
            task_id: None,
            token_budget: None,
        }
    }

    /// 指定新工具调用。
    #[must_use]
    pub fn tool_invocations(mut self, value: &'a [ToolInvocation]) -> Self {
        self.tool_invocations = value;
        self
    }

    /// 指定前一段摘要。
    #[must_use]
    pub fn previous(mut self, value: &'a TrajectorySummary) -> Self {
        self.previous = Some(value);
        self
    }

    /// 指定所属 Branch。
    #[must_use]
    pub fn branch(mut self, value: &'a BranchId) -> Self {
        self.branch_id = Some(value);
        self
    }

    /// 指定本段 token 预算覆盖。
    #[must_use]
    pub fn token_budget(mut self, value: Option<i64>) -> Self {
        self.token_budget = value;
        self
    }
}

/// 把一段轨迹折叠进滚动、预算感知的摘要。
#[derive(Debug, Clone)]
pub struct TrajectorySummarizer {
    token_budget: i64,
    max_verbatim: usize,
}

impl Default for TrajectorySummarizer {
    fn default() -> Self {
        Self::new()
    }
}

impl TrajectorySummarizer {
    /// 构造器（Python 默认 `token_budget=2048`、`max_verbatim_entries=8`，
    /// 两者均经 `max(1, …)` 下限钳制）。
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(2048, DEFAULT_MAX_VERBATIM)
    }

    /// 指定预算与逐字上限构造。
    #[must_use]
    pub fn with_limits(token_budget: i64, max_verbatim_entries: usize) -> Self {
        Self {
            token_budget: token_budget.max(1),
            max_verbatim: max_verbatim_entries.max(1),
        }
    }

    /// 产出下一段滚动摘要（`summarize`）。
    ///
    /// `observations` / `tool_invocations` 是自 `previous` 以来的**新**步；
    /// 前一摘要的叙述被折叠进来而非重算——这正是摘要成本随轨迹增长保持
    /// 常数的原因。
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // token 计数远小于 2^53，`as f64` 无精度损失。
    pub fn summarize(&self, input: &SummarizeInput<'_>) -> TrajectorySummary {
        let budget = match input.token_budget {
            Some(value) if value != 0 => value.max(1),
            _ => self.token_budget,
        };

        let previous = input.previous;

        let (verbatim, open_questions, new_key_observations) =
            classify_observations(input.observations);
        let failed_tools = failed_tool_notes(input.tool_invocations);

        let key_observations = self.cap(
            previous
                .map(|item| item.key_observations.iter())
                .unwrap_or_default()
                .chain(new_key_observations.iter())
                .cloned()
                .collect(),
        );
        let failure_boundaries = self.cap(
            previous
                .map(|item| item.failure_boundaries.iter())
                .unwrap_or_default()
                .chain(verbatim.iter())
                .chain(failed_tools.iter())
                .cloned()
                .collect(),
        );
        let open_questions = self.cap(
            previous
                .map(|item| item.open_questions.iter())
                .unwrap_or_default()
                .chain(open_questions.iter())
                .cloned()
                .collect(),
        );
        let narrative = Self::render(
            previous,
            input.observations,
            input.tool_invocations,
            &key_observations,
            &failure_boundaries,
            &open_questions,
        );

        let raw_tokens = Self::raw_tokens(previous, input.observations, input.tool_invocations);
        let summary_tokens = estimate_tokens(&narrative);
        // 压力度量的是"不做压缩就得回放"的轨迹；摘要大小另行作为压缩后
        // 的实际上下文成本跟踪。
        let ratio = raw_tokens as f64 / budget as f64;
        let cumulative = previous.map_or(0, |item| item.cumulative_step_count)
            + i64::try_from(input.observations.len()).unwrap_or(i64::MAX);
        let new_evidence_ids = collect_ids(input.observations, |obs| &obs.related_evidence_ids);
        let new_finding_ids = collect_ids(input.observations, |obs| &obs.related_finding_ids);
        let evidence_ids = merge_sorted(
            previous
                .map(|item| item.evidence_ids.iter())
                .unwrap_or_default(),
            new_evidence_ids.into_iter(),
        );
        let finding_ids = merge_sorted(
            previous
                .map(|item| item.finding_ids.iter())
                .unwrap_or_default(),
            new_finding_ids.into_iter(),
        );
        let tool_ids = merge_sorted(
            previous
                .map(|item| item.tool_invocation_ids.iter())
                .unwrap_or_default(),
            input
                .tool_invocations
                .iter()
                .map(|tool| tool.id.as_str().to_string()),
        );

        let mut summary =
            TrajectorySummary::new(input.project_id.clone(), input.run_id.clone(), narrative);
        summary.mission_id = input.mission_id.cloned();
        summary.branch_id = input.branch_id.cloned();
        summary.task_id = input.task_id.cloned();
        summary.segment_index = previous.map_or(0, |item| item.segment_index + 1);
        summary.previous_summary_id = previous.map(|item| item.id.clone());
        summary.covered_step_count = i64::try_from(input.observations.len()).unwrap_or(i64::MAX);
        summary.cumulative_step_count = cumulative;
        summary.key_observations = key_observations;
        summary.failure_boundaries = failure_boundaries;
        summary.open_questions = open_questions;
        summary.evidence_ids = evidence_ids;
        summary.finding_ids = finding_ids;
        summary.tool_invocation_ids = tool_ids;
        summary.raw_tokens = raw_tokens;
        summary.summary_tokens = summary_tokens;
        summary.token_budget = budget;
        summary.pressure_ratio = py_round(ratio, 4);
        summary.pressure = TokenPressure::from_ratio(ratio);
        summary
    }

    /// 渲染摘要正文（`_render`）。
    fn render(
        previous: Option<&TrajectorySummary>,
        observations: &[Observation],
        tool_invocations: &[ToolInvocation],
        key_observations: &[String],
        failure_boundaries: &[String],
        open_questions: &[String],
    ) -> String {
        let mut lines: Vec<String> = Vec::new();
        if let Some(previous) = previous {
            lines.push(format!(
                "Prior trajectory: {} step(s) across {} summarized segment(s).",
                previous.cumulative_step_count,
                previous.segment_index + 1
            ));
        }

        // Counter(obs.observation_type.value)：按 wire 值聚合计数再按名排序。
        let mut by_type: Vec<(String, usize)> = Vec::new();
        for obs in observations {
            let name = obs.observation_type.as_str();
            match by_type.iter_mut().find(|(existing, _)| existing == name) {
                Some((_, count)) => *count += 1,
                None => by_type.push((name.to_string(), 1)),
            }
        }
        by_type.sort();
        if by_type.is_empty() {
            lines.push("This segment: no new observations.".to_string());
        } else {
            let breakdown = by_type
                .iter()
                .map(|(name, count)| format!("{count}x {name}"))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "This segment: {} observation(s) — {breakdown}.",
                observations.len()
            ));
        }

        if !tool_invocations.is_empty() {
            let ok = tool_invocations
                .iter()
                .filter(|tool| tool.status == models::lifecycle::ToolStatus::Ok)
                .count();
            lines.push(format!(
                "Tools: {} invocation(s), {ok} ok, {} not ok.",
                tool_invocations.len(),
                tool_invocations.len() - ok
            ));
        }

        if !key_observations.is_empty() {
            lines.push("Key observations:".to_string());
            lines.extend(key_observations.iter().map(|item| format!("- {item}")));
        }
        if !failure_boundaries.is_empty() {
            lines.push("Failure boundaries:".to_string());
            lines.extend(failure_boundaries.iter().map(|item| format!("- {item}")));
        }
        if !open_questions.is_empty() {
            lines.push("Open questions:".to_string());
            lines.extend(open_questions.iter().map(|item| format!("- {item}")));
        }

        lines.join("\n")
    }

    /// 去重、保序、至多保留 `max_verbatim` 项（`_cap`）。
    fn cap(&self, mut values: Vec<String>) -> Vec<String> {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut result: Vec<String> = Vec::new();
        for value in values.drain(..) {
            if !seen.insert(value.clone()) {
                continue;
            }
            result.push(value);
            if result.len() >= self.max_verbatim {
                break;
            }
        }
        result
    }

    /// 估计未压缩轨迹将花费的 token 数（`_raw_tokens`）。
    fn raw_tokens(
        previous: Option<&TrajectorySummary>,
        observations: &[Observation],
        tool_invocations: &[ToolInvocation],
    ) -> i64 {
        let mut total = previous.map_or(0, |item| item.raw_tokens);
        for obs in observations {
            total += estimate_tokens(&obs.summary) + estimate_tokens(&py_repr_map(&obs.data));
        }
        for tool in tool_invocations {
            total += estimate_tokens(&tool.input_summary);
            total += estimate_tokens(&tool.output_summary);
            total += tool.error.as_deref().map_or(0, estimate_tokens);
        }
        total
    }
}

/// 把新观察分拣进逐字前传 / 开放问题 / 关键观察三类（`summarize` 内联循环
/// 的拆分）。
///
/// 返回 `(verbatim, open_questions, key_observations)`；空白摘要直接跳过。
fn classify_observations(observations: &[Observation]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut verbatim: Vec<String> = Vec::new();
    let mut open_questions: Vec<String> = Vec::new();
    let mut key_observations: Vec<String> = Vec::new();
    for obs in observations {
        let stripped = obs.summary.trim();
        if stripped.is_empty() {
            continue;
        }
        let observation_type = obs.observation_type;
        if VERBATIM_TYPES.contains(&observation_type) {
            verbatim.push(format!("{}: {stripped}", observation_type.as_str()));
        }
        if OPEN_QUESTION_TYPES.contains(&observation_type) {
            open_questions.push(format!("{}: {stripped}", observation_type.as_str()));
        }
        if matches!(
            observation_type,
            ObservationType::ToolResult | ObservationType::Decision
        ) {
            key_observations.push(stripped.to_string());
        }
    }
    (verbatim, open_questions, key_observations)
}

/// 未成功工具调用的"名称: 原因"注记列表（`summarize` 内联循环的拆分）。
fn failed_tool_notes(tool_invocations: &[ToolInvocation]) -> Vec<String> {
    let mut failed_tools: Vec<String> = Vec::new();
    for tool in tool_invocations {
        if tool.status == models::lifecycle::ToolStatus::Ok {
            continue;
        }
        let reason = match tool.error.as_deref() {
            Some(value) if !value.is_empty() => value.to_string(),
            _ => tool.status.as_str().to_string(),
        };
        failed_tools.push(format!("{}: {reason}", tool.tool_name));
    }
    failed_tools
}

/// Python `sorted(set(previous) | new)`：并集去重后字典序排序。
///
/// UTF-8 字节序与码点序一致，因此 Rust `sort()` 与 Python `sorted()` 对
/// 字符串集合产出相同顺序。
fn merge_sorted<'a, I1, I2>(previous: I1, new: I2) -> Vec<String>
where
    I1: Iterator<Item = &'a String>,
    I2: Iterator<Item = String>,
{
    let mut merged: Vec<String> = previous.cloned().collect();
    merged.extend(new);
    merged.sort();
    merged.dedup();
    merged
}

/// Python `_collect`：把观察的关联 id 列表收集进集合再排序。
fn collect_ids(
    observations: &[Observation],
    selector: fn(&Observation) -> &Vec<String>,
) -> Vec<String> {
    let mut collected: Vec<String> = Vec::new();
    for obs in observations {
        collected.extend(selector(obs).iter().cloned());
    }
    collected.sort();
    collected.dedup();
    collected
}

/// Python `round(value, digits)`：十进制正确舍入（银行家舍入）。
///
/// `format!` 的定点格式化对同一二进制值做与 `CPython` `_Py_dg_dtoa` 一致的
/// 最近舍入（半值取偶），因此经字符串往返可镜像 Python 结果。
fn py_round(value: f64, digits: usize) -> f64 {
    let formatted = format!("{value:.digits$}");
    formatted.parse().unwrap_or(value)
}

/// Python `str(dict)` 的镜像：`{'k': 'v', 'n': 1}`（插入序、单引号）。
///
/// 仅用于 token 估算的 `str(obs.data)`——字符数进入 `raw_tokens`，与
/// Python 侧的估计保持逐字符一致才能让压力比值不漂移。
fn py_repr_map(map: &serde_json::Map<String, Value>) -> String {
    let items = map
        .iter()
        .map(|(key, value)| format!("{}: {}", py_repr_str(key), py_repr_value(value)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{items}}}")
}

/// Python `repr(value)` 的 JSON 值镜像。
fn py_repr_value(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(flag) => {
            if *flag {
                "True".to_string()
            } else {
                "False".to_string()
            }
        }
        Value::Number(number) => {
            if let Some(int) = number.as_i64() {
                int.to_string()
            } else if let Some(uint) = number.as_u64() {
                uint.to_string()
            } else {
                let float = number.as_f64().unwrap_or(0.0);
                py_repr_float(float)
            }
        }
        Value::String(text) => py_repr_str(text),
        Value::Array(items) => {
            let inner = items
                .iter()
                .map(py_repr_value)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{inner}]")
        }
        Value::Object(map) => py_repr_map(map),
    }
}

/// Python `repr(float)`：短往返表示。
#[allow(clippy::float_cmp)] // 整值判定需要位级相等，比较对象是同一变量
fn py_repr_float(value: f64) -> String {
    if value == value.trunc() && value.abs() < 1e16 {
        format!("{value:.1}")
    } else {
        format!("{value}")
    }
}

/// Python `repr(str)`：单引号偏好 + 常见转义。
fn py_repr_str(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::lifecycle::ToolStatus;
    use models::trajectory::TokenPressure;
    use serde_json::json;

    fn project_id() -> ProjectId {
        ProjectId::new("project_1".to_string())
    }

    fn run_id() -> RunId {
        RunId::new("run_1".to_string())
    }

    fn branch_id() -> BranchId {
        BranchId::new("branch_1".to_string())
    }

    fn observation(observation_type: ObservationType, summary: &str) -> Observation {
        let mut obs = Observation::new(project_id(), run_id(), summary.to_string());
        obs.branch_id = Some(branch_id());
        obs.observation_type = observation_type;
        obs
    }

    fn failed_tool() -> ToolInvocation {
        let mut tool = ToolInvocation::new("probe".to_string(), "x".repeat(80));
        tool.id = models::ids::ToolInvocationId::new("tool_1".to_string());
        tool.status = ToolStatus::Denied;
        tool.error = Some("policy denied".to_string());
        tool
    }

    #[test]
    fn estimate_tokens_counts_chars_not_bytes() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abc"), 1);
        // 中文按码点计数：Python len("审计项") == 3 → max(1, 0) == 1。
        assert_eq!(estimate_tokens("审计项"), 1);
        // len == 6 → 6 // 4 == 1（按字节则是 18 // 4 == 4）。
        assert_eq!(estimate_tokens("审计审计审计"), 1);
    }

    #[test]
    fn py_repr_map_mirrors_python_str_dict() {
        let mut data = serde_json::Map::new();
        data.insert("body".to_string(), json!("x".repeat(100)));
        assert_eq!(py_repr_map(&data).chars().count(), 112);
        let mixed = serde_json::json!({"a": 1, "b": null, "c": true, "d": "it's"})
            .as_object()
            .cloned();
        let map = mixed.unwrap_or_default();
        assert_eq!(
            py_repr_map(&map),
            "{'a': 1, 'b': None, 'c': True, 'd': 'it\\'s'}"
        );
    }

    #[test]
    fn summarizer_rolls_forward_boundaries_and_pressure() {
        let mut first_obs = observation(ObservationType::ToolResult, "Candidate route discovered");
        first_obs
            .data
            .insert("body".to_string(), json!("x".repeat(100)));
        first_obs.related_evidence_ids = vec!["evd_1".to_string()];
        first_obs.related_finding_ids = vec!["finding_1".to_string()];
        let boundary = observation(
            ObservationType::FailureBoundary,
            "Do not retry the denied endpoint",
        );

        let summarizer = TrajectorySummarizer::with_limits(20, DEFAULT_MAX_VERBATIM);
        let first = summarizer.summarize(
            &SummarizeInput::new(&project_id(), &run_id(), &[first_obs, boundary])
                .tool_invocations(&[failed_tool()]),
        );
        assert_eq!(first.pressure, TokenPressure::Critical);
        assert_eq!(first.evidence_ids, ["evd_1".to_string()]);
        assert_eq!(first.finding_ids, ["finding_1".to_string()]);
        assert!(
            first
                .failure_boundaries
                .iter()
                .any(|item| item.contains("Do not retry"))
        );
        assert_eq!(first.segment_index, 0);
        assert_eq!(first.covered_step_count, 2);
        assert_eq!(first.cumulative_step_count, 2);
        assert_eq!(first.token_budget, 20);

        let decision = observation(ObservationType::Decision, "Switch to static validation");
        let second = summarizer.summarize(
            &SummarizeInput::new(&project_id(), &run_id(), &[decision]).previous(&first),
        );
        assert_eq!(second.previous_summary_id, Some(first.id.clone()));
        assert_eq!(second.segment_index, 1);
        assert_eq!(second.cumulative_step_count, 3);
        assert!(
            second
                .failure_boundaries
                .iter()
                .any(|item| item.contains("Do not retry"))
        );
        assert!(
            second
                .key_observations
                .iter()
                .any(|item| item == "Switch to static validation")
        );
        assert!(!second.summary.contains(&first.summary));
    }

    #[test]
    fn summarizer_renders_python_shaped_narrative() {
        let obs = observation(ObservationType::Decision, "pick plan b");
        let summarizer = TrajectorySummarizer::new();
        let summary = summarizer.summarize(&SummarizeInput::new(&project_id(), &run_id(), &[obs]));
        assert_eq!(
            summary.summary,
            "This segment: 1 observation(s) — 1x decision.\nKey observations:\n- pick plan b"
        );
        assert_eq!(summary.created_by, "trajectory_summarizer");
    }

    #[test]
    fn summarizer_caps_verbatim_entries_and_dedupes() {
        let observations: Vec<Observation> = (0..10)
            .map(|_| observation(ObservationType::Blockage, "same blockage"))
            .collect();
        let summarizer = TrajectorySummarizer::with_limits(2048, 8);
        let summary = summarizer.summarize(&SummarizeInput::new(
            &project_id(),
            &run_id(),
            &observations,
        ));
        assert_eq!(
            summary.failure_boundaries,
            ["blockage: same blockage".to_string()]
        );
        let many: Vec<Observation> = (0..10)
            .map(|index| observation(ObservationType::Blockage, &format!("blockage {index}")))
            .collect();
        let capped = summarizer.summarize(&SummarizeInput::new(&project_id(), &run_id(), &many));
        assert_eq!(capped.failure_boundaries.len(), 8);
    }

    #[test]
    fn summarizer_collects_and_sorts_related_ids() {
        let mut obs_a = observation(ObservationType::ToolResult, "a");
        obs_a.related_evidence_ids = vec!["evd_2".to_string(), "evd_1".to_string()];
        let mut obs_b = observation(ObservationType::ToolResult, "b");
        obs_b.related_evidence_ids = vec!["evd_1".to_string(), "evd_3".to_string()];
        obs_b.related_finding_ids = vec!["finding_1".to_string()];
        let summarizer = TrajectorySummarizer::new();
        let summary = summarizer.summarize(&SummarizeInput::new(
            &project_id(),
            &run_id(),
            &[obs_a, obs_b],
        ));
        assert_eq!(
            summary.evidence_ids,
            [
                "evd_1".to_string(),
                "evd_2".to_string(),
                "evd_3".to_string()
            ]
        );
        assert_eq!(summary.finding_ids, ["finding_1".to_string()]);
        assert!(summary.tool_invocation_ids.is_empty());
    }

    #[test]
    fn summarizer_uses_zero_budget_fallback_and_clamps_minimums() {
        let summarizer = TrajectorySummarizer::with_limits(0, 0);
        assert_eq!(summarizer.token_budget, 1);
        assert_eq!(summarizer.max_verbatim, 1);
        let obs = observation(ObservationType::Progress, "step");
        let default_budget = summarizer.summarize(
            &SummarizeInput::new(&project_id(), &run_id(), std::slice::from_ref(&obs))
                .token_budget(None),
        );
        assert_eq!(default_budget.token_budget, 1);
        let overridden = summarizer.summarize(
            &SummarizeInput::new(&project_id(), &run_id(), &[obs]).token_budget(Some(0)),
        );
        assert_eq!(overridden.token_budget, 1);
    }

    #[test]
    #[allow(clippy::float_cmp)] // 银行家舍入的预期是精确字面量，位级相等即语义相等
    fn py_round_mirrors_python_bankers_rounding() {
        assert_eq!(py_round(0.5, 0), 0.0);
        assert_eq!(py_round(1.5, 0), 2.0);
        assert_eq!(py_round(2.5, 0), 2.0);
        assert_eq!(py_round(0.123_449, 4), 0.1234);
        assert_eq!(py_round(0.123_456, 4), 0.1235);
    }
}
