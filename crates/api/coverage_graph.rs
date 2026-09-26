//! 资产覆盖图 —— 由后端推导的资产层级覆盖关系。
//!
//! 覆盖图是**后端**产物：它把任务的全部在范围内的资产按类型铺开，
//! 用根域名/公司节点把它们连成一张图，每个资产节点自带 `tested` /
//! `in_scope` 两个标志，前端只负责渲染。Lynceus 这里同样在后端推导，
//! 理由有三：
//!
//! 1. **推导规则是领域知识，不该每家前端各写一遍。** 层级关系
//!    （根域名 → 子域名 → 应用 → 接口，IP → 服务）来自资产语义，
//!    放后端才能被 CLI / 其他消费方复用。
//! 2. **`tested` 需要跨实体关联。** 一个资产是否被"行使过"要看
//!    `evidence_ids ∪ finding_ids ∪ tool_invocation_ids`，这些列表分散在
//!    不同仓储读取里，后端一次取齐比分页前端拼更可靠。
//! 3. **覆盖是验收指标，不能靠前端现算。** 前端算出来的覆盖率没法被
//!    测试和 CLI 复现。
//!
//! 设计取舍（相对常见做法的差异，有意为之）：
//! - 常见做法用力导向布局，节点坐标由后端算；Lynceus 只产出
//!   **逻辑图**（节点 + 父子边），坐标交给前端的 tidy-tree 布局器。
//!   理由：坐标是纯展示关注点，且 Lynceus 前端已有稳定的折叠/展开交互。
//! - 常见做法带覆盖开关与公司维度；Lynceus 没有公司实体，
//!   一律以 Mission 根节点兜底，不引入假维度。

use std::collections::{BTreeMap, BTreeSet};

use models::asset::{MissionAsset, MissionAssetType};
use models::ids::MissionId;
use models::Finding;
use models::lifecycle::Severity;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::{ApiError, ApiState};
use axum::extract::{Path, State};
use axum::Json;
use runtime::errors::EngineError;

/// 覆盖图节点类别（前端 `KIND_META` 的权威来源，避免两边各写一份）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageNodeKind {
    /// Mission 根节点。
    Mission,
    /// 根域名（`domain` 资产）。
    RootDomain,
    /// 主机名/子域名（`host` 资产）。
    Subdomain,
    /// IP。
    Ip,
    /// 应用（`url` 资产）。
    App,
    /// 服务（`service` 资产）。
    Service,
    /// 接口（`endpoint` / `api` 资产）。
    Endpoint,
    /// 无法归类的资产，统一挂到 Mission 根下。
    Other,
}

/// 覆盖图节点。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CoverageNode {
    /// 稳定键（资产 id；Mission 根节点为 `mission:<id>`）。
    pub key: String,
    /// 类别。
    pub kind: CoverageNodeKind,
    /// 展示名。
    pub label: String,
    /// 资产值（Mission 根节点为 Mission 标题）。
    pub value: String,
    /// 资产类别 wire 值（Mission 根节点为 `null`）。
    pub asset_type: Option<String>,
    /// 是否已被任务行使过（证据/发现/工具调用任一关联即算）。
    pub tested: bool,
    /// 置信度（Mission 根节点为 `null`）。
    pub confidence: Option<f64>,
    /// 关联 Finding ID。
    pub finding_ids: Vec<String>,
    /// 关联 Evidence ID。
    pub evidence_ids: Vec<String>,
    /// 关联 ToolInvocation ID。
    pub tool_invocation_ids: Vec<String>,
    /// 原始扩展属性（供抽屉展示）。
    pub metadata: Map<String, Value>,
}

/// 覆盖图边（`from` 是 `to` 的父节点）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CoverageEdge {
    /// 父节点键。
    pub from: String,
    /// 子节点键。
    pub to: String,
}

/// 覆盖图汇总。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoverageStats {
    /// 节点总数（不含 Mission 根）。
    pub total: usize,
    /// 已行使节点数。
    pub tested: usize,
    /// 各类别计数（键 = [`CoverageNodeKind`] 的 wire 值）。
    pub by_kind: BTreeMap<String, usize>,
}

