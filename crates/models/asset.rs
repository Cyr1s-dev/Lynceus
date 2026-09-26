//! Mission 范围资产模型 —— `server/core/models/asset.py` 的移植。
//!
//! `MissionAsset` 是 Mission Canvas 的数据底座：审计执行中发现的 URL、
//! 主机、凭证等资产信号。归一化键（`normalize_mission_asset_value`）
//! 是仓储层 `(mission_id, asset_type, normalized_value)` 去重索引的来源；
//! `merge_mission_assets` 是 upsert 冲突时的保守合并策略。

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::common::Timestamp;
use crate::common::new_id;
use crate::common::utcnow;
use crate::ids::BranchId;
use crate::ids::MissionAssetId;
use crate::ids::MissionId;
use crate::ids::ProjectId;
use crate::ids::RunId;

/// Mission Canvas 可渲染的资产类别（`MissionAssetType`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MissionAssetType {
    /// URL。
    #[serde(rename = "url")]
    Url,
    /// API 端点。
    #[serde(rename = "endpoint")]
    Endpoint,
    /// 主机名。
    #[serde(rename = "host")]
    Host,
    /// 域名。
    #[serde(rename = "domain")]
    Domain,
    /// IP 地址。
    #[serde(rename = "ip")]
    Ip,
    /// 服务。
    #[serde(rename = "service")]
    Service,
    /// 代码仓库。
    #[serde(rename = "repository")]
    Repository,
    /// 源码路径。
    #[serde(rename = "source_path")]
    SourcePath,
    /// 二进制。
    #[serde(rename = "binary")]
    Binary,
    /// 流量捕获文件。
    #[serde(rename = "traffic_capture")]
    TrafficCapture,
    /// 云资源。
    #[serde(rename = "cloud_resource")]
    CloudResource,
    /// API。
    #[serde(rename = "api")]
    Api,
    /// 秘密。
    #[serde(rename = "secret")]
    Secret,
    /// 凭证。
    #[serde(rename = "credential")]
    Credential,
    /// 账户。
    #[serde(rename = "account")]
    Account,
    /// 软件包。
    #[serde(rename = "package")]
    Package,
    /// 容器。
    #[serde(rename = "container")]
    Container,
    /// 未知。
    #[serde(rename = "unknown")]
    Unknown,
}

impl MissionAssetType {
    /// wire 值（Python `.value` 镜像，用于去重键拼接与 SQL 列存储）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            MissionAssetType::Url => "url",
            MissionAssetType::Endpoint => "endpoint",
            MissionAssetType::Host => "host",
            MissionAssetType::Domain => "domain",
            MissionAssetType::Ip => "ip",
            MissionAssetType::Service => "service",
            MissionAssetType::Repository => "repository",
            MissionAssetType::SourcePath => "source_path",
            MissionAssetType::Binary => "binary",
            MissionAssetType::TrafficCapture => "traffic_capture",
            MissionAssetType::CloudResource => "cloud_resource",
            MissionAssetType::Api => "api",
            MissionAssetType::Secret => "secret",
            MissionAssetType::Credential => "credential",
            MissionAssetType::Account => "account",
            MissionAssetType::Package => "package",
            MissionAssetType::Container => "container",
            MissionAssetType::Unknown => "unknown",
        }
    }

    /// 从 wire 值解析（Python `MissionAssetType(value)` 的可判别对应）。
    ///
    /// # Errors
    ///
    /// wire 值不在枚举内。
    pub fn parse(raw: &str) -> Result<Self, String> {
        Ok(match raw {
            "url" => MissionAssetType::Url,
            "endpoint" => MissionAssetType::Endpoint,
            "host" => MissionAssetType::Host,
            "domain" => MissionAssetType::Domain,
            "ip" => MissionAssetType::Ip,
            "service" => MissionAssetType::Service,
            "repository" => MissionAssetType::Repository,
            "source_path" => MissionAssetType::SourcePath,
            "binary" => MissionAssetType::Binary,
            "traffic_capture" => MissionAssetType::TrafficCapture,
            "cloud_resource" => MissionAssetType::CloudResource,
            "api" => MissionAssetType::Api,
            "secret" => MissionAssetType::Secret,
            "credential" => MissionAssetType::Credential,
            "account" => MissionAssetType::Account,
            "package" => MissionAssetType::Package,
            "container" => MissionAssetType::Container,
            "unknown" => MissionAssetType::Unknown,
            other => return Err(format!("unknown mission asset type: {other}")),
        })
    }
}

