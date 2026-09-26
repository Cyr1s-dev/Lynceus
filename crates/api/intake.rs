//! 自然语言 intake —— `server/api/intake.py` + `api/schemas.py`
//! intake DTO 群的 Rust 镜像。
//!
//! 红线：intake 层可以让配置的 provider 把原始输入分析成经过校验的计划，
//! 但**绝不允许模型输出改写 raw query、写状态或执行工具**；状态变更一律
//! 经 [`runtime::AuditManager`]。provider/config/校验任何失败都
//! 降级到确定性分类器，intake 永不阻塞。

// 本模块先把文本统一 lower 再做扩展名比较；clippy 的
// case_sensitive_file_extension_comparisons 无法识别这一前置归一，
// 属于误报，按规范整模块豁免。
#![allow(clippy::case_sensitive_file_extension_comparisons)]

use std::collections::HashSet;
use std::sync::Arc;

use agents::branch_generator::{BranchGenerationInput, BranchGenerator};
use agents::llm::LlmMessage;
use models::{
    AuditDomain, AuditRun, Branch, GoalContractSource, GoalContractStatus, Intent,
    MaxIntrusiveness, Mission, MissionGoalContract, Project, RawInputEnvelope, RunStatus,
    StructuredConstraintContract,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::mission_workspace;

/// Python `INTAKE_PURPOSE`。
pub const INTAKE_PURPOSE: &str = "natural_language_intake";

// ---------------------------------------------------------------------------
// DTO（IntakeApiModel extra="forbid" 镜像）
// ---------------------------------------------------------------------------

/// 项目草稿（Python `IntakeProjectDraft`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntakeProjectDraft {
    /// 展示名（模型摘要或确定性标题）。
    pub name: String,
    /// 审计域。
    pub audit_domain: AuditDomain,
    /// 描述。
    #[serde(default)]
    pub description: Option<String>,
    /// 目标键值（字符串值约束）。
    #[serde(default)]
    pub target: Map<String, Value>,
    /// 目标。
    #[serde(default = "default_project_goal")]
    pub goal: String,
}

fn default_project_goal() -> String {
    "Find high-confidence vulnerabilities with reproducible evidence".to_string()
}

/// 流水线草稿（Python `IntakePipelineDraft`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntakePipelineDraft {
    /// 画像名。
    #[serde(default = "default_profile")]
    pub profile: String,
    /// 审计域列表。
    #[serde(default)]
    pub audit_domains: Vec<AuditDomain>,
    /// 域配置（不含命令与工具执行）。
    #[serde(default)]
    pub config: Map<String, Value>,
    /// 启动方式。
    #[serde(default)]
    pub start_mode: String,
}

fn default_profile() -> String {
    "web_full".to_string()
}

/// Mission 草稿（Python `IntakeMissionDraft`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntakeMissionDraft {
    /// 用户目标原文。
    pub user_goal: String,
    /// 目标键值。
    #[serde(default)]
    pub target: Map<String, Value>,
    /// 约束。
    #[serde(default)]
    pub constraints: Vec<String>,
    /// 成功判据。
    #[serde(default)]
    pub success_criteria: Vec<String>,
    /// 完成契约。
    #[serde(default)]
    pub goal_contract: MissionGoalContract,
    /// 项目草稿。
    pub project: IntakeProjectDraft,
    /// 流水线草稿。
    pub pipeline: IntakePipelineDraft,
    /// 元数据。
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

/// 分支提示（Python `IntakeBranchHint`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntakeBranchHint {
    /// 分支标题。
    pub title: String,
    /// 假设。
    pub hypothesis: String,
    /// 依据。
    #[serde(default)]
    pub rationale: String,
    /// 分支类别。
    #[serde(default)]
    pub branch_kind: Option<String>,
    /// 优先级。
    #[serde(default = "default_priority")]
    pub priority: i64,
    /// 置信度。
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    /// 审计域。
    #[serde(default)]
    pub audit_domains: Vec<AuditDomain>,
}

fn default_priority() -> i64 {
    50
}

fn default_confidence() -> f64 {
    0.5
}

/// 计划（Python `IntakePlan`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntakePlan {
    /// 项目草稿。
    pub project: IntakeProjectDraft,
    /// 流水线草稿。
    pub pipeline: IntakePipelineDraft,
    /// 完成契约。
    #[serde(default)]
    pub goal_contract: MissionGoalContract,
    /// 推荐意图。
    #[serde(default)]
    pub recommended_intents: Vec<String>,
    /// 引用工件 id。
    #[serde(default)]
    pub artifact_record_ids: Vec<String>,
    /// 工件摘要。
    #[serde(default)]
    pub artifacts_summary: Vec<Map<String, Value>>,
    /// 置信度。
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    /// 依据。
    #[serde(default)]
    pub rationale: Option<String>,
    /// 元数据。
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

/// 分析请求（Python `IntakeAnalyzeRequest`）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntakeAnalyzeRequest {
    /// 原始输入（不翻译不改写）。
    pub prompt: String,
    /// 指定 provider。
    #[serde(default)]
    pub provider_id: Option<String>,
    /// 引用工件。
    #[serde(default)]
    pub artifact_record_ids: Vec<String>,
}

/// 分析响应（Python `IntakeAnalyzeResponse`）。
#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IntakeAnalyzeResponse {
    /// 原始输入信封。
    pub raw_input: RawInputEnvelope,
    /// 计划。
    pub plan: IntakePlan,
    /// Mission 草稿。
    pub mission_draft: IntakeMissionDraft,
    /// 目标键值。
    pub target: Map<String, Value>,
    /// 约束。
    pub constraints: Vec<String>,
    /// 成功判据。
    pub success_criteria: Vec<String>,
    /// 推荐审计域。
    pub recommended_audit_domains: Vec<AuditDomain>,
    /// 推荐分支提示。
    pub recommended_branch_hints: Vec<IntakeBranchHint>,
    /// 建议分支（与 hints 同值的历史别名）。
    pub suggested_branches: Vec<IntakeBranchHint>,
    /// 置信度。
    pub confidence: f64,
    /// 依据。
    pub rationale: Option<String>,
    /// Provider。
    pub provider_id: Option<String>,
    /// 模型调用审计 id。
    pub model_invocation_id: Option<String>,
    /// 是否走了确定性回退。
    pub used_fallback: bool,
    /// 回退原因。
    pub fallback_reason: Option<String>,
}

fn default_true() -> bool {
    true
}

/// 启动请求（Python `IntakeStartRequest`）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntakeStartRequest {
    /// 已确认的计划。
    pub plan: IntakePlan,
    /// 复用既有 Mission。
    #[serde(default)]
    pub mission_id: Option<String>,
    /// 追加工件 id。
    #[serde(default)]
    pub artifact_record_ids: Vec<String>,
    /// 是否启动流水线。
    #[serde(default = "default_true")]
    pub start_pipeline: bool,
    /// 是否启动审计。
    #[serde(default)]
    pub start_audit: bool,
}

/// 异步 intake 请求：提交原始问题即返回草稿 Mission，模型分析与
/// 派发在后台完成（前端立即进入任务页等结果）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntakeAsyncRequest {
    /// 原始用户输入（不翻译不改写）。
    pub prompt: String,
    /// 指定 provider（缺省按 `natural_language_intake` 路由）。
    #[serde(default)]
    pub provider_id: Option<String>,
    /// 引用工件 id。
    #[serde(default)]
    pub artifact_record_ids: Vec<String>,
}

/// 异步 intake 响应：立即返回的草稿 Mission（后台完成后转为 running）。
#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IntakeAsyncResponse {
    /// 草稿 Mission（`draft` 状态；后台分析完成后自动进入 running）。
    pub mission: Mission,
}

/// 启动响应（Python `IntakeStartResponse`）。
#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IntakeStartResponse {
    /// 项目。
    pub project: Project,
    /// Mission（quick_todo 模式为空）。
    pub mission: Option<Mission>,
    /// 计划。
    pub plan: IntakePlan,
    /// 意图。
    #[serde(default)]
    pub intents: Vec<Intent>,
    /// 运行。
    pub run: Option<AuditRun>,
    /// 分支。
    #[serde(default)]
    pub branches: Vec<Branch>,
    /// 创建的意图。
    #[serde(default)]
    pub created_intents: Vec<Intent>,
    /// 创建的运行。
    pub created_run: Option<AuditRun>,
    /// 流水线状态。
    pub pipeline_status: Map<String, Value>,
}

// ---------------------------------------------------------------------------
// 文本分析（regex + 后过滤镜像 Python lookaround 语义）
// ---------------------------------------------------------------------------
//
// 正则模式全部为静态字面量，编译期即可人工验证恒可编译；`expect` 是
// 对这一不变式的表达，不是对运行期输入的假设。

#[allow(clippy::expect_used)]
fn url_pattern() -> &'static regex::Regex {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(r#"(?i)https?://[^\s<>'\")\]]+"#).expect("URL 正则必须可编译")
    })
}

#[allow(clippy::expect_used)]
fn windows_path_pattern() -> &'static regex::Regex {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(r#"\b[A-Za-z]:[\\/][^\s<>'\"]+"#).expect("Windows 路径正则必须可编译")
    })
}

#[allow(clippy::expect_used)]
fn posix_path_pattern() -> &'static regex::Regex {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(r"(?:~|\.{1,2}|/)[\w .@~+/\\:-]+").expect("POSIX 路径正则必须可编译")
    })
}

#[allow(clippy::expect_used)]
fn domain_pattern() -> &'static regex::Regex {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PATTERN.get_or_init(|| {
        regex::Regex::new(r"(?i)\b(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+(?:[a-z]{2,63})\b")
            .expect("域名正则必须可编译")
    })
}

const BINARY_EXTENSIONS: [&str; 8] = [
    ".exe", ".elf", ".bin", ".dll", ".so", ".dylib", ".idb", ".i64",
];
const TRAFFIC_EXTENSIONS: [&str; 6] = [".har", ".burp", ".xml", ".json", ".pcap", ".pcapng"];
const SOURCE_EXTENSIONS: [&str; 17] = [
    ".py", ".js", ".jsx", ".ts", ".tsx", ".java", ".kt", ".kts", ".scala", ".php", ".rb", ".go",
    ".cs", ".cpp", ".c", ".h", ".hpp",
];
const REPO_MARKERS: [&str; 8] = [
    "repo_path",
    "repository",
    "source code",
    "source root",
    "source_root",
    "codebase",
    "代码",
    "源码",
];
const TRAFFIC_MARKERS: [&str; 5] = ["har", "burp", "chrome json", "network json", "traffic"];
const BINARY_MARKERS: [&str; 8] = [
    "binary", " exe", ".exe", " elf", " ida", " idb", " i64", "ghidra",
];

/// Python `_strip_trailing_punctuation`。
fn strip_trailing_punctuation(value: &str) -> String {
    value
        .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '\'', '"'])
        .to_string()
}

/// Python `_find_urls`。
#[must_use]
pub fn find_urls(text: &str) -> Vec<String> {
    url_pattern()
        .find_iter(text)
        .map(|matched| strip_trailing_punctuation(matched.as_str()))
        .collect()
}

/// Python `_find_paths`（lookbehind `(?<!\w)` 以"匹配起点前不是词字符"后过滤镜像）。
#[must_use]
pub fn find_paths(text: &str) -> Vec<String> {
    let mut paths: Vec<String> = windows_path_pattern()
        .find_iter(text)
        .map(|matched| matched.as_str().trim().to_string())
        .collect();
    for matched in posix_path_pattern().find_iter(text) {
        let start = matched.start();
        let preceded_by_word = start > 0
            && text[..start]
                .chars()
                .next_back()
                .is_some_and(|character| character.is_alphanumeric() || character == '_');
        if preceded_by_word {
            continue;
        }
        paths.push(matched.as_str().trim().to_string());
    }
    paths
        .into_iter()
        .filter(|path| !path.is_empty() && !path.starts_with("//"))
        .map(|path| strip_trailing_punctuation(&path))
        .collect()
}

/// Python `_find_domains`（负向前瞻以 `http.`/`https.` 前缀后过滤镜像）。
#[must_use]
pub fn find_domains(text: &str) -> Vec<String> {
    let mut domains: Vec<String> = Vec::new();
    for matched in domain_pattern().find_iter(text) {
        let candidate = strip_trailing_punctuation(matched.as_str()).to_lowercase();
        if candidate.starts_with("http.") || candidate.starts_with("https.") {
            continue;
        }
        if candidate.starts_with("www.")
            || candidate.starts_with("api.")
            || !candidate.contains('.')
            || !domains.contains(&candidate)
        {
            domains.push(candidate);
        }
    }
    domains
}

/// Python `_find_ports`。
///
/// # Panics
/// 从不——正则模式为静态字面量。
#[must_use]
pub fn find_ports(text: &str) -> Vec<String> {
    let mut ports = Vec::new();
    #[allow(clippy::expect_used)] // 静态模式字面量
    let port_pattern = regex::Regex::new(r"(?i)\b(?:port|ports?)\s*[:=]?\s*(\d{1,5})\b")
        .expect("端口正则必须可编译");
    for matched in port_pattern.captures_iter(text) {
        ports.push(matched[1].to_string());
    }
    #[allow(clippy::expect_used)] // 静态模式字面量
    let proto_pattern =
        regex::Regex::new(r"(?i)\b(?:tcp|udp)/(\d{1,5})\b").expect("协议端口正则必须可编译");
    for matched in proto_pattern.captures_iter(text) {
        ports.push(matched[1].to_string());
    }
    ports
}

