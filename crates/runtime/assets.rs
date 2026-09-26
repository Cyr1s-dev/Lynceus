//! Mission 资产投影 —— `manager.py` 的 `_project_mission_assets*` 族。
//!
//! 把 Mission target / Fact / Evidence / Finding / ToolInvocation 中的资产
//! 信号（URL、主机、凭证……）投影成 [`MissionAsset`] 追加到仓储。这是
//! Mission Canvas 的数据底座，绝不是审计真值：投影失败绝不能打断编排
//! （内部 `AuditManager::project_mission_assets_safe` 吞掉一切失败并记录
//! "skipped" 事件，Python `except Exception` 的镜像）。
//!
//! 敏感值红线：SECRET/CREDENTIAL 类资产的原始值绝不落盘——非安全引用
// Projection code preserves Python field-by-field order and fallback semantics.
#![allow(clippy::assigning_clones)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::expect_used)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::needless_question_mark)]
//! 键以 SHA-256 前 12 位十六进制摘要代替（Python
//! `_redacted_sensitive_value`）。

use std::net::IpAddr;
use std::str::FromStr;

use models::AuditEventType;
use models::Mission;
use models::MissionAssetId;
use models::RunId;
use models::asset::MissionAsset;
use models::asset::MissionAssetSensitivity;
use models::asset::MissionAssetSource;
use models::asset::MissionAssetType;
use models::evidence::Evidence;
use models::fact::Fact;
use models::finding::Finding;
use models::lifecycle::Severity;
use models::new_id;
use models::tool_invocation::ToolInvocation;
use models::utcnow;
use serde_json::Map;
use serde_json::Value;

use crate::errors::EngineError;
use crate::events::EventDraft;
use crate::manager::AuditManager;

impl AuditManager {
    /// Upsert a MissionAsset after validating Mission/project ownership
    /// （Python `AuditManager.upsert_mission_asset`）。
    ///
    /// # Errors
    /// Mission 不存在、asset 的 project 与 Mission 归属不一致或仓储写入失败。
    pub fn upsert_mission_asset(
        &self,
        asset: &models::asset::MissionAsset,
    ) -> Result<models::asset::MissionAsset, EngineError> {
        let mission = self.require_mission(asset.mission_id.as_str())?;
        if mission.project_id != asset.project_id {
            return Err(EngineError::ReferenceValidationError(
                "asset project_id does not belong to mission".to_string(),
            ));
        }
        Ok(self.repository().upsert_mission_asset(asset)?)
    }

    /// 追加不含大文件内容的工件元数据。
    ///
    /// # Errors
    /// Project 不存在、URI 为空、大小为负数或仓储写入失败。
    pub fn add_artifact_record(
        &self,
        artifact: &models::ArtifactRecord,
    ) -> Result<models::ArtifactRecord, EngineError> {
        if let Some(project_id) = artifact.project_id.as_ref() {
            self.require_project(project_id.as_str())?;
        }
        if artifact.uri.trim().is_empty() {
            return Err(EngineError::Value(
                "artifact uri must not be blank".to_string(),
            ));
        }
        if artifact.size_bytes.is_some_and(|size| size < 0) {
            return Err(EngineError::Value(
                "artifact size_bytes must be non-negative".to_string(),
            ));
        }
        Ok(self.repository().add_artifact_record(artifact)?)
    }

    /// 更新工件元数据（intake 移动工件后回写 workspace 元数据用；
    /// Python 侧路由直调仓储，Rust 收口在 Manager 门面）。
    ///
    /// # Errors
    /// 仓储写入失败。
    pub fn update_artifact_record(
        &self,
        artifact: &models::ArtifactRecord,
    ) -> Result<models::ArtifactRecord, EngineError> {
        Ok(self.repository().update_artifact_record(artifact)?)
    }

    /// 投影 Mission 资产并吞掉一切失败（Python `_project_mission_assets_safe`）。
    ///
    /// # Errors
    /// 永不返回——投影失败只记录 "skipped" 事件。
    pub(crate) async fn project_mission_assets_safe(
        &self,
        mission: &Mission,
        facts: &[Fact],
        evidence: &[Evidence],
        findings: &[Finding],
        tools: &[ToolInvocation],
        record_events: bool,
    ) {
        if let Err(exc) = self
            .project_mission_assets(mission, facts, evidence, findings, tools, record_events)
            .await
        {
            self.record_event_safe(EventDraft {
                run_id: mission.active_run_id.as_ref(),
                message: Some(&exc.to_string()),
                status: Some("skipped"),
                data: Some(Map::from_iter([(
                    "mission_id".to_string(),
                    Value::String(mission.id.as_str().to_string()),
                )])),
                ..EventDraft::new(
                    &mission.project_id,
                    AuditEventType::UserNote,
                    "asset_projection",
                    "Mission asset projection skipped",
                )
            })
            .await;
        }
    }

    /// 投影并落盘全部资产候选（Python `_project_mission_assets`）。
    ///
    /// # Errors
    /// 仓储写入失败或候选值非法（Python pydantic ValidationError 同族，
    /// 值去除首尾空白后为空）。
    pub(crate) async fn project_mission_assets(
        &self,
        mission: &Mission,
        facts: &[Fact],
        evidence: &[Evidence],
        findings: &[Finding],
        tools: &[ToolInvocation],
        record_events: bool,
    ) -> Result<Vec<MissionAsset>, EngineError> {
        let mut candidates: Vec<MissionAsset> = Vec::new();
        candidates.extend(assets_from_mission_target(mission)?);
        for fact in facts {
            candidates.extend(assets_from_fact(mission, fact)?);
        }
        for item in evidence {
            candidates.extend(assets_from_evidence(mission, item)?);
        }
        for finding in findings {
            candidates.extend(assets_from_finding(mission, finding)?);
        }
        for tool in tools {
            candidates.extend(assets_from_tool_invocation(mission, tool)?);
        }

        let mut saved: Vec<MissionAsset> = Vec::new();
        for asset in candidates {
            let persisted = self.repository().upsert_mission_asset(&asset)?;
            if record_events {
                self.record_event_safe(EventDraft {
                    run_id: asset.run_id.as_ref().or(mission.active_run_id.as_ref()),
                    status: Some(asset.sensitivity.as_str()),
                    data: Some(Map::from_iter([
                        (
                            "mission_id".to_string(),
                            Value::String(mission.id.as_str().to_string()),
                        ),
                        (
                            "asset_id".to_string(),
                            Value::String(persisted.id.as_str().to_string()),
                        ),
                        (
                            "asset_type".to_string(),
                            Value::String(persisted.asset_type.as_str().to_string()),
                        ),
                        (
                            "source".to_string(),
                            Value::String(persisted.source.as_str().to_string()),
                        ),
                        (
                            "sensitivity".to_string(),
                            Value::String(persisted.sensitivity.as_str().to_string()),
                        ),
                    ])),
                    ..EventDraft::new(
                        &mission.project_id,
                        AuditEventType::AssetDiscovered,
                        "asset_projection",
                        &format!(
                            "Mission asset discovered: {}",
                            persisted.asset_type.as_str()
                        ),
                    )
                })
                .await;
            }
            saved.push(persisted);
        }
        Ok(saved)
    }
}

