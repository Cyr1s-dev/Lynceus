//! 报告导出 —— `GET /reports/{report_id}` 的落地实现。
//!
//! 契约（`contracts/openapi.json`，README 段）原本只签了路径没有实现，
//! 前端因此只能在浏览器里拿画布数据现拼 Markdown。这里把它补上：
//!
//! - `report_id` 可以是 **Mission id / AuditRun id / Project id**，按这个
//!   顺序解析，报告范围随之确定；
//! - **永不 404**：id 解析不出来时返回 `status="not_ready"` 的结构化信封，
//!   让前端可以降级展示而不是抛错（契约原文要求）；
//! - `available_formats` 只声明**真的实现了**的表示形式。当前实现
//!   `json` 与 `markdown`；`sarif` / `html` 没有生成器，绝不对外宣称支持。
//!
//! 报告内容完全由已落库的实体汇总而来，不调用任何模型、不补写任何结论——
//! 报告是取证产物，不是再创作。

use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use models::lifecycle::{FindingStatus, Severity};
use models::{Finding, MissionAsset, ToolInvocation};
use serde::Deserialize;
use serde::Serialize;

use crate::{ApiError, ApiState};

/// 报告范围（`report_id` 解析成了哪种实体）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportScope {
    /// 单任务。
    Mission,
    /// 单次审计运行。
    Run,
    /// 整个项目。
    Project,
}

/// 严重度分布。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SeverityTally {
    /// 信息级。
    pub info: usize,
    /// 低危。
    pub low: usize,
    /// 中危。
    pub medium: usize,
    /// 高危。
    pub high: usize,
    /// 严重。
    pub critical: usize,
}

impl SeverityTally {
    fn push(&mut self, severity: Severity) {
        match severity {
            Severity::Info => self.info += 1,
            Severity::Low => self.low += 1,
            Severity::Medium => self.medium += 1,
            Severity::High => self.high += 1,
            Severity::Critical => self.critical += 1,
        }
    }

    /// 总计。
    #[must_use]
    pub const fn total(&self) -> usize {
        self.info + self.low + self.medium + self.high + self.critical
    }
}

/// 报告里的单条发现。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReportFinding {
    /// Finding id。
    pub id: String,
    /// 标题。
    pub title: String,
    /// 严重度 wire 值。
    pub severity: String,
    /// 状态 wire 值。
    pub status: String,
    /// CWE。
    pub cwe: Option<String>,
    /// 规则 id。
    pub rule_id: Option<String>,
    /// 描述。
    pub description: Option<String>,
    /// 证据 id 列表。
    pub evidence_ids: Vec<String>,
    /// 产生该发现的 Task。
    pub produced_by_task_id: Option<String>,
    /// 发现时间（ISO8601）。
    pub created_at: String,
    /// 更新时间（ISO8601）。
    pub updated_at: String,
}

/// 报告里的单个资产。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReportAsset {
    /// 资产 id。
    pub id: String,
    /// 资产类别 wire 值。
    pub asset_type: String,
    /// 资产值。
    pub value: String,
    /// 置信度。
    pub confidence: f64,
    /// 是否已被行使过（证据/发现/工具调用任一关联）。
    pub tested: bool,
}

/// 单个工具的调用统计。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolTally {
    /// 工具名。
    pub tool: String,
    /// 调用次数。
    pub total: usize,
    /// 失败次数（error/timeout/denied）。
    pub errors: usize,
}

/// 报告正文（`format=json` 时的 `payload`）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReportPayload {
    /// 报告标识（即 `report_id`）。
    pub id: String,
    /// 范围。
    pub scope: ReportScope,
    /// 范围实体标题。
    pub title: String,
    /// 生成时间（ISO8601）。
    pub generated_at: String,
    /// 范围实体的创建时间（ISO8601，解析不到时为空串）。
    pub created_at: String,
    /// 发现总数。
    pub finding_total: usize,
    /// 严重度分布。
    pub severity: SeverityTally,
    /// 状态分布（键 = [`FindingStatus`] wire 值）。
    pub status: BTreeMap<String, usize>,
    /// 资产总数。
    pub asset_total: usize,
    /// 资产分布（键 = 资产类别 wire 值）。
    pub assets_by_type: BTreeMap<String, usize>,
    /// 已被行使的资产数。
    pub assets_tested: usize,
    /// 工具调用总数。
    pub tool_total: usize,
    /// 按工具聚合的调用统计（按调用次数降序）。
    pub tools: Vec<ToolTally>,
    /// 发现明细（按严重度降序、时间升序）。
    pub findings: Vec<ReportFinding>,
    /// 资产明细（按类别、值排序）。
    pub assets: Vec<ReportAsset>,
}