/// `GET /missions/{mission_id}/coverage-graph` 的响应。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CoverageGraph {
    /// Mission 标识。
    pub mission_id: String,
    /// 节点（首元素恒为 Mission 根）。
    pub nodes: Vec<CoverageNode>,
    /// 父子边。
    pub edges: Vec<CoverageEdge>,
    /// 汇总。
    pub stats: CoverageStats,
}

/// 资产是否已被任务行使过。
///
/// 只认真实关联：三项列表任一非空即算。不根据资产存在时间、来源或
/// 置信度猜测——未关联就是未测，覆盖图宁可少算不可虚报。
#[must_use]
pub fn asset_is_tested(asset: &MissionAsset) -> bool {
    !asset.evidence_ids.is_empty()
        || !asset.finding_ids.is_empty()
        || !asset.tool_invocation_ids.is_empty()
}

/// URL → `(scheme, host, port)`；解析不了返回 `None`。
fn split_url(value: &str) -> Option<(String, String, Option<u16>)> {
    let trimmed = value.trim();
    let (scheme, rest) = trimmed.split_once("://")?;
    if scheme.is_empty() || rest.is_empty() {
        return None;
    }
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .to_string();
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) && !port.is_empty() => {
            (host.to_string(), port.parse::<u16>().ok())
        }
        _ => (authority.clone(), None),
    };
    if host.is_empty() {
        return None;
    }
    Some((scheme.to_lowercase(), host.to_lowercase(), port))
}

/// 主机名 → 根域名（取最后两段）；IP 原样返回。
#[must_use]
pub fn root_domain_of(host: &str) -> String {
    let trimmed = host.trim().trim_end_matches('.').to_lowercase();
    if trimmed.parse::<std::net::IpAddr>().is_ok() {
        return trimmed;
    }
    let labels = trimmed.split('.').collect::<Vec<_>>();
    if labels.len() <= 2 {
        return trimmed;
    }
    labels[labels.len() - 2..].join(".")
}

/// 节点类别（资产类别 → 覆盖图类别；与前端 `ASSET_GROUPS` 一致）。
#[must_use]
pub fn node_kind_of(asset_type: MissionAssetType) -> CoverageNodeKind {
    match asset_type {
        MissionAssetType::Domain => CoverageNodeKind::RootDomain,
        MissionAssetType::Ip => CoverageNodeKind::Ip,
        MissionAssetType::Host => CoverageNodeKind::Subdomain,
        MissionAssetType::Url => CoverageNodeKind::App,
        MissionAssetType::Service => CoverageNodeKind::Service,
        MissionAssetType::Endpoint | MissionAssetType::Api => CoverageNodeKind::Endpoint,
        _ => CoverageNodeKind::Other,
    }
}

/// 单个资产 → 节点。
fn node_for(asset: &MissionAsset) -> CoverageNode {
    CoverageNode {
        key: asset.id.as_str().to_string(),
        kind: node_kind_of(asset.asset_type),
        label: asset
            .label
            .clone()
            .unwrap_or_else(|| asset.value.trim().to_string()),
        value: asset.value.trim().to_string(),
        asset_type: Some(asset.asset_type.as_str().to_string()),
        tested: asset_is_tested(asset),
        confidence: Some(asset.confidence),
        finding_ids: asset.finding_ids.clone(),
        evidence_ids: asset.evidence_ids.clone(),
        tool_invocation_ids: asset.tool_invocation_ids.clone(),
        metadata: asset.metadata.clone(),
    }
}

