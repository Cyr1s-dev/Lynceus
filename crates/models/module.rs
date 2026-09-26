//! 模块配置模型 —— `server/core/models/module.py` 的移植。
//!
//! Module 是外部审计能力的受控集成点：内置工具、本地适配器或远端 MCP
//! 服务器。Agent 绝不直接调用外部工具，只寻址经 `tool_allowlist` 与
//! `capability_map` 解析的具名能力。

use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;

use crate::common::StrMap;
use crate::common::Timestamp;
use crate::common::new_id;
use crate::common::utcnow;
use crate::ids::ModuleId;

/// 模块提供方式（`ModuleType`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleType {
    /// 内置模块。
    Builtin,
    /// 本地工具适配。
    LocalTool,
    /// 远端 MCP 服务器。
    McpRemote,
}

/// 模块覆盖的安全审计域（`ModuleDomain`）。
///
/// 值集与 [`crate::domain::AuditDomain`] 相同但独立演化；CoverageChecker
/// 通过 `TryFrom<ModuleDomain> for AuditDomain` 折算，两侧值集漂移会让
/// 该转换编译失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleDomain {
    /// 资产测绘。
    AssetRecon,
    /// Web 侦察。
    WebRecon,
    /// 内容发现。
    ContentDiscovery,
    /// 指纹识别。
    FingerprintIntelligence,
    /// 暴露面情报。
    ExposureIntelligence,
    /// Web 静态分析。
    WebSast,
    /// Web 动态测试。
    WebDast,
    /// Web 交互式测试。
    WebIast,
    /// Web 漏洞验证。
    WebValidation,
    /// 可利用性验证。
    ExploitabilityValidation,
    /// 内网面。
    InternalSurface,
    /// 流量情报。
    TrafficIntelligence,
    /// 深度代码静态分析。
    CodeDeepSast,
    /// 二进制静态分析。
    BinaryStatic,
    /// 二进制动态分析。
    BinaryDynamic,
    /// 可利用性。
    Exploitability,
    /// 模糊测试。
    Fuzzing,
    /// 供应链。
    SupplyChain,
    /// 云原生。
    CloudNative,
    /// 复合（跨域）。
    Composite,
}

impl ModuleDomain {
    /// wire 值（Python `domain.value` 的镜像）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ModuleDomain::AssetRecon => "asset_recon",
            ModuleDomain::WebRecon => "web_recon",
            ModuleDomain::ContentDiscovery => "content_discovery",
            ModuleDomain::FingerprintIntelligence => "fingerprint_intelligence",
            ModuleDomain::ExposureIntelligence => "exposure_intelligence",
            ModuleDomain::WebSast => "web_sast",
            ModuleDomain::WebDast => "web_dast",
            ModuleDomain::WebIast => "web_iast",
            ModuleDomain::WebValidation => "web_validation",
            ModuleDomain::ExploitabilityValidation => "exploitability_validation",
            ModuleDomain::InternalSurface => "internal_surface",
            ModuleDomain::TrafficIntelligence => "traffic_intelligence",
            ModuleDomain::CodeDeepSast => "code_deep_sast",
            ModuleDomain::BinaryStatic => "binary_static",
            ModuleDomain::BinaryDynamic => "binary_dynamic",
            ModuleDomain::Exploitability => "exploitability",
            ModuleDomain::Fuzzing => "fuzzing",
            ModuleDomain::SupplyChain => "supply_chain",
            ModuleDomain::CloudNative => "cloud_native",
            ModuleDomain::Composite => "composite",
        }
    }
}

/// 模块域字符串无法解析。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown module domain {text:?}; available: {available}")]
pub struct ModuleDomainParseError {
    /// 解析失败的原字符串。
    pub text: String,
    /// 合法值列表（逗号连接，与 Python 报错文本一致）。
    pub available: String,
}

