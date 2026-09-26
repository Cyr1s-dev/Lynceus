//! 领域可复用逻辑：受限 argv 构造（白名单参数、`--` 终结符、逐项校验）
//! 与归一化（Evidence + Finding，CANDIDATE 状态、带稳定指纹）。
//! 执行与编排统一走 [`crate::harness`]（`ToolGateway` 无 shell 执行、
//! 工件落盘、审计）；本模块不再承载 solve 流程。
//! 红线：不伪造 source 标签；失败保持审计链，绝不静默吞掉。

use agents::solver::SolverContext;
use models::{
    CodeLocation, Evidence, EvidenceKind, Fact, Finding, FindingStatus, GraphNodeType, ProjectId,
    RunId, Severity, TaskId,
};
use serde_json::{Map, Value};
use sha2::Digest;

pub(crate) mod content_discovery;

/// 归一化输出：Evidence + Finding 对（Python `NormalizedResults`）。
#[derive(Debug, Default)]
pub struct NormalizedResults {
    /// 证据链。
    pub evidence: Vec<Evidence>,
    /// 候选发现。
    pub findings: Vec<Finding>,
}

/// mission config 优先、catalog settings.params 兜底的参数视图。
///
/// 兜底只在 mission config 缺 key、值为 `null` 或类型读取失败时生效；
/// mission 显式提供的值永远优先（设计契约第 5 条：solver 消费优先级）。
/// 适配器构造 argv 前用它取代裸的 `section.get(...)` 链。
pub(crate) struct MissionFirstParams<'a> {
    mission: &'a Map<String, Value>,
    catalog: Option<&'a Map<String, Value>>,
}

impl<'a> MissionFirstParams<'a> {
    pub(crate) fn new(
        mission: &'a Map<String, Value>,
        catalog: Option<&'a Map<String, Value>>,
    ) -> Self {
        Self { mission, catalog }
    }

    fn value(&self, key: &str) -> Option<&Value> {
        self.mission
            .get(key)
            .filter(|value| !value.is_null())
            .or_else(|| {
                self.catalog
                    .and_then(|catalog| catalog.get(key))
                    .filter(|value| !value.is_null())
            })
    }

    /// 字符串参数（string / path 共用读取形态）。
    pub(crate) fn string(&self, key: &str) -> Option<&str> {
        self.value(key).and_then(Value::as_str)
    }

    /// 非负整数参数。
    pub(crate) fn integer(&self, key: &str) -> Option<u64> {
        self.value(key).and_then(Value::as_u64)
    }

    /// 布尔参数。
    pub(crate) fn boolean(&self, key: &str) -> Option<bool> {
        self.value(key).and_then(Value::as_bool)
    }