/// 报告信封。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReportEnvelope {
    /// 报告标识（即请求里的 `report_id`）。
    pub id: String,
    /// `ready` / `not_ready`。
    pub status: String,
    /// `not_ready` 的原因；`ready` 时为 `null`。
    pub reason: Option<String>,
    /// 已实现的表示形式。
    pub available_formats: Vec<String>,
    /// `format=markdown` 且非下载时的 Markdown 正文；其余情况为 `null`。
    ///
    /// 放在信封顶层而不是 `payload` 里：`payload` 是结构化事实，Markdown 是
    /// 它的另一种表示形式。契约里两个字段都在，不会出现「实现比schema 多一个
    /// 字段」的漂移。
    pub markdown: Option<String>,
    /// 报告正文；`not_ready` 时为 `null`。
    pub payload: Option<ReportPayload>,
}

/// 查询参数（契约 `GET /reports/{report_id}`）。
#[derive(Debug, Deserialize)]
pub struct ReportQuery {
    /// 表示形式；缺省 `json`。
    #[serde(default = "default_format")]
    pub format: String,
    /// 包含哪些状态的发现；逗号分隔或 `all`。缺省只含 `confirmed`。
    #[serde(default)]
    pub include_statuses: Option<String>,
    /// 直接返回原始文档而不是 JSON 信封。
    #[serde(default)]
    pub download: bool,
}

fn default_format() -> String {
    "json".to_string()
}

/// 本实现真正支持的表示形式。
const SUPPORTED_FORMATS: [&str; 2] = ["json", "markdown"];

/// 工具调用是否算失败。
fn is_error_status(status: &str) -> bool {
    matches!(status, "error" | "timeout" | "denied")
}

/// 资产是否已被行使过。
fn asset_tested(asset: &MissionAsset) -> bool {
    !asset.evidence_ids.is_empty()
        || !asset.finding_ids.is_empty()
        || !asset.tool_invocation_ids.is_empty()
}

/// 状态过滤：默认只收 `confirmed`，`all` 或显式列表放开。
fn status_filter(raw: Option<&str>) -> Vec<FindingStatus> {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None => vec![FindingStatus::Confirmed],
        Some(value) if value.eq_ignore_ascii_case("all") => FindingStatus::ALL.to_vec(),
        Some(value) => value
            .split(',')
            .filter_map(|item| FindingStatus::from_wire(item.trim()))
            .collect(),
    }
}

/// 由范围实体组装报告正文。
fn build_payload(
    id: &str,
    scope: ReportScope,
    title: &str,
    created_at: &str,
    findings: &[Finding],
    assets: &[MissionAsset],
    invocations: &[ToolInvocation],
    statuses: &[FindingStatus],
) -> ReportPayload {
    let mut severity = SeverityTally::default();
    let mut status = BTreeMap::new();
    let mut report_findings = Vec::new();
    for finding in findings {
        if !statuses.contains(&finding.status) {
            continue;
        }
        severity.push(finding.severity);
        *status
            .entry(finding.status.as_str().to_string())
            .or_insert(0) += 1;
        report_findings.push(ReportFinding {
            id: finding.id.as_str().to_string(),
            title: finding.title.clone(),
            severity: finding.severity.as_str().to_string(),
            status: finding.status.as_str().to_string(),
            cwe: finding.cwe.clone(),
            rule_id: finding.rule_id.clone(),
            description: finding.description.clone(),
            evidence_ids: finding.evidence_ids.clone(),
            produced_by_task_id: finding
                .produced_by_task_id
                .as_ref()
                .map(|task| task.as_str().to_string()),
            created_at: finding.created_at.isoformat(),
            updated_at: finding.updated_at.isoformat(),
        });
    }
    report_findings.sort_by(|a, b| {
        // 严重度降序（越严重越靠前），同严重度按发现时间升序。
        severity_rank(&b.severity)
            .cmp(&severity_rank(&a.severity))
            .then_with(|| a.created_at.cmp(&b.created_at))
    });

    let mut assets_by_type: BTreeMap<String, usize> = BTreeMap::new();
    let mut assets_tested = 0usize;
    let mut report_assets = Vec::new();
    for asset in assets {
        *assets_by_type
            .entry(asset.asset_type.as_str().to_string())
            .or_insert(0) += 1;
        if asset_tested(asset) {
            assets_tested += 1;
        }
        report_assets.push(ReportAsset {
            id: asset.id.as_str().to_string(),
            asset_type: asset.asset_type.as_str().to_string(),
            value: asset.value.clone(),
            confidence: asset.confidence,
            tested: asset_tested(asset),
        });
    }
    report_assets.sort_by(|a, b| {
        a.asset_type
            .cmp(&b.asset_type)
            .then_with(|| a.value.cmp(&b.value))
    });

    let mut per_tool: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for invocation in invocations {
        let entry = per_tool
            .entry(invocation.tool_name.clone())
            .or_insert((0, 0));
        entry.0 += 1;
        if is_error_status(invocation.status.as_str()) {
            entry.1 += 1;
        }
    }
    let mut tools = per_tool
        .into_iter()
        .map(|(tool, (total, errors))| ToolTally { tool, total, errors })
        .collect::<Vec<_>>();
    tools.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.tool.cmp(&b.tool)));

    ReportPayload {
        id: id.to_string(),
        scope,
        title: title.to_string(),
        generated_at: models::utcnow().isoformat(),
        created_at: created_at.to_string(),
        finding_total: report_findings.len(),
        severity,
        status,
        asset_total: report_assets.len(),
        assets_by_type,
        assets_tested,
        tool_total: invocations.len(),
        tools,
        findings: report_findings,
        assets: report_assets,
    }
}