/// Python `_capture_after_markers`。
fn capture_after_markers(text: &str, markers: &[&str]) -> Vec<String> {
    let mut results = Vec::new();
    let lowered = text.to_lowercase();
    for marker in markers {
        let Some(index) = lowered.find(marker) else {
            continue;
        };
        let tail = &text[index + marker.len()..];
        let candidate = tail
            .split('.')
            .next()
            .unwrap_or_default()
            .split(',')
            .next()
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        if !candidate.is_empty() {
            results.push(candidate);
        }
    }
    results
}

/// Python `_unique_strings`。
fn unique_strings(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for value in values {
        let stripped = value.trim().to_string();
        if stripped.is_empty() || !seen.insert(stripped.clone()) {
            continue;
        }
        result.push(stripped);
    }
    result
}

/// Python `_extract_structured_constraints`。
#[must_use]
pub fn extract_structured_constraints(raw_user_query: &str) -> StructuredConstraintContract {
    let text = raw_user_query;
    let lowered = text.to_lowercase();
    let in_scope = unique_strings(
        find_urls(text)
            .into_iter()
            .chain(find_paths(text))
            .chain(find_domains(text)),
    );
    let forbidden_targets = unique_strings(capture_after_markers(
        text,
        &["except", "exclude", "forbid", "avoid", "not on"],
    ));
    let forbidden_ports = unique_strings(find_ports(text));
    let forbidden_actions = unique_strings(
        [
            "translate",
            "rewrite",
            "normalize",
            "exploit",
            "fuzz",
            "scan",
        ]
        .into_iter()
        .filter(|action| {
            lowered.contains(&format!("no {action}"))
                || lowered.contains(&format!("do not {action}"))
                || lowered.contains(&format!("don't {action}"))
                || lowered.contains(&format!("without {action}"))
        })
        .map(str::to_string),
    );
    let max_intrusiveness = if lowered.contains("passive")
        || lowered.contains("non-invasive")
        || lowered.contains("read only")
    {
        MaxIntrusiveness::Passive
    } else if ["exploit", "poc", "active"]
        .iter()
        .any(|token| lowered.contains(token))
    {
        MaxIntrusiveness::Exploit
    } else {
        MaxIntrusiveness::Active
    };
    let mut notes = vec!["raw_user_query preserved without rewrite".to_string()];
    if !in_scope.is_empty() {
        notes.push(format!("detected target hints: {}", in_scope.join(", ")));
    }
    StructuredConstraintContract::new(
        &in_scope,
        &forbidden_targets,
        &forbidden_ports,
        &forbidden_actions,
        &unique_strings(notes),
    )
    .with_intrusiveness(max_intrusiveness)
}

/// Python `_output_language_hint`。
#[must_use]
pub fn output_language_hint(raw_user_query: &str) -> Option<String> {
    if raw_user_query
        .chars()
        .any(|character| ('\u{4e00}'..='\u{9fff}').contains(&character))
    {
        return Some("zh-CN".to_string());
    }
    let lowered = raw_user_query.to_lowercase();
    if lowered.contains("english") || lowered.contains("en-us") {
        return Some("en".to_string());
    }
    None
}

/// Python `_region_hint`。
#[must_use]
pub fn region_hint(raw_user_query: &str) -> Option<String> {
    let lowered = raw_user_query.to_lowercase();
    if lowered.contains("china") || format!(" {lowered} ").contains(" cn ") {
        return Some("CN".to_string());
    }
    if lowered.contains("usa")
        || lowered.contains("united states")
        || format!(" {lowered} ").contains(" us ")
    {
        return Some("US".to_string());
    }
    if lowered.contains("europe") || format!(" {lowered} ").contains(" eu ") {
        return Some("EU".to_string());
    }
    None
}

/// Python `_raw_input_envelope`。
///
/// # Panics
/// 仅当 `raw_user_query` 为空白——请求校验已排除该情形。
#[must_use]
#[allow(clippy::expect_used)]
pub fn raw_input_envelope(raw_user_query: &str) -> RawInputEnvelope {
    let mut envelope = RawInputEnvelope::try_new(raw_user_query.to_string())
        .expect("intake prompt 已由请求校验保证非空")
        .with_constraints(extract_structured_constraints(raw_user_query));
    envelope.output_language_hint = output_language_hint(raw_user_query);
    envelope.region_hint = region_hint(raw_user_query);
    envelope.metadata.insert(
        "source".to_string(),
        Value::String("intake.analyze".to_string()),
    );
    envelope
}

// ---------------------------------------------------------------------------
// 计划构建链
// ---------------------------------------------------------------------------

/// Python `_string_target`：只保留可字符串化的标量值。
fn string_target(target: &Map<String, Value>) -> Map<String, Value> {
    let mut cleaned = Map::new();
    for (key, value) in target {
        let serialized = match value {
            Value::String(text) => Some(text.clone()),
            Value::Bool(flag) => Some(flag.to_string()),
            Value::Number(number) => Some(number.to_string()),
            Value::Null | Value::Array(_) | Value::Object(_) => continue,
        };
        if let Some(text) = serialized {
            cleaned.insert(key.clone(), Value::String(text));
        }
    }
    cleaned
}

/// Python `_is_http_url`。
fn is_http_url(value: &str) -> bool {
    let Ok(parsed) = url::Url::parse(value) else {
        return false;
    };
    let scheme = parsed.scheme().to_lowercase();
    (scheme == "http" || scheme == "https") && parsed.has_host()
}

/// Python `_canonical_target`：补稳定执行键而不丢弃 provider 元数据。
fn canonical_target(target: Map<String, Value>) -> Map<String, Value> {
    let mut normalized = target;
    let current = normalized
        .get("url")
        .and_then(Value::as_str)
        .map(str::to_string);
    if current.is_some_and(|value| is_http_url(&value)) {
        return normalized;
    }
    for key in [
        "challenge_url",
        "primary_url",
        "target_url",
        "endpoint_url",
        "base_url",
        "website",
        "target",
    ] {
        let value = normalized
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string);
        if value.as_ref().is_some_and(|value| is_http_url(value)) {
            normalized.insert("url".to_string(), Value::String(value.unwrap_or_default()));
            break;
        }
    }
    normalized
}

/// Python `_normalized_plan`。
#[must_use]
pub fn normalized_plan(mut plan: IntakePlan) -> IntakePlan {
    let mut audit_domains = if plan.pipeline.audit_domains.is_empty() {
        vec![plan.project.audit_domain]
    } else {
        plan.pipeline.audit_domains.clone()
    };
    if !audit_domains.contains(&plan.project.audit_domain)
        && plan.project.audit_domain != AuditDomain::Composite
    {
        audit_domains.insert(0, plan.project.audit_domain);
    }
    plan.project.target = canonical_target(string_target(&plan.project.target));
    plan.pipeline.audit_domains = audit_domains;
    plan
}

/// Python `_title_from_prompt`。
fn title_from_prompt(prompt: &str, domain: AuditDomain) -> String {
    let compact = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return format!("{} intake", domain.as_str());
    }
    compact.chars().take(80).collect()
}

// Python `_PLACEHOLDER_TITLES` + 独立 `name` 键合并后的展示名占位集合。
const PLACEHOLDER_TITLES_EXTENDED: [&str; 7] =
    ["string", "null", "none", "n/a", "na", "title", "name"];

/// Python `_mission_title`：展示标题优先取模型摘要名，绝不原样回显 query。
fn mission_title(plan: &IntakePlan) -> String {
    let candidate = plan
        .project
        .name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let candidate = candidate
        .trim_matches(|c: char| {
            matches!(
                c,
                '"' | '\'' | '\u{201c}' | '\u{201d}' | '\u{2018}' | '\u{2019}' | ' '
            )
        })
        .to_string();
    if !candidate.is_empty()
        && !PLACEHOLDER_TITLES_EXTENDED.contains(&candidate.to_lowercase().as_str())
    {
        return candidate.chars().take(120).collect();
    }
    let raw = plan
        .metadata
        .get("raw_user_query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    title_from_prompt(&raw, plan.project.audit_domain)
}

/// Python `_plan`：确定性分类结果的公共骨架。
#[allow(clippy::too_many_arguments)]
fn build_plan(
    prompt: &str,
    domain: AuditDomain,
    target: &Map<String, Value>,
    profile: &str,
    audit_domains: Vec<AuditDomain>,
    config: Map<String, Value>,
    intents: Vec<String>,
    confidence: f64,
    rationale: &str,
) -> IntakePlan {
    let title = title_from_prompt(prompt, domain);
    let target = string_target(target);
    let goal_contract = MissionGoalContract {
        source: GoalContractSource::DeterministicFallback,
        rationale: Some(
            "No model-classified success predicate is available; refuse automatic completion until the outcome is classified."
                .to_string(),
        ),
        ..MissionGoalContract::default()
    };
    normalized_plan(IntakePlan {
        project: IntakeProjectDraft {
            name: title,
            audit_domain: domain,
            description: Some(prompt.to_string()),
            target,
            goal: if prompt.is_empty() {
                "Find high-confidence vulnerabilities with reproducible evidence".to_string()
            } else {
                prompt.to_string()
            },
        },
        pipeline: IntakePipelineDraft {
            profile: profile.to_string(),
            audit_domains,
            config,
            start_mode: "pipeline".to_string(),
        },
        goal_contract,
        recommended_intents: intents,
        artifact_record_ids: Vec::new(),
        artifacts_summary: Vec::new(),
        confidence,
        rationale: Some(rationale.to_string()),
        metadata: Map::new(),
    })
}

/// Python `_unknown_plan`。
fn unknown_plan(prompt: &str) -> IntakePlan {
    let goal_contract = MissionGoalContract {
        source: GoalContractSource::DeterministicFallback,
        rationale: Some("Target and success predicate both require clarification.".to_string()),
        ..MissionGoalContract::default()
    };
    IntakePlan {
        project: IntakeProjectDraft {
            name: title_from_prompt(prompt, AuditDomain::Composite),
            audit_domain: AuditDomain::Composite,
            description: Some(prompt.to_string()),
            target: string_target(&Map::from_iter([(
                "raw_prompt".to_string(),
                Value::String(prompt.to_string()),
            )])),
            goal: if prompt.is_empty() {
                "Find high-confidence vulnerabilities with reproducible evidence".to_string()
            } else {
                prompt.to_string()
            },
        },
        pipeline: IntakePipelineDraft {
            profile: "composite".to_string(),
            audit_domains: vec![AuditDomain::Composite],
            config: Map::new(),
            start_mode: "pipeline".to_string(),
        },
        goal_contract,
        // 不再生成澄清问题：任何输入（包括一句"你好"）都直接作为可执行
        // 目标派发，缺什么由执行端在运行中暴露，而不是在入口拦住用户。
        recommended_intents: vec![if prompt.is_empty() {
            "Find high-confidence vulnerabilities with reproducible evidence".to_string()
        } else {
            prompt.to_string()
        }],
        artifact_record_ids: Vec::new(),
        artifacts_summary: Vec::new(),
        confidence: 0.35,
        rationale: Some(
            "The prompt did not contain a recognizable URL, domain, path, binary, or traffic artifact."
                .to_string(),
        ),
        metadata: Map::new(),
    }
}

/// Python `_contains_any`。
fn contains_any(text: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| text.contains(marker))
}

/// Python `_first_matching_path`。
fn first_matching_path(paths: &[String], predicate: impl Fn(&str) -> bool) -> Option<&str> {
    paths
        .iter()
        .map(String::as_str)
        .find(|path| predicate(path))
}

/// Python `_is_traffic_artifact`。
fn is_traffic_artifact(path: &str) -> bool {
    let lowered = path.to_lowercase();
    TRAFFIC_EXTENSIONS
        .iter()
        .any(|extension| lowered.ends_with(extension))
        && (lowered.ends_with(".pcap")
            || lowered.ends_with(".pcapng")
            || lowered.ends_with(".har")
            || lowered.contains("burp")
            || lowered.contains("chrome")
            || lowered.contains("network")
            || lowered.ends_with(".xml"))
}

/// Python `_is_binary_artifact`。
fn is_binary_artifact(path: &str) -> bool {
    let lowered = path.to_lowercase();
    BINARY_EXTENSIONS
        .iter()
        .any(|extension| lowered.ends_with(extension))
        || ["elf", "ida", "ghidra"]
            .iter()
            .any(|marker| lowered.contains(marker))
}

/// Python `_is_source_or_repo_path`。
fn is_source_or_repo_path(path: &str) -> bool {
    let lowered = path.to_lowercase();
    SOURCE_EXTENSIONS
        .iter()
        .any(|extension| lowered.ends_with(extension))
        || [".git", "/src", "\\src", "/repo", "\\repo", "source", "code"]
            .iter()
            .any(|marker| lowered.contains(marker))
}

/// Python `_looks_deep_source_prompt`。
fn looks_deep_source_prompt(lowered_prompt: &str, source: &str) -> bool {
    let lowered_source = source.to_lowercase();
    ["deep", "source-sink", "java"]
        .iter()
        .any(|token| lowered_prompt.contains(token))
        || [".java", ".kt", ".kts", ".scala"]
            .iter()
            .any(|extension| lowered_source.ends_with(extension))
}