/// 资产是否应按敏感处理（`MissionAssetSensitivity`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MissionAssetSensitivity {
    /// 敏感。
    #[serde(rename = "sensitive")]
    Sensitive,
    /// 非敏感。
    #[serde(rename = "non_sensitive")]
    NonSensitive,
    /// 未知。
    #[serde(rename = "unknown")]
    Unknown,
}

impl MissionAssetSensitivity {
    /// wire 值（Python `.value` 镜像）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            MissionAssetSensitivity::Sensitive => "sensitive",
            MissionAssetSensitivity::NonSensitive => "non_sensitive",
            MissionAssetSensitivity::Unknown => "unknown",
        }
    }

    fn rank(self) -> u8 {
        match self {
            MissionAssetSensitivity::NonSensitive => 0,
            MissionAssetSensitivity::Unknown => 1,
            MissionAssetSensitivity::Sensitive => 2,
        }
    }
}

/// `MissionAsset` 的来源（`MissionAssetSource`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MissionAssetSource {
    /// 用户目标。
    #[serde(rename = "user_target")]
    UserTarget,
    /// 证据。
    #[serde(rename = "evidence")]
    Evidence,
    /// 工具调用。
    #[serde(rename = "tool_invocation")]
    ToolInvocation,
    /// Finding。
    #[serde(rename = "finding")]
    Finding,
    /// Fact。
    #[serde(rename = "fact")]
    Fact,
    /// 暴露面。
    #[serde(rename = "exposure")]
    Exposure,
    /// 手工。
    #[serde(rename = "manual")]
    Manual,
    /// Agent 推断。
    #[serde(rename = "agent_inference")]
    AgentInference,
    /// 外部情报晋升（Intelligence Hub candidate → asset）。
    #[serde(rename = "intelligence")]
    Intelligence,
}

impl MissionAssetSource {
    /// wire 值（Python `.value` 镜像）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            MissionAssetSource::UserTarget => "user_target",
            MissionAssetSource::Evidence => "evidence",
            MissionAssetSource::ToolInvocation => "tool_invocation",
            MissionAssetSource::Finding => "finding",
            MissionAssetSource::Fact => "fact",
            MissionAssetSource::Exposure => "exposure",
            MissionAssetSource::Manual => "manual",
            MissionAssetSource::AgentInference => "agent_inference",
            MissionAssetSource::Intelligence => "intelligence",
        }
    }
}

fn default_asset_id() -> MissionAssetId {
    MissionAssetId::new(new_id("asset"))
}

fn default_asset_sensitivity() -> MissionAssetSensitivity {
    MissionAssetSensitivity::Unknown
}

fn default_asset_confidence() -> f64 {
    0.5
}

/// Mission 内发现或声明的资产信号（`MissionAsset`）。
///
/// Python `field_validator`（value 非空白、tags 规范化）在构造与解析两
/// 条路径都生效，这里以 [`MissionAsset::validated`]（构造路径）+ serde
/// `try_from`（解析路径，见内部 `MissionAssetWire`）双入口镜像。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "MissionAssetWire")]
pub struct MissionAsset {
    /// 资产标识符。
    #[serde(default = "default_asset_id")]
    pub id: MissionAssetId,
    /// 所属 Project。
    pub project_id: ProjectId,
    /// 所属 Mission。
    pub mission_id: MissionId,
    /// 资产类别。
    pub asset_type: MissionAssetType,
    /// 资产值（非空白）。
    pub value: String,
    /// 展示标签。
    #[serde(default)]
    pub label: Option<String>,
    /// 敏感级别。
    #[serde(default = "default_asset_sensitivity")]
    pub sensitivity: MissionAssetSensitivity,
    /// 置信度（Python 侧约束 `[0.0, 1.0]`）。
    #[serde(default = "default_asset_confidence")]
    pub confidence: f64,
    /// 来源。
    pub source: MissionAssetSource,
    /// 来源实体 id。
    #[serde(default)]
    pub source_id: Option<String>,
    /// 所属 Branch。
    #[serde(default)]
    pub branch_id: Option<BranchId>,
    /// 所属 Run。
    #[serde(default)]
    pub run_id: Option<RunId>,
    /// 关联 Evidence ID 列表。
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    /// 关联 Finding ID 列表。
    #[serde(default)]
    pub finding_ids: Vec<String>,
    /// 关联 `ToolInvocation` ID 列表。
    #[serde(default)]
    pub tool_invocation_ids: Vec<String>,
    /// 标签（去重、小写、最多 16 个）。
    #[serde(default)]
    pub tags: Vec<String>,
    /// 结构化元数据（键序 = 插入序）。
    #[serde(default)]
    pub metadata: Map<String, Value>,
    /// 创建时间。
    #[serde(default = "utcnow")]
    pub created_at: Timestamp,
    /// 更新时间。
    #[serde(default = "utcnow")]
    pub updated_at: Timestamp,
}