/// 严重度排序权重（越大越严重）。
fn severity_rank(raw: &str) -> u8 {
    match raw {
        "critical" => 4,
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

/// 渲染 Markdown（与 `payload` 同源，不引入第二套事实）。
fn render_markdown(payload: &ReportPayload) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", payload.title));
    out.push_str(&format!(
        "- 报告 ID：`{}`\n- 范围：{}\n- 生成时间：{}\n\n",
        payload.id,
        match payload.scope {
            ReportScope::Mission => "任务",
            ReportScope::Run => "审计运行",
            ReportScope::Project => "项目",
        },
        payload.generated_at
    ));
    out.push_str("## 执行摘要\n\n");
    out.push_str(&format!(
        "| 指标 | 值 |\n| --- | --- |\n| 发现 | {} |\n| 资产 | {}（已测 {}） |\n| 工具调用 | {} |\n\n",
        payload.finding_total, payload.asset_total, payload.assets_tested, payload.tool_total
    ));
    out.push_str("## 漏洞分布\n\n");
    out.push_str(&format!(
        "| 严重度 | 数量 |\n| --- | --- |\n| 严重 | {} |\n| 高危 | {} |\n| 中危 | {} |\n| 低危 | {} |\n| 信息 | {} |\n\n",
        payload.severity.critical,
        payload.severity.high,
        payload.severity.medium,
        payload.severity.low,
        payload.severity.info
    ));
    out.push_str("## 漏洞发现\n\n");
    if payload.findings.is_empty() {
        out.push_str("（当前状态过滤下没有发现）\n\n");
    } else {
        out.push_str("| 严重度 | 状态 | 标题 | CWE | 证据 |\n| --- | --- | --- | --- | --- |\n");
        for finding in &payload.findings {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                finding.severity,
                finding.status,
                md_cell(&finding.title),
                finding.cwe.as_deref().unwrap_or("—"),
                finding.evidence_ids.len()
            ));
        }
        out.push('\n');
        out.push_str("## 发现详情\n\n");
        for finding in &payload.findings {
            out.push_str(&format!("### {} ({})\n\n", finding.title, finding.severity));
            if let Some(description) = finding.description.as_deref()
                && !description.trim().is_empty()
            {
                out.push_str(description.trim());
                out.push_str("\n\n");
            }
            if !finding.evidence_ids.is_empty() {
                out.push_str(&format!(
                    "证据：{}\n\n",
                    finding
                        .evidence_ids
                        .iter()
                        .map(|id| format!("`{id}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
    }
    out.push_str("## 资产清单\n\n");
    if payload.assets.is_empty() {
        out.push_str("（暂无资产）\n\n");
    } else {
        out.push_str("| 类型 | 值 | 置信度 | 已测 |\n| --- | --- | --- | --- |\n");
        for asset in &payload.assets {
            out.push_str(&format!(
                "| {} | {} | {:.2} | {} |\n",
                asset.asset_type,
                md_cell(&asset.value),
                asset.confidence,
                if asset.tested { "是" } else { "否" }
            ));
        }
        out.push('\n');
    }
    out.push_str("## 工具调用\n\n");
    if payload.tools.is_empty() {
        out.push_str("（暂无工具调用）\n\n");
    } else {
        out.push_str("| 工具 | 调用 | 失败 |\n| --- | --- | --- |\n");
        for tool in &payload.tools {
            out.push_str(&format!("| {} | {} | {} |\n", tool.tool, tool.total, tool.errors));
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "---\n\n由 Lynceus 于 {} 生成。\n",
        payload.generated_at
    ));
    out
}

/// Markdown 表格单元格转义。
fn md_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(title: &str, severity: Severity, status: FindingStatus) -> Finding {
        serde_json::from_value(serde_json::json!({
            "id": format!("find_{title}"),
            "project_id": "proj_x",
            "title": title,
            "description": format!("desc {title}"),
            "severity": severity.as_str(),
            "status": status.as_str(),
            "cwe": "CWE-95",
            "rule_id": "web_sast.eval_injection",
            "evidence_ids": ["evd_1"],
            "created_at": "2026-08-24T12:00:00.123456Z",
            "updated_at": "2026-08-24T12:00:00.123456Z",
        }))
        .expect("Finding fixture must parse")
    }

    fn invocation(tool: &str, status: &str) -> ToolInvocation {
        serde_json::from_value(serde_json::json!({
            "id": format!("tool_{tool}"),
            "project_id": "proj_x",
            "tool_name": tool,
            "input_summary": "scan src/",
            "output_summary": "ok",
            "status": status,
            "started_at": "2026-08-24T12:00:00.123456Z",
        }))
        .expect("ToolInvocation fixture must parse")
    }

    fn asset(value: &str, asset_type: &str, tested: bool) -> MissionAsset {
        let mut ids = serde_json::json!({});
        if tested {
            ids = serde_json::json!({ "finding_ids": ["find_eval"] });
        }
        serde_json::from_value(serde_json::json!({
            "id": format!("asset_{}", value.replace(['.', ':', '/'], "_")),
            "project_id": "proj_x",
            "mission_id": "mission_x",
            "asset_type": asset_type,
            "value": value,
            "confidence": 0.9,
            "source": "manual",
            "finding_ids": ids.get("finding_ids").cloned().unwrap_or(serde_json::json!([])),
            "evidence_ids": [],
            "tool_invocation_ids": [],
        }))
        .expect("MissionAsset fixture must parse")
    }

    #[test]
    fn payload_aggregates_only_the_requested_statuses() {
        let findings = vec![
            finding("confirmed_high", Severity::High, FindingStatus::Confirmed),
            finding("confirmed_low", Severity::Low, FindingStatus::Confirmed),
            finding("candidate_high", Severity::High, FindingStatus::Candidate),
        ];
        let invocations = vec![invocation("semgrep", "ok"), invocation("nuclei", "error")];
        let assets = vec![
            asset("example.com", "domain", true),
            asset("a.example.com", "host", false),
        ];

        let payload = build_payload(
            "mission_x",
            ReportScope::Mission,
            "审计任务",
            "2026-08-24T12:00:00.123456Z",
            &findings,
            &assets,
            &invocations,
            &[FindingStatus::Confirmed],
        );

        assert_eq!(payload.finding_total, 2, "默认只收 confirmed");
        assert_eq!(payload.severity.high, 1);
        assert_eq!(payload.severity.low, 1);
        assert_eq!(payload.severity.total(), 2);
        assert_eq!(payload.status.get("confirmed"), Some(&2));
        assert!(!payload.status.contains_key("candidate"));

        assert_eq!(payload.asset_total, 2);
        assert_eq!(payload.assets_tested, 1);
        assert_eq!(payload.assets_by_type.get("domain"), Some(&1));
        assert_eq!(payload.assets_by_type.get("host"), Some(&1));

        assert_eq!(payload.tool_total, 2);
        assert_eq!(payload.tools.len(), 2);
        // 按调用次数降序，次数相同按工具名。
        assert_eq!(payload.tools[0].tool, "nuclei");
        assert_eq!(payload.tools[0].errors, 1);
        assert_eq!(payload.tools[1].errors, 0);

        // 严重度降序：high 在 low 前。
        assert_eq!(payload.findings[0].title, "confirmed_high");
        assert_eq!(payload.findings[1].title, "confirmed_low");
    }

    #[test]
    fn all_status_filter_lets_everything_through() {
        let findings = vec![finding("a", Severity::Medium, FindingStatus::Confirmed)];
        let payload = build_payload(
            "p",
            ReportScope::Project,
            "p",
            "",
            &findings,
            &[],
            &[],
            &FindingStatus::ALL.to_vec(),
        );
        assert_eq!(payload.finding_total, 1);
    }

    #[test]
    fn markdown_is_derived_from_the_same_payload() {
        let findings = vec![finding("eval|pipe", Severity::Critical, FindingStatus::Confirmed)];
        let payload = build_payload(
            "mission_x",
            ReportScope::Mission,
            "任务标题",
            "2026-08-24T12:00:00.123456Z",
            &findings,
            &[asset("example.com", "domain", true)],
            &[invocation("semgrep", "ok")],
            &[FindingStatus::Confirmed],
        );
        let markdown = render_markdown(&payload);

        assert!(markdown.starts_with("# 任务标题\n"));
        assert!(markdown.contains("| 严重 | 1 |"), "severity 表必须落在 Markdown 里");
        assert!(markdown.contains("eval\\|pipe"), "表格单元格里的竖线必须转义");
        assert!(markdown.contains("`evd_1`"), "证据 id 必须出现在详情里");
        assert!(markdown.contains("| semgrep | 1 | 0 |"));
        assert!(markdown.contains("由 Lynceus 于"));
        // 空资产/工具时也不能 panic。
        let empty = build_payload("m", ReportScope::Mission, "t", "", &[], &[], &[], &[FindingStatus::Confirmed]);
        let empty_md = render_markdown(&empty);
        assert!(empty_md.contains("（当前状态过滤下没有发现）"));
        assert!(empty_md.contains("（暂无资产）"));
        assert!(empty_md.contains("（暂无工具调用）"));
    }

    #[test]
    fn not_ready_envelope_carries_the_implemented_formats() {
        let envelope = not_ready("nope", "unknown report id");
        assert_eq!(envelope.status, "not_ready");
        assert_eq!(envelope.reason.as_deref(), Some("unknown report id"));
        assert_eq!(envelope.available_formats, vec!["json", "markdown"]);
        assert!(envelope.payload.is_none());
    }

    #[test]
    fn status_filter_defaults_to_confirmed_only() {
        assert_eq!(status_filter(None), vec![FindingStatus::Confirmed]);
        assert_eq!(status_filter(Some("")), vec![FindingStatus::Confirmed]);
        assert_eq!(status_filter(Some("all")).len(), FindingStatus::ALL.len());
        assert_eq!(
            status_filter(Some("confirmed, fixed")),
            vec![FindingStatus::Confirmed, FindingStatus::Fixed]
        );
        // 未注册的状态值被丢弃，而不是让整份报告失败。
        assert!(status_filter(Some("nonsense")).is_empty());
    }

    #[test]
    fn error_status_classification() {
        assert!(is_error_status("error"));
        assert!(is_error_status("timeout"));
        assert!(is_error_status("denied"));
        assert!(!is_error_status("ok"));
    }
}

/// `not_ready` 信封。
fn not_ready(id: &str, reason: &str) -> ReportEnvelope {
    ReportEnvelope {
        id: id.to_string(),
        status: "not_ready".to_string(),
        reason: Some(reason.to_string()),
        available_formats: SUPPORTED_FORMATS.iter().map(|f| (*f).to_string()).collect(),
        markdown: None,
        payload: None,
    }
}

/// `ready` 信封（`markdown` 仅在 `format=markdown` 时给出）。
fn ready(payload: &ReportPayload, markdown: Option<String>) -> ReportEnvelope {
    ReportEnvelope {
        id: payload.id.clone(),
        status: "ready".to_string(),
        reason: None,
        available_formats: SUPPORTED_FORMATS.iter().map(|f| (*f).to_string()).collect(),
        markdown,
        payload: Some(payload.clone()),
    }
}

/// `GET /reports/{report_id}`。
///
/// 契约要求永不 404：解析不出范围时返回 200 + `not_ready` 信封。
pub(crate) async fn get_report(
    State(state): State<ApiState>,
    Path(report_id): Path<String>,
    Query(query): Query<ReportQuery>,
) -> Result<Response, ApiError> {
    let format = query.format.trim().to_ascii_lowercase();
    if !SUPPORTED_FORMATS.contains(&format.as_str()) {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "detail": format!(
                    "unsupported format: {}; supported: {}",
                    format,
                    SUPPORTED_FORMATS.join(", ")
                )
            })),
        )
            .into_response());
    }
    let statuses = status_filter(query.include_statuses.as_deref());

    let payload = match resolve_report(
        &state,
        &report_id,
        &statuses,
    )
    .await?
    {
        Some(payload) => payload,
        None => {
            return Ok(Json(not_ready(
                &report_id,
                &format!("report id {report_id} does not resolve to a mission, audit run or project"),
            ))
            .into_response());
        }
    };

    if query.download {
        let (body, content_type) = match format.as_str() {
            "markdown" => (render_markdown(&payload), "text/markdown; charset=utf-8"),
            _ => (
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string()),
                "application/json",
            ),
        };
        return Ok((
            StatusCode::OK,
            [
                (CONTENT_TYPE, HeaderValue::from_static(content_type)),
                (
                    axum::http::header::CONTENT_DISPOSITION,
                    HeaderValue::from_str(&format!(
                        "attachment; filename=\"lynceus-report-{}.{}\"",
                        payload.id,
                        if format == "markdown" { "md" } else { "json" }
                    ))
                    .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
                ),
            ],
            body,
        )
            .into_response());
    }

    if format == "markdown" {
        // 非下载也允许直接拿 Markdown 文本（信封里放正文，方便前端预览）。
        return Ok(Json(ready(&payload, Some(render_markdown(&payload)))).into_response());
    }
    Ok(Json(ready(&payload, None)).into_response())
}