/// Python `normalize_module_domain`：接受大小写/连字符变体，拒绝未知值。
///
/// # Errors
/// 未知域值返回 [`ModuleDomainParseError`]，报错文本与 Python 一致。
pub fn normalize_module_domain(value: &str) -> Result<ModuleDomain, ModuleDomainParseError> {
    let normalized = value.trim().to_lowercase().replace('-', "_");
    let candidates = [
        ModuleDomain::AssetRecon,
        ModuleDomain::WebRecon,
        ModuleDomain::ContentDiscovery,
        ModuleDomain::FingerprintIntelligence,
        ModuleDomain::ExposureIntelligence,
        ModuleDomain::WebSast,
        ModuleDomain::WebDast,
        ModuleDomain::WebIast,
        ModuleDomain::WebValidation,
        ModuleDomain::ExploitabilityValidation,
        ModuleDomain::InternalSurface,
        ModuleDomain::TrafficIntelligence,
        ModuleDomain::CodeDeepSast,
        ModuleDomain::BinaryStatic,
        ModuleDomain::BinaryDynamic,
        ModuleDomain::Exploitability,
        ModuleDomain::Fuzzing,
        ModuleDomain::SupplyChain,
        ModuleDomain::CloudNative,
        ModuleDomain::Composite,
    ];
    candidates
        .into_iter()
        .find(|domain| domain.as_str() == normalized)
        .ok_or_else(|| ModuleDomainParseError {
            text: value.to_string(),
            available: candidates
                .iter()
                .map(|domain| domain.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        })
}

impl std::str::FromStr for ModuleDomain {
    type Err = ModuleDomainParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        normalize_module_domain(text)
    }
}

/// 外部模块传输方式（`ModuleTransport`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleTransport {
    /// Streamable HTTP。
    StreamableHttp,
    /// SSE。
    Sse,
    /// 标准输入输出。
    Stdio,
    /// 无传输（内置模块）。
    None,
}

/// 模块安全配置（`ModuleProfile`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleProfile {
    /// 完全放开。
    FullAccess,
    /// 只读。
    Readonly,
    /// 标准。
    Standard,
    /// 不安全（禁止）。
    Unsafe,
}

/// readonly profile 下被拒绝的工具名模式（`READONLY_DENIED_TOOL_PATTERNS`）。
pub const READONLY_DENIED_TOOL_PATTERNS: [&str; 13] = [
    "patch",
    "put_int",
    "patch_asm",
    "rename",
    "set_type",
    "declare_type",
    "define_func",
    "undefine",
    "dbg_*",
    "py_eval",
    "py_exec_file",
    "diff_before_after",
    "idb_save",
];

/// 精确匹配的拒绝名单（无通配后缀的模式）。
const READONLY_DENIED_EXACT: [&str; 12] = [
    "patch",
    "put_int",
    "patch_asm",
    "rename",
    "set_type",
    "declare_type",
    "define_func",
    "undefine",
    "py_eval",
    "py_exec_file",
    "diff_before_after",
    "idb_save",
];

/// 前缀匹配的拒绝名单（`dbg_*` 去掉通配符）。
const READONLY_DENIED_PREFIXES: [&str; 1] = ["dbg_"];

/// 工具名是否被 readonly profile 拒绝（`is_readonly_denied_tool`）。
#[must_use]
pub fn is_readonly_denied_tool(tool_name: &str) -> bool {
    READONLY_DENIED_EXACT.contains(&tool_name)
        || READONLY_DENIED_PREFIXES
            .iter()
            .any(|prefix| tool_name.starts_with(prefix))
}

/// 过滤掉 readonly 拒绝的工具（`filter_readonly_tools`）。
#[must_use]
pub fn filter_readonly_tools(tool_names: &[String]) -> Vec<String> {
    tool_names
        .iter()
        .filter(|name| !is_readonly_denied_tool(name))
        .cloned()
        .collect()
}

/// `ModuleConfig` 校验失败（镜像 pydantic 字段/模型校验器）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModuleConfigError {
    /// `tool_allowlist` 含空条目。
    #[error("tool_allowlist entries must be non-empty")]
    EmptyAllowlistEntry,
    /// `capability_map` 键或值为空。
    #[error("capability_map keys and values must be non-empty")]
    EmptyCapabilityMapEntry,
    /// `mcp_remote` 模块的传输方式不是 `streamable_http`/`sse`。
    #[error("mcp_remote modules require streamable_http or sse transport")]
    McpRemoteTransport,
    /// `mcp_remote` 模块缺少 `endpoint_url`。
    #[error("mcp_remote modules require endpoint_url")]
    McpRemoteEndpointUrl,
    /// 内置模块的传输方式不是 none。
    #[error("builtin modules must use transport='none'")]
    BuiltinTransport,
    /// readonly 模块允许了不安全工具（名单逗号连接，按字母序去重）。
    #[error("readonly module cannot allow unsafe tool(s): {names}")]
    ReadonlyUnsafeTools {
        /// 不安全工具名单。
        names: String,
    },
}

fn default_module_id() -> ModuleId {
    ModuleId::new(new_id("mod"))
}