/// 目标值是不是占位符（空 / "未指定" / "unknown" 一类）。
///
/// 实测 intake 模型对模糊输入回过 `{"domain": "未指定"}`、
/// `{"raw_target": "不知道"}`——键名像定位符，值什么都不是。
fn is_placeholder_target_value(value: &str) -> bool {
    const PLACEHOLDER_VALUES: &[&str] = &[
        "", "未指定", "待定", "待补充", "无", "不知道", "unspecified", "unknown", "n/a", "na",
        "none", "null", "tbd", "todo",
    ];
    let value = value.trim();
    PLACEHOLDER_VALUES
        .iter()
        .any(|placeholder| value.eq_ignore_ascii_case(placeholder))
}

/// Mission target 键值 → 资产候选（Python `_assets_from_mission_target`）。
fn assets_from_mission_target(mission: &Mission) -> Result<Vec<MissionAsset>, EngineError> {
    let mut assets = Vec::new();
    for (key, value) in mission.target.iter() {
        // 占位值（`{"domain": "未指定"}` 这类模型乱答）不是资产：键名像
        // 定位符，值什么都不是——建出来只是污染 coverage map。
        if is_placeholder_target_value(value) {
            continue;
        }
        if let Some(asset_type) = target_asset_type(key, value) {
            assets.push(mission_asset(
                mission,
                asset_type,
                value,
                MissionAssetSource::UserTarget,
                Some(mission.id.as_str().to_string()),
                None,
                None,
                1.0,
                None,
                None,
                &["mission-target".to_string()],
                &Map::from_iter([("target_key".to_string(), Value::String(key.to_string()))]),
            )?);
            continue;
        }
        // 键不认识（`raw_prompt` / `notes` 这类描述键）时从值里嗅探：
        // 摄入回退（`unknown_plan`）只写 `raw_prompt`，用户手动建任务时
        // target 干脆是空的——不嗅探这些输入就永远零资产，coverage map
        // 永远是空的。
        assets.extend(sniffed_target_assets(mission, key, value)?);
    }
    // Mission 目标原文同样嗅探（手动建任务不走摄入，target 为空时目标原文
    // 是唯一线索）。与 target 键值重复的候选由 upsert 的
    // (mission, type, value) 去重。
    assets.extend(sniffed_target_assets(mission, "user_goal", &mission.user_goal)?);
    Ok(assets)
}

/// 嗅探候选 → 资产（标签带 `sniffed`，元数据记录来源键）。
fn sniffed_target_assets(
    mission: &Mission,
    key: &str,
    value: &str,
) -> Result<Vec<MissionAsset>, EngineError> {
    let mut assets = Vec::new();
    for (asset_type, sniffed) in sniff_target_value(value) {
        assets.push(mission_asset(
            mission,
            asset_type,
            &sniffed,
            MissionAssetSource::UserTarget,
            Some(mission.id.as_str().to_string()),
            None,
            None,
            1.0,
            None,
            None,
            &["mission-target".to_string(), "sniffed".to_string()],
            &Map::from_iter([
                ("target_key".to_string(), Value::String(key.to_string())),
                ("sniffed".to_string(), Value::Bool(true)),
            ]),
        )?);
    }
    Ok(assets)
}

/// 自由文本 → 可指向的资产。
///
/// 依次尝试 URL → IPv4 → 域名，取第一个命中的形状；URL 命中时附带主机
/// 资产（服务与其主机互相可查）。纯自然语言（"审计一下某站点"）返回空——
/// 解析不出目标就不创建资产，绝不编造。
///
/// 实现按「切词 → 逐词判定」走，而不是一条大正则：目标文本里混着中文、
/// 标点和版本号，逐词判定更容易把"1.2.3.4 不是 IP"这类假阳性挡掉。
fn sniff_target_value(value: &str) -> Vec<(MissionAssetType, String)> {
    if let Some(url) = urls_from_text(value).into_iter().next() {
        let mut found = vec![(MissionAssetType::Url, url.clone())];
        if let Some(host) = url_host(&url) {
            found.push((host_asset_type(&host), host));
        }
        return found;
    }
    if let Some(ip) = first_ipv4(value) {
        return vec![(MissionAssetType::Ip, ip)];
    }
    if let Some(domain) = first_domain(value) {
        return vec![(MissionAssetType::Domain, domain)];
    }
    Vec::new()
}

/// 把自由文本切成候选词：空白与常见标点都算边界。
///
/// 端口用单独的剥离步骤处理，所以这里不需要在词形里兼容 `host:port`。
fn candidate_words(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '|' | '"' | '\'' | '(' | ')' | '[' | ']' | '<' | '>'))
        .filter(|word| !word.is_empty())
}

/// 剥掉尾部的 `:端口`，返回主机部分。没有端口则原样返回。
fn strip_port(word: &str) -> &str {
    match word.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => word,
    }
}

/// 四段式点分十进制，且每段在 0..=255 内。
///
/// 逐段解析而不是整串正则：这样 "1.2.3.4.5" 和 "999.1.1.1" 天然落选，
/// 不需要额外的假阳性过滤。
fn parse_ipv4(word: &str) -> Option<String> {
    let mut octets = [0u16; 4];
    for (index, part) in word.split('.').enumerate() {
        if index == 4 || part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        octets[index] = part.parse().ok()?;
    }
    if word.split('.').count() != 4 || octets.iter().any(|octet| *octet > 255) {
        return None;
    }
    Some(
        octets
            .iter()
            .map(|octet| octet.to_string())
            .collect::<Vec<_>>()
            .join("."),
    )
}

/// 至少一个点、末段必须是两个字母以上的纯字母 TLD。
///
/// 排斥下划线与连续点：那在域名里非法，却常出现在报错信息和路径片段里。
fn parse_domain(word: &str) -> Option<String> {
    let lowered = word.to_ascii_lowercase();
    let (labels, tld) = lowered.rsplit_once('.')?;
    if tld.len() < 2 || !tld.bytes().all(|b| b.is_ascii_lowercase()) {
        return None;
    }
    if labels.is_empty() || lowered.contains("..") {
        return None;
    }
    let ok = labels.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    });
    ok.then_some(lowered)
}