/// Python `_language_hint`。
fn language_hint(lowered_prompt: &str, source: &str) -> Option<String> {
    for language in [
        "java",
        "javascript",
        "typescript",
        "python",
        "csharp",
        "go",
        "cpp",
        "c",
    ] {
        if lowered_prompt.contains(language) {
            return Some(language.to_string());
        }
    }
    let lowered_source = source.to_lowercase();
    let extension_map = [
        (".java", "java"),
        (".kt", "java"),
        (".kts", "java"),
        (".scala", "java"),
        (".js", "javascript"),
        (".jsx", "javascript"),
        (".ts", "typescript"),
        (".tsx", "typescript"),
        (".py", "python"),
        (".cs", "csharp"),
        (".go", "go"),
        (".cpp", "cpp"),
        (".c", "c"),
    ];
    extension_map
        .iter()
        .find(|(extension, _)| lowered_source.ends_with(extension))
        .map(|(_, language)| (*language).to_string())
}

/// Python `_source_target`。
fn source_target(source: &str, lowered_prompt: &str, domain: AuditDomain) -> Map<String, Value> {
    if domain == AuditDomain::CodeDeepSast {
        let mut target = Map::new();
        target.insert("source_root".to_string(), Value::String(source.to_string()));
        target.insert("repo_path".to_string(), Value::String(source.to_string()));
        if let Some(language) = language_hint(lowered_prompt, source) {
            target.insert("language".to_string(), Value::String(language));
        }
        return target;
    }
    Map::from_iter([
        ("repo_path".to_string(), Value::String(source.to_string())),
        ("repo".to_string(), Value::String(source.to_string())),
    ])
}

/// Python `_traffic_format`。
fn traffic_format(path: &str, lowered_prompt: &str) -> String {
    let lowered = path.to_lowercase();
    if lowered.ends_with(".pcapng") {
        return "pcapng".to_string();
    }
    if lowered.ends_with(".pcap") {
        return "pcap".to_string();
    }
    if lowered.contains("burp") || lowered_prompt.contains("burp") {
        return if lowered.ends_with(".xml") {
            "burp_xml".to_string()
        } else {
            "burp".to_string()
        };
    }
    if lowered.contains("chrome") || lowered_prompt.contains("chrome") {
        return "chrome_json".to_string();
    }
    if lowered.ends_with(".har") || lowered_prompt.contains("har") {
        return "har".to_string();
    }
    if lowered.ends_with(".xml") {
        return "xml".to_string();
    }
    "json".to_string()
}

/// Python `_strip_path_separator`。
fn strip_path_separator(path: &str) -> String {
    path.trim_end_matches(['/', '\\']).to_string()
}

fn str_map(target: &Map<String, Value>) -> models::StrMap {
    target
        .iter()
        .filter_map(|(key, value)| {
            let text = match value {
                Value::String(text) => Some(text.clone()),
                Value::Bool(flag) => Some(flag.to_string()),
                Value::Number(number) => Some(number.to_string()),
                _ => None,
            }?;
            Some((key.clone(), text))
        })
        .collect()
}

fn value_map(entries: &[(&str, Value)]) -> Map<String, Value> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_string(), value.clone()))
        .collect()
}


/// 目标定位键白名单：告诉模型用这些键承载真实目标。
///
/// 摄入提示词只承诺 `target` 是"object with string values"，键名由模型
/// 自选，所以这里宁可多认也不能少认：少认一个键就会把合法的域名测绘
/// （`domain`）、内网目标（`internal_url`）、代码仓库（`repository` /
/// `codebase`）挤到描述性键上，下游资产嗅探就提取不到目标。
const TARGET_LOCATOR_KEYS: &[&str] = &[
    // URL 类
    "url",
    "base_url",
    "target_url",
    "primary_url",
    "challenge_url",
    "endpoint_url",
    "website",
    "internal_url",
    // 域名 / 主机类
    "domain",
    "target_domain",
    "host",
    // 代码仓库类
    "repo",
    "repo_path",
    "source_root",
    "source",
    "repository",
    "codebase",
    // 二进制类
    "binary",
    "binary_path",
    "ida_database",
    // 流量工件类
    "artifact_path",
    "traffic_artifact",
    "traffic_capture",
    "har",
    "burp_xml",
    "pcap",
    // 云资产类
    "cloud_account",
    "cloud_asset",
    "cluster",
    "kubeconfig",
    "iac_path",
];

/// Python `_constraints`。
#[must_use]
pub fn plan_constraints(plan: &IntakePlan) -> Vec<String> {
    // 摄入不再生成澄清问题，历史签名保留以稳定 Python 镜像与调用方。
    let _ = plan;
    vec![
        "Do not execute destructive actions without explicit user approval".to_string(),
        "Keep evidence provenance linked to branches, tasks, and tool invocations".to_string(),
    ]
}

/// Python `_success_criteria`。
#[must_use]
pub fn plan_success_criteria(plan: &IntakePlan) -> Vec<String> {
    let domains = plan
        .pipeline
        .audit_domains
        .iter()
        .map(|domain| domain.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut criteria = vec![
        "Produce evidence-backed findings or explicit capability gaps".to_string(),
        "Record tool failures and blocked branches instead of fabricating results".to_string(),
    ];
    if !domains.is_empty() {
        criteria.push(format!("Cover recommended audit domain(s): {domains}"));
    }
    criteria
}

/// Python `_merge_ids`。
fn merge_ids(groups: &[Vec<String>]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut merged = Vec::new();
    for group in groups {
        for item in group {
            if seen.insert(item.clone()) {
                merged.push(item.clone());
            }
        }
    }
    merged
}

/// Python `_count_branch_statuses`。
fn count_branch_statuses(branches: &[Branch]) -> Map<String, Value> {
    let mut counts: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for branch in branches {
        *counts
            .entry(branch.status.as_str().to_string())
            .or_insert(0) += 1;
    }
    counts
        .into_iter()
        .map(|(status, count)| (status, Value::Number(count.into())))
        .collect()
}

/// Deterministic pipeline-only routing fallback.  It creates durable Intents
/// without invoking tools; a later audit start reuses the same branch and lets
/// the capability router make the final solver choice.
fn solver_for_domain(domain: AuditDomain) -> &'static str {
    match domain {
        AuditDomain::AssetRecon => "asset_recon",
        AuditDomain::WebRecon
        | AuditDomain::WebIast
        | AuditDomain::Fuzzing
        | AuditDomain::SupplyChain
        | AuditDomain::CloudNative
        | AuditDomain::Composite
        | AuditDomain::Misc => "web_recon",
        AuditDomain::ContentDiscovery => "content_discovery",
        AuditDomain::FingerprintIntelligence => "fingerprint_intelligence",
        AuditDomain::ExposureIntelligence => "exposure_intelligence",
        AuditDomain::WebSast => "web_sast",
        AuditDomain::WebDast => "web_dast",
        AuditDomain::WebValidation => "web_validation",
        AuditDomain::ExploitabilityValidation => "exploitability_validation",
        AuditDomain::InternalSurface => "internal_surface",
        AuditDomain::TrafficIntelligence => "traffic_intelligence",
        AuditDomain::CodeDeepSast => "code_deep_sast",
        AuditDomain::BinaryStatic | AuditDomain::BinaryDynamic => "binary_analysis",
        AuditDomain::Exploitability => "web_exploit",
    }
}

/// Python `_traffic_format_from_artifact`。
fn traffic_format_from_artifact(artifact: &models::ArtifactRecord) -> String {
    let extension = artifact
        .metadata
        .get("extension")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    let name = artifact
        .metadata
        .get("original_filename")
        .and_then(Value::as_str)
        .unwrap_or(&artifact.uri)
        .to_lowercase();
    if extension == ".pcap" || extension == ".pcapng" {
        return extension.trim_start_matches('.').to_string();
    }
    if extension == ".har" {
        return "har".to_string();
    }
    if extension == ".saz" || name.contains("burp") {
        return "burp".to_string();
    }
    if extension == ".xml" {
        return "xml".to_string();
    }
    "json".to_string()
}

/// Python `_profile_for_audit_domain`。
fn profile_for_audit_domain(audit_domain: AuditDomain, current: &str) -> String {
    match audit_domain {
        AuditDomain::TrafficIntelligence => "traffic_intelligence".to_string(),
        AuditDomain::BinaryStatic => "binary_static".to_string(),
        AuditDomain::WebSast => "source_fast".to_string(),
        AuditDomain::Composite if current.is_empty() => "composite".to_string(),
        _ => current.to_string(),
    }
}

// ---------------------------------------------------------------------------
// 服务
// ---------------------------------------------------------------------------