fn default_module_type() -> ModuleType {
    ModuleType::Builtin
}

fn default_module_domain() -> ModuleDomain {
    ModuleDomain::Composite
}

fn default_module_transport() -> ModuleTransport {
    ModuleTransport::None
}

fn default_module_enabled() -> bool {
    true
}

fn default_module_profile() -> ModuleProfile {
    ModuleProfile::FullAccess
}

/// ModuleConfig：一个外部或内置审计模块的存储配置（`ModuleConfig`）。
///
/// 字段顺序与默认值冻结自 Python 模型；`_validate_module_invariants`
/// 与字段校验器经 [`Self::validated`]（构造路径）与 serde `try_from`
/// （解析路径）双入口镜像。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ModuleConfigWire")]
pub struct ModuleConfig {
    /// 模块标识符。
    pub id: ModuleId,
    /// 模块名。
    pub name: String,
    /// 提供方式。
    pub module_type: ModuleType,
    /// 覆盖的审计域。
    pub domain: ModuleDomain,
    /// 远端端点。
    pub endpoint_url: Option<String>,
    /// 传输方式。
    pub transport: ModuleTransport,
    /// 是否启用。
    pub enabled: bool,
    /// 安全配置。
    pub profile: ModuleProfile,
    /// 允许的工具名（构造时去重，保持首现顺序）。
    pub tool_allowlist: Vec<String>,
    /// 能力名 → 原始工具名。
    pub capability_map: StrMap,
    /// 附加元数据（键序 = 插入序）。
    pub metadata: Map<String, Value>,
    /// 创建时间。
    pub created_at: Timestamp,
    /// 最后更新时间。
    pub updated_at: Timestamp,
}

impl ModuleConfig {
    /// 以 Python 默认值构造（`ModuleConfig(name=...)`）。
    #[must_use]
    pub fn new(name: String) -> Self {
        Self {
            id: default_module_id(),
            name,
            module_type: default_module_type(),
            domain: default_module_domain(),
            endpoint_url: None,
            transport: default_module_transport(),
            enabled: default_module_enabled(),
            profile: default_module_profile(),
            tool_allowlist: Vec::new(),
            capability_map: StrMap::new(),
            metadata: Map::new(),
            created_at: utcnow(),
            updated_at: utcnow(),
        }
    }

    /// 去重 allowlist 并校验模块不变量，返回可持久化配置。
    ///
    /// 消费 `self`：`tool_allowlist` 去重结果在返回值上，与 Python
    /// `field_validator` 的规范化语义一致。
    ///
    /// # Errors
    /// 见 [`ModuleConfigError`]；错误文本与 Python 逐字节一致。
    pub fn validated(mut self) -> Result<Self, ModuleConfigError> {
        let mut seen: Vec<&str> = Vec::new();
        let mut deduped: Vec<String> = Vec::new();
        for name in &self.tool_allowlist {
            if name.is_empty() {
                return Err(ModuleConfigError::EmptyAllowlistEntry);
            }
            if !seen.contains(&name.as_str()) {
                seen.push(name.as_str());
                deduped.push(name.clone());
            }
        }
        self.tool_allowlist = deduped;

        for (capability, tool_name) in self.capability_map.iter() {
            if capability.is_empty() || tool_name.is_empty() {
                return Err(ModuleConfigError::EmptyCapabilityMapEntry);
            }
        }

        if self.module_type == ModuleType::McpRemote {
            if self.transport != ModuleTransport::StreamableHttp
                && self.transport != ModuleTransport::Sse
            {
                return Err(ModuleConfigError::McpRemoteTransport);
            }
            if self.endpoint_url.as_deref().unwrap_or("").is_empty() {
                return Err(ModuleConfigError::McpRemoteEndpointUrl);
            }
        }
        if self.module_type == ModuleType::Builtin && self.transport != ModuleTransport::None {
            return Err(ModuleConfigError::BuiltinTransport);
        }

        if self.profile == ModuleProfile::Readonly {
            let mut denied: Vec<String> = self
                .tool_allowlist
                .iter()
                .filter(|name| is_readonly_denied_tool(name))
                .cloned()
                .collect();
            denied.extend(
                self.capability_map
                    .iter()
                    .map(|(_, raw_name)| raw_name.to_string())
                    .filter(|raw_name| is_readonly_denied_tool(raw_name)),
            );
            if !denied.is_empty() {
                denied.sort();
                denied.dedup();
                return Err(ModuleConfigError::ReadonlyUnsafeTools {
                    names: denied.join(", "),
                });
            }
        }

        Ok(self)
    }
}