/// 文本中第一个 IPv4（端口剥除，值经逐段校验，版本号形状直接落选）。
fn first_ipv4(value: &str) -> Option<String> {
    candidate_words(value).find_map(|word| parse_ipv4(strip_port(word)))
}

/// 文本中第一个域名（端口剥除，转小写）。
fn first_domain(value: &str) -> Option<String> {
    candidate_words(value).find_map(|word| parse_domain(strip_port(word)))
}

/// URL → 主机名（`urlsplit().hostname` 镜像：剥用户信息、端口、路径；
/// IPv6 字面量去方括号，方括号内不再按 `:` 切分）。
fn url_host(url: &str) -> Option<String> {
    let (_, rest) = split_scheme(url)?;
    let after = rest.strip_prefix("//")?;
    let netloc_end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let netloc = &after[..netloc_end];
    let netloc = netloc.rsplit('@').next().unwrap_or(netloc);
    let host = if let Some(inner) = netloc.strip_prefix('[') {
        // IPv6 字面量：[::1]:9000 → ::1（端口在右方括号后，不再切分）。
        inner.split(']').next().unwrap_or(inner)
    } else {
        netloc.split(':').next().unwrap_or(netloc)
    };
    let host = host.trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_lowercase())
    }
}

/// Fact → 资产候选（Python `_assets_from_fact`）。
fn assets_from_fact(mission: &Mission, fact: &Fact) -> Result<Vec<MissionAsset>, EngineError> {
    let source = if fact.kind == "exposure.breakthrough_candidate" {
        MissionAssetSource::Exposure
    } else {
        MissionAssetSource::Fact
    };
    let base = AssetProjectionBase::new(mission, source)
        .with_source(
            fact.id.as_str().to_string(),
            fact.branch_id.clone(),
            fact.run_id.clone(),
        )
        .with_confidence(fact.confidence)
        .with_tags(vec!["fact".to_string(), fact.kind.clone()])
        .with_metadata(Map::from_iter([(
            "fact_kind".to_string(),
            Value::String(fact.kind.clone()),
        )]));
    let data = &fact.data;

    if fact.kind == "asset.subdomain" || fact.kind == "asset.host" {
        let value = string_value(data.get("host").or_else(|| data.get("ip")));
        if let Some(value) = value {
            return Ok(vec![asset_from_base(
                &base,
                host_asset_type(&value),
                &value,
                &[],
            )?]);
        }
    }
    let endpoint_fact_kinds = [
        "asset.http_service",
        "web.endpoint",
        "web.directory",
        "web.interesting_route",
    ];
    if endpoint_fact_kinds.contains(&fact.kind.as_str()) {
        if let Some(url) = string_value(data.get("url")) {
            let asset_type = if fact.kind.starts_with("web.") {
                MissionAssetType::Endpoint
            } else {
                MissionAssetType::Url
            };
            return Ok(vec![asset_from_base(&base, asset_type, &url, &[])?]);
        }
    }
    if fact.kind == "traffic.request" {
        if let Some(url) = string_value(data.get("url")) {
            let mut metadata = base.metadata.clone();
            metadata.insert(
                "method".to_string(),
                data.get("method").cloned().unwrap_or(Value::Null),
            );
            return Ok(vec![asset_from_base(
                &base,
                MissionAssetType::Endpoint,
                &url,
                &[],
            )?]);
        }
    }
    if fact.kind == "asset.open_port" || fact.kind == "asset.service" {
        let host = string_value(data.get("host").or_else(|| data.get("ip")));
        let port = data.get("port");
        let service = string_value(data.get("service").or_else(|| data.get("protocol")));
        if let Some(value) = service_value(host.as_deref(), port, service.as_deref()) {
            let mut metadata = base.metadata.clone();
            metadata.insert("host".to_string(), string_or_null(host.as_deref()));
            metadata.insert("port".to_string(), port.cloned().unwrap_or(Value::Null));
            metadata.insert("service".to_string(), string_or_null(service.as_deref()));
            return Ok(vec![asset_from_base(
                &base,
                MissionAssetType::Service,
                &value,
                &[],
            )?]);
        }
    }
    Ok(asset_specs_from_mapping(data, Some(fact.kind.as_str()))
        .into_iter()
        .map(|spec| mission_asset_from_spec(&base, spec, None))
        .collect::<Result<Vec<_>, _>>()?)
}

/// Evidence → 资产候选（Python `_assets_from_evidence`）。
fn assets_from_evidence(
    mission: &Mission,
    evidence: &Evidence,
) -> Result<Vec<MissionAsset>, EngineError> {
    let sensitivity = sensitivity_from_evidence(evidence);
    let base = AssetProjectionBase::new(mission, MissionAssetSource::Evidence)
        .with_source(
            evidence.id.as_str().to_string(),
            evidence.branch_id.clone(),
            evidence.run_id.clone(),
        )
        .with_evidence_ids(vec![evidence.id.as_str().to_string()])
        .with_tool_invocation_ids(
            evidence
                .produced_by_tool_invocation_id
                .as_ref()
                .map(|id| vec![id.as_str().to_string()])
                .unwrap_or_default(),
        )
        .with_confidence(0.75)
        .with_tags(vec![
            "evidence".to_string(),
            evidence.kind.as_str().to_string(),
        ])
        .with_metadata(Map::from_iter([(
            "evidence_kind".to_string(),
            Value::String(evidence.kind.as_str().to_string()),
        )]));

    let mut assets: Vec<MissionAsset> = asset_specs_from_mapping(&evidence.content, None)
        .into_iter()
        .map(|spec| mission_asset_from_spec(&base, spec, Some(sensitivity)))
        .collect::<Result<Vec<_>, _>>()?;
    for url in urls_from_text(&evidence.summary) {
        assets.push(asset_from_base(&base, MissionAssetType::Url, &url, &[])?);
    }
    for location in &evidence.locations {
        if looks_url(&location.artifact) {
            assets.push(asset_from_base(
                &base,
                MissionAssetType::Url,
                &location.artifact,
                &[],
            )?);
        }
    }
    Ok(assets)
}