/// 推导资产的父节点键。
///
/// 顺序固定，保证同一份资产永远得到同一张图（测试可复现）：
/// `domain`/`ip` → Mission 根；`host` → 同名根域名（否则 Mission 根）；
/// `service` → `metadata.host` 指向的 host/ip（否则 Mission 根）；
/// `url` → origin 对应 host（否则根域名，否则 Mission 根）；
/// `endpoint`/`api` → origin 对应 url（否则 host，否则根域名，否则 Mission 根）。
fn parent_key_of(
    asset: &MissionAsset,
    mission_key: &str,
    by_value: &BTreeMap<(String, String), String>,
) -> String {
    let lookup = |asset_type: MissionAssetType, value: &str| {
        by_value
            .get(&(asset_type.as_str().to_string(), value.trim().to_lowercase()))
            .cloned()
    };
    match asset.asset_type {
        MissionAssetType::Domain | MissionAssetType::Ip => mission_key.to_string(),
        MissionAssetType::Host => {
            let root = root_domain_of(&asset.value);
            lookup(MissionAssetType::Domain, &root).unwrap_or_else(|| mission_key.to_string())
        }
        MissionAssetType::Service => {
            let host = asset
                .metadata
                .get("host")
                .and_then(Value::as_str)
                .map(str::to_string);
            match host.as_deref() {
                Some(host) => lookup(MissionAssetType::Host, host)
                    .or_else(|| lookup(MissionAssetType::Ip, host))
                    .unwrap_or_else(|| mission_key.to_string()),
                None => mission_key.to_string(),
            }
        }
        MissionAssetType::Url => {
            let Some((_, host, _)) = split_url(&asset.value) else {
                return mission_key.to_string();
            };
            lookup(MissionAssetType::Host, &host)
                .or_else(|| lookup(MissionAssetType::Domain, &root_domain_of(&host)))
                .unwrap_or_else(|| mission_key.to_string())
        }
        MissionAssetType::Endpoint | MissionAssetType::Api => {
            let Some((_, host, _)) = split_url(&asset.value) else {
                return mission_key.to_string();
            };
            let origin = split_url(&asset.value)
                .map(|(scheme, host, port)| match port {
                    Some(port) => format!("{scheme}://{host}:{port}"),
                    None => format!("{scheme}://{host}"),
                })
                .unwrap_or_default();
            lookup(MissionAssetType::Url, &origin)
                .or_else(|| lookup(MissionAssetType::Host, &host))
                .or_else(|| lookup(MissionAssetType::Domain, &root_domain_of(&host)))
                .unwrap_or_else(|| mission_key.to_string())
        }
        _ => mission_key.to_string(),
    }
}

/// 由 Mission 资产推导覆盖图。
///
/// `assets` 必须已经按 Mission 过滤过；标题只用于 Mission 根节点展示。
#[must_use]
pub fn build_coverage_graph(
    mission_id: &MissionId,
    mission_title: &str,
    assets: &[MissionAsset],
) -> CoverageGraph {
    let mission_key = format!("mission:{}", mission_id.as_str());
    let mut by_value: BTreeMap<(String, String), String> = BTreeMap::new();
    for asset in assets {
        by_value.insert(
            (
                asset.asset_type.as_str().to_string(),
                asset.value.trim().to_lowercase(),
            ),
            asset.id.as_str().to_string(),
        );
        // `url` 资产的值带路径，接口要靠 origin 反查它：额外按 origin 建一条索引。
        if asset.asset_type == MissionAssetType::Url
            && let Some((scheme, host, port)) = split_url(&asset.value)
        {
            let origin = match port {
                Some(port) => format!("{scheme}://{host}:{port}"),
                None => format!("{scheme}://{host}"),
            };
            by_value
                .entry((MissionAssetType::Url.as_str().to_string(), origin))
                .or_insert_with(|| asset.id.as_str().to_string());
        }
    }

    let mut nodes = vec![CoverageNode {
        key: mission_key.clone(),
        kind: CoverageNodeKind::Mission,
        label: mission_title.trim().to_string(),
        value: mission_title.trim().to_string(),
        asset_type: None,
        tested: false,
        confidence: None,
        finding_ids: Vec::new(),
        evidence_ids: Vec::new(),
        tool_invocation_ids: Vec::new(),
        metadata: Map::new(),
    }];
    let mut edges = Vec::new();
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut tested_total = 0usize;

    // 稳定排序：先类别后值，保证同一份资产产出同一张图。
    let mut ordered = assets.iter().collect::<Vec<_>>();
    ordered.sort_by(|a, b| {
        a.asset_type
            .as_str()
            .cmp(b.asset_type.as_str())
            .then_with(|| a.value.cmp(&b.value))
    });
    for asset in ordered {
        let node = node_for(asset);
        *by_kind
            .entry(node_kind_of(asset.asset_type).wire_value().to_string())
            .or_insert(0) += 1;
        if node.tested {
            tested_total += 1;
        }
        let parent = parent_key_of(asset, &mission_key, &by_value);
        edges.push(CoverageEdge {
            from: parent,
            to: node.key.clone(),
        });
        nodes.push(node);
    }

    // 父节点不存在的边（理论上看不到，但绝不静默丢弃）。
    let known = nodes
        .iter()
        .map(|node| node.key.clone())
        .collect::<BTreeSet<_>>();
    edges.retain(|edge| known.contains(&edge.from) && known.contains(&edge.to));

    CoverageGraph {
        mission_id: mission_id.as_str().to_string(),
        stats: CoverageStats {
            total: nodes.len() - 1,
            tested: tested_total,
            by_kind,
        },
        nodes,
        edges,
    }
}