/// 按 mission → run → project 的顺序解析 `report_id`。
async fn resolve_report(
    state: &crate::ApiState,
    report_id: &str,
    statuses: &[FindingStatus],
) -> Result<Option<ReportPayload>, ApiError> {
    let repository = state.manager.repository();

    if let Some(mission) = repository.get_mission(report_id)? {
        let findings = repository.list_findings(mission.project_id.as_str())?;
        let invocations = repository.list_tool_invocations(Some(mission.project_id.as_str()))?;
        let assets = repository.list_mission_assets(Some(mission.id.as_str()), None, None, None)?;
        // 任务范围内：显式挂本 Mission、挂本 Mission 分支、或由本 Mission 的
        // Task 产出——与 mission_canvas 的 scope 语义保持一致。
        let branch_ids = repository
            .list_branches(Some(mission.project_id.as_str()), Some(mission.id.as_str()), None)?
            .into_iter()
            .map(|branch| branch.id.as_str().to_string())
            .collect::<std::collections::BTreeSet<_>>();
        let mission_findings = findings
            .iter()
            .filter(|finding| {
                finding.mission_id.as_ref() == Some(&mission.id)
                    || finding
                        .branch_id
                        .as_ref()
                        .is_some_and(|id| branch_ids.contains(id.as_str()))
            })
            .cloned()
            .collect::<Vec<_>>();
        let mission_invocations = invocations
            .iter()
            .filter(|invocation| invocation.mission_id.as_ref() == Some(&mission.id))
            .cloned()
            .collect::<Vec<_>>();
        return Ok(Some(build_payload(
            mission.id.as_str(),
            ReportScope::Mission,
            mission.title.as_deref().unwrap_or("未命名任务"),
            &mission.created_at.isoformat(),
            &mission_findings,
            &assets,
            &mission_invocations,
            statuses,
        )));
    }

    if let Some(run) = repository.get_run(report_id)? {
        let findings = repository
            .list_findings(run.project_id.as_str())?
            .into_iter()
            .filter(|finding| finding.run_id.as_ref() == Some(&run.id))
            .collect::<Vec<_>>();
        let invocations = repository
            .list_tool_invocations(Some(run.project_id.as_str()))?
            .into_iter()
            .filter(|invocation| invocation.run_id.as_ref() == Some(&run.id))
            .collect::<Vec<_>>();
        let assets = repository.list_mission_assets(None, None, None, None)?;
        return Ok(Some(build_payload(
            run.id.as_str(),
            ReportScope::Run,
            &format!("审计运行 {}", run.id.as_str()),
            &run.created_at.isoformat(),
            &findings,
            &assets,
            &invocations,
            statuses,
        )));
    }

    if let Some(project) = repository.get_project(report_id)? {
        let findings = repository.list_findings(project.id.as_str())?;
        let invocations = repository.list_tool_invocations(Some(project.id.as_str()))?;
        let assets = repository.list_mission_assets(None, None, None, None)?;
        return Ok(Some(build_payload(
            project.id.as_str(),
            ReportScope::Project,
            project.name.as_str(),
            &project.created_at.isoformat(),
            &findings,
            &assets,
            &invocations,
            statuses,
        )));
    }

    let _ = report_id;
    Ok(None)
}