/// Finding → 资产候选（Python `_assets_from_finding`）。
fn assets_from_finding(
    mission: &Mission,
    finding: &Finding,
) -> Result<Vec<MissionAsset>, EngineError> {
    let sensitivity = sensitivity_from_finding(finding);
    let base = AssetProjectionBase::new(mission, MissionAssetSource::Finding)
        .with_source(
            finding.id.as_str().to_string(),
            finding.branch_id.clone(),
            finding.run_id.clone(),
        )
        .with_evidence_ids(finding.evidence_ids.clone())
        .with_finding_ids(vec![finding.id.as_str().to_string()])
        .with_sensitivity(sensitivity)
        .with_confidence(0.8)
        .with_tags(vec![
            "finding".to_string(),
            finding.severity.as_str().to_string(),
        ])
        .with_metadata(Map::from_iter([
            (
                "finding_status".to_string(),
                Value::String(finding.status.as_str().to_string()),
            ),
            (
                "finding_severity".to_string(),
                Value::String(finding.severity.as_str().to_string()),
            ),
            (
                "rule_id".to_string(),
                string_or_null(finding.rule_id.as_deref()),
            ),
        ]));

    let mut assets = Vec::new();
    for label in [
        finding.source_label.as_deref(),
        finding.sink_label.as_deref(),
    ] {
        let Some(value) = label.and_then(non_blank) else {
            continue;
        };
        let asset_type = infer_asset_type_from_value(&value);
        if asset_type == MissionAssetType::Unknown {
            continue;
        }
        assets.push(asset_from_base(&base, asset_type, &value, &[])?);
    }
    Ok(assets)
}

/// ToolInvocation → 资产候选（Python `_assets_from_tool_invocation`）。
fn assets_from_tool_invocation(
    mission: &Mission,
    tool: &ToolInvocation,
) -> Result<Vec<MissionAsset>, EngineError> {
    let base = AssetProjectionBase::new(mission, MissionAssetSource::ToolInvocation)
        .with_source(
            tool.id.as_str().to_string(),
            tool.branch_id.clone(),
            tool.run_id.clone(),
        )
        .with_tool_invocation_ids(vec![tool.id.as_str().to_string()])
        .with_confidence(0.65)
        .with_tags(vec!["tool".to_string(), tool.tool_name.clone()])
        .with_metadata(Map::from_iter([
            (
                "tool_name".to_string(),
                Value::String(tool.tool_name.clone()),
            ),
            (
                "tool_status".to_string(),
                Value::String(tool.status.as_str().to_string()),
            ),
        ]));

    let mut assets: Vec<MissionAsset> = urls_from_text(&tool.input_summary)
        .into_iter()
        .chain(urls_from_text(&tool.output_summary))
        .map(|url| asset_from_base(&base, MissionAssetType::Url, &url, &[]))
        .collect::<Result<Vec<_>, _>>()?;
    for artifact_path in &tool.artifact_paths {
        assets.push(asset_from_base(
            &base,
            artifact_asset_type(artifact_path),
            artifact_path,
            &[],
        )?);
    }
    Ok(assets)
}

/// 投影基座（Python `_AssetProjectionBase`）：一次来源解析的公共参数组。
struct AssetProjectionBase<'a> {
    mission: &'a Mission,
    source: MissionAssetSource,
    source_id: Option<String>,
    branch_id: Option<models::BranchId>,
    run_id: Option<RunId>,
    evidence_ids: Vec<String>,
    finding_ids: Vec<String>,
    tool_invocation_ids: Vec<String>,
    sensitivity: Option<MissionAssetSensitivity>,
    confidence: f64,
    tags: Vec<String>,
    metadata: Map<String, Value>,
}

impl<'a> AssetProjectionBase<'a> {
    /// 最小基座（Python 数据类默认值：无来源、置信度 0.5）。
    fn new(mission: &'a Mission, source: MissionAssetSource) -> Self {
        Self {
            mission,
            source,
            source_id: None,
            branch_id: None,
            run_id: None,
            evidence_ids: Vec::new(),
            finding_ids: Vec::new(),
            tool_invocation_ids: Vec::new(),
            sensitivity: None,
            confidence: 0.5,
            tags: Vec::new(),
            metadata: Map::new(),
        }
    }

    fn with_source(
        mut self,
        source_id: String,
        branch_id: Option<models::BranchId>,
        run_id: Option<RunId>,
    ) -> Self {
        self.source_id = Some(source_id);
        self.branch_id = branch_id;
        self.run_id = run_id;
        self
    }

    fn with_evidence_ids(mut self, ids: Vec<String>) -> Self {
        self.evidence_ids = ids;
        self
    }

    fn with_finding_ids(mut self, ids: Vec<String>) -> Self {
        self.finding_ids = ids;
        self
    }

    fn with_tool_invocation_ids(mut self, ids: Vec<String>) -> Self {
        self.tool_invocation_ids = ids;
        self
    }

    fn with_sensitivity(mut self, sensitivity: MissionAssetSensitivity) -> Self {
        self.sensitivity = Some(sensitivity);
        self
    }

    fn with_confidence(mut self, confidence: f64) -> Self {
        self.confidence = confidence;
        self
    }

    fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    fn with_metadata(mut self, metadata: Map<String, Value>) -> Self {
        self.metadata = metadata;
        self
    }
}

/// 构造 [`MissionAsset`]（Python `_mission_asset`）。
///
/// # Errors
/// 值去除首尾空白后为空（Python pydantic ValidationError 同族）。
#[allow(clippy::fn_params_excessive_bools)]
#[allow(clippy::too_many_arguments)]
fn mission_asset(
    mission: &Mission,
    asset_type: MissionAssetType,
    value: &str,
    source: MissionAssetSource,
    source_id: Option<String>,
    label: Option<&str>,
    sensitivity: Option<MissionAssetSensitivity>,
    confidence: f64,
    branch_id: Option<models::BranchId>,
    run_id: Option<RunId>,
    tags: &[String],
    metadata: &Map<String, Value>,
) -> Result<MissionAsset, EngineError> {
    MissionAsset {
        id: MissionAssetId::new(new_id("asset")),
        project_id: mission.project_id.clone(),
        mission_id: mission.id.clone(),
        asset_type,
        value: value.to_string(),
        label: label.map(str::to_string),
        sensitivity: sensitivity.unwrap_or_else(|| default_asset_sensitivity(asset_type)),
        confidence,
        source,
        source_id,
        branch_id,
        run_id,
        evidence_ids: Vec::new(),
        finding_ids: Vec::new(),
        tool_invocation_ids: Vec::new(),
        tags: tags.to_vec(),
        metadata: metadata.clone(),
        created_at: utcnow(),
        updated_at: utcnow(),
    }
    .validated()
    .map_err(EngineError::ReferenceValidationError)
}