impl MissionAsset {
    /// Python `field_validator("value")` 的构造路径镜像：去首尾空白并
    /// 拒绝空值。
    ///
    /// # Errors
    ///
    /// 值去除首尾空白后为空。
    pub fn validated(mut self) -> Result<Self, String> {
        self.value = self.value.trim().to_string();
        if self.value.is_empty() {
            return Err("mission asset value must not be blank".to_string());
        }
        self.tags = normalize_asset_tags(&self.tags);
        Ok(self)
    }
}

/// 解析路径的宽松 wire 形态：先按原始字段反序列化，再走 [`MissionAsset::validated`]
///（镜像 `model_validate_json` 同样执行 validator）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MissionAssetWire {
    #[serde(default = "default_asset_id")]
    id: MissionAssetId,
    project_id: ProjectId,
    mission_id: MissionId,
    asset_type: MissionAssetType,
    value: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default = "default_asset_sensitivity")]
    sensitivity: MissionAssetSensitivity,
    #[serde(default = "default_asset_confidence")]
    confidence: f64,
    source: MissionAssetSource,
    #[serde(default)]
    source_id: Option<String>,
    #[serde(default)]
    branch_id: Option<BranchId>,
    #[serde(default)]
    run_id: Option<RunId>,
    #[serde(default)]
    evidence_ids: Vec<String>,
    #[serde(default)]
    finding_ids: Vec<String>,
    #[serde(default)]
    tool_invocation_ids: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    metadata: Map<String, Value>,
    #[serde(default = "utcnow")]
    created_at: Timestamp,
    #[serde(default = "utcnow")]
    updated_at: Timestamp,
}

impl TryFrom<MissionAssetWire> for MissionAsset {
    type Error = String;

    fn try_from(wire: MissionAssetWire) -> Result<Self, Self::Error> {
        MissionAsset {
            id: wire.id,
            project_id: wire.project_id,
            mission_id: wire.mission_id,
            asset_type: wire.asset_type,
            value: wire.value,
            label: wire.label,
            sensitivity: wire.sensitivity,
            confidence: wire.confidence,
            source: wire.source,
            source_id: wire.source_id,
            branch_id: wire.branch_id,
            run_id: wire.run_id,
            evidence_ids: wire.evidence_ids,
            finding_ids: wire.finding_ids,
            tool_invocation_ids: wire.tool_invocation_ids,
            tags: wire.tags,
            metadata: wire.metadata,
            created_at: wire.created_at,
            updated_at: wire.updated_at,
        }
        .validated()
    }
}

/// Python `field_validator("tags")` 镜像：strip、lower、去重保序、截 16。
#[must_use]
pub fn normalize_asset_tags(raw: &[String]) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut normalized: Vec<String> = Vec::new();
    for tag in raw {
        let tag = tag.trim().to_lowercase();
        if tag.is_empty() || seen.contains(&tag) {
            continue;
        }
        seen.insert(tag.clone());
        normalized.push(tag);
        if normalized.len() >= 16 {
            break;
        }
    }
    normalized
}