impl CoverageNodeKind {
    /// wire 值（与 `#[serde(rename_all = "snake_case")]` 保持一致）。
    #[must_use]
    pub const fn wire_value(self) -> &'static str {
        match self {
            CoverageNodeKind::Mission => "mission",
            CoverageNodeKind::RootDomain => "root_domain",
            CoverageNodeKind::Subdomain => "subdomain",
            CoverageNodeKind::Ip => "ip",
            CoverageNodeKind::App => "app",
            CoverageNodeKind::Service => "service",
            CoverageNodeKind::Endpoint => "endpoint",
            CoverageNodeKind::Other => "other",
        }
    }
}

/// `GET /missions/{mission_id}/coverage-graph`。
///
/// Mission 不存在返回 404；资产为空返回只有 Mission 根的图（不是错误——
/// "还没有资产"本身就是有效的覆盖状态）。
pub(crate) async fn mission_coverage_graph(
    State(state): State<ApiState>,
    Path(mission_id): Path<String>,
) -> Result<Json<CoverageGraph>, ApiError> {
    let mission = state
        .manager
        .repository()
        .get_mission(&mission_id)?
        .ok_or_else(|| EngineError::MissionNotFound(format!("unknown mission: {mission_id}")))?;
    let assets = state
        .manager
        .repository()
        .list_mission_assets(Some(mission_id.as_str()), None, None, None)?
        .into_iter()
        .filter(|asset| {
            asset
                .run_id
                .as_ref()
                .is_none_or(|id| Some(id.as_str()) == active_run_id(&mission))
        })
        .collect::<Vec<_>>();
    Ok(Json(build_coverage_graph(
        &mission.id,
        mission.title.as_deref().unwrap_or(""),
        &assets,
    )))
}

/// 当前活跃 run（与 `mission_canvas` 的 scope 语义一致：无 run 或属于该 run）。
fn active_run_id(mission: &models::Mission) -> Option<&str> {
    mission.active_run_id.as_ref().map(|id| id.as_str())
}

/// 跨任务资产树节点:按(类别, 归一化值)跨 mission 合并同类资产,携带该资产
/// **直接关联** finding 的严重度计数。父子关系由 [`build_finding_asset_tree`]
/// 复用覆盖图的层级规则推导(项目级,顶层是根域名 / IP,无 mission 根)。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FindingAssetNode {
    /// 稳定键 `asset:<kind>:<value_lower>`。
    pub key: String,
    /// 资产类别(与覆盖图一致)。
    pub kind: CoverageNodeKind,
    /// 展示名。
    pub label: String,
    /// 归一化值(小写)。
    pub value: String,
    /// 父节点键;顶层(根域名 / IP)为 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// 直接关联的 critical 发现数。
    pub critical: usize,
    /// 直接关联的 high 发现数。
    pub high: usize,
    /// 直接关联的发现总数。
    pub total: usize,
}

/// 跨任务资产树(项目级)。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FindingAssetTree {
    /// 树节点(前端按 `parent` 组装;顶层 `parent` 为空)。
    pub nodes: Vec<FindingAssetNode>,
    /// 全树关联的发现总数(按 finding_id 去重)。
    pub finding_total: usize,
}