/// 基座 + 显式字段 → [`MissionAsset`]（Python `_asset_from_base`）。
///
/// Python 语义细节：`metadata or base.metadata`——显式 metadata 为空时
/// 回落基座 metadata。
fn asset_from_base(
    base: &AssetProjectionBase<'_>,
    asset_type: MissionAssetType,
    value: &str,
    extra_tags: &[String],
) -> Result<MissionAsset, EngineError> {
    let mut tags = base.tags.clone();
    tags.extend(extra_tags.iter().cloned());
    let mut asset = mission_asset(
        base.mission,
        asset_type,
        value,
        base.source,
        base.source_id.clone(),
        None,
        base.sensitivity,
        base.confidence,
        base.branch_id.clone(),
        base.run_id.clone(),
        &tags,
        &base.metadata,
    )?;
    asset.evidence_ids = base.evidence_ids.clone();
    asset.finding_ids = base.finding_ids.clone();
    asset.tool_invocation_ids = base.tool_invocation_ids.clone();
    Ok(asset)
}

/// 内部资产规格（Python `_asset_spec_*` 产出的 dict 形态的结构化镜像）。
struct AssetSpec {
    asset_type: MissionAssetType,
    value: String,
    label: Option<String>,
    sensitivity: Option<MissionAssetSensitivity>,
    metadata: Map<String, Value>,
    tags: Vec<String>,
}

/// 规格化资产（Python `_mission_asset_from_spec`）。
///
/// Python 语义细节：规格 metadata 覆盖基座 metadata（`metadata.update`）；
/// `label` 仅字符串形态生效；`sensitivity` 规格优先、缺省回落传入的
/// `default_sensitivity`（Evidence 投影传敏感级别）。
fn mission_asset_from_spec(
    base: &AssetProjectionBase<'_>,
    spec: AssetSpec,
    default_sensitivity: Option<MissionAssetSensitivity>,
) -> Result<MissionAsset, EngineError> {
    let mut metadata = base.metadata.clone();
    for (key, value) in spec.metadata {
        metadata.insert(key, value);
    }
    let sensitivity = spec.sensitivity.or(default_sensitivity);
    let mut tags = base.tags.clone();
    tags.extend(spec.tags.iter().cloned());
    let mut asset = mission_asset(
        base.mission,
        spec.asset_type,
        &spec.value,
        base.source,
        base.source_id.clone(),
        spec.label.as_deref(),
        sensitivity,
        base.confidence,
        base.branch_id.clone(),
        base.run_id.clone(),
        &tags,
        &metadata,
    )?;
    asset.evidence_ids = base.evidence_ids.clone();
    asset.finding_ids = base.finding_ids.clone();
    asset.tool_invocation_ids = base.tool_invocation_ids.clone();
    Ok(asset)
}

/// 映射载荷 → 显式 + 结构化字段规格（Python `_asset_specs_from_mapping`）。
fn asset_specs_from_mapping(
    payload: &Map<String, Value>,
    source_kind: Option<&str>,
) -> Vec<AssetSpec> {
    let mut specs = Vec::new();
    for raw in explicit_asset_items(payload) {
        if let Some(spec) = asset_spec_from_explicit_item(raw) {
            specs.push(spec);
        }
    }
    for (key, asset_type) in STRUCTURED_ASSET_FIELDS {
        let Some(raw_value) = payload.get(*key) else {
            continue;
        };
        for value in string_values(raw_value) {
            let stored_value = if matches!(
                asset_type,
                MissionAssetType::Secret | MissionAssetType::Credential
            ) {
                redacted_sensitive_value(&value, key)
            } else {
                value.clone()
            };
            specs.push(AssetSpec {
                asset_type: *asset_type,
                value: stored_value.clone(),
                label: asset_label(payload, key),
                sensitivity: Some(sensitivity_for_structured_key(key, *asset_type)),
                metadata: Map::from_iter([
                    ("field".to_string(), Value::String((*key).to_string())),
                    (
                        "source_kind".to_string(),
                        source_kind
                            .map(str::to_string)
                            .map_or(Value::Null, Value::String),
                    ),
                    ("redacted".to_string(), Value::Bool(stored_value != value)),
                ]),
                tags: Vec::new(),
            });
        }
    }
    specs
}

/// 显式资产条目（Python `_explicit_asset_items`）：`assets` 列表 + 单个
/// `asset`（非 Null 才进列表）。
fn explicit_asset_items(payload: &Map<String, Value>) -> Vec<&Value> {
    let mut items = Vec::new();
    if let Some(Value::Array(raw_assets)) = payload.get("assets") {
        items.extend(raw_assets.iter());
    }
    if let Some(raw_asset) = payload.get("asset").filter(|value| !value.is_null()) {
        items.push(raw_asset);
    }
    items
}

/// 单条显式资产 → 规格（Python `_asset_spec_from_explicit_item`）。
fn asset_spec_from_explicit_item(raw: &Value) -> Option<AssetSpec> {
    if let Value::String(text) = raw {
        let stripped = text.trim().to_string();
        if stripped.is_empty() {
            return None;
        }
        return Some(AssetSpec {
            asset_type: infer_asset_type_from_value(&stripped),
            value: stripped,
            label: None,
            sensitivity: None,
            metadata: Map::new(),
            tags: Vec::new(),
        });
    }
    let Value::Object(map) = raw else {
        return None;
    };
    let raw_type = map
        .get("asset_type")
        .or_else(|| map.get("type"))
        .or_else(|| map.get("kind"))
        .filter(|value| !value.is_null());
    let mut asset_type = raw_type.and_then(Value::as_str).map(coerce_asset_type);
    let mut value = string_value(map.get("value"));
    if value.is_none() {
        for (key, inferred_type) in STRUCTURED_ASSET_FIELDS {
            if let Some(found) = string_value(map.get(*key)) {
                value = Some(found);
                asset_type = asset_type.or(Some(*inferred_type));
                break;
            }
        }
    }
    let value = value?;
    let asset_type = asset_type.unwrap_or_else(|| infer_asset_type_from_value(&value));
    let stored_value = if matches!(
        asset_type,
        MissionAssetType::Secret | MissionAssetType::Credential
    ) {
        redacted_sensitive_value(&value, "value")
    } else {
        value.clone()
    };
    let sensitivity = map
        .get("sensitivity")
        .filter(|value| !value.is_null())
        .and_then(Value::as_str)
        .map(coerce_asset_sensitivity)
        .or_else(|| Some(default_asset_sensitivity(asset_type)));
    Some(AssetSpec {
        asset_type,
        value: stored_value.clone(),
        label: string_value(map.get("label").or_else(|| map.get("name"))),
        sensitivity,
        metadata: Map::from_iter([
            ("explicit_asset".to_string(), Value::Bool(true)),
            ("redacted".to_string(), Value::Bool(stored_value != value)),
        ]),
        tags: Vec::new(),
    })
}