/// 返回资产值的仓储去重键（`normalize_mission_asset_value`）。
#[must_use]
pub fn normalize_mission_asset_value(asset_type: MissionAssetType, value: &str) -> String {
    let stripped = value.trim();
    match asset_type {
        MissionAssetType::Url
        | MissionAssetType::Endpoint
        | MissionAssetType::Api
        | MissionAssetType::Service => normalize_url_like(stripped),
        MissionAssetType::Host
        | MissionAssetType::Domain
        | MissionAssetType::Ip
        | MissionAssetType::CloudResource
        | MissionAssetType::Package
        | MissionAssetType::Container => stripped.trim_end_matches('.').to_lowercase(),
        MissionAssetType::Repository
        | MissionAssetType::SourcePath
        | MissionAssetType::Binary
        | MissionAssetType::TrafficCapture => stripped
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_lowercase(),
        _ => stripped.to_string(),
    }
}

/// Python `_normalize_url_like`：`urlsplit`/`urlunsplit` 的最小等价。
///
/// 有 scheme 且有 netloc 时：scheme/netloc 折叠小写、path 去尾斜杠
/// （空则补 `/`）、query 保留、fragment 丢弃；否则整体去尾斜杠后
/// 折叠小写。
#[must_use]
pub fn normalize_url_like(value: &str) -> String {
    let Some((scheme, rest)) = split_scheme(value) else {
        return value.trim_end_matches('/').to_lowercase();
    };
    let Some((netloc, tail)) = split_netloc(&rest) else {
        return value.trim_end_matches('/').to_lowercase();
    };
    if scheme.is_empty() || netloc.is_empty() {
        return value.trim_end_matches('/').to_lowercase();
    }
    let tail = tail.as_str();
    let (path, query) = match tail.find('?') {
        Some(index) => (&tail[..index], &tail[index + 1..]),
        None => (tail, ""),
    };
    let path = if path.is_empty() {
        "/".to_string()
    } else {
        path.trim_end_matches('/').to_string()
    };
    if query.is_empty() {
        format!(
            "{}://{}{}",
            scheme.to_lowercase(),
            netloc.to_lowercase(),
            path
        )
    } else {
        format!(
            "{}://{}{}?{}",
            scheme.to_lowercase(),
            netloc.to_lowercase(),
            path,
            query
        )
    }
}

/// `urlsplit` 的 scheme 切分：首个 `:` 之前无 `/`、`?`、`#` 才算 scheme。
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

/// `urlsplit` 的 netloc 切分：剩余部分以 `//` 开头时到下一个路径分隔符。
fn split_netloc(rest: &str) -> Option<(String, String)> {
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        return Some((after[..end].to_string(), after[end..].to_string()));
    }
    None
}

/// 保守合并重复 `MissionAsset`（`merge_mission_assets`）：置信度与敏感级别
/// 取更严一侧，关联列表与标签取并集（保序去重），metadata 递归合并。
#[must_use]
pub fn merge_mission_assets(existing: &MissionAsset, incoming: &MissionAsset) -> MissionAsset {
    let mut merged = existing.clone();
    merged.confidence = existing.confidence.max(incoming.confidence);
    merged.sensitivity = if existing.sensitivity.rank() >= incoming.sensitivity.rank() {
        existing.sensitivity
    } else {
        incoming.sensitivity
    };
    merged.label = existing.label.clone().or_else(|| incoming.label.clone());
    merged.source_id = existing
        .source_id
        .clone()
        .or_else(|| incoming.source_id.clone());
    merged.branch_id = existing
        .branch_id
        .clone()
        .or_else(|| incoming.branch_id.clone());
    merged.run_id = existing.run_id.clone().or_else(|| incoming.run_id.clone());
    merged.evidence_ids = merge_unique(&existing.evidence_ids, &incoming.evidence_ids);
    merged.finding_ids = merge_unique(&existing.finding_ids, &incoming.finding_ids);
    merged.tool_invocation_ids =
        merge_unique(&existing.tool_invocation_ids, &incoming.tool_invocation_ids);
    let mut tags = merge_unique(&existing.tags, &incoming.tags);
    tags.truncate(16);
    merged.tags = tags;
    merged.metadata = merge_metadata(&existing.metadata, &incoming.metadata);
    merged.updated_at = utcnow();
    merged
}

fn merge_unique(left: &[String], right: &[String]) -> Vec<String> {
    let mut merged: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for value in left.iter().chain(right) {
        if seen.contains(value) {
            continue;
        }
        seen.insert(value.clone());
        merged.push(value.clone());
    }
    merged
}