/// [`ModuleConfig`] 的解析镜像：缺省字段取 Python 默认值，未知字段拒绝
/// （`extra="forbid"`），`domain` 字符串走 `normalize_module_domain`
/// （`field_validator(mode="before")`），解析后走 `validated()`。
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ModuleConfigWire {
    id: ModuleId,
    name: String,
    module_type: ModuleType,
    #[serde(deserialize_with = "deserialize_module_domain")]
    domain: ModuleDomain,
    endpoint_url: Option<String>,
    transport: ModuleTransport,
    enabled: bool,
    profile: ModuleProfile,
    tool_allowlist: Vec<String>,
    capability_map: StrMap,
    metadata: Map<String, Value>,
    created_at: Timestamp,
    updated_at: Timestamp,
}

/// Python `field_validator("domain", mode="before")`：字符串值经规范化后
/// 再折算为枚举（接受大小写/连字符变体）。
fn deserialize_module_domain<'de, D>(deserializer: D) -> Result<ModuleDomain, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;

    let text = <&str>::deserialize(deserializer)?;
    normalize_module_domain(text)
        .map_err(|error| D::Error::custom(format!("unknown module domain {error}")))
}

impl Default for ModuleConfigWire {
    fn default() -> Self {
        let base = ModuleConfig::new(String::new());
        Self {
            id: base.id,
            name: base.name,
            module_type: base.module_type,
            domain: base.domain,
            endpoint_url: base.endpoint_url,
            transport: base.transport,
            enabled: base.enabled,
            profile: base.profile,
            tool_allowlist: base.tool_allowlist,
            capability_map: base.capability_map,
            metadata: base.metadata,
            created_at: base.created_at,
            updated_at: base.updated_at,
        }
    }
}

impl TryFrom<ModuleConfigWire> for ModuleConfig {
    type Error = ModuleConfigError;