/// 结构化资产字段 → 类别表（Python `_STRUCTURED_ASSET_FIELDS`，插入序保留）。
const STRUCTURED_ASSET_FIELDS: &[(&str, MissionAssetType)] = &[
    ("url", MissionAssetType::Url),
    ("base_url", MissionAssetType::Url),
    ("target_url", MissionAssetType::Url),
    ("matched_at", MissionAssetType::Endpoint),
    ("endpoint", MissionAssetType::Endpoint),
    ("api", MissionAssetType::Api),
    ("host", MissionAssetType::Host),
    ("hostname", MissionAssetType::Host),
    ("domain", MissionAssetType::Domain),
    ("target_domain", MissionAssetType::Domain),
    ("ip", MissionAssetType::Ip),
    ("service", MissionAssetType::Service),
    ("repo", MissionAssetType::Repository),
    ("repository", MissionAssetType::Repository),
    ("repo_path", MissionAssetType::SourcePath),
    ("source_path", MissionAssetType::SourcePath),
    ("source_root", MissionAssetType::SourcePath),
    ("path", MissionAssetType::SourcePath),
    ("artifact", MissionAssetType::SourcePath),
    ("artifact_path", MissionAssetType::TrafficCapture),
    ("binary", MissionAssetType::Binary),
    ("binary_path", MissionAssetType::Binary),
    ("cloud", MissionAssetType::CloudResource),
    ("cloud_resource", MissionAssetType::CloudResource),
    ("resource_id", MissionAssetType::CloudResource),
    ("cluster", MissionAssetType::CloudResource),
    ("account", MissionAssetType::Account),
    ("account_id", MissionAssetType::Account),
    ("subscription", MissionAssetType::CloudResource),
    ("package", MissionAssetType::Package),
    ("container", MissionAssetType::Container),
    ("secret_ref", MissionAssetType::Secret),
    ("secret_name", MissionAssetType::Secret),
    ("secret", MissionAssetType::Secret),
    ("token", MissionAssetType::Secret),
    ("api_key", MissionAssetType::Secret),
    ("private_key", MissionAssetType::Secret),
    ("credential_ref", MissionAssetType::Credential),
    ("credential", MissionAssetType::Credential),
    ("username", MissionAssetType::Account),
];

/// target 键 → 资产类别（Python `_target_asset_type`，`None` = 不投影）。
fn target_asset_type(key: &str, value: &str) -> Option<MissionAssetType> {
    let normalized_key = key.trim().to_lowercase();
    if normalized_key == "url" || normalized_key == "base_url" {
        return Some(MissionAssetType::Url);
    }
    if normalized_key == "target" && looks_url(value) {
        return Some(MissionAssetType::Url);
    }
    if normalized_key == "domain" || normalized_key == "target_domain" {
        return Some(MissionAssetType::Domain);
    }
    if normalized_key == "host" || normalized_key == "hostname" {
        return Some(host_asset_type(value));
    }
    if matches!(normalized_key.as_str(), "repo" | "repository" | "git_url") {
        return Some(MissionAssetType::Repository);
    }
    if matches!(
        normalized_key.as_str(),
        "repo_path" | "local_path" | "path" | "source_root" | "source"
    ) {
        return Some(MissionAssetType::SourcePath);
    }
    if matches!(
        normalized_key.as_str(),
        "binary" | "binary_path" | "ida_database"
    ) {
        return Some(MissionAssetType::Binary);
    }
    if matches!(
        normalized_key.as_str(),
        "artifact_path" | "traffic_artifact" | "har" | "pcap" | "capture"
    ) {
        return Some(MissionAssetType::TrafficCapture);
    }
    if matches!(
        normalized_key.as_str(),
        "cloud" | "cloud_resource" | "resource_id" | "cluster"
    ) {
        return Some(MissionAssetType::CloudResource);
    }
    // `account` / `subscription` 单独存在时就是账号资产；只有伴随
    // `cloud` / `cluster` 键才表示云资源。
    if normalized_key == "account" || normalized_key == "subscription" {
        return Some(MissionAssetType::Account);
    }
    if matches!(
        normalized_key.as_str(),
        "secret" | "secret_ref" | "credential" | "credential_ref"
    ) {
        return Some(MissionAssetType::Secret);
    }
    None
}

/// 工件路径后缀 → 类别（Python `_artifact_asset_type`）。
fn artifact_asset_type(path: &str) -> MissionAssetType {
    let lowered = path.to_lowercase();
    if [".har", ".pcap", ".pcapng", ".saz"]
        .iter()
        .any(|suffix| lowered.ends_with(suffix))
    {
        return MissionAssetType::TrafficCapture;
    }
    if [".exe", ".dll", ".so", ".dylib", ".bin", ".elf"]
        .iter()
        .any(|suffix| lowered.ends_with(suffix))
    {
        return MissionAssetType::Binary;
    }
    MissionAssetType::SourcePath
}

/// 值形态推断类别（Python `_infer_asset_type_from_value`）。
fn infer_asset_type_from_value(value: &str) -> MissionAssetType {
    let stripped = value.trim();
    if looks_url(stripped) {
        return MissionAssetType::Url;
    }
    if stripped.starts_with('/') || stripped.contains(":\\") || stripped.contains('/') {
        return MissionAssetType::SourcePath;
    }
    if stripped.chars().any(char::is_whitespace) {
        return MissionAssetType::Unknown;
    }
    if IpAddr::from_str(stripped).is_ok() {
        return MissionAssetType::Ip;
    }
    if looks_host(stripped) {
        return MissionAssetType::Host;
    }
    MissionAssetType::Unknown
}

/// 主机值 → IP/HOST（Python `_host_asset_type`）。
fn host_asset_type(value: &str) -> MissionAssetType {
    if IpAddr::from_str(value.trim()).is_ok() {
        MissionAssetType::Ip
    } else {
        MissionAssetType::Host
    }
}

/// host[:port] 服务值（Python `_service_value`）。
fn service_value(
    host: Option<&str>,
    port: Option<&Value>,
    service: Option<&str>,
) -> Option<String> {
    let host = host?;
    let port_value = match port {
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::String(text)) if !text.trim().is_empty() => text.trim().to_string(),
        _ => String::new(),
    };
    match service {
        Some(service) if !service.is_empty() => Some(if port_value.is_empty() {
            format!("{service}://{host}")
        } else {
            format!("{service}://{host}:{port_value}")
        }),
        _ => Some(if port_value.is_empty() {
            host.to_string()
        } else {
            format!("{host}:{port_value}")
        }),
    }
}