fn merge_metadata(
    existing: &Map<String, Value>,
    incoming: &Map<String, Value>,
) -> Map<String, Value> {
    let mut merged = existing.clone();
    for (key, value) in incoming {
        let current = merged.get(key);
        if let (Some(Value::Object(current_map)), Value::Object(value_map)) = (current, value) {
            let nested = merge_metadata(current_map, value_map);
            merged.insert(key.clone(), Value::Object(nested));
            continue;
        }
        let replace = match current {
            None | Some(Value::Null) => true,
            Some(Value::String(text)) => text.is_empty(),
            Some(Value::Array(items)) => items.is_empty(),
            Some(Value::Object(map)) => map.is_empty(),
            Some(_) => false,
        };
        if replace {
            merged.insert(key.clone(), value.clone());
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_type_wire_values_mirror_python() {
        for (value, wire) in [
            (MissionAssetType::Url, "url"),
            (MissionAssetType::Endpoint, "endpoint"),
            (MissionAssetType::Host, "host"),
            (MissionAssetType::Domain, "domain"),
            (MissionAssetType::Ip, "ip"),
            (MissionAssetType::Service, "service"),
            (MissionAssetType::Repository, "repository"),
            (MissionAssetType::SourcePath, "source_path"),
            (MissionAssetType::Binary, "binary"),
            (MissionAssetType::TrafficCapture, "traffic_capture"),
            (MissionAssetType::CloudResource, "cloud_resource"),
            (MissionAssetType::Api, "api"),
            (MissionAssetType::Secret, "secret"),
            (MissionAssetType::Credential, "credential"),
            (MissionAssetType::Account, "account"),
            (MissionAssetType::Package, "package"),
            (MissionAssetType::Container, "container"),
            (MissionAssetType::Unknown, "unknown"),
        ] {
            assert_eq!(value.as_str(), wire);
            assert_eq!(MissionAssetType::parse(wire), Ok(value));
        }
    }

    #[test]
    fn sensitivity_rank_prefers_stricter() {
        assert_eq!(
            merge_mission_assets(
                &asset(MissionAssetSensitivity::Unknown),
                &asset(MissionAssetSensitivity::Sensitive)
            )
            .sensitivity,
            MissionAssetSensitivity::Sensitive
        );
        assert_eq!(
            merge_mission_assets(
                &asset(MissionAssetSensitivity::NonSensitive),
                &asset(MissionAssetSensitivity::Unknown)
            )
            .sensitivity,
            MissionAssetSensitivity::Unknown
        );
    }

    fn asset(sensitivity: MissionAssetSensitivity) -> MissionAsset {
        MissionAsset {
            id: MissionAssetId::new("asset_x".to_string()),
            project_id: ProjectId::new("p1".to_string()),
            mission_id: MissionId::new("m1".to_string()),
            asset_type: MissionAssetType::Host,
            value: "example.com".to_string(),
            label: None,
            sensitivity,
            confidence: 0.5,
            source: MissionAssetSource::Manual,
            source_id: None,
            branch_id: None,
            run_id: None,
            evidence_ids: Vec::new(),
            finding_ids: Vec::new(),
            tool_invocation_ids: Vec::new(),
            tags: Vec::new(),
            metadata: Map::new(),
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        }
    }

    #[test]
    fn normalize_url_like_mirrors_python() {
        assert_eq!(
            normalize_url_like("https://Example.COM/path/"),
            "https://example.com/path"
        );
        assert_eq!(normalize_url_like("example.com/path/"), "example.com/path");
        assert_eq!(
            normalize_url_like("https://host/a/?q=1"),
            "https://host/a?q=1"
        );
        assert_eq!(normalize_url_like("https://host"), "https://host/");
    }

    #[test]
    fn normalize_mission_asset_value_by_type() {
        assert_eq!(
            normalize_mission_asset_value(MissionAssetType::Host, "Example.COM."),
            "example.com"
        );
        assert_eq!(
            normalize_mission_asset_value(MissionAssetType::SourcePath, "src\\Main.rs/"),
            "src/main.rs"
        );
        assert_eq!(
            normalize_mission_asset_value(MissionAssetType::Secret, " tok "),
            "tok"
        );
    }

    #[test]
    fn validated_rejects_blank_value() {
        let mut base = asset(MissionAssetSensitivity::Unknown);
        base.value = "  ".to_string();
        assert!(base.validated().is_err());
    }
}