/// 由项目全部资产 + 发现推导跨任务资产树。
///
/// 同类资产(同 `asset_type` + 归一化值)跨 mission 合并为一个节点,finding 计数
/// 按 `finding_id` 去重;父子关系复用覆盖图的层级查找规则,但顶层是根域名 / IP
/// (项目级没有 mission 根)。
#[must_use]
pub fn build_finding_asset_tree(assets: &[MissionAsset], findings: &[Finding]) -> FindingAssetTree {
    // finding_id → (critical, high)。
    let mut severity_by_finding: BTreeMap<&str, (bool, bool)> = BTreeMap::new();
    for finding in findings {
        let rank = match finding.severity {
            Severity::Critical => (true, false),
            Severity::High => (false, true),
            _ => (false, false),
        };
        severity_by_finding.insert(finding.id.as_str(), rank);
    }

    // (asset_type, value_lower) → node key,供父子推导(含 url origin 索引)。
    let mut by_value: BTreeMap<(String, String), String> = BTreeMap::new();
    let mut nodes: BTreeMap<String, FindingAssetNode> = BTreeMap::new();
    let mut ordered: Vec<&MissionAsset> = assets.iter().collect();
    ordered.sort_by(|a, b| {
        a.asset_type
            .as_str()
            .cmp(b.asset_type.as_str())
            .then_with(|| a.value.cmp(&b.value))
    });
    let mut finding_total = 0usize;
    let mut counted: BTreeSet<String> = BTreeSet::new();
    for asset in &ordered {
        let kind = node_kind_of(asset.asset_type);
        let value_lower = asset.value.trim().to_lowercase();
        let key = format!("asset:{}:{}", kind.wire_value(), value_lower);
        let entry = nodes.entry(key.clone()).or_insert_with(|| FindingAssetNode {
            key: key.clone(),
            kind,
            label: asset
                .label
                .clone()
                .unwrap_or_else(|| asset.value.trim().to_string()),
            value: value_lower.clone(),
            parent: None,
            critical: 0,
            high: 0,
            total: 0,
        });
        by_value
            .entry((asset.asset_type.as_str().to_string(), value_lower.clone()))
            .or_insert_with(|| key.clone());
        if asset.asset_type == MissionAssetType::Url
            && let Some((scheme, host, port)) = split_url(&asset.value)
        {
            let origin = match port {
                Some(port) => format!("{scheme}://{host}:{port}"),
                None => format!("{scheme}://{host}"),
            };
            by_value
                .entry((MissionAssetType::Url.as_str().to_string(), origin))
                .or_insert_with(|| key.clone());
        }
        for finding_id in &asset.finding_ids {
            if !counted.insert(finding_id.clone()) {
                continue;
            }
            entry.total += 1;
            finding_total += 1;
            if let Some((critical, high)) = severity_by_finding.get(finding_id.as_str()) {
                if *critical {
                    entry.critical += 1;
                }
                if *high {
                    entry.high += 1;
                }
            }
        }
    }

    // 父子推导:顶层 = 根域名 / IP(parent=None),其余复用覆盖图查找规则。
    let lookup = |asset_type: MissionAssetType, value: &str| {
        by_value
            .get(&(asset_type.as_str().to_string(), value.trim().to_lowercase()))
            .cloned()
    };
    for asset in &ordered {
        let kind = node_kind_of(asset.asset_type);
        let value_lower = asset.value.trim().to_lowercase();
        let key = format!("asset:{}:{}", kind.wire_value(), value_lower);
        let parent = match asset.asset_type {
            MissionAssetType::Domain | MissionAssetType::Ip => None,
            MissionAssetType::Host => {
                lookup(MissionAssetType::Domain, &root_domain_of(&asset.value))
            }
            MissionAssetType::Service => {
                let host = asset.metadata.get("host").and_then(Value::as_str);
                match host {
                    Some(host) => lookup(MissionAssetType::Host, host)
                        .or_else(|| lookup(MissionAssetType::Ip, host)),
                    None => None,
                }
            }
            MissionAssetType::Url => match split_url(&asset.value).map(|(_, host, _)| host) {
                Some(host) => lookup(MissionAssetType::Host, &host)
                    .or_else(|| lookup(MissionAssetType::Domain, &root_domain_of(&host))),
                None => None,
            },
            MissionAssetType::Endpoint | MissionAssetType::Api => {
                match split_url(&asset.value) {
                    Some((_, host, _)) => {
                        let origin = split_url(&asset.value)
                            .map(|(scheme, host, port)| match port {
                                Some(port) => format!("{scheme}://{host}:{port}"),
                                None => format!("{scheme}://{host}"),
                            })
                            .unwrap_or_default();
                        lookup(MissionAssetType::Url, &origin)
                            .or_else(|| lookup(MissionAssetType::Host, &host))
                            .or_else(|| lookup(MissionAssetType::Domain, &root_domain_of(&host)))
                    }
                    None => None,
                }
            }
            _ => None,
        };
        if let Some(node) = nodes.get_mut(&key)
            && node.parent.is_none()
        {
            node.parent = parent;
        }
    }

    FindingAssetTree {
        nodes: nodes.into_values().collect(),
        finding_total,
    }
}