/// Evidence → 敏感级别（Python `_sensitivity_from_evidence`）。
fn sensitivity_from_evidence(evidence: &Evidence) -> MissionAssetSensitivity {
    if let Some(Value::String(severity)) = evidence.content.get("severity") {
        let severity = severity.to_lowercase();
        if severity == "high" || severity == "critical" {
            return MissionAssetSensitivity::Sensitive;
        }
    }
    let text = format!(
        "{} {} {}",
        evidence.kind.as_str(),
        evidence.summary,
        Value::Object(evidence.content.clone())
    )
    .to_lowercase();
    if SECRET_WORDS.iter().any(|word| text.contains(word)) {
        return MissionAssetSensitivity::Sensitive;
    }
    MissionAssetSensitivity::Unknown
}

/// Finding → 敏感级别（Python `_sensitivity_from_finding`）。
fn sensitivity_from_finding(finding: &Finding) -> MissionAssetSensitivity {
    if matches!(finding.severity, Severity::High | Severity::Critical) {
        return MissionAssetSensitivity::Sensitive;
    }
    let text = format!(
        "{} {} {}",
        finding.title,
        finding.description.clone().unwrap_or_default(),
        finding.rule_id.clone().unwrap_or_default()
    )
    .to_lowercase();
    if SECRET_WORDS.iter().any(|word| text.contains(word)) {
        return MissionAssetSensitivity::Sensitive;
    }
    MissionAssetSensitivity::Unknown
}

/// 敏感词表（Python `_sensitivity_from_*` 的 `secret_words`）。
const SECRET_WORDS: [&str; 5] = ["secret", "credential", "token", "api key", "private key"];

/// 类别 → 默认敏感级别（Python `_default_asset_sensitivity`）。
fn default_asset_sensitivity(asset_type: MissionAssetType) -> MissionAssetSensitivity {
    match asset_type {
        MissionAssetType::Secret | MissionAssetType::Credential | MissionAssetType::Account => {
            MissionAssetSensitivity::Sensitive
        }
        MissionAssetType::Url
        | MissionAssetType::Endpoint
        | MissionAssetType::Host
        | MissionAssetType::Domain
        | MissionAssetType::Ip
        | MissionAssetType::Service
        | MissionAssetType::Repository
        | MissionAssetType::SourcePath
        | MissionAssetType::Binary
        | MissionAssetType::TrafficCapture
        | MissionAssetType::Api
        | MissionAssetType::Package
        | MissionAssetType::Container => MissionAssetSensitivity::NonSensitive,
        _ => MissionAssetSensitivity::Unknown,
    }
}

/// 结构化字段键 → 敏感级别（Python `_sensitivity_for_structured_key`）。
fn sensitivity_for_structured_key(
    key: &str,
    asset_type: MissionAssetType,
) -> MissionAssetSensitivity {
    if matches!(
        key,
        "secret"
            | "secret_ref"
            | "secret_name"
            | "token"
            | "api_key"
            | "private_key"
            | "credential"
            | "credential_ref"
            | "account"
            | "account_id"
            | "username"
    ) {
        return MissionAssetSensitivity::Sensitive;
    }
    default_asset_sensitivity(asset_type)
}

/// 自由文本 → 类别（Python `_coerce_asset_type` 的字符串分支；别名表
/// 之外的未知值回落 `UNKNOWN`）。
fn coerce_asset_type(raw: &str) -> MissionAssetType {
    let normalized = raw.trim().to_lowercase().replace('-', "_");
    let aliases = [
        ("uri", "url"),
        ("route", "endpoint"),
        ("subdomain", "host"),
        ("repo", "repository"),
        ("repo_path", "source_path"),
        ("source", "source_path"),
        ("path", "source_path"),
        ("artifact_path", "traffic_capture"),
        ("token", "secret"),
        ("api_key", "secret"),
        ("key", "secret"),
        ("user", "account"),
    ];
    let mapped = aliases
        .iter()
        .find(|(alias, _)| *alias == normalized)
        .map(|(_, canonical)| (*canonical).to_string())
        .unwrap_or(normalized);
    MissionAssetType::parse(&mapped).unwrap_or(MissionAssetType::Unknown)
}

/// 自由文本 → 敏感级别（Python `_coerce_asset_sensitivity`；未知值回落
/// `UNKNOWN`——Python 侧直接抛 ValueError，此处唯一调用方已经过 JSON
/// 解析容错）。
fn coerce_asset_sensitivity(raw: &str) -> MissionAssetSensitivity {
    match raw {
        "sensitive" => MissionAssetSensitivity::Sensitive,
        "non_sensitive" => MissionAssetSensitivity::NonSensitive,
        _ => MissionAssetSensitivity::Unknown,
    }
}