/// intake 失败模式（映射 Python 三个 ValueError 子类 + SolverConfigError）。
#[derive(Debug, thiserror::Error)]
pub enum IntakeServiceError {
    /// 引用的工件不存在（HTTP 404）。
    #[error("artifact record not found: {0}")]
    ArtifactNotFound(String),
    /// 引用的 Mission 不存在（HTTP 404）。
    #[error("mission not found: {0}")]
    MissionNotFound(String),
    /// 执行模式禁止创建执行状态（HTTP 422）。
    #[error("{0}")]
    ExecutionMode(String),
    /// 审计启动配置无法从目标推导（HTTP 422）。
    #[error("{0}")]
    SolverConfig(String),
    /// 上层错误透传。
    #[error(transparent)]
    Engine(#[from] runtime::EngineError),
    /// 仓储错误透传。
    #[error(transparent)]
    Storage(#[from] storage::StorageError),
}

use runtime::AuditManager;

/// 自然语言 intake 服务（Python `IntakeService`）。
pub struct IntakeService {
    manager: Arc<AuditManager>,
    workspace_root: std::path::PathBuf,
    branch_generator: BranchGenerator,
}

/// 复测只读评估的返回（
/// `IntakeService::assess_readonly`）。
///
/// `had_whiteboard` 单独返回而不是塞进 answer：调用方要能区分
/// 「worker 自己读了白板」和「只拿到内联快照」。后者仍然可用，但结论的置信度
/// 不同，这个区别必须体现在复测记录里，不能默默抹掉。
#[derive(Debug, Clone)]
pub struct ReadonlyAssessment {
    /// worker 的终稿原文（保留原样，便于人工回看）。
    pub answer: String,
    /// 实际使用的 runtime 标识。
    pub model: Option<String>,
    /// 是否成功拿到只读 grant（false = 只依据内联上下文）。
    pub had_whiteboard: bool,
}

/// 复测 worker 的只读工具白名单（复测 Agent 的能力集）。
///
/// 刻意**不含** `blackboard_append` / `blackboard_claim`：复测是评估动作，
/// 不该改写被评估的任务白板。`assess_readonly` 发的 grant 只挂这三项，
/// 即使 runtime 侧配错，worker 也会因为拿不到写工具而写不进去。
pub const READONLY_RETEST_TOOLS: [&str; 3] =
    ["blackboard_read", "knowledge_search", "knowledge_get"];

/// 任何写工具（复测**绝不**授予）。
pub const WRITE_TOOLS: [&str; 2] = ["blackboard_append", "blackboard_claim"];

impl IntakeService {
    /// 以 Manager 门面构造（Python 传 Runtime；Rust 侧 Manager 是唯一写者）。
    #[must_use]
    pub fn new(manager: Arc<AuditManager>, workspace_root: std::path::PathBuf) -> Self {
        Self {
            manager,
            workspace_root,
            branch_generator: BranchGenerator,
        }
    }

    /// Python `analyze`：provider 输出仅在严格解析 + 校验后接受，任何失败
    /// 降级确定性回退。
    ///
    /// # Errors
    /// 引用工件缺失（404 语义）。
    pub async fn analyze(
        &self,
        request: &IntakeAnalyzeRequest,
    ) -> Result<IntakeAnalyzeResponse, IntakeServiceError> {
        let artifacts = self.load_artifacts(&request.artifact_record_ids)?;
        let raw_input = raw_input_envelope(&request.prompt);

        // 显式指定 = 钉定（校验存在且启用）；未指定 = 按用途路由：空
        // provider_id 由网关解析（`natural_language_intake` 路由表优先，
        // 回落默认 provider；两者皆无时由调用失败路径降级确定性回退）。
        let pinned_provider = match request.provider_id.as_deref() {
            Some(requested) => match self.resolve_provider_id(Some(requested)).await {
                Some(id) => Some(id),
                None => {
                    return Ok(self.fallback_response(
                        &raw_input,
                        &artifacts,
                        None,
                        None,
                        &format!("requested provider is not available: {requested}"),
                    )
                    .await)
                }
            },
            None => None,
        };
        let provider_id = pinned_provider.clone().unwrap_or_default();

        let before_ids = self.model_invocation_ids()?;
        let messages = Self::messages(&raw_input, &artifacts);
        let runtime = self.manager.provider_runtime();
        let call = async {
            let runtime = runtime
                .ok_or_else(|| agents::llm::ProviderCallError::new("provider runtime is absent"))?;
            agents::llm::ProviderRuntime::generate_structured(
                runtime.as_ref(),
                agents::llm::StructuredGenerationRequest {
                    provider_id: &provider_id,
                    messages: &messages,
                    purpose: INTAKE_PURPOSE,
                    project_id: None,
                    run_id: None,
                    task_id: None,
                },
            )
            .await
        };
        match call.await {
            Ok(raw) => {
                let model_invocation_id = self.latest_new_model_invocation_id(&before_ids)?;
                // 模型输出不合契约（未知字段 / 缺 plan / 类型不符）与调用
                // 失败同待：降级确定性回退——模块红线“校验任何失败都降级，
                // intake 永不阻塞”，serde 错误绝不直达客户端。
                let plan = match Self::validate_provider_plan(&raw) {
                    Ok(plan) => plan,
                    Err(error) => {
                        return Ok(self
                            .fallback_response(
                                &raw_input,
                                &artifacts,
                                pinned_provider,
                                model_invocation_id,
                                &error.to_string(),
                            )
                            .await);
                    }
                };
                let plan = plan_with_raw_input(plan, &raw_input);
                let plan = plan_with_artifacts(plan, &artifacts);
                Ok(self
                    .analyze_response(
                        plan,
                        &raw_input,
                        pinned_provider,
                        model_invocation_id,
                        false,
                        None,
                    )
                    .await)
            }
            Err(error) => {
                let model_invocation_id = self.latest_new_model_invocation_id(&before_ids)?;
                Ok(self
                    .fallback_response(
                        &raw_input,
                        &artifacts,
                        pinned_provider,
                        model_invocation_id,
                        &format!("ProviderCallError: {error}"),
                    )
                    .await)
            }
        }
    }

    /// 异步 intake：同步创建草稿 Mission（毫秒级返回），后台跑
    /// analyze → start（复用该 Mission 并派发执行）。
    ///
    /// # Errors
    /// 草稿 Mission 创建失败。
    pub async fn start_async(
        &self,
        request: &IntakeAsyncRequest,
    ) -> Result<IntakeAsyncResponse, IntakeServiceError> {
        let input = runtime::mission_lifecycle::CreateMissionInput {
            user_goal: request.prompt.clone(),
            title: None,
            target: None,
            project_id: None,
            constraints: Vec::new(),
            success_criteria: Vec::new(),
            goal_contract: None,
            tags: Vec::new(),
            category: None,
            approval_mode: models::ApprovalMode::AskForApproval,
            created_by: "intake-async".to_string(),
            metadata: {
                let mut metadata = Map::new();
                metadata.insert(
                    "source".to_string(),
                    Value::String("intake.async".to_string()),
                );
                metadata.insert(
                    "raw_user_query".to_string(),
                    Value::String(request.prompt.clone()),
                );
                metadata
            },
        };
        let mission = self.manager.create_mission(input).await?;

        let service = IntakeService::new(
            Arc::clone(&self.manager),
            self.workspace_root.clone(),
        );
        let prompt = request.prompt.clone();
        let provider_id = request.provider_id.clone();
        let artifact_record_ids = request.artifact_record_ids.clone();
        let mission_id = mission.id.clone();
        tokio::spawn(async move {
            let analysis = match service
                .analyze(&IntakeAnalyzeRequest {
                    prompt,
                    provider_id,
                    artifact_record_ids,
                })
                .await
            {
                Ok(analysis) => analysis,
                Err(error) => {
                    eprintln!("async intake analysis failed; mission stays draft: {error}");
                    return;
                }
            };
            let start = IntakeStartRequest {
                plan: analysis.plan.clone(),
                mission_id: Some(mission_id.as_str().to_string()),
                artifact_record_ids: Vec::new(),
                start_pipeline: true,
                // 任何输入都直接派发执行：模糊输入缺什么由执行端在运行中
                // 暴露，入口不再拦截或追问。
                start_audit: true,
            };
            if let Err(error) = service.start(&start).await {
                eprintln!("async intake start failed; mission stays draft: {error}");
            }
        });
        Ok(IntakeAsyncResponse { mission })
    }

    /// 起一个临时顾问 worker 并取回它的终稿答案。
    ///
    /// `advise`（可写白板）与 `assess_readonly`（只读）共用这一条执行路径；
    /// 两者的差别只在 `mcp_binding` 里那个 grant 的 allowlist，以及
    /// `workdir_sub` 落在哪个子目录。`label` 只用于错误信息，方便区分是哪
    /// 条路挂了。
    ///
    /// 三条不变式：
    /// - workdir 用**绝对路径**：相对路径会让 codex 把 cwd 与 CODEX_HOME
    ///   错算成 `data/artifacts/data/artifacts/...`；
    /// - grant 用完立刻撤销，无论成功失败；
    /// - 答案取尾部连续 Output 事件；取不到就报错，不返回空串让上层假装成功。
    async fn run_advisor_worker(
        &self,
        runtime: &dyn agents::worker::WorkerRuntime,
        runtime_type: &str,
        instruction: String,
        workdir_sub: &str,
        mcp_binding: Option<(
            engines::broker::mcp::WorkerGrantCredentials,
            String,
            String,
        )>,
        label: &str,
    ) -> Result<(String, Option<String>), IntakeServiceError> {
        let mut request = agents::worker::WorkerExecutionRequest::start(instruction, 240);
        // 绝对 workdir：相对路径会让 codex 把 cwd 与 CODEX_HOME 错算成
        // data/artifacts/data/artifacts/...（分支任务用绝对路径，advise 同构）。
        let workdir = std::env::current_dir()
            .map_err(|error| {
                IntakeServiceError::Engine(runtime::EngineError::Value(error.to_string()))
            })?
            .join("data")
            .join("artifacts")
            .join(workdir_sub);
        std::fs::create_dir_all(&workdir).map_err(|error| {
            IntakeServiceError::Engine(runtime::EngineError::Value(format!(
                "cannot create {workdir_sub} workdir: {error}"
            )))
        })?;
        request.workdir = Some(workdir);
        if let Some((credentials, endpoint, worker_run_id)) = &mcp_binding {
            request.worker_run_id = Some(worker_run_id.clone());
            request.mcp_url = Some(endpoint.clone());
            request.mcp_bearer_token = Some(credentials.bearer_token.clone());
        }

        let outcome = runtime.start(request).await.map_err(|error| {
            IntakeServiceError::ExecutionMode(format!("{label} worker failed: {error}"))
        })?;
        if let Some((credentials, _, _)) = &mcp_binding {
            if let Some(mcp) = self.manager.worker_mcp() {
                mcp.revoke_worker_grant(&credentials.grant_id);
            }
        }

        // 答案 = 尾部连续 Output 事件（claude 终稿 / codex agent_message）。
        let mut answer_parts: Vec<String> = Vec::new();
        for event in outcome.run.events.iter().rev() {
            if event.kind == models::worker::WorkerEventKind::Output
                && !event.message.trim().is_empty()
            {
                answer_parts.push(event.message.clone());
            } else if !answer_parts.is_empty() {
                break;
            }
        }
        answer_parts.reverse();
        let answer = answer_parts.join("\n\n");
        if answer.trim().is_empty() {
            let detail = outcome
                .run
                .error
                .clone()
                .or_else(|| {
                    outcome
                        .run
                        .events
                        .iter()
                        .rev()
                        .find(|event| event.kind == models::worker::WorkerEventKind::Error)
                        .map(|event| event.message.clone())
                })
                .unwrap_or_default();
            return Err(IntakeServiceError::ExecutionMode(format!(
                "{label} worker produced no answer (status {}): {}",
                outcome.run.status.as_str(),
                detail
            )));
        }
        Ok((answer, Some(runtime_type.to_string())))
    }

    /// 任务顾问：起一次带 MCP 白板读写能力的临时 harness worker
    ///（blackboard_read/blackboard_append/knowledge_search），回答操作员
    /// 对当前任务的问题。与 muteki BTW 同构：不入群、不占并发槽、用完即收。
    /// 答案取该 run 的尾部 Output 事件（claude 终稿 / codex agent_message）。
    ///
    /// # Errors
    /// worker runtime 不可用、执行失败或超时。
    pub async fn advise(
        &self,
        mission: &Mission,
        question: &str,
        history: &[(String, String)],
    ) -> Result<(String, Option<String>), IntakeServiceError> {
        let selector = self.manager.worker_runtime().ok_or_else(|| {
            IntakeServiceError::ExecutionMode("worker runtime unavailable".to_string())
        })?;
        let runtime = selector.select(None).await.map_err(|error| {
            IntakeServiceError::ExecutionMode(format!("no worker runtime available: {error}"))
        })?;
        let runtime_type = runtime.runtime_type();

        // 白板通道：发放只读+追加写权限的 grant（用完撤销）。scope 校验
        // 要求 mission/run/task/worker id 全部非空，缺失时 grant 发放失败
        // 会导致顾问静默退化到无 MCP 状态（绝不静默）。
        let advise_run_id = mission.active_run_id.clone().or_else(|| {
            self.manager
                .repository()
                .list_runs(mission.project_id.as_str())
                .ok()
                .and_then(|runs| {
                    runs.into_iter()
                        .filter(|run| run.mission_id.as_ref() == Some(&mission.id))
                        .max_by(|left, right| left.created_at.cmp(&right.created_at))
                        .map(|run| run.id)
                })
        });
        let mcp_binding = self.manager.worker_mcp().and_then(|mcp| {
            let worker_run_id = format!("worker-advise-{}", uuid::Uuid::new_v4().simple());
            let scope = engines::broker::ExecutionScope {
                project_id: Some(mission.project_id.clone()),
                mission_id: Some(mission.id.clone()),
                run_id: advise_run_id.clone(),
                task_id: Some(models::TaskId::new(format!(
                    "advise-task-{}",
                    uuid::Uuid::new_v4().simple()
                ))),
                branch_id: None,
                intent_id: None,
                worker_id: Some("advise-worker".to_string()),
                worker_run_id: Some(worker_run_id.clone()),
                artifact_dir: Some(
                    std::path::PathBuf::from("data")
                        .join("artifacts")
                        .join(&worker_run_id),
                ),
            };
            match mcp.issue_worker_grant(engines::broker::mcp::WorkerGrantSpec::new(scope, None)) {
                Ok(credentials) => Some((credentials, mcp.endpoint().to_string(), worker_run_id)),
                Err(error) => {
                    eprintln!("advise: worker grant issuance failed ({error}); running without MCP whiteboard");
                    None
                }
            }
        });

        let mut context_hint = format!(
            "任务目标：{}
目标：{}
任务状态：{}",
            mission.user_goal,
            serde_json::to_string(&mission.target).unwrap_or_default(),
            mission.status.as_str(),
        );
        if let Ok(branches) = self.manager.repository().list_branches(
            Some(mission.project_id.as_str()),
            Some(mission.id.as_str()),
            None,
        ) {
            let lines: Vec<String> = branches
                .iter()
                .take(12)
                .map(|branch| format!("- [{}] {}", branch.status.as_str(), branch.title))
                .collect();
            if !lines.is_empty() {
                context_hint.push_str("
分支：
");
                context_hint.push_str(&lines.join("
"));
            }
        }

        let mut instruction = format!(
            "你是 Lynceus 任务顾问（只读侧分析 worker）。规则：
             1. 先用 blackboard_read 读任务白板了解当前状态；需要背景知识可用 knowledge_search。
             2. 用中文简洁回答，基于证据，不编造不存在的发现；证据不足就直说。
             3. 若结论值得保留，用 blackboard_append 写回白板一条，参数必须是：kind=\"summary\"、             idempotency_key=\"advise-<时间戳>（自拟唯一值）、content=<结论>；kind 只能是              task/hypothesis/observation/artifact_ref/evidence_ref/question/blocker/decision/summary 之一。
             4. 最终回答：先一句话结论，然后要点列表。

             任务上下文：
{context_hint}

操作员问题：{question}"
        );
        if !history.is_empty() {
            instruction.push_str("

最近对话：");
            for (role, content) in history.iter().take(8) {
                instruction.push_str(&format!("
{role}: {content}"));
            }
        }

        self.run_advisor_worker(
            runtime.as_ref(),
            runtime_type.as_str(),
            instruction,
            "advise",
            mcp_binding,
            "advise",
        )
        .await
    }

    /// 复测评估器：起一个**只读**临时 worker。
    ///
    /// 与 [`Self::advise`] 的关键差别——复测是「评估一个既有漏洞」，不是
    /// 「回答操作员问题」，所以权限必须收窄：
    ///
    /// 1. **grant 只发只读工具**（`blackboard_read` / `knowledge_search` /
    ///    `knowledge_get`）。不复用 advise 的三件套：`blackboard_append` 会让
    ///    复测把自己的结论写回任务白板，之后所有读白板的 worker 都会把
    ///    「复测认为已修复」当成事实——评估动作不该改写被评估的对象。
    /// 2. **上下文始终内联进 prompt**（调用方传入的 `context_block`）。grant
    ///    发不下来时不再像 advise 那样静默退化到零上下文：复测拿到的信息
    ///    至少是这份快照，`had_whiteboard` 会如实告诉调用方它没能自己读白板。
    ///    要求一个从未见过证据的 worker 给 verdict 是编造结论的标准入口，
    ///    所以这里绝不沉默。
    /// 3. 指令明确禁止调用任何写工具、禁止读写文件、禁止执行命令——
    ///    即使 runtime 侧配错了 allowlist，worker 也知道自己不该写。
    ///
    /// # Errors
    /// worker runtime 不可用、执行失败或超时。
    pub async fn assess_readonly(
        &self,
        mission: &Mission,
        question: &str,
        context_block: &str,
    ) -> Result<ReadonlyAssessment, IntakeServiceError> {
        let selector = self.manager.worker_runtime().ok_or_else(|| {
            IntakeServiceError::ExecutionMode("worker runtime unavailable".to_string())
        })?;
        let runtime = selector.select(None).await.map_err(|error| {
            IntakeServiceError::ExecutionMode(format!("no worker runtime available: {error}"))
        })?;
        let runtime_type = runtime.runtime_type();

        let assess_run_id = mission.active_run_id.clone().or_else(|| {
            self.manager
                .repository()
                .list_runs(mission.project_id.as_str())
                .ok()
                .and_then(|runs| {
                    runs
                        .into_iter()
                        .filter(|run| run.mission_id.as_ref() == Some(&mission.id))
                        .max_by(|left, right| left.created_at.cmp(&right.created_at))
                        .map(|run| run.id)
                })
        });
        // 只读 grant：allowlist 里没有任何写工具。
        let mcp_binding = self.manager.worker_mcp().and_then(|mcp| {
            let worker_run_id = format!("worker-retest-{}", uuid::Uuid::new_v4().simple());
            let scope = engines::broker::ExecutionScope {
                project_id: Some(mission.project_id.clone()),
                mission_id: Some(mission.id.clone()),
                run_id: assess_run_id.clone(),
                task_id: Some(models::TaskId::new(format!(
                    "retest-task-{}",
                    uuid::Uuid::new_v4().simple()
                ))),
                branch_id: None,
                intent_id: None,
                worker_id: Some("retest-worker".to_string()),
                worker_run_id: Some(worker_run_id.clone()),
                artifact_dir: Some(
                    std::path::PathBuf::from("data")
                        .join("artifacts")
                        .join(&worker_run_id),
                ),
            };
            let spec = engines::broker::mcp::WorkerGrantSpec::new(
                scope,
                Some(
                    READONLY_RETEST_TOOLS
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                ),
            );
            match mcp.issue_worker_grant(spec) {
                Ok(credentials) => Some((credentials, mcp.endpoint().to_string(), worker_run_id)),
                Err(error) => {
                    // 不复刻 advise 的静默降级：had_whiteboard 会让调用方知道。
                    eprintln!(
                        "assess_readonly: read-only grant issuance failed ({error}); \
                         the assessment will rely on the inline context only"
                    );
                    None
                }
            }
        });
        let had_whiteboard = mcp_binding.is_some();

        let instruction = format!(
            "你是 Lynceus 漏洞复测评估员（**只读** worker）。规则：\n\
             1. 你没有任何写权限：不得调用 blackboard_append / blackboard_claim，不得读写文件，不得执行命令。\n\
             2. 判断只能基于下面「既有证据快照」；没有列出的证据就是不存在的，不得假设还有别的。\n\
             3. 证据不足就直说 inconclusive，不得为了让报告完整而挑选结论。\n\
             4. 用中文回答，先给三行结构化结论，再给简短理由。\n\
             \n\
             既有证据快照：\n\
             {context_block}\n\
             \n\
             复测要求：{question}\n\
             \n\
             请严格按这个格式输出最后三行（可附带说明文字，但这三行必须单独成行）：\n\
             VERDICT: reproduced|fixed|inconclusive\n\
             SUMMARY: 一到两句话的结论\n\
             EVIDENCE: 你依据的具体证据 id 或内容"
        );

        let (answer, model) = self
            .run_advisor_worker(
                runtime.as_ref(),
                runtime_type.as_str(),
                instruction,
                "retest",
                mcp_binding,
                "assess_readonly",
            )
            .await?;
        Ok(ReadonlyAssessment {
            answer,
            model,
            had_whiteboard,
        })
    }


    /// Python `create_project`：创建兼容 Project + 一等 Mission，不启动。
    ///
    /// # Errors
    /// 工件/Mission 缺失、S0 直答模式禁止建状态或引擎错误。
    pub async fn create_project(
        &self,
        request: &IntakeStartRequest,
    ) -> Result<IntakeStartResponse, IntakeServiceError> {
        let (plan, artifacts) = self.plan_and_artifacts_for_start(request)?;
        let (project, mission, plan) = self
            .prepare_project_and_mission_for_start(&plan, &artifacts, request.mission_id.as_deref())
            .await?;
        let start_mode = plan.pipeline.start_mode.clone();
        Ok(IntakeStartResponse {
            project,
            mission: Some(mission),
            plan,
            intents: Vec::new(),
            run: None,
            branches: Vec::new(),
            created_intents: Vec::new(),
            created_run: None,
            pipeline_status: value_map(&[
                ("start_mode", Value::String(start_mode)),
                ("mission_created", Value::Bool(true)),
                ("mission_started", Value::Bool(false)),
                ("audit_requested", Value::Bool(false)),
                ("audit_started", Value::Bool(false)),
                ("pipeline_requested", Value::Bool(false)),
                ("pipeline_started", Value::Bool(false)),
            ]),
        })
    }

    /// Python `start`：创建 Mission-first 工作区并经 Mission Control 启动。
    ///
    /// # Errors
    /// 工件/Mission 缺失、S0 直答、审计配置无法推导或引擎错误。
    /// Python 同构 82 行；`too_many_lines` 豁免保持镜像。
    #[allow(clippy::too_many_lines)]
    pub async fn start(
        &self,
        request: &IntakeStartRequest,
    ) -> Result<IntakeStartResponse, IntakeServiceError> {
        let (plan, artifacts) = self.plan_and_artifacts_for_start(request)?;
        let (project, mission, plan) = self
            .prepare_project_and_mission_for_start(&plan, &artifacts, request.mission_id.as_deref())
            .await?;
        let should_start_audit = request.start_audit || plan.pipeline.start_mode == "audit";

        let mut start_config: Map<String, Value> = Map::new();
        if should_start_audit {
            start_config = audit_config(&plan)?;
        }
        let start_config = mission_workspace::config_with_mission_workspace(
            start_config,
            &self.workspace_root,
            &mission,
        )
        .map_err(|error| IntakeServiceError::ExecutionMode(error.to_string()))?;

        let result = self
            .manager
            .start_mission(
                &mission.id,
                Some(start_config),
                should_start_audit,
                should_start_audit,
                None,
                None,
            )
            .await?;
        let mission = result.mission;
        let run = self.manager.repository().get_run(result.run_id.as_str())?;
        let branches = self.manager.repository().list_branches(
            Some(mission.project_id.as_str()),
            Some(mission.id.as_str()),
            None,
        )?;
        // Pipeline-only starts still create durable, pending Intents.  They do
        // not execute a runtime until the caller explicitly starts an audit,
        // but they are now visible to the task queue instead of returning the
        // old permanent placeholder error.
        let mut intents: Vec<Intent> = Vec::new();
        if request.start_pipeline && !should_start_audit {
            let existing = self
                .manager
                .repository()
                .list_intents(project.id.as_str())?;
            for branch in &branches {
                if existing.iter().any(|item| {
                    item.run_id.as_ref() == run.as_ref().map(|value| &value.id)
                        && item.branch_id.as_ref() == Some(&branch.id)
                        && !matches!(item.status, models::IntentStatus::Dismissed)
                }) {
                    continue;
                }
                let mut intent = Intent::new(project.id.clone(), branch.title.clone());
                intent.mission_id = Some(mission.id.clone());
                intent.branch_id = Some(branch.id.clone());
                intent.run_id = run.as_ref().map(|value| value.id.clone());
                intent.description = Some(branch.hypothesis.clone());
                intent.source_fact_ids.clone_from(&branch.related_fact_ids);
                intent.solver = Some(solver_for_domain(plan.project.audit_domain).to_string());
                intent.priority = branch.priority;
                intent.max_steps = branch.budget_steps.max(1);
                intent.created_by = "intake".to_string();
                let stored = self.manager.repository().add_intent(&intent)?;
                intents.push(stored);
            }
        }
        let start_mode = plan.pipeline.start_mode.clone();
        let pipeline_status = {
            let mut status = Map::new();
            status.insert("start_mode".to_string(), Value::String(start_mode));
            status.insert("mission_created".to_string(), Value::Bool(true));
            status.insert(
                "mission_started".to_string(),
                Value::Bool(should_start_audit),
            );
            status.insert(
                "mission_status".to_string(),
                Value::String(mission.status.as_str().to_string()),
            );
            status.insert(
                "audit_requested".to_string(),
                Value::Bool(should_start_audit),
            );
            let audit_started = should_start_audit
                && run
                    .as_ref()
                    .is_some_and(|run| run.status != RunStatus::Pending);
            status.insert("audit_started".to_string(), Value::Bool(audit_started));
            status.insert(
                "audit_status".to_string(),
                run.as_ref().map_or(Value::Null, |run| {
                    Value::String(run.status.as_str().to_string())
                }),
            );
            status.insert(
                "branch_count".to_string(),
                Value::Number(i64::try_from(branches.len()).unwrap_or(i64::MAX).into()),
            );
            status.insert(
                "branch_statuses".to_string(),
                Value::Object(count_branch_statuses(&branches)),
            );
            status.insert(
                "pipeline_requested".to_string(),
                Value::Bool(request.start_pipeline),
            );
            status.insert(
                "pipeline_started".to_string(),
                Value::Bool(!intents.is_empty() || should_start_audit),
            );
            status.insert(
                "pipeline_driver".to_string(),
                Value::String(
                    if should_start_audit {
                        "mission_runtime"
                    } else {
                        "agent_pipeline"
                    }
                    .to_string(),
                ),
            );
            status.insert(
                "created_intent_count".to_string(),
                Value::Number(i64::try_from(intents.len()).unwrap_or(i64::MAX).into()),
            );
            status
        };
        Ok(IntakeStartResponse {
            project,
            mission: Some(mission),
            plan,
            intents: intents.clone(),
            run: run.clone(),
            branches,
            created_intents: intents,
            created_run: run,
            pipeline_status,
        })
    }

    /// Python `_prepare_project_and_mission_for_start`。
    /// Python 同构 129 行；`too_many_lines` 豁免保持镜像。
    #[allow(clippy::too_many_lines)]
    async fn prepare_project_and_mission_for_start(
        &self,
        plan: &IntakePlan,
        artifacts: &[models::ArtifactRecord],
        mission_id: Option<&str>,
    ) -> Result<(Project, Mission, IntakePlan), IntakeServiceError> {
        let mut plan = plan_with_artifacts(plan.clone(), artifacts);
        let raw_query = plan
            .metadata
            .get("raw_user_query")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut mission;
        let mut project;
        if mission_id.is_none() {
            let description = raw_query
                .clone()
                .or_else(|| plan.project.description.clone());
            let goal = raw_query
                .clone()
                .unwrap_or_else(|| plan.project.goal.clone());
            project = self
                .manager
                .create_project(
                    &plan.project.name,
                    plan.project.audit_domain,
                    description.as_deref(),
                    Some(&str_map(&plan.project.target)),
                    Some(&goal),
                )
                .await?;
            let mut metadata = Map::new();
            metadata.insert("source".to_string(), Value::String("intake".to_string()));
            metadata.insert(
                "intake_plan".to_string(),
                serde_json::to_value(&plan).unwrap_or(Value::Null),
            );
            metadata.insert(
                "audit_domain".to_string(),
                Value::String(plan.project.audit_domain.as_str().to_string()),
            );
            metadata.insert(
                "pipeline_profile".to_string(),
                Value::String(plan.pipeline.profile.clone()),
            );
            metadata.insert(
                "raw_user_query".to_string(),
                raw_query.clone().map_or(Value::Null, Value::String),
            );
            if let Some(constraints) = plan.metadata.get("structured_constraints") {
                metadata.insert("structured_constraints".to_string(), constraints.clone());
            }
            let input = runtime::mission_lifecycle::CreateMissionInput {
                user_goal: raw_query
                    .clone()
                    .unwrap_or_else(|| plan.project.goal.clone()),
                title: Some(mission_title(&plan)),
                target: Some(plan.project.target.clone()),
                project_id: Some(project.id.clone()),
                constraints: plan_constraints(&plan),
                success_criteria: plan_success_criteria(&plan),
                goal_contract: Some(plan.goal_contract.clone()),
                tags: Vec::new(),
                category: None,
                approval_mode: models::ApprovalMode::AskForApproval,
                created_by: "intake".to_string(),
                metadata,
            };
            mission = self.manager.create_mission(input).await?;
        } else {
            let Some(mission_id) = mission_id else {
                unreachable!("外层 is_none 已排除");
            };
            let existing_mission = self
                .manager
                .repository()
                .get_mission(mission_id)?
                .ok_or_else(|| {
                    IntakeServiceError::MissionNotFound(format!("mission not found: {mission_id}"))
                })?;
            let existing_project = self
                .manager
                .repository()
                .get_project(existing_mission.project_id.as_str())?
                .ok_or_else(|| {
                    IntakeServiceError::MissionNotFound(format!(
                        "mission project not found: {}",
                        existing_mission.project_id
                    ))
                })?;
            project = existing_project;
            project.target = str_map(&plan.project.target);
            project.audit_domain = plan.project.audit_domain;
            project.description.clone_from(&plan.project.description);
            project = self.manager.repository().update_project(&project)?;
            mission = existing_mission;
            let mut metadata = mission.metadata.clone();
            metadata.insert("source".to_string(), Value::String("intake".to_string()));
            metadata.insert(
                "intake_plan".to_string(),
                serde_json::to_value(&plan).unwrap_or(Value::Null),
            );
            metadata.insert(
                "audit_domain".to_string(),
                Value::String(plan.project.audit_domain.as_str().to_string()),
            );
            metadata.insert(
                "pipeline_profile".to_string(),
                Value::String(plan.pipeline.profile.clone()),
            );
            metadata.insert(
                "raw_user_query".to_string(),
                raw_query.clone().map_or(Value::Null, Value::String),
            );
            if let Some(constraints) = plan.metadata.get("structured_constraints") {
                metadata.insert("structured_constraints".to_string(), constraints.clone());
            }
            mission.user_goal = raw_query
                .clone()
                .unwrap_or_else(|| plan.project.goal.clone());
            if mission.title.is_none() {
                mission.title = Some(mission_title(&plan));
            }
            mission.target = str_map(&plan.project.target);
            mission.constraints = plan_constraints(&plan);
            mission.success_criteria = plan_success_criteria(&plan);
            mission.goal_contract = plan.goal_contract.clone();
            mission.metadata = metadata;
            mission.updated_at = models::utcnow();
            mission = self.manager.repository().update_mission(&mission)?;
        }
        let workspace_path =
            mission_workspace::workspace_path_from_mission(&self.workspace_root, &mission)
                .map_err(|error| IntakeServiceError::ExecutionMode(error.to_string()))?;
        mission = mission_workspace::mission_with_workspace_metadata(&mission, &workspace_path);
        mission = self.manager.repository().update_mission(&mission)?;

        let mut moved_artifacts: Vec<models::ArtifactRecord> = Vec::new();
        for artifact in artifacts {
            let moved = mission_workspace::move_artifact_into_mission_uploads(
                &self.workspace_root,
                artifact,
                &mission,
            )
            .map_err(|error| IntakeServiceError::ExecutionMode(error.to_string()))?;
            let moved = self.manager.update_artifact_record(&moved)?;
            let asset = engines::upload_intake::bind_artifact_to_mission(
                &moved,
                &mission,
                models::MissionAssetSource::UserTarget,
            )
            .map_err(IntakeServiceError::ExecutionMode)?;
            self.manager.upsert_mission_asset(&asset)?;
            moved_artifacts.push(moved);
        }
        if !moved_artifacts.is_empty() {
            plan = plan_with_artifacts(plan, &moved_artifacts);
            project.target = str_map(&plan.project.target);
            project.audit_domain = plan.project.audit_domain;
            project = self.manager.repository().update_project(&project)?;
            let mut metadata = mission.metadata.clone();
            metadata.insert(
                "intake_plan".to_string(),
                serde_json::to_value(&plan).unwrap_or(Value::Null),
            );
            mission.target = str_map(&plan.project.target);
            mission.metadata = metadata;
            mission.updated_at = models::utcnow();
            mission = self.manager.repository().update_mission(&mission)?;
        }
        let mut fact_data = Map::new();
        fact_data.insert(
            "intake_plan".to_string(),
            serde_json::to_value(&plan).unwrap_or(Value::Null),
        );
        fact_data.insert(
            "artifact_record_ids".to_string(),
            Value::Array(
                plan.artifact_record_ids
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
        fact_data.insert(
            "artifacts_summary".to_string(),
            Value::Array(
                plan.artifacts_summary
                    .iter()
                    .cloned()
                    .map(Value::Object)
                    .collect(),
            ),
        );
        fact_data.insert("strategy_board_context".to_string(), Value::Bool(true));
        fact_data.insert(
            "raw_user_query".to_string(),
            raw_query.map_or(Value::Null, Value::String),
        );
        if let Some(constraints) = plan.metadata.get("structured_constraints") {
            fact_data.insert("structured_constraints".to_string(), constraints.clone());
        }
        self.manager.add_user_fact(
            project.id.as_str(),
            "intake.plan",
            &format!(
                "Natural-language intake plan: {}; {}",
                plan.project.audit_domain.as_str(),
                plan.recommended_intents.join(", ")
            ),
            fact_data,
            Vec::new(),
            plan.confidence,
        )?;
        Ok((project, mission, plan))
    }

    async fn resolve_provider_id(&self, requested: Option<&str>) -> Option<String> {
        let runtime = self.manager.provider_runtime()?;
        if let Some(requested) = requested {
            let provider = runtime.get_provider(requested).await.ok()??;
            if !provider.enabled {
                return None;
            }
            return Some(provider.id.as_str().to_string());
        }
        runtime
            .resolve_default_provider()
            .await
            .ok()?
            .map(|provider| provider.id.as_str().to_string())
    }

    fn model_invocation_ids(&self) -> Result<HashSet<String>, IntakeServiceError> {
        Ok(self
            .manager
            .repository()
            .list_model_invocations(None)?
            .into_iter()
            .map(|item| item.id.as_str().to_string())
            .collect())
    }

    fn latest_new_model_invocation_id(
        &self,
        before_ids: &HashSet<String>,
    ) -> Result<Option<String>, IntakeServiceError> {
        let created: Vec<_> = self
            .manager
            .repository()
            .list_model_invocations(None)?
            .into_iter()
            .filter(|item| !before_ids.contains(item.id.as_str()))
            .collect();
        Ok(created.last().map(|item| item.id.as_str().to_string()))
    }

    fn load_artifacts(
        &self,
        artifact_ids: &[String],
    ) -> Result<Vec<models::ArtifactRecord>, IntakeServiceError> {
        let mut artifacts = Vec::new();
        for artifact_id in artifact_ids {
            let artifact = self
                .manager
                .repository()
                .get_artifact_record(artifact_id)?
                .ok_or_else(|| {
                    IntakeServiceError::ArtifactNotFound(format!(
                        "artifact record not found: {artifact_id}"
                    ))
                })?;
            artifacts.push(artifact);
        }
        Ok(artifacts)
    }

    fn plan_and_artifacts_for_start(
        &self,
        request: &IntakeStartRequest,
    ) -> Result<(IntakePlan, Vec<models::ArtifactRecord>), IntakeServiceError> {
        let artifact_ids = merge_ids(&[
            request.plan.artifact_record_ids.clone(),
            request.artifact_record_ids.clone(),
        ]);
        let artifacts = self.load_artifacts(&artifact_ids)?;
        let plan = plan_with_artifacts(request.plan.clone(), &artifacts);
        Ok((plan, artifacts))
    }

    /// Python `_validate_provider_plan`。
    fn validate_provider_plan(raw: &Map<String, Value>) -> Result<IntakePlan, IntakeServiceError> {
        let payload = raw
            .get("plan")
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or_else(|| Value::Object(raw.clone()));
        let mut plan: IntakePlan = serde_json::from_value(payload).map_err(|error| {
            IntakeServiceError::ExecutionMode(format!("ValidationError: {error}"))
        })?;
        let resolved = plan.goal_contract.status == GoalContractStatus::Resolved
            && plan.goal_contract.confidence >= 0.75;
        plan.goal_contract.source = GoalContractSource::IntakeModel;
        plan.goal_contract.status = if resolved {
            plan.goal_contract.status
        } else {
            GoalContractStatus::NeedsReview
        };
        plan.goal_contract.auto_complete = resolved;
        Ok(normalized_plan(plan))
    }

    async fn fallback_response(
        &self,
        raw_input: &RawInputEnvelope,
        artifacts: &[models::ArtifactRecord],
        provider_id: Option<String>,
        model_invocation_id: Option<String>,
        reason: &str,
    ) -> IntakeAnalyzeResponse {
        let plan = plan_with_artifacts(
            plan_with_raw_input(unknown_plan(&raw_input.raw_user_query), raw_input),
            artifacts,
        );
        self.analyze_response(
            plan,
            raw_input,
            provider_id,
            model_invocation_id,
            true,
            Some(reason.to_string()),
        )
        .await
    }

    /// Python `_analyze_response`。
    async fn analyze_response(
        &self,
        plan: IntakePlan,
        raw_input: &RawInputEnvelope,
        provider_id: Option<String>,
        model_invocation_id: Option<String>,
        used_fallback: bool,
        fallback_reason: Option<String>,
    ) -> IntakeAnalyzeResponse {
        let plan = normalized_plan(plan);
        let plan_confidence = plan.confidence;
        let plan_rationale = plan.rationale.clone();
        let constraints = plan_constraints(&plan);
        let success_criteria = plan_success_criteria(&plan);
        let (branch_hints, branch_hints_error) =
            self.branch_hints(&plan, provider_id.as_deref()).await;
        let mut metadata = Map::new();
        metadata.insert(
            "source".to_string(),
            Value::String("intake.analyze".to_string()),
        );
        metadata.insert(
            "audit_domain".to_string(),
            Value::String(plan.project.audit_domain.as_str().to_string()),
        );
        metadata.insert(
            "pipeline_profile".to_string(),
            Value::String(plan.pipeline.profile.clone()),
        );
        metadata.insert(
            "raw_user_query".to_string(),
            Value::String(raw_input.raw_user_query.clone()),
        );
        if let Ok(constraints_json) = serde_json::to_value(&raw_input.structured_constraints) {
            metadata.insert("structured_constraints".to_string(), constraints_json);
        }
        if let Some(error) = &branch_hints_error {
            metadata.insert("branch_hints_error".to_string(), Value::String(error.clone()));
        }
        IntakeAnalyzeResponse {
            raw_input: raw_input.clone(),
            mission_draft: IntakeMissionDraft {
                user_goal: plan.project.goal.clone(),
                target: plan.project.target.clone(),
                constraints: constraints.clone(),
                success_criteria: success_criteria.clone(),
                goal_contract: plan.goal_contract.clone(),
                project: plan.project.clone(),
                pipeline: plan.pipeline.clone(),
                metadata,
            },
            plan: plan.clone(),
            target: string_target(&plan.project.target),
            constraints,
            success_criteria,
            recommended_audit_domains: if plan.pipeline.audit_domains.is_empty() {
                vec![plan.project.audit_domain]
            } else {
                plan.pipeline.audit_domains.clone()
            },
            recommended_branch_hints: branch_hints.clone(),
            suggested_branches: branch_hints,
            confidence: plan_confidence,
            rationale: plan_rationale,
            provider_id,
            model_invocation_id,
            used_fallback,
            fallback_reason,
        }
    }

    /// Python `_branch_hints`。
    ///
    /// 分支由模型生成；provider 运行时缺失或调用失败时返回空提示——
    /// 预览态不该因为模型不可用而整单失败。
    ///
    /// `provider_id` 必须是真的网关 provider id：`generate_structured` 按 id
    /// 解析路由，空串会让网关直接找不到 provider，模型根本没被调用。
    async fn branch_hints(
        &self,
        plan: &IntakePlan,
        provider_id: Option<&str>,
    ) -> (Vec<IntakeBranchHint>, Option<String>) {
        let Some(runtime) = self.manager.provider_runtime() else {
            return (Vec::new(), None);
        };
        let provider_id = provider_id.unwrap_or_default();
        let mut project = Project::new(plan.project.name.clone(), plan.project.audit_domain);
        project.description.clone_from(&plan.project.description);
        project.target = str_map(&plan.project.target);
        let mut mission = Mission::new(project.id.clone(), plan.project.goal.clone());
        mission.target = str_map(&plan.project.target);
        mission.constraints = plan_constraints(plan);
        mission.success_criteria = plan_success_criteria(plan);
        mission.created_by = "intake.preview".to_string();
        let branches = self
            .branch_generator
            .generate(
                runtime.as_ref(),
                provider_id,
                &BranchGenerationInput {
                    mission: &mission,
                    project: &project,
                    facts: &[],
                    hints: &[],
                    strategy_board_id: None,
                    knowledge_results: &[],
                    retrieval_invocation_id: None,
                    run_id: None,
                },
            )
            .await;
        // 分支提示失败不能拖垮预览态，但原因必须显式返回给调用方落到
        // `metadata.branch_hints_error`——静默吞掉会让"为什么没有分支"
        // 变成无法排查的黑洞。
        let (branches, branch_hints_error) = match branches {
            Ok(branches) => (branches, None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
        let audit_domains = if plan.pipeline.audit_domains.is_empty() {
            vec![plan.project.audit_domain]
        } else {
            plan.pipeline.audit_domains.clone()
        };
        let hints = branches
            .iter()
            .map(|branch| IntakeBranchHint {
                title: branch.title.clone(),
                hypothesis: branch.hypothesis.clone(),
                rationale: branch.rationale.clone(),
                branch_kind: branch
                    .metadata
                    .get("branch_kind")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                priority: branch.priority,
                confidence: branch.confidence,
                audit_domains: audit_domains.clone(),
            })
            .collect::<Vec<_>>();
        (hints, branch_hints_error)
    }

    /// Python `_messages`：provider 消息载荷（`raw_user_query` 原文直传）。
    ///
    /// 与 Python 的**有意**差异：`response_contract.output_fields` 枚举
    /// [`IntakePlan`] 的实际字段并声明未知顶层字段拒绝。早期契约只标
    /// `root` 类型名，模型无从得知字段清单，会照输入 payload 的形状编造
    /// 输出（实测 glm-5.3-flash 回显 `"root":"IntakePlan"` 与
    /// `raw_user_query`），被 `deny_unknown_fields` 整体拒绝。
    // 字段清单内嵌于 json! 字面量，行数随契约字段增长；`too_many_lines`
    // 豁免保持载荷构造单处可读。
    #[allow(clippy::too_many_lines)]
    fn messages(
        raw_input: &RawInputEnvelope,
        artifacts: &[models::ArtifactRecord],
    ) -> Vec<LlmMessage> {
        // 任务类型清单（按项目已有 solver 能力整理，中文口径为面向模型
        // 的分类说明；用户提供的清单 + 若干补充项）。形如
        // "web_sast — Web 白盒审计 / SAST"：模型按 id 输出。
        let supported_domains: Vec<String> = [
            (AuditDomain::AssetRecon, "资产测绘 / Web 入口发现（子域、端口、资产清点）"),
            (AuditDomain::WebRecon, "Web 侦察 / 攻击面发现（URL、目录、参数、JS 入口）"),
            (AuditDomain::ContentDiscovery, "内容发现（目录爆破、敏感文件与备份探测）"),
            (AuditDomain::FingerprintIntelligence, "指纹识别（技术栈、组件、版本识别）"),
            (AuditDomain::ExposureIntelligence, "暴露面情报（泄露凭据、公开暴露面盘查）"),
            (AuditDomain::WebSast, "Web 白盒审计 / SAST（源码静态分析）"),
            (AuditDomain::WebDast, "Web 黑盒审计 / DAST（黑盒动态扫描）"),
            (AuditDomain::WebIast, "Web 灰盒审计 / IAST（插桩交互式测试）"),
            (AuditDomain::WebValidation, "专项漏洞验证（指定漏洞类型的定向验证）"),
            (AuditDomain::ExploitabilityValidation, "可利用性验证（PoC 复现与危害证明）"),
            (AuditDomain::InternalSurface, "内网面（内网可达面与横向入口盘查）"),
            (AuditDomain::TrafficIntelligence, "流量情报（抓包/流量工件分析）"),
            (AuditDomain::CodeDeepSast, "深度代码审计（跨文件数据流/污点分析）"),
            (AuditDomain::BinaryStatic, "二进制静态分析（反编译/逆向）"),
            (AuditDomain::BinaryDynamic, "二进制动态分析（调试/运行时观测）"),
            (AuditDomain::Exploitability, "可利用性分析（exploit 编写与推演）"),
            (AuditDomain::Fuzzing, "Fuzzing（模糊测试，规划中）"),
            (AuditDomain::SupplyChain, "供应链审计（依赖/组件风险，规划中）"),
            (AuditDomain::CloudNative, "云原生配置审计（容器/K8s/IaC，规划中）"),
            (AuditDomain::Composite, "综合审计（跨域复合任务）"),
            (AuditDomain::Misc, "杂项（Base64/Hex 等编码解码、密码学小谜题、隐写、杂项 CTF 题——无法归入其他类型时选此项）"),
        ]
        .iter()
        .map(|(domain, label)| format!("{} — {}", domain.as_str(), label))
        .collect();
        let system_prompt = "You are Lynceus NaturalLanguageIntake. Analyze raw user input \
into a project draft and pipeline plan. Return exactly one JSON \
object matching the provided contract. Preserve raw_user_query \
as immutable source text. Do not translate, rewrite, normalize, \
or summarize raw_user_query. Do not include markdown. Do not call \
tools. Do not write databases. Do not execute commands. Every input \
is executable: when the target is vague or missing, keep the raw \
input as the goal, pick the closest audit domain, and never ask \
clarifying questions. Classify the requested end result into \
goal_contract. A CTF-style challenge that asks for its answer token \
is flag_capture regardless of language or phrasing. A request to \
discover or prove a security weakness is confirmed_finding. A \
mapping, inventory, or audit-completeness request is coverage. If \
the result cannot be expressed safely, choose custom with status \
needs_review and auto_complete false. Planner completion, exhausted \
budget, or an empty work queue never count as user-goal success.";
        let contract = serde_json::json!({
            "format": "strict_json_only",
            "root": "IntakePlan",
            // 模型必须拿到确切字段清单：早期契约只标 `root` 类型名，模型
            // 无从得知 IntakePlan 的形状，会照输入 payload 回显编造（实测
            // glm-5.3-flash 输出 `"root":"IntakePlan"` + `raw_user_query`），
            // 被 `deny_unknown_fields` 整体拒绝。解析失败已降级确定性回退。
            "output_fields": {
                "project": {
                    "name": "string, required — concise display title",
                    "audit_domain": "required, one of allowed_audit_domains",
                    "description": "string",
                    "goal": "string",
                    "target": "object with string values — use exactly one of the locator keys below as the key (for example {\"url\": \"https://app.example.test\"} or {\"domain\": \"example.test\"}); add extra descriptive keys only alongside a real locator, never instead of one",
                },
                "pipeline": {
                    "profile": "string",
                    "audit_domains": "array of allowed_audit_domains entries",
                    "start_mode": "string",
                    "config": "object",
                },
                "goal_contract": {
                    "outcome_type": "flag_capture | confirmed_finding | verified_evidence | coverage | custom",
                    "status": "resolved | needs_review",
                    "confidence": "number in [0, 1]",
                    "description": "string",
                    "rationale": "string",
                },
                "recommended_intents": "array of strings",
                "artifact_record_ids": "array of strings",
                "artifacts_summary": "array of objects",
                "confidence": "number in [0, 1]",
                "rationale": "string",
                "metadata": "object",
            },
            "unknown_top_level_fields": "rejected — return only output_fields keys",
            "allowed_audit_domains": supported_domains,
            // 定位键白名单：模型必须用这些键之一承载真实目标。自由发挥的键
            // （`raw_target` / `notes` / `subject` …）会被当成描述性元数据，
            // 下游资产嗅探提取不到目标。没有可识别目标时用 `raw_prompt`
            // 键把原文带下去——摄入绝不因此拒绝执行。
            "target_locator_keys": TARGET_LOCATOR_KEYS,
        });
        let mut payload = Map::new();
        payload.insert(
            "raw_user_query".to_string(),
            Value::String(raw_input.raw_user_query.clone()),
        );
        if let Ok(constraints) = serde_json::to_value(&raw_input.structured_constraints) {
            payload.insert("structured_constraints".to_string(), constraints);
        }
        payload.insert(
            "output_language_hint".to_string(),
            raw_input
                .output_language_hint
                .clone()
                .map_or(Value::Null, Value::String),
        );
        payload.insert(
            "region_hint".to_string(),
            raw_input
                .region_hint
                .clone()
                .map_or(Value::Null, Value::String),
        );
        payload.insert(
            "artifacts_summary".to_string(),
            Value::Array(
                engines::upload_intake::artifact_summaries(artifacts)
                    .into_iter()
                    .map(Value::Object)
                    .collect(),
            ),
        );
        payload.insert("response_contract".to_string(), contract);
        vec![
            LlmMessage {
                role: "system".to_string(),
                content: system_prompt.to_string(),
            },
            LlmMessage {
                role: "user".to_string(),
                content: engines::upload_intake::python_json_dumps(&Value::Object(payload)),
            },
        ]
    }
}

/// Python `_plan_with_raw_input`。
#[must_use]
pub fn plan_with_raw_input(mut plan: IntakePlan, raw_input: &RawInputEnvelope) -> IntakePlan {
    plan.metadata.insert(
        "raw_user_query".to_string(),
        Value::String(raw_input.raw_user_query.clone()),
    );
    if let Ok(constraints) = serde_json::to_value(&raw_input.structured_constraints) {
        plan.metadata
            .insert("structured_constraints".to_string(), constraints);
    }
    if let Some(hint) = raw_input.output_language_hint.clone() {
        plan.metadata
            .insert("output_language_hint".to_string(), Value::String(hint));
    }
    if let Some(hint) = raw_input.region_hint.clone() {
        plan.metadata
            .insert("region_hint".to_string(), Value::String(hint));
    }
    if !raw_input.metadata.is_empty() {
        plan.metadata.insert(
            "raw_input_metadata".to_string(),
            Value::Object(raw_input.metadata.clone()),
        );
    }
    plan.project.description = Some(raw_input.raw_user_query.clone());
    plan.project.goal.clone_from(&raw_input.raw_user_query);
    normalized_plan(plan)
}

/// Python `_plan_with_artifacts`。
#[must_use]
pub fn plan_with_artifacts(plan: IntakePlan, artifacts: &[models::ArtifactRecord]) -> IntakePlan {
    if artifacts.is_empty() {
        return normalized_plan(plan);
    }
    let summaries = engines::upload_intake::artifact_summaries(artifacts);
    let target_type = engines::upload_intake::artifact_target_type(artifacts);
    let audit_domain = engines::upload_intake::audit_domain_for_target_type(target_type);
    let mut target = plan.project.target.clone();
    for (key, value) in engines::upload_intake::mission_target_updates(artifacts) {
        target.insert(key, value);
    }
    let mut pipeline = plan.pipeline.clone();
    let audit_domains = if audit_domain == AuditDomain::Composite {
        if pipeline.audit_domains.is_empty() {
            vec![AuditDomain::Composite]
        } else {
            pipeline.audit_domains.clone()
        }
    } else {
        vec![audit_domain]
    };
    if audit_domain == AuditDomain::TrafficIntelligence
        && !pipeline.config.contains_key("traffic_intelligence")
        && let Some(traffic) = artifacts.iter().find(|artifact| {
            artifact
                .metadata
                .get("detected_input_type")
                .and_then(Value::as_str)
                == Some("traffic_capture")
        })
    {
        pipeline.config.insert(
            "traffic_intelligence".to_string(),
            Value::Object(value_map(&[
                ("artifact_path", Value::String(traffic.uri.clone())),
                (
                    "format",
                    Value::String(traffic_format_from_artifact(traffic)),
                ),
                (
                    "artifact_record_id",
                    Value::String(traffic.id.as_str().to_string()),
                ),
            ])),
        );
    }
    pipeline.profile = profile_for_audit_domain(audit_domain, &pipeline.profile);
    pipeline.audit_domains = audit_domains;
    let intents = {
        let mut desired: Vec<String> = Vec::new();
        match target_type {
            "traffic" => {
                desired.push("Import observed traffic into the audit graph".to_string());
            }
            "binary" => {
                desired.push("Run binary static triage".to_string());
            }
            "source" => {
                desired.push("Run fast source security scan".to_string());
            }
            "mixed" => {
                desired.push("Triage uploaded artifacts and select audit paths".to_string());
            }
            _ => {}
        }
        merge_ids(&[plan.recommended_intents.clone(), desired])
    };
    let rationale = plan
        .rationale
        .clone()
        .unwrap_or_else(|| "Artifact metadata was included in intake context.".to_string());
    let mut updated = plan;
    updated.project.audit_domain = audit_domain;
    updated.project.target = target;
    updated.pipeline = pipeline;
    updated.recommended_intents = intents;
    updated.artifact_record_ids = artifacts
        .iter()
        .map(|artifact| artifact.id.as_str().to_string())
        .collect();
    updated.artifacts_summary = summaries
        .into_iter()
        .map(Value::Object)
        .collect::<Vec<_>>()
        .into_iter()
        .map(|value| match value {
            Value::Object(fields) => fields,
            _ => Map::new(),
        })
        .collect();
    updated.rationale = Some(rationale);
    normalized_plan(updated)
}

/// Python `_audit_config`：从目标推导可执行的默认域配置
/// （Python 同构 113 行；`too_many_lines` 豁免保持镜像）。
///
/// # Errors
/// 无任何域可从目标推导出安全的可执行默认配置（HTTP 422）。
#[allow(clippy::too_many_lines)]
pub fn audit_config(plan: &IntakePlan) -> Result<Map<String, Value>, IntakeServiceError> {
    // 统一派发：不再按域推导引擎配置（旧版的 nuclei/semgrep 等域
    // 参数表已删除）。工具由 Agent 经 lynceus MCP 自助发现——tool_list /
    // tool_search 拿到清单，tool_describe 拿到参数 schema，tool_execute
    // 调用；模型在 pipeline.config 里显式给定的对象原样透传。
    let mut config = plan.pipeline.config.clone();
    let domains: Vec<AuditDomain> = if plan.pipeline.audit_domains.is_empty() {
        vec![plan.project.audit_domain]
    } else {
        plan.pipeline.audit_domains.clone()
    };
    config.insert(
        "requested_audit_domains".to_string(),
        Value::Array(
            domains
                .iter()
                .map(|domain| Value::String(domain.as_str().to_string()))
                .collect(),
        ),
    );
    config.insert(
        "audit_domains".to_string(),
        Value::Array(
            domains
                .iter()
                .map(|domain| Value::String(domain.as_str().to_string()))
                .collect(),
        ),
    );
    Ok(config)
}

// ---------------------------------------------------------------------------
// 路由（Python routes/intake.py）
// ---------------------------------------------------------------------------

use crate::ApiError;
use crate::ApiState;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;

pub(crate) fn map_intake_error(error: IntakeServiceError) -> ApiError {
    use runtime::EngineError;
    match error {
        IntakeServiceError::ArtifactNotFound(detail) => ApiError(EngineError::ProjectNotFound(
            format!("unknown artifact: {detail}"),
        )),
        IntakeServiceError::MissionNotFound(detail) => {
            ApiError(EngineError::MissionNotFound(detail))
        }
        IntakeServiceError::ExecutionMode(detail) | IntakeServiceError::SolverConfig(detail) => {
            ApiError(EngineError::Value(detail))
        }
        IntakeServiceError::Engine(error) => ApiError(error),
        IntakeServiceError::Storage(error) => ApiError(EngineError::Storage(error)),
    }
}

/// `POST /intake/analyze`。
///
/// # Errors
/// 引用工件缺失（404 语义）。
pub(crate) async fn analyze_intake(
    State(state): State<ApiState>,
    Json(body): Json<IntakeAnalyzeRequest>,
) -> Result<Json<IntakeAnalyzeResponse>, ApiError> {
    let service = IntakeService::new(
        Arc::clone(&state.manager),
        state.mission_workspace_root.clone(),
    );
    let response = service.analyze(&body).await.map_err(map_intake_error)?;
    Ok(Json(response))
}

/// `POST /intake/create-project`。
///
/// # Errors
/// 引用缺失（404）、S0/S1 语义（422）或引擎错误。
pub(crate) async fn create_intake_project(
    State(state): State<ApiState>,
    Json(body): Json<IntakeStartRequest>,
) -> Result<(StatusCode, Json<IntakeStartResponse>), ApiError> {
    let service = IntakeService::new(
        Arc::clone(&state.manager),
        state.mission_workspace_root.clone(),
    );
    let response = service
        .create_project(&body)
        .await
        .map_err(map_intake_error)?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// `POST /intake/start`。
///
/// # Errors
/// 引用缺失（404）、S0/S1 语义、审计配置无法推导（422）或引擎错误。
pub(crate) async fn start_intake(
    State(state): State<ApiState>,
    Json(body): Json<IntakeStartRequest>,
) -> Result<(StatusCode, Json<IntakeStartResponse>), ApiError> {
    let service = IntakeService::new(
        Arc::clone(&state.manager),
        state.mission_workspace_root.clone(),
    );
    let response = Box::pin(service.start(&body))
        .await
        .map_err(map_intake_error)?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// `POST /intake/async`：提交即返回草稿 Mission，模型分析在后台完成。
pub(crate) async fn async_intake(
    State(state): State<ApiState>,
    Json(body): Json<IntakeAsyncRequest>,
) -> Result<(StatusCode, Json<IntakeAsyncResponse>), ApiError> {
    let service = IntakeService::new(
        Arc::clone(&state.manager),
        state.mission_workspace_root.clone(),
    );
    let response = service.start_async(&body).await.map_err(map_intake_error)?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_provider_plan_rejects_model_echoed_envelope() {
        // 真实事故形状（2026-09，z-ai/glm-5.3-flash via Cline）：模型把
        // response_contract 里的 `"root": "IntakePlan"` 与输入键照抄进输出，
        // 不含任何 IntakePlan 字段。
        let raw = serde_json::json!({
            "root": "IntakePlan",
            "raw_user_query": "hi",
            "raw_user_query_preserved": true,
            "intent_assessment": {
                "classification": "non_actionable_greeting",
                "confidence": 0.98,
            },
        });
        let error =
            IntakeService::validate_provider_plan(raw.as_object().expect("envelope is an object"))
                .expect_err("模型回显 envelope 必须被拒绝");
        let message = error.to_string();
        assert!(message.contains("ValidationError"), "actual: {message}");
        assert!(
            message.contains("unknown field `root`"),
            "actual: {message}"
        );
    }

    #[test]
    fn validate_provider_plan_accepts_bare_plan_and_marks_goal_contract() {
        let raw = serde_json::json!({
            "project": {
                "name": "Login surface audit",
                "audit_domain": "web_dast",
            },
            "pipeline": {"profile": "web_full"},
            "goal_contract": {"outcome_type": "confirmed_finding", "confidence": 0.9},
            "confidence": 0.8,
        });
        let plan =
            IntakeService::validate_provider_plan(raw.as_object().expect("plan is an object"))
                .expect("裸 IntakePlan 形状必须可解析");
        assert_eq!(plan.project.audit_domain, AuditDomain::WebDast);
        // 模型提供的 goal_contract 强制走 intake 来源与 needs_review 门。
        assert_eq!(plan.goal_contract.source, GoalContractSource::IntakeModel);
        assert_eq!(plan.goal_contract.status, GoalContractStatus::NeedsReview);
        assert!(!plan.goal_contract.auto_complete);
    }


    #[test]
    fn unknown_prompt_falls_back_to_composite_without_questions() {
        let plan = unknown_plan("do the thing please");
        assert_eq!(plan.project.audit_domain, AuditDomain::Composite);
        assert_eq!(
            plan.project
                .target
                .get("raw_prompt")
                .and_then(Value::as_str),
            Some("do the thing please")
        );
        assert_eq!(plan.recommended_intents, ["do the thing please"]);
        assert!((plan.confidence - 0.35).abs() < 1e-12);
        // 未分类目标的完成契约必须拒绝自动完成。
        assert!(!plan.goal_contract.auto_complete);
        assert_eq!(plan.goal_contract.status, GoalContractStatus::NeedsReview);
    }

    /// 用户真实输入"你好"时走的就是这条：摄入落到 `unknown_plan`，原文
    /// 原样带成目标直接派发——不提问、不拦截、不分类。
    #[test]
    fn greeting_input_produces_a_runnable_composite_plan() {
        let plan = unknown_plan("你好");
        assert_eq!(
            plan.project.target.get("raw_prompt").and_then(Value::as_str),
            Some("你好")
        );
        assert_eq!(plan.project.goal, "你好");
        assert_eq!(plan.recommended_intents, ["你好"]);
        assert_eq!(plan.project.audit_domain, AuditDomain::Composite);
        assert_eq!(plan.goal_contract.status, GoalContractStatus::NeedsReview);
    }

    #[test]
    fn structured_constraints_extracted_without_rewriting_query() {
        // 中文表述（"8080 端口"）不触发端口正则——与 Python 实际行为一致。
        let raw = "扫描 https://app.example.test，不要扫描 8080 端口，不要做 exploit";
        let constraints = extract_structured_constraints(raw);
        assert!(constraints.forbidden_ports.is_empty());
        assert!(constraints.forbidden_actions.is_empty());
        assert!(
            constraints
                .in_scope
                .iter()
                .any(|item| item.contains("app.example.test"))
        );
        // 英文表述触发端口与动作提取。
        let english = extract_structured_constraints(
            "scan https://app.example.test, do not scan port 8080, do not exploit",
        );
        assert_eq!(english.forbidden_ports, ["8080"]);
        // "do not scan" 同样命中 scan 排除词。
        assert_eq!(english.forbidden_actions, ["exploit", "scan"]);
        // 约束提取不改写原文：note 只记录提示，不带改写查询。
        assert!(constraints.notes[0].contains("preserved without rewrite"));
        let envelope = raw_input_envelope(raw);
        assert_eq!(envelope.raw_user_query, raw, "原文必须逐字保留");
        assert_eq!(envelope.output_language_hint.as_deref(), Some("zh-CN"));
    }

    #[test]
    fn plan_with_raw_input_records_metadata_and_keeps_goal_verbatim() {
        let raw = "评估 https://app.example.test/login 的Web漏洞";
        let envelope = raw_input_envelope(raw);
        let plan = plan_with_raw_input(unknown_plan(raw), &envelope);
        assert_eq!(plan.project.goal, raw);
        assert_eq!(plan.project.description.as_deref(), Some(raw));
        assert_eq!(
            plan.metadata.get("raw_user_query").and_then(Value::as_str),
            Some(raw)
        );
        assert!(plan.metadata.contains_key("structured_constraints"));
        // 复杂度重算写入评估。
        let service_plan = normalized_plan(plan);
        assert!(service_plan.metadata.contains_key("raw_user_query"));
    }

    #[test]
    fn canonical_target_promotes_challenge_url_to_url_key() {
        let mut target = Map::new();
        target.insert(
            "challenge_url".to_string(),
            Value::String("https://ctf.example.test".to_string()),
        );
        target.insert(
            "internal_url".to_string(),
            Value::String("http://10.0.0.1:8080".to_string()),
        );
        let normalized = canonical_target(string_target(&target));
        assert_eq!(
            normalized.get("url").and_then(Value::as_str),
            Some("https://ctf.example.test")
        );
        // SSRF 内网地址保留为次级元数据。
        assert!(normalized.contains_key("internal_url"));
    }

    #[test]

    #[test]
    fn plan_with_artifacts_redirects_traffic_target() {
        let plan = unknown_plan("Audit https://app.example.test/login");
        let mut artifact = models::ArtifactRecord::new("/tmp/capture.pcap".to_string());
        artifact.metadata.insert(
            "detected_input_type".to_string(),
            Value::String("traffic_capture".to_string()),
        );
        artifact.metadata.insert(
            "detected_target_type".to_string(),
            Value::String("traffic".to_string()),
        );
        artifact
            .metadata
            .insert("extension".to_string(), Value::String(".pcap".to_string()));
        let updated = plan_with_artifacts(plan, std::slice::from_ref(&artifact));
        // 上传的是流量工件，目标类型由工件本身给出 → TrafficIntelligence
        //（不再把用户目标与工件"混类"降级成 Composite）。
        assert_eq!(
            updated.project.audit_domain,
            AuditDomain::TrafficIntelligence
        );
        assert!(updated.project.target.contains_key("traffic_artifact"));
        assert_eq!(
            updated.artifact_record_ids,
            [artifact.id.as_str().to_string()]
        );
        // 流量工件追加流量导入意图（Python `_intents_with_artifacts`）。
        assert!(
            updated
                .recommended_intents
                .iter()
                .any(|intent| intent.contains("Import observed traffic"))
        );
    }
}

/// WP4 提示词 parity：内置预设（resources/prompts 文件）渲染结果与改造前
/// 硬编码逐字节一致。这两条锁住「任务顾问」与「Intake 分析」两侧；
/// 「任务执行」侧的 parity 守护在 `engines/worker/dispatch.rs`。
#[cfg(test)]
mod prompt_parity_tests {
    #![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
    use models::agent_preset::{builtin_templates, render_template};
    use serde_json::json;

    /// 改造前「任务顾问」指令的冻结参照物（与旧 `format!` 字面量逐字节
    /// 一致，含源码里的 13/14 空格续行缩进与插值位置）。
    fn advisor_v1_reference(context_hint: &str, question: &str) -> String {
        format!(
            "你是 Lynceus 任务顾问（只读侧分析 worker）。规则：\n             1. 先用 blackboard_read 读任务白板了解当前状态；需要背景知识可用 knowledge_search。\n             2. 用中文简洁回答，基于证据，不编造不存在的发现；证据不足就直说。\n             3. 若结论值得保留，用 blackboard_append 写回白板一条，参数必须是：kind=\"summary\"、             idempotency_key=\"advise-<时间戳>（自拟唯一值）、content=<结论>；kind 只能是              task/hypothesis/observation/artifact_ref/evidence_ref/question/blocker/decision/summary 之一。\n             4. 最终回答：先一句话结论，然后要点列表。\n\n             任务上下文：\n{context_hint}\n\n操作员问题：{question}"
        )
    }

    #[test]
    fn mission_advisor_builtin_renders_v1_byte_for_byte() {
        let context_hint = "任务目标：demo\n目标：{}\n任务状态：running";
        let question = "当前分支进展如何？";
        let vars = serde_json::Map::from_iter([
            ("context_hint".to_string(), json!(context_hint)),
            ("question".to_string(), json!(question)),
            ("history".to_string(), json!("")),
        ]);
        let rendered = render_template(builtin_templates::MISSION_ADVISOR_V1, &vars);
        assert_eq!(
            rendered,
            advisor_v1_reference(context_hint, question),
            "无对话历史时必须与旧硬编码逐字节一致"
        );
    }

    #[test]
    fn mission_advisor_history_block_appends_like_v1() {
        // 旧实现：history 非空时 push "\n\n最近对话：" + 逐条 "\n{role}: {content}"。
        let history = "\n\n最近对话：\nuser: 白板上有什么？\nassistant: 0 条。";
        let vars = serde_json::Map::from_iter([
            ("context_hint".to_string(), json!("H")),
            ("question".to_string(), json!("Q")),
            ("history".to_string(), json!(history)),
        ]);
        let rendered = render_template(builtin_templates::MISSION_ADVISOR_V1, &vars);
        let mut expected = advisor_v1_reference("H", "Q");
        expected.push_str(history);
        assert_eq!(rendered, expected, "对话历史追加位置与旧实现一致");
    }

    #[test]
    fn intake_analyze_builtin_matches_frozen_reference() {
        // 冻结参照：内置 analyze 系统提示词为纯静态文本（无变量占位，
        // 渲染必须是恒等）。锚点为 v2 中文版的关键句——2026-09 随
        // 「内置预设中文化」一并更新，此前锁的是英文 v1 锚点。
        let text = builtin_templates::INTAKE_ANALYZE_V1;
        assert!(text.starts_with("你是 Lynceus 的 NaturalLanguageIntake。"));
        assert!(text.contains("严格返回一个符合所提供契约的 JSON 对象"));
        assert!(text.contains("不可变的源文本"));
        assert!(text.contains("flag_capture"));
        assert!(text.contains("confirmed_finding"));
        assert!(text.contains("coverage"));
        assert!(text.contains("needs_review"));
        assert!(text.ends_with("都不算用户目标达成。"));
        assert!(
            !text.contains("{{"),
            "analyze 提示词为纯静态文本，不得含模板占位"
        );
        assert_eq!(render_template(text, &serde_json::Map::new()), text);
    }
}

#[cfg(test)]
mod readonly_retest_tests {
    use super::*;

    #[test]
    fn readonly_allowlist_contains_no_write_tool() {
        for tool in READONLY_RETEST_TOOLS {
            assert!(
                !WRITE_TOOLS.contains(&tool),
                "只读白名单里出现了写工具 {tool}：复测会污染任务白板"
            );
        }
        // 反向也锁一遍：写工具一个都不许进。
        for tool in WRITE_TOOLS {
            assert!(
                !READONLY_RETEST_TOOLS.contains(&tool),
                "写工具 {tool} 不得出现在复测只读白名单里"
            );
        }
    }

    #[test]
    fn readonly_allowlist_is_the_three_read_tools() {
        assert_eq!(
            READONLY_RETEST_TOOLS,
            ["blackboard_read", "knowledge_search", "knowledge_get"]
        );
    }
}