    /// 字符串列表参数。mission 数组按既有语义过滤字符串项；mission 提供
    /// 非数组值时保持既有语义（整键忽略），不回落 catalog；仅当 mission
    /// 缺 key 或为 `null` 时才读 catalog 的 `string_list` 配置。
    pub(crate) fn string_list(&self, key: &str) -> Option<Vec<String>> {
        match self.mission.get(key) {
            Some(Value::Array(items)) => Some(
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
            ),
            Some(_) => Some(Vec::new()),
            None => match self.value(key)? {
                Value::Array(items) => Some(
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect(),
                ),
                _ => None,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// web_sast · Semgrep
// ---------------------------------------------------------------------------

/// Python `_SEVERITY_MAP`（semgrep）。
fn semgrep_severity(raw: Option<&str>) -> Severity {
    match raw.map(str::to_uppercase).as_deref() {
        Some("ERROR") => Severity::High,
        Some("INFO") => Severity::Low,
        _ => Severity::Medium,
    }
}

/// Python `_first_cwe`（semgrep metadata）。
fn first_cwe(metadata: &Map<String, Value>) -> Option<String> {
    match metadata.get("cwe") {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(items)) => items.first().and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}

/// Python `_fingerprint`：优先 semgrep 自带指纹，否则哈希 `check_id`+`path`+行号。
fn semgrep_fingerprint(result: &Map<String, Value>) -> String {
    if let Some(Value::String(fp)) = result
        .get("extra")
        .and_then(|extra| extra.get("fingerprint"))
        .filter(|fp| fp.is_string())
        && !fp.is_empty()
    {
        return fp.clone();
    }
    let start = result.get("start").and_then(Value::as_object);
    let end = result.get("end").and_then(Value::as_object);
    let line_to_string =
        |value: Option<i64>| value.map_or_else(String::new, |line| line.to_string());
    let basis = [
        result
            .get("check_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        result
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        line_to_string(
            start
                .and_then(|start| start.get("line"))
                .and_then(Value::as_i64),
        ),
        line_to_string(end.and_then(|end| end.get("line")).and_then(Value::as_i64)),
    ]
    .join("|");
    let digest = sha2::Sha256::digest(basis.as_bytes());
    let hex = format!("semgrep:{digest:x}");
    hex[..23].to_string()
}

/// Python `_source_snippet`：redacted 输出回退到本地文件重建。
fn source_snippet(
    path: &str,
    start_line: Option<i64>,
    end_line: Option<i64>,
    raw: Option<&str>,
) -> Option<String> {
    if let Some(raw) = raw {
        let trimmed = raw.trim();
        if !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("requires login") {
            return Some(raw.to_string());
        }
    }
    let Some(start_line) = start_line else {
        return raw.filter(|raw| !raw.trim().is_empty()).map(str::to_string);
    };
    let mut end = end_line.unwrap_or(start_line);
    if end < start_line {
        end = start_line;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return raw.filter(|raw| !raw.trim().is_empty()).map(str::to_string);
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = usize::try_from(start_line.saturating_sub(1))
        .unwrap_or(usize::MAX)
        .min(lines.len());
    let end = usize::try_from(end).unwrap_or(usize::MAX).min(lines.len());
    let selected = &lines[start..end];
    if selected.is_empty() {
        None
    } else {
        Some(selected.join("\n"))
    }
}

/// Python `semgrep_normalizer.normalize`：Semgrep JSON → Evidence + Finding。
#[must_use]
pub fn normalize_semgrep(
    payload: &Map<String, Value>,
    project_id: &ProjectId,
    run_id: Option<&RunId>,
    task_id: Option<&TaskId>,
    scan_fact_id: Option<&str>,
) -> NormalizedResults {
    let mut out = NormalizedResults::default();
    let Some(results) = payload.get("results").and_then(Value::as_array) else {
        return out;
    };
    let supports: Vec<String> = scan_fact_id
        .map(|id| vec![id.to_string()])
        .unwrap_or_default();

    for result in results {
        let Some(result) = result.as_object() else {
            continue;
        };
        let check_id = result
            .get("check_id")
            .and_then(Value::as_str)
            .unwrap_or("semgrep.unknown");
        let path = result
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("<unknown>");
        let extra = result
            .get("extra")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let metadata = extra
            .get("metadata")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let message = extra
            .get("message")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .unwrap_or(check_id);
        let start = result.get("start").and_then(Value::as_object);
        let end = result.get("end").and_then(Value::as_object);
        let start_line = start
            .and_then(|start| start.get("line"))
            .and_then(Value::as_i64);
        let end_line = end.and_then(|end| end.get("line")).and_then(Value::as_i64);

        let mut evidence = Evidence::new(
            project_id.clone(),
            EvidenceKind::SourceSnippet,
            message.chars().take(500).collect(),
        );
        evidence.run_id = run_id.cloned();
        evidence.produced_by_task_id = task_id.cloned();
        evidence.content = Map::from_iter([
            ("engine".to_string(), Value::String("semgrep".to_string())),
            ("check_id".to_string(), Value::String(check_id.to_string())),
            (
                "severity".to_string(),
                extra.get("severity").cloned().unwrap_or(Value::Null),
            ),
            ("metadata".to_string(), Value::Object(metadata.clone())),
        ]);
        evidence.locations = vec![CodeLocation {
            artifact: path.to_string(),
            start_line,
            end_line,
            address: None,
            symbol: None,
            snippet: source_snippet(
                path,
                start_line,
                end_line,
                extra.get("lines").and_then(Value::as_str),
            ),
        }];
        evidence.supports_fact_ids.clone_from(&supports);

        let mut finding = Finding::new(project_id.clone(), message.chars().take(200).collect());
        finding.run_id = run_id.cloned();
        finding.produced_by_task_id = task_id.cloned();
        finding.description = Some(message.to_string());
        finding.severity = semgrep_severity(extra.get("severity").and_then(Value::as_str));
        finding.status = FindingStatus::Candidate;
        finding.cwe = first_cwe(&metadata);
        finding.rule_id = Some(check_id.to_string());
        finding.evidence_ids = vec![evidence.id.as_str().to_string()];
        finding.related_fact_ids.clone_from(&supports);
        finding.sink_label = Some(check_id.to_string());
        finding.fingerprint = Some(semgrep_fingerprint(result));

        out.evidence.push(evidence);
        out.findings.push(finding);
    }
    out
}

// ---------------------------------------------------------------------------
// web_dast · Nuclei
// ---------------------------------------------------------------------------

/// Python `_SEVERITY_MAP`（nuclei，未匹配一律 INFO）。
fn nuclei_severity(raw: Option<&str>) -> Severity {
    match raw.map(str::trim).map(str::to_lowercase).as_deref() {
        Some("critical") => Severity::Critical,
        Some("high") => Severity::High,
        Some("medium") => Severity::Medium,
        Some("low") => Severity::Low,
        _ => Severity::Info,
    }
}

/// Python `_SNIPPET_LIMIT`。
const NUCLEI_SNIPPET_LIMIT: usize = 2000;

/// Python `_snippet`：超长截断。
fn nuclei_snippet(raw: Option<&Value>) -> Option<String> {
    let text = raw?.as_str()?.trim().to_string();
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= NUCLEI_SNIPPET_LIMIT {
        Some(text)
    } else {
        Some(text.chars().take(NUCLEI_SNIPPET_LIMIT).collect::<String>() + "...[truncated]")
    }
}

/// Python `_normalize_cwe`。
fn normalize_cwe(value: &str) -> String {
    let upper = value.to_uppercase();
    if upper.starts_with("CWE-") {
        upper
    } else if value.chars().all(|c| c.is_ascii_digit()) {
        format!("CWE-{value}")
    } else {
        value.to_string()
    }
}

/// Python `_extract_cwe`：classification 优先，其次 metadata。
fn nuclei_cwe(info: &Map<String, Value>) -> Option<String> {
    let first_cwe_value = |value: Option<&Value>| -> Option<String> {
        match value? {
            Value::String(text) if !text.trim().is_empty() => Some(normalize_cwe(text.trim())),
            Value::Array(items) => items.iter().find_map(|item| {
                item.as_str()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(normalize_cwe)
            }),
            _ => None,
        }
    };
    let classification = info.get("classification").and_then(Value::as_object);
    if let Some(classification) = classification {
        for key in ["cwe-id", "cwe_id", "cwe"] {
            if let Some(cwe) = first_cwe_value(classification.get(key)) {
                return Some(cwe);
            }
        }
    }
    let metadata = info.get("metadata").and_then(Value::as_object);
    if let Some(metadata) = metadata {
        for key in ["cwe", "cwe-id", "cwe_id"] {
            if let Some(cwe) = first_cwe_value(metadata.get(key)) {
                return Some(cwe);
            }
        }
    }
    None
}

/// Python `_fingerprint`（nuclei）。
fn nuclei_fingerprint(template_id: &str, matched_at: Option<&str>, host: Option<&str>) -> String {
    let basis = [
        template_id,
        matched_at.unwrap_or_default(),
        host.unwrap_or_default(),
    ]
    .join("|");
    let digest = sha2::Sha256::digest(basis.as_bytes());
    let hex = format!("nuclei:{digest:x}");
    hex[..23].to_string()
}

/// Python `nuclei_normalizer.normalize`：JSONL 记录 → scan Fact + Evidence/Finding
/// （Python 同构 90 行；`too_many_lines` 豁免保持镜像）。
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn normalize_nuclei(
    results: &[Map<String, Value>],
    project_id: &ProjectId,
    target: &str,
    parse_warning_count: usize,
    run_id: Option<&RunId>,
    task_id: Option<&TaskId>,
    tool_invocation_id: Option<&str>,
) -> (Fact, NormalizedResults) {
    let mut scan_fact = Fact::new(
        project_id.clone(),
        "nuclei_scan_completed".to_string(),
        format!("Nuclei scan completed for {target}"),
    );
    scan_fact.node_type = GraphNodeType::Fact;
    scan_fact.data = Map::from_iter([
        ("target".to_string(), Value::String(target.to_string())),
        (
            "result_count".to_string(),
            Value::Number(i64::try_from(results.len()).unwrap_or(i64::MAX).into()),
        ),
        (
            "parse_warning_count".to_string(),
            Value::Number(
                i64::try_from(parse_warning_count)
                    .unwrap_or(i64::MAX)
                    .into(),
            ),
        ),
        ("engine".to_string(), Value::String("nuclei".to_string())),
    ]);

    let mut out = NormalizedResults::default();
    for item in results {
        let template_id = item
            .get("template-id")
            .or_else(|| item.get("template_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty());
        let Some(template_id) = template_id else {
            continue;
        };
        let info = item
            .get("info")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let template_name = info
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or(template_id);
        let matcher_name = item
            .get("matcher-name")
            .or_else(|| item.get("matcher_name"))
            .and_then(Value::as_str);
        let matched_at = item
            .get("matched-at")
            .or_else(|| item.get("matched_at"))
            .and_then(Value::as_str);
        let host = item.get("host").and_then(Value::as_str);
        let severity = nuclei_severity(info.get("severity").and_then(Value::as_str));
        let metadata = info
            .get("metadata")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let classification = info
            .get("classification")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let where_at = matched_at.or(host).unwrap_or(target);

        let mut evidence = Evidence::new(
            project_id.clone(),
            EvidenceKind::ToolOutput,
            format!("Nuclei matched {template_id} at {where_at}")
                .chars()
                .take(500)
                .collect(),
        );
        evidence.run_id = run_id.cloned();
        evidence.produced_by_task_id = task_id.cloned();
        evidence.produced_by_tool_invocation_id =
            tool_invocation_id.map(|id| models::ToolInvocationId::new(id.to_string()));
        evidence.content = Map::from_iter([
            ("engine".to_string(), Value::String("nuclei".to_string())),
            (
                "template_id".to_string(),
                Value::String(template_id.to_string()),
            ),
            (
                "template_name".to_string(),
                Value::String(template_name.to_string()),
            ),
            (
                "matcher_name".to_string(),
                matcher_name.map_or(Value::Null, |name| Value::String(name.to_string())),
            ),
            (
                "matched_at".to_string(),
                matched_at.map_or(Value::Null, |at| Value::String(at.to_string())),
            ),
            (
                "host".to_string(),
                host.map_or(Value::Null, |host| Value::String(host.to_string())),
            ),
            (
                "request".to_string(),
                nuclei_snippet(item.get("request")).map_or(Value::Null, Value::String),
            ),
            (
                "response".to_string(),
                nuclei_snippet(item.get("response")).map_or(Value::Null, Value::String),
            ),
            ("metadata".to_string(), Value::Object(metadata)),
            ("classification".to_string(), Value::Object(classification)),
        ]);
        evidence.locations = vec![CodeLocation {
            artifact: where_at.to_string(),
            start_line: None,
            end_line: None,
            address: None,
            symbol: None,
            snippet: None,
        }];
        evidence.supports_fact_ids = vec![scan_fact.id.as_str().to_string()];

        let description = info
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map_or_else(
                || format!("Nuclei template {template_id} ({template_name}) matched {where_at}."),
                str::to_string,
            );

        let mut finding = Finding::new(
            project_id.clone(),
            template_name.chars().take(200).collect(),
        );
        finding.run_id = run_id.cloned();
        finding.produced_by_task_id = task_id.cloned();
        finding.description = Some(description);
        finding.severity = severity;
        finding.status = FindingStatus::Candidate;
        finding.cwe = nuclei_cwe(&info);
        finding.rule_id = Some(format!("nuclei:{template_id}"));
        finding.evidence_ids = vec![evidence.id.as_str().to_string()];
        finding.related_fact_ids = vec![scan_fact.id.as_str().to_string()];
        finding.sink_label = Some(template_id.to_string());
        finding.fingerprint = Some(nuclei_fingerprint(template_id, matched_at, host));

        out.evidence.push(evidence);
        out.findings.push(finding);
    }
    (scan_fact, out)
}

/// 解析 nuclei `-jsonl` 输出：逐行 JSON，坏行计入 parse warnings
/// （Python `nuclei_normalizer` 前置解析的镜像）。
#[must_use]
pub fn parse_nuclei_jsonl(stdout: &str) -> (Vec<Map<String, Value>>, Vec<String>) {
    let mut results = Vec::new();
    let mut warnings = Vec::new();
    for (index, line) in stdout.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(Value::Object(fields)) => results.push(fields),
            Ok(_) => warnings.push(format!("line {index}: not a JSON object")),
            Err(error) => warnings.push(format!("line {index}: {error}")),
        }
    }
    (results, warnings)
}

/// Semgrep `-json` 输出解析；非法 JSON 即失败（Python 适配器同语义）。
///
/// # Errors
/// stdout 不是合法 JSON 或根不是对象。
pub fn parse_semgrep_json(stdout: &str) -> Result<Map<String, Value>, String> {
    serde_json::from_str::<Value>(stdout)
        .map_err(|error| format!("failed to parse semgrep JSON: {error}"))
        .and_then(|value| {
            value
                .as_object()
                .cloned()
                .ok_or_else(|| "semgrep JSON root is not an object".to_string())
        })
}

/// 扫描器/原生工具工件的输出根目录：mission config `artifact_dir` >
/// 环境变量 `LYNCEUS_ARTIFACT_DIR` > 默认 `data/artifacts`。
#[must_use]
pub fn artifact_dir_for(context: &SolverContext) -> std::path::PathBuf {
    context
        .config
        .get("artifact_dir")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var("LYNCEUS_ARTIFACT_DIR")
                .ok()
                .map(std::path::PathBuf::from)
        })
        .unwrap_or_else(|| std::path::PathBuf::from("data").join("artifacts"))
}

/// Semgrep argv 构造（受限白名单，mission config 优先、catalog settings
/// 兜底；`config`/`include`/`exclude`/`timeout_seconds` 与 YAML invocation
/// 声明键一致）。返回 `(argv, timeout_seconds)`。
pub(crate) fn semgrep_argv(params: &MissionFirstParams<'_>, target: &str) -> (Vec<String>, u64) {
    let config = params.string("config").unwrap_or("auto").to_string();
    let timeout_seconds = params.integer("timeout_seconds").unwrap_or(120).max(1);
    let mut args = vec![
        "scan".to_string(),
        "--json".to_string(),
        "--config".to_string(),
        config,
    ];
    for (field, flag) in [("include", "--include"), ("exclude", "--exclude")] {
        if let Some(items) = params.string_list(field) {
            for item in items {
                args.push(flag.to_string());
                args.push(item);
            }
        }
    }
    args.push("--".to_string());
    args.push(target.to_string());
    (args, timeout_seconds)
}

/// Nuclei argv 构造（受限白名单，mission config 优先、catalog settings
/// 兜底；声明键与 YAML invocation 一致）。返回 `(argv, timeout_seconds)`。
pub(crate) fn nuclei_argv(params: &MissionFirstParams<'_>, target: &str) -> (Vec<String>, u64) {
    let timeout_seconds = params.integer("timeout_seconds").unwrap_or(180).max(1);
    let mut args = vec![
        "-jsonl".to_string(),
        "-target".to_string(),
        target.to_string(),
    ];
    for (field, flag) in [
        ("templates", "-templates"),
        ("severity", "-severity"),
        ("tags", "-tags"),
        ("exclude_tags", "-exclude-tags"),
    ] {
        if let Some(items) = params.string_list(field) {
            let joined = items.join(",");
            if !joined.is_empty() {
                args.push(flag.to_string());
                args.push(joined);
            }
        }
    }
    if let Some(rate) = params.integer("rate_limit") {
        args.push("-rate-limit".to_string());
        args.push(rate.to_string());
    }
    args.push("-timeout".to_string());
    args.push(timeout_seconds.to_string());
    if params.boolean("headless") == Some(true) {
        args.push("-headless".to_string());
    }
    (args, timeout_seconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::{ProjectId, RunId, Severity};
    use serde_json::json;

    fn project() -> ProjectId {
        ProjectId::new("proj_domains".to_string())
    }

    #[test]
    fn semgrep_normalizer_maps_results_to_evidence_and_findings() {
        let payload: Map<String, Value> = serde_json::from_value(json!({
            "results": [{
                "check_id": "python.flask.security.injection",
                "path": "app/handler.py",
                "extra": {
                    "message": "Possible SQL injection",
                    "severity": "ERROR",
                    "lines": "query = f\"SELECT * FROM users WHERE id={uid}\"",
                    "metadata": {"cwe": ["CWE-89: SQL Injection"]},
                    "fingerprint": "fp_semgrep_001"
                },
                "start": {"line": 42},
                "end": {"line": 42}
            }]
        }))
        .expect("payload");
        let normalized = normalize_semgrep(&payload, &project(), None, None, Some("fact_scan"));
        assert_eq!(normalized.evidence.len(), 1);
        assert_eq!(normalized.findings.len(), 1);
        let finding = &normalized.findings[0];
        assert_eq!(finding.severity, Severity::High);
        assert_eq!(finding.status, FindingStatus::Candidate);
        assert_eq!(
            finding.rule_id.as_deref(),
            Some("python.flask.security.injection")
        );
        assert_eq!(finding.cwe.as_deref(), Some("CWE-89: SQL Injection"));
        assert_eq!(finding.fingerprint.as_deref(), Some("fp_semgrep_001"));
        // 不伪造 source；sink = check_id（Python 注释明确的语义）。
        assert!(finding.source_label.is_none());
        assert_eq!(
            finding.sink_label.as_deref(),
            Some("python.flask.security.injection")
        );
        let evidence = &normalized.evidence[0];
        assert_eq!(evidence.supports_fact_ids, ["fact_scan"]);
        assert_eq!(
            evidence.locations[0].snippet.as_deref(),
            Some("query = f\"SELECT * FROM users WHERE id={uid}\"")
        );
    }

    #[test]
    fn semgrep_fingerprint_falls_back_to_hash_without_upstream_fp() {
        let payload: Map<String, Value> = serde_json::from_value(json!({
            "results": [{
                "check_id": "r1",
                "path": "a.py",
                "extra": {"message": "m", "severity": "WARNING"},
                "start": {"line": 1},
                "end": {"line": 3}
            }, {
                "check_id": "r1",
                "path": "a.py",
                "extra": {"message": "m", "severity": "WARNING"},
                "start": {"line": 1},
                "end": {"line": 3}
            }]
        }))
        .expect("payload");
        let normalized = normalize_semgrep(&payload, &project(), None, None, None);
        let fp0 = normalized.findings[0].fingerprint.clone().expect("fp");
        let fp1 = normalized.findings[1].fingerprint.clone().expect("fp");
        assert!(fp0.starts_with("semgrep:"));
        // 相同位置/规则 → 同指纹（去重闸的输入）。
        assert_eq!(fp0, fp1);
    }

    #[test]
    fn semgrep_redacted_lines_reconstruct_from_local_file() {
        let mut temp = tempfile::NamedTempFile::new().expect("temp file");
        std::io::Write::write_all(&mut temp, b"line one\nsecret line\nline three").expect("write");
        let path = temp.path().to_string_lossy().into_owned();
        let payload: Map<String, Value> = serde_json::from_value(json!({
            "results": [{
                "check_id": "r",
                "path": path,
                "extra": {"message": "m", "severity": "INFO", "lines": "requires login"},
                "start": {"line": 2},
                "end": {"line": 2}
            }]
        }))
        .expect("payload");
        let normalized = normalize_semgrep(&payload, &project(), None, None, None);
        assert_eq!(
            normalized.evidence[0].locations[0].snippet.as_deref(),
            Some("secret line")
        );
    }

    #[test]
    fn nuclei_normalizer_maps_jsonl_records_with_cwe_and_fingerprint() {
        let records: Vec<Map<String, Value>> = vec![
            serde_json::from_value(json!({
                "template-id": "CVE-2024-1234",
                "info": {
                    "name": "Product X RCE",
                    "severity": "critical",
                    "description": "Remote code execution in Product X",
                    "classification": {"cwe-id": "89"}
                },
                "matcher-name": "default",
                "matched-at": "https://target.example.test/api",
                "host": "target.example.test",
                "request": "GET /api HTTP/1.1",
                "response": "HTTP/1.1 200 OK"
            }))
            .expect("record"),
            serde_json::from_value(json!({"info": {}})).expect("no template id record"),
        ];
        let run = RunId::new("run_1".to_string());
        let (scan_fact, normalized) = normalize_nuclei(
            &records,
            &project(),
            "https://target.example.test",
            2,
            Some(&run),
            None,
            Some("ti_1"),
        );
        assert_eq!(scan_fact.kind, "nuclei_scan_completed");
        assert_eq!(scan_fact.data["result_count"], json!(2));
        assert_eq!(scan_fact.data["parse_warning_count"], json!(2));
        // 无 template-id 的记录被跳过。
        assert_eq!(normalized.evidence.len(), 1);
        assert_eq!(normalized.findings.len(), 1);
        let finding = &normalized.findings[0];
        assert_eq!(finding.severity, Severity::Critical);
        assert_eq!(finding.rule_id.as_deref(), Some("nuclei:CVE-2024-1234"));
        assert_eq!(finding.cwe.as_deref(), Some("CWE-89"));
        assert!(
            finding
                .fingerprint
                .as_deref()
                .is_some_and(|fp| fp.starts_with("nuclei:"))
        );
        let evidence = &normalized.evidence[0];
        assert_eq!(evidence.supports_fact_ids, [scan_fact.id.as_str()]);
        assert_eq!(
            evidence
                .produced_by_tool_invocation_id
                .as_ref()
                .map(models::ToolInvocationId::as_str),
            Some("ti_1")
        );
        assert_eq!(finding.evidence_ids, [evidence.id.as_str().to_string()]);
    }

    #[test]
    fn nuclei_jsonl_parser_counts_warnings_and_skips_blank_lines() {
        let (records, warnings) = parse_nuclei_jsonl(
            "{\"template-id\":\"a\"}\n\nnot json at all\n{\"template-id\":\"b\"}\n",
        );
        assert_eq!(records.len(), 2);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with("line 2:"));
    }

    #[test]
    fn semgrep_json_parser_rejects_non_object_root() {
        assert!(parse_semgrep_json("[]").is_err());
        assert!(parse_semgrep_json("{\"results\": []}").is_ok());
    }

    #[test]
    fn semgrep_argv_prefers_mission_values_over_catalog_settings() {
        let mission = object(&json!({
            "config": "p/custom",
            "timeout_seconds": 42,
            "include": ["a.py"],
            "exclude": ["vendor"]
        }));
        let catalog = object(&json!({
            "config": "auto",
            "timeout_seconds": 60,
            "include": ["z.go"]
        }));
        let (args, timeout) =
            semgrep_argv(&MissionFirstParams::new(&mission, Some(&catalog)), "src");
        let position = |flag: &str| {
            args.iter()
                .position(|item| item == flag)
                .unwrap_or_else(|| panic!("missing flag {flag} in {args:?}"))
        };
        assert_eq!(args[position("--config") + 1], "p/custom");
        assert_eq!(args[position("--include") + 1], "a.py");
        assert_eq!(args[position("--exclude") + 1], "vendor");
        assert_eq!(timeout, 42);
        assert!(args.ends_with(&["--".to_string(), "src".to_string()]));
    }

    #[test]
    fn semgrep_argv_falls_back_to_catalog_when_mission_missing() {
        let mission = Map::new();
        let catalog = object(&json!({
            "config": "p/strict",
            "timeout_seconds": 90,
            "include": ["lib/**"]
        }));
        let (args, timeout) =
            semgrep_argv(&MissionFirstParams::new(&mission, Some(&catalog)), "src");
        let position = |flag: &str| {
            args.iter()
                .position(|item| item == flag)
                .unwrap_or_else(|| panic!("missing flag {flag} in {args:?}"))
        };
        assert_eq!(args[position("--config") + 1], "p/strict");
        assert_eq!(args[position("--include") + 1], "lib/**");
        assert_eq!(timeout, 90);
        // catalog 缺 key 时回到适配器默认值。
        assert!(
            args.iter().position(|item| item == "--exclude").is_none(),
            "no catalog exclude must leave no --exclude flag"
        );
    }

    #[test]
    fn nuclei_argv_mission_priority_with_catalog_fallback() {
        let mission = object(&json!({"severity": ["high", "critical"]}));
        let catalog = object(&json!({
            "severity": ["low"],
            "rate_limit": 10,
            "headless": true,
            "tags": ["cve"]
        }));
        let (args, timeout) = nuclei_argv(
            &MissionFirstParams::new(&mission, Some(&catalog)),
            "https://t",
        );
        let position = |flag: &str| {
            args.iter()
                .position(|item| item == flag)
                .unwrap_or_else(|| panic!("missing flag {flag} in {args:?}"))
        };
        assert_eq!(args[position("-severity") + 1], "high,critical");
        assert_eq!(args[position("-rate-limit") + 1], "10");
        assert!(args.contains(&"-headless".to_string()));
        assert_eq!(args[position("-tags") + 1], "cve");
        assert_eq!(timeout, 180);
        assert!(args.windows(2).any(|pair| pair == ["-timeout", "180"]));
    }

    fn object(value: &serde_json::Value) -> Map<String, Value> {
        value.as_object().cloned().expect("test value is object")
    }
}