/// 文本中的全部 URL（Python `_urls_from_text`，尾部 `.,);]` 剥除）。
///
/// 字符类用 RFC 3986 允许的 URL 字符（unreserved + reserved + `%`）正向
/// 列举：中文用户写"审计 http://x.test/admin，检查注入"时 URL 后不打
/// 空格，容忍非 ASCII 会把"，检查注入"整段吞进 URL 值里。
fn urls_from_text(text: &str) -> Vec<String> {
    static URL_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let url_re = URL_RE.get_or_init(|| {
        regex::RegexBuilder::new(r#"(?i)https?://[A-Za-z0-9\-._~:/?#\[\]@!$&'()*+,;=%]+"#)
            .build()
            .expect("静态正则合法")
    });
    url_re
        .find_iter(text)
        .map(|matched| matched.as_str().trim_end_matches(".,);]").to_string())
        .collect()
}

/// 是否为 http(s) URL（Python `_looks_url`）。
fn looks_url(value: &str) -> bool {
    let Some((scheme, rest)) = split_scheme(value) else {
        return false;
    };
    let scheme = scheme.to_lowercase();
    if scheme != "http" && scheme != "https" {
        return false;
    }
    match rest.strip_prefix("//") {
        Some(after) => {
            let netloc_end = after.find(['/', '?', '#']).unwrap_or(after.len());
            !after[..netloc_end].is_empty()
        }
        None => false,
    }
}

/// 主机名形态（Python `_ASSET_HOST_RE`：localhost 或点分域名标签）。
fn looks_host(value: &str) -> bool {
    static HOST_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let host_re = HOST_RE.get_or_init(|| {
        regex::RegexBuilder::new(
            r"(?i)^(localhost|[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)+)$",
        )
        .build()
        .expect("静态正则合法")
    });
    host_re.is_match(value)
}

/// `urlsplit` 的 scheme 切分（与 models::asset 模块同规则）。
fn split_scheme(value: &str) -> Option<(String, String)> {
    let index = value.find(':')?;
    let scheme = &value[..index];
    if scheme.is_empty() || !scheme.starts_with(char::is_alphabetic) {
        return None;
    }
    if !scheme
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    {
        return None;
    }
    Some((scheme.to_string(), value[index + 1..].to_string()))
}

/// JSON 值 → 非空字符串（Python `_string_value`）。
fn string_value(raw: Option<&Value>) -> Option<String> {
    raw.and_then(|value| value.as_str().and_then(non_blank))
}

/// 字符串 → 去空白非空形态（Python `_string_value` 的 str 分支）。
fn non_blank(raw: &str) -> Option<String> {
    let stripped = raw.trim();
    if stripped.is_empty() {
        None
    } else {
        Some(stripped.to_string())
    }
}

/// 标量或列表 → 全部非空字符串（Python `_string_values`）。
fn string_values(raw: &Value) -> Vec<String> {
    match raw {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.as_str().and_then(non_blank))
            .collect(),
        Value::String(text) => non_blank(text).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// 载荷标签（Python `_asset_label`：`label` / `name` / 键名）。
fn asset_label(payload: &Map<String, Value>, key: &str) -> Option<String> {
    string_value(payload.get("label").or_else(|| payload.get("name")))
        .or_else(|| Some(key.to_string()))
}

/// 敏感值脱敏（Python `_redacted_sensitive_value`）。
///
/// 安全引用键保留原文；其余以 `key:sha256:` + SHA-256 前 12 位十六进制
/// 摘要代替。
fn redacted_sensitive_value(value: &str, key: &str) -> String {
    const SAFE_REFERENCE_KEYS: [&str; 4] = ["secret_ref", "secret_name", "credential_ref", "name"];
    if SAFE_REFERENCE_KEYS.contains(&key) {
        return value.to_string();
    }
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(value.as_bytes());
    format!("{key}:sha256:{}", hex_prefix(&digest, 12))
}

/// 摘要前 `len` 个十六进制字符。
fn hex_prefix(bytes: &[u8], len: usize) -> String {
    bytes
        .iter()
        .flat_map(|byte| [byte / 16, byte % 16])
        .take(len)
        .map(|nibble| char::from_digit(u32::from(nibble), 16).unwrap_or('0'))
        .collect()
}

/// `Option<&str>` → JSON 字符串或 Null。
fn string_or_null(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |text| Value::String(text.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mission_with_target(user_goal: &str, target: &[(&str, &str)]) -> Mission {
        let mut mission = Mission::new(
            models::ProjectId::new("proj_test".to_string()),
            user_goal.to_string(),
        );
        mission.target = target
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        mission
    }

    #[test]
    fn sniff_prefers_url_and_attaches_host() {
        let found = sniff_target_value("审计 https://shop.example.test/api/v1/orders 的 sort 参数");
        assert_eq!(
            found,
            vec![
                (
                    MissionAssetType::Url,
                    "https://shop.example.test/api/v1/orders".to_string()
                ),
                (MissionAssetType::Host, "shop.example.test".to_string()),
            ]
        );
    }

    #[test]
    fn sniff_url_with_ip_host_yields_ip_asset() {
        let found = sniff_target_value("目标 http://10.0.0.8:8080/admin，注意越权");
        assert_eq!(
            found,
            vec![
                (
                    MissionAssetType::Url,
                    "http://10.0.0.8:8080/admin".to_string()
                ),
                (MissionAssetType::Ip, "10.0.0.8".to_string()),
            ]
        );
    }

    #[test]
    fn sniff_ipv4_strips_port_and_validates_octets() {
        assert_eq!(
            sniff_target_value("内网 192.168.1.10:8443 的面板"),
            vec![(MissionAssetType::Ip, "192.168.1.10".to_string())]
        );
        // 非法八位组不是 IP；也不构成域名（末段是数字）——零资产。
        assert!(sniff_target_value("版本 999.1.1.1 有问题").is_empty());
    }

    #[test]
    fn sniff_domain_strips_port_and_lowercases() {
        assert_eq!(
            sniff_target_value("扫一下 Example.COM:8443 的登录口"),
            vec![(MissionAssetType::Domain, "example.com".to_string())]
        );
    }

    #[test]
    fn sniff_prose_yields_nothing() {
        assert!(sniff_target_value("你好").is_empty());
        assert!(sniff_target_value("审计一下某站点").is_empty());
    }

    #[test]
    fn url_host_strips_userinfo_port_path_and_ipv6_brackets() {
        assert_eq!(url_host("https://a.example.test/x?y#z"), Some("a.example.test".to_string()));
        assert_eq!(url_host("http://user:pw@b.example.test:8080/"), Some("b.example.test".to_string()));
        assert_eq!(url_host("http://[::1]:9000/"), Some("::1".to_string()));
        assert_eq!(url_host("not a url"), None);
    }

    #[test]
    fn raw_prompt_key_is_sniffed_from_value() {
        let mission = mission_with_target(
            "审计一下 https://shop.example.test",
            &[("raw_prompt", "审计一下 https://shop.example.test")],
        );
        let assets = assets_from_mission_target(&mission).expect("投影不得失败");
        let pairs: Vec<(MissionAssetType, String)> = assets
            .iter()
            .map(|asset| (asset.asset_type.clone(), asset.value.clone()))
            .collect();
        assert!(
            pairs.contains(&(MissionAssetType::Url, "https://shop.example.test".to_string())),
            "raw_prompt 里的 URL 必须被嗅探出来: {pairs:?}"
        );
        assert!(
            pairs.contains(&(MissionAssetType::Host, "shop.example.test".to_string())),
            "URL 主机必须一并落资产: {pairs:?}"
        );
        // 嗅探来源可追溯。
        assert!(
            assets
                .iter()
                .all(|asset| asset.tags.contains(&"sniffed".to_string())
                    && asset.metadata.get("target_key").is_some()),
            "嗅探资产必须带 sniffed 标签与来源键"
        );
    }

    #[test]
    fn known_key_path_wins_over_sniffing() {
        let mission = mission_with_target(
            "审计 https://shop.example.test",
            &[("url", "https://shop.example.test")],
        );
        let assets = assets_from_mission_target(&mission).expect("投影不得失败");
        // 已知键走键映射（无 sniffed 标签）；user_goal 嗅探出的同值资产
        // 由 upsert 去重，这里只断言键映射产物存在且不带 sniffed 标签。
        let key_mapped: Vec<&MissionAsset> = assets
            .iter()
            .filter(|asset| asset.value == "https://shop.example.test")
            .collect();
        assert!(!key_mapped.is_empty(), "已知键必须产出资产");
    }

    #[test]
    fn plain_text_goal_projects_no_target_asset() {
        let mission = mission_with_target("你好", &[("raw_prompt", "你好")]);
        let assets = assets_from_mission_target(&mission).expect("投影不得失败");
        assert!(
            assets.is_empty(),
            "纯自然语言不编造资产: {:?}",
            assets.iter().map(|a| a.value.clone()).collect::<Vec<_>>()
        );
    }
}