    fn try_from(wire: ModuleConfigWire) -> Result<Self, Self::Error> {
        Self {
            id: wire.id,
            name: wire.name,
            module_type: wire.module_type,
            domain: wire.domain,
            endpoint_url: wire.endpoint_url,
            transport: wire.transport,
            enabled: wire.enabled,
            profile: wire.profile,
            tool_allowlist: wire.tool_allowlist,
            capability_map: wire.capability_map,
            metadata: wire.metadata,
            created_at: wire.created_at,
            updated_at: wire.updated_at,
        }
        .validated()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::assert_wire_values;

    #[test]
    fn module_enums_match_python_wire_values() {
        assert_wire_values(&[
            (ModuleType::Builtin, "builtin"),
            (ModuleType::LocalTool, "local_tool"),
            (ModuleType::McpRemote, "mcp_remote"),
        ]);
        assert_wire_values(&[
            (ModuleTransport::StreamableHttp, "streamable_http"),
            (ModuleTransport::Sse, "sse"),
            (ModuleTransport::Stdio, "stdio"),
            (ModuleTransport::None, "none"),
        ]);
        assert_wire_values(&[
            (ModuleProfile::FullAccess, "full_access"),
            (ModuleProfile::Readonly, "readonly"),
            (ModuleProfile::Standard, "standard"),
            (ModuleProfile::Unsafe, "unsafe"),
        ]);
        assert_wire_values(&[
            (ModuleDomain::AssetRecon, "asset_recon"),
            (ModuleDomain::WebRecon, "web_recon"),
            (ModuleDomain::ContentDiscovery, "content_discovery"),
            (
                ModuleDomain::FingerprintIntelligence,
                "fingerprint_intelligence",
            ),
            (ModuleDomain::ExposureIntelligence, "exposure_intelligence"),
            (ModuleDomain::WebSast, "web_sast"),
            (ModuleDomain::WebDast, "web_dast"),
            (ModuleDomain::WebIast, "web_iast"),
            (ModuleDomain::WebValidation, "web_validation"),
            (
                ModuleDomain::ExploitabilityValidation,
                "exploitability_validation",
            ),
            (ModuleDomain::InternalSurface, "internal_surface"),
            (ModuleDomain::TrafficIntelligence, "traffic_intelligence"),
            (ModuleDomain::CodeDeepSast, "code_deep_sast"),
            (ModuleDomain::BinaryStatic, "binary_static"),
            (ModuleDomain::BinaryDynamic, "binary_dynamic"),
            (ModuleDomain::Exploitability, "exploitability"),
            (ModuleDomain::Fuzzing, "fuzzing"),
            (ModuleDomain::SupplyChain, "supply_chain"),
            (ModuleDomain::CloudNative, "cloud_native"),
            (ModuleDomain::Composite, "composite"),
        ]);
    }

    #[test]
    fn normalize_module_domain_accepts_variants_and_rejects_unknown() {
        assert_eq!(
            normalize_module_domain("WEB-DAST"),
            Ok(ModuleDomain::WebDast)
        );
        assert_eq!(
            normalize_module_domain(" cloud_native ").map(ModuleDomain::as_str),
            Ok("cloud_native")
        );
        let error = normalize_module_domain("nope").unwrap_err();
        assert_eq!(error.text, "nope");
        assert!(error.available.starts_with("asset_recon"));
    }

    #[test]
    fn readonly_denied_tools_match_python_patterns() {
        assert!(is_readonly_denied_tool("patch"));
        assert!(is_readonly_denied_tool("dbg_register"));
        assert!(!is_readonly_denied_tool("dbgx"));
        assert!(!is_readonly_denied_tool("read_memory"));
        assert_eq!(
            filter_readonly_tools(&[
                "read_memory".to_string(),
                "patch".to_string(),
                "dbg_trace".to_string(),
            ]),
            vec!["read_memory".to_string()]
        );
    }

    #[test]
    fn module_config_validated_enforces_invariants() {
        let module = ModuleConfig::new("Custom DAST module".to_string())
            .validated()
            .unwrap_or_else(|error| panic!("默认构造必须合法: {error}"));
        assert_eq!(module.module_type, ModuleType::Builtin);
        assert_eq!(module.transport, ModuleTransport::None);
        assert_eq!(module.domain, ModuleDomain::Composite);

        let mut mcp = ModuleConfig::new("m".to_string());
        mcp.module_type = ModuleType::McpRemote;
        assert_eq!(
            mcp.clone().validated().unwrap_err(),
            ModuleConfigError::McpRemoteTransport
        );
        mcp.transport = ModuleTransport::StreamableHttp;
        assert_eq!(
            mcp.clone().validated().unwrap_err(),
            ModuleConfigError::McpRemoteEndpointUrl
        );
        mcp.endpoint_url = Some("https://mcp.test".to_string());
        assert!(mcp.validated().is_ok());

        let mut builtin = ModuleConfig::new("b".to_string());
        builtin.transport = ModuleTransport::Stdio;
        assert_eq!(
            builtin.validated().unwrap_err(),
            ModuleConfigError::BuiltinTransport
        );
    }

    #[test]
    fn module_config_readonly_rejects_unsafe_tools_sorted() {
        let mut module = ModuleConfig::new("r".to_string());
        module.profile = ModuleProfile::Readonly;
        module.tool_allowlist = vec!["py_eval".to_string(), "dbg_trace".to_string()];
        let error = module.validated().unwrap_err();
        assert_eq!(
            error.to_string(),
            "readonly module cannot allow unsafe tool(s): dbg_trace, py_eval"
        );
    }

    #[test]
    fn module_config_dedupes_allowlist_keeping_first_occurrence() {
        let mut module = ModuleConfig::new("m".to_string());
        module.tool_allowlist = vec!["b".to_string(), "a".to_string(), "b".to_string()];
        let module = module
            .validated()
            .unwrap_or_else(|error| panic!("合法配置: {error}"));
        assert_eq!(module.tool_allowlist, vec!["b", "a"]);
    }

    #[test]
    fn module_config_wire_normalizes_domain_and_validates() {
        let json = r#"{"id":"mod_x","name":"Custom DAST module","domain":"WEB-DAST"}"#;
        let module: ModuleConfig = serde_json::from_str(json)
            .unwrap_or_else(|error| panic!("domain 规范化后必须可解析: {error}"));
        assert_eq!(module.domain, ModuleDomain::WebDast);
        assert_eq!(module.id.as_str(), "mod_x");

        let result: Result<ModuleConfig, _> =
            serde_json::from_str(r#"{"name":"x","domain":"bogus"}"#);
        assert!(result.is_err(), "未知 domain 必须被拒绝");
        let result: Result<ModuleConfig, _> = serde_json::from_str(r#"{"name":"x","extra":1}"#);
        assert!(result.is_err(), "extra=forbid：未知字段必须被拒绝");
    }
}