/// `GET /projects/{project_id}/findings/tree`。
///
/// 跨任务资产树:项目全部资产(跨 mission 合并)+ 其关联 finding 的严重度计数。
/// Project 不存在返回 404;无资产返回空树(不是错误)。
pub(crate) async fn project_finding_asset_tree(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
) -> Result<Json<FindingAssetTree>, ApiError> {
    let project = state
        .manager
        .repository()
        .get_project(&project_id)?
        .ok_or_else(|| EngineError::ProjectNotFound(format!("unknown project: {project_id}")))?;
    let assets = state
        .manager
        .repository()
        .list_mission_assets(None, Some(project.id.as_str()), None, None)?;
    let findings = state
        .manager
        .repository()
        .list_findings(project.id.as_str())?;
    Ok(Json(build_finding_asset_tree(&assets, &findings)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::asset::{MissionAssetSensitivity, MissionAssetSource};

    /// 测试用资产构造（`MissionAsset` 没有 `new`，仓储侧统一从 JSON 解析）。
    fn asset(asset_type: MissionAssetType, value: &str) -> MissionAsset {
        let mut asset: MissionAsset = serde_json::from_value(serde_json::json!({
            "id": format!("asset_{}", value.replace(['.', ':', '/'], "_")),
            "project_id": "proj_x",
            "mission_id": "mission_x",
            "asset_type": asset_type.as_str(),
            "value": value,
            "source": MissionAssetSource::UserTarget.as_str(),
        }))
        .expect("asset json must parse");
        asset.sensitivity = MissionAssetSensitivity::Unknown;
        asset
    }

    /// 测试用 finding 构造(severity 决定计数;status=candidate 免证据不变量)。
    fn finding(id: &str, severity: &str) -> Finding {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "project_id": "proj_x",
            "title": "t",
            "severity": severity,
            "status": "candidate",
        }))
        .expect("finding json must parse")
    }

    #[test]
    fn finding_asset_tree_merges_assets_and_counts_by_severity() {
        let domain = asset(MissionAssetType::Domain, "example.com");
        let mut host = asset(MissionAssetType::Host, "a.example.com");
        host.finding_ids = vec![
            "find_crit".to_string(),
            "find_high".to_string(),
            "find_low".to_string(),
        ];
        let ip = asset(MissionAssetType::Ip, "10.0.0.1");
        let findings = vec![
            finding("find_crit", "critical"),
            finding("find_high", "high"),
            finding("find_low", "low"),
        ];
        let tree = build_finding_asset_tree(&[host, domain, ip], &findings);

        let host_node = tree
            .nodes
            .iter()
            .find(|n| n.value == "a.example.com")
            .expect("host node");
        assert_eq!(host_node.total, 3);
        assert_eq!(host_node.critical, 1);
        assert_eq!(host_node.high, 1);
        assert_eq!(
            host_node.parent.as_deref(),
            Some("asset:root_domain:example.com")
        );

        let domain_node = tree
            .nodes
            .iter()
            .find(|n| n.value == "example.com")
            .expect("domain node");
        assert_eq!(domain_node.total, 0);
        assert!(domain_node.parent.is_none(), "root domain is top-level");

        assert_eq!(tree.finding_total, 3);
    }

    #[test]
    fn finding_asset_tree_dedups_findings_across_merged_assets() {
        // 同一 host 在两个 mission 各有一条资产,关联同一个 finding → 只计一次。
        let mut a = asset(MissionAssetType::Host, "a.example.com");
        a.finding_ids = vec!["f1".to_string()];
        let mut b = asset(MissionAssetType::Host, "A.Example.com"); // 归一化后同值
        b.finding_ids = vec!["f1".to_string()];
        let tree = build_finding_asset_tree(&[a, b], &[finding("f1", "high")]);
        let node = tree
            .nodes
            .iter()
            .find(|n| n.value == "a.example.com")
            .expect("merged host node");
        assert_eq!(node.total, 1, "same finding counted once across merged assets");
        assert_eq!(tree.finding_total, 1);
    }

    #[test]
    fn root_domain_of_handles_ip_and_multi_label_hosts() {
        assert_eq!(root_domain_of("a.b.example.com"), "example.com");
        assert_eq!(root_domain_of("example.com"), "example.com");
        assert_eq!(root_domain_of("10.0.0.1"), "10.0.0.1");
        assert_eq!(root_domain_of("Example.COM."), "example.com");
    }

    #[test]
    fn split_url_extracts_host_and_port() {
        assert_eq!(
            split_url("https://api.example.com:8443/v1/users?id=1"),
            Some(("https".to_string(), "api.example.com".to_string(), Some(8443)))
        );
        assert_eq!(split_url("not a url"), None);
        assert_eq!(split_url("https://"), None);
    }

    #[test]
    fn hierarchy_links_host_url_endpoint_and_service() {
        let mission_id = MissionId::new("mission_x".to_string());
        let domain = asset(MissionAssetType::Domain, "example.com");
        let host = asset(MissionAssetType::Host, "api.example.com");
        let app = asset(MissionAssetType::Url, "https://api.example.com/login");
        let endpoint = asset(MissionAssetType::Endpoint, "https://api.example.com/api/v1/user");
        let mut service = asset(MissionAssetType::Service, "https");
        service
            .metadata
            .insert("host".to_string(), Value::String("api.example.com".to_string()));

        let graph = build_coverage_graph(&mission_id, "Mission X", &[domain, host, app, endpoint, service]);
        let edge = |to: &str| {
            graph
                .edges
                .iter()
                .find(|edge| edge.to == to)
                .map(|edge| edge.from.clone())
        };
        assert_eq!(edge("asset_example_com").as_deref(), Some("mission:mission_x"));
        assert_eq!(
            edge("asset_api_example_com").as_deref(),
            Some("asset_example_com")
        );
        assert_eq!(
            edge("asset_https___api_example_com_login").as_deref(),
            Some("asset_api_example_com")
        );
        assert_eq!(
            edge("asset_https___api_example_com_api_v1_user").as_deref(),
            Some("asset_https___api_example_com_login")
        );
        assert_eq!(
            edge("asset_https").as_deref(),
            Some("asset_api_example_com")
        );
        assert_eq!(graph.stats.total, 5);
        assert_eq!(graph.stats.tested, 0);
        assert_eq!(graph.stats.by_kind.get("root_domain"), Some(&1));
        assert_eq!(graph.stats.by_kind.get("subdomain"), Some(&1));
        assert_eq!(graph.stats.by_kind.get("app"), Some(&1));
        assert_eq!(graph.stats.by_kind.get("endpoint"), Some(&1));
        assert_eq!(graph.stats.by_kind.get("service"), Some(&1));
    }

    #[test]
    fn tested_requires_a_real_association() {
        let mission_id = MissionId::new("mission_x".to_string());
        let bare = asset(MissionAssetType::Host, "a.example.com");
        let mut exercised = asset(MissionAssetType::Host, "b.example.com");
        exercised.finding_ids = vec!["find_1".to_string()];

        let graph = build_coverage_graph(&mission_id, "M", &[bare, exercised]);
        assert_eq!(graph.stats.total, 2);
        assert_eq!(graph.stats.tested, 1);
        let used = graph
            .nodes
            .iter()
            .find(|node| node.key == "asset_b_example_com")
            .expect("exercised asset must be present");
        assert!(used.tested);
        assert_eq!(used.finding_ids, vec!["find_1".to_string()]);
        let bare_node = graph
            .nodes
            .iter()
            .find(|node| node.key == "asset_a_example_com")
            .expect("bare asset must be present");
        assert!(!bare_node.tested);
    }

    #[test]
    fn graph_is_deterministic_for_the_same_input() {
        let mission_id = MissionId::new("mission_x".to_string());
        let mut assets = vec![
            asset(MissionAssetType::Host, "b.example.com"),
            asset(MissionAssetType::Host, "a.example.com"),
            asset(MissionAssetType::Domain, "example.com"),
        ];
        let first = build_coverage_graph(&mission_id, "M", &assets);
        assets.reverse();
        let second = build_coverage_graph(&mission_id, "M", &assets);
        assert_eq!(first, second, "同一份资产必须产出同一张图");
    }
}
