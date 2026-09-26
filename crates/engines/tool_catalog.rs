//! Curated tool catalog, local detection, recommendations and install jobs.
//!
//! Catalog recipes are compiled into the binary from the reviewed YAML file.
//! Configuration and health checks never accept arbitrary argv: the executable
//! path comes from local configuration and version arguments come from the
//! embedded catalog. Automatic provisioning is integrity-checked per family:
//! `github_release` downloads the pinned asset, verifies its SHA-256 and
//! extracts it in-process (traversal/symlink-safe); `pip`/`go`/`cargo` shell
//! out to the pinned toolchain; `manual` only prints guidance.
//! 【统一工具系统核心】Tool Catalog 是 Lynceus 唯一的工具元数据源：
//! curated_tools.yaml（source of truth）+ PATH/配置探测。lynceus-mcp
//! broker（tool_search/tool_describe/tool_execute）直接消费本模块。

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use models::{
    Evidence, Fact, Finding, Mission, Timestamp, ToolStatus, new_id, utcnow,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use regex::Regex;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};

use crate::tool_gateway::{ToolGateway, ToolRequest};

const EMBEDDED_CATALOG: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../resources/tool-catalog/curated_tools.yaml"
));
const INSTALL_TIMEOUT: Duration = Duration::from_mins(30);
const MAX_INSTALL_MESSAGE_CHARS: usize = 1_000;

/// Local availability source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolAvailability {
    /// No full detection snapshot exists yet.
    Unknown,
    /// Explicit local-tools.json entry.
    Configured,
    /// Resolved from PATH.
    Path,
    /// No usable executable found.
    Missing,
}

/// Reviewed install recipe family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolInstallMethod {
    /// Go package recipe.
    Go,
    /// Python package recipe.
    Pip,
    /// Cargo package recipe.
    Cargo,
    /// Pinned GitHub release recipe.
    GithubRelease,
    /// Human-directed installation.
    Manual,
}

impl ToolInstallMethod {
    /// Stable wire value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Go => "go",
            Self::Pip => "pip",
            Self::Cargo => "cargo",
            Self::GithubRelease => "github_release",
            Self::Manual => "manual",
        }
    }
}

/// Curated install recipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolInstall {
    /// Recipe family.
    pub method: ToolInstallMethod,
    /// Pinned package expression.
    #[serde(default)]
    pub package: Option<String>,
    /// Pinned version/commit.
    #[serde(default)]
    pub version: Option<String>,
    /// GitHub owner/repository.
    #[serde(default)]
    pub repository: Option<String>,
    /// Pinned release tag.
    #[serde(default)]
    pub release_tag: Option<String>,
    /// Platform → release asset regex.
    #[serde(default)]
    pub asset_patterns: IndexMap<String, String>,
    /// Platform → expected SHA-256.
    #[serde(default)]
    pub sha256_by_platform: IndexMap<String, String>,
    /// Platform → executable path regex inside archive.
    #[serde(default)]
    pub archive_executable_patterns: IndexMap<String, String>,
    /// Additional allow-listed archive members.
    #[serde(default)]
    pub include_archive_files: Vec<String>,
    /// Whether the installed module is enabled by default.
    #[serde(default = "default_true")]
    pub enabled_by_default: bool,
    /// Whether a Go recipe needs a C compiler.
    #[serde(default)]
    pub requires_cgo: bool,
    /// Human guidance.
    #[serde(default)]
    pub note: Option<String>,
}

const fn default_true() -> bool {
    true
}

/// Declared invocation parameter data kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolParamKind {
    /// Free-form non-empty string.
    String,
    /// Bounded integer.
    Integer,
    /// Boolean switch.
    Boolean,
    /// List of non-empty strings.
    StringList,
    /// Filesystem path string.
    Path,
}

impl ToolParamKind {
    /// Stable name exposed to model-facing schemas and audit output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Boolean => "boolean",
            Self::StringList => "string_list",
            Self::Path => "path",
        }
    }
}

/// Adapter default of a declared invocation parameter.
///
/// YAML `default` is a string/integer/boolean/string-list union; untagged
/// deserialization resolves the variant from the scalar type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolParamDefault {
    /// String default.
    Text(String),
    /// Integer default.
    Integer(i64),
    /// Boolean default.
    Boolean(bool),
    /// String list default.
    List(Vec<String>),
}

/// One declared invocation parameter.
///
/// `key` is the exact spelling the adapter uses to read mission-config args;
/// there is deliberately no translation layer between settings and argv.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolParamSpec {
    /// Adapter args key (exact spelling).
    pub key: String,
    /// Value kind.
    pub kind: ToolParamKind,
    /// CLI flag shown in the UI; argv is always built by the adapter.
    #[serde(default)]
    pub flag: Option<String>,
    /// Adapter default used when neither mission config nor stored settings
    /// provide a value.
    #[serde(default)]
    pub default: Option<ToolParamDefault>,
    /// Whether a value is required before the tool can run.
    #[serde(default)]
    pub required: bool,
    /// UI-facing description.
    #[serde(default)]
    pub description: Option<String>,
    /// Inclusive integer lower bound (integer kind only).
    #[serde(default)]
    pub minimum: Option<i64>,
    /// Inclusive integer upper bound (integer kind only).
    #[serde(default)]
    pub maximum: Option<i64>,
}

/// One declared subprocess environment variable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolEnvKeySpec {
    /// Environment variable name (exact, case-sensitive).
    pub name: String,
    /// UI-facing description.
    #[serde(default)]
    pub description: Option<String>,
    /// Whether the key is required for meaningful operation.
    #[serde(default)]
    pub required: bool,
}

/// Declared invocation surface of a catalog tool: configurable params and env
/// keys. Presence of this spec is what makes a tool configurable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolInvocationSpec {
    /// Declared parameters, in declaration order.
    #[serde(default)]
    pub params: Vec<ToolParamSpec>,
    /// Declared env keys, in declaration order.
    #[serde(default)]
    pub env_keys: Vec<ToolEnvKeySpec>,
}

/// Persisted per-tool invocation settings (`settings` field of a
/// local-tools.json entry).
///
/// Env values are plaintext by necessity (they are injected into the tool's
/// subprocess environment); they live only in this file and never re-enter
/// any API response, log, or audit record — responses carry
/// [`ToolConfiguredSettingsView`], which masks them to configured names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolConfiguredSettings {
    /// Configured parameter values (full-replacement semantics).
    #[serde(default)]
    pub params: Map<String, Value>,
    /// Configured env values, sorted by key for stable file layout.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Sanitized configured-settings view for API responses: params are echoed
/// verbatim, env only as the sorted list of configured key names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolConfiguredSettingsView {
    /// Configured parameter values.
    #[serde(default)]
    pub params: Map<String, Value>,
    /// Names (never values) of configured env keys, sorted.
    #[serde(default)]
    pub env_set: Vec<String>,
}

/// Static catalog entry loaded from the embedded YAML.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogTool {
    /// Stable catalog ID.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Audit domain wire value.
    pub domain: String,
    /// Short description.
    pub description: String,
    /// Upstream project URL.
    pub upstream_url: String,
    /// Candidate executable names.
    #[serde(default)]
    pub executable_names: Vec<String>,
    /// Fixed health/version arguments.
    #[serde(default)]
    pub version_args: Vec<String>,
    /// Supported OS families.
    #[serde(default)]
    pub supported_platforms: Vec<String>,
    /// Expected adapter output format.
    pub output_format: String,
    /// Adapter implementation status.
    pub adapter_status: String,
    /// Operational risk guidance.
    #[serde(default)]
    pub risk_notes: Vec<String>,
    /// Optional install recipe.
    #[serde(default)]
    pub install: Option<ToolInstall>,
    /// Declared invocation surface, when the tool accepts invocation config.
    #[serde(default)]
    pub invocation: Option<ToolInvocationSpec>,
    /// CLI flag(s) used to pass the target to the tool, e.g. `["-d"]` for
    /// `subfinder -d DOMAIN`.  Empty means the tool takes target as a
    /// positional argument (appended at the end of argv).
    #[serde(default)]
    pub target_flag: Vec<String>,
    /// Extra CLI flags appended unconditionally to request structured output,
    /// e.g. `["-json"]` or `["--format", "json"]`.
    #[serde(default)]
    pub output_flags: Vec<String>,
}

/// Detected local state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDetection {
    /// Whether a usable path was found.
    pub available: bool,
    /// Detection source.
    pub availability: ToolAvailability,
    /// Resolved path.
    pub executable_path: Option<String>,
    /// Source label.
    pub source: Option<String>,
    /// Detected version, when a separate probe supplied it.
    pub version: Option<String>,
    /// Executable digest, when verified.
    pub sha256: Option<String>,
    /// Download source URL, when verified.
    pub source_url: Option<String>,
    /// Integrity status.
    pub integrity_status: String,
    /// Integrity explanation.
    pub integrity_message: Option<String>,
}

impl ToolDetection {
    fn missing() -> Self {
        Self {
            available: false,
            availability: ToolAvailability::Missing,
            executable_path: None,
            source: None,
            version: None,
            sha256: None,
            source_url: None,
            integrity_status: "unverified".to_string(),
            integrity_message: None,
        }
    }

    fn unknown() -> Self {
        Self {
            available: false,
            availability: ToolAvailability::Unknown,
            executable_path: None,
            source: None,
            version: None,
            sha256: None,
            source_url: None,
            integrity_status: "unknown".to_string(),
            integrity_message: None,
        }
    }
}

/// Catalog entry plus local detection.
fn default_tool_enabled() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCatalogEntry {
    /// Stable catalog ID.
    pub id: String,
    /// 运行时启停（local-tools.json 的 `enabled`；未配置默认 true）。
    #[serde(default = "default_tool_enabled")]
    pub enabled: bool,
    /// Display name.
    pub name: String,
    /// Audit domain.
    pub domain: String,
    /// Description.
    pub description: String,
    /// Upstream URL.
    pub upstream_url: String,
    /// Candidate executables.
    pub executable_names: Vec<String>,
    /// Fixed health/version argv.
    pub version_args: Vec<String>,
    /// Supported OS families.
    pub supported_platforms: Vec<String>,
    /// Output format.
    pub output_format: String,
    /// Adapter status.
    pub adapter_status: String,
    /// Risk guidance.
    pub risk_notes: Vec<String>,
    /// Install recipe.
    pub install: Option<ToolInstall>,
    /// Declared invocation surface, when the tool accepts invocation config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<ToolInvocationSpec>,
    /// CLI flag(s) used to pass the target to the tool. Internal execution
    /// metadata; never exposed through the frozen public API.
    #[serde(skip)]
    pub target_flag: Vec<String>,
    /// Extra CLI flags for structured output. Internal execution metadata;
    /// never exposed through the frozen public API.
    #[serde(skip)]
    pub output_flags: Vec<String>,
    /// Local detection.
    pub detection: ToolDetection,
    /// Sanitized configured invocation settings; absent when nothing is
    /// configured (settings all empty count as unconfigured).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configured_settings: Option<ToolConfiguredSettingsView>,
}

impl ToolCatalogEntry {
    /// 从 catalog 声明 + 检测结果 + 本地配置视图构造条目（检索评测等
    /// crate 外装配场景同样需要此构造路径）。
    #[must_use]
    pub fn from_tool(
        tool: CatalogTool,
        detection: ToolDetection,
        configured_settings: Option<ToolConfiguredSettingsView>,
    ) -> Self {
        Self {
            enabled: true,
            id: tool.id,
            name: tool.name,
            domain: tool.domain,
            description: tool.description,
            upstream_url: tool.upstream_url,
            executable_names: tool.executable_names,
            version_args: tool.version_args,
            supported_platforms: tool.supported_platforms,
            output_format: tool.output_format,
            adapter_status: tool.adapter_status,
            risk_notes: tool.risk_notes,
            install: tool.install,
            invocation: tool.invocation,
            target_flag: tool.target_flag,
            output_flags: tool.output_flags,
            detection,
            configured_settings,
        }
    }
}

/// Ranked tool recommendation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRecommendation {
    /// Catalog tool ID.
    pub tool_id: String,
    /// Display name.
    pub name: String,
    /// Domain.
    pub domain: String,
    /// Priority from 0 to 100.
    pub priority: u8,
    /// Explainable deterministic rationale.
    pub rationale: String,
    /// Whether locally detected.
    pub installed: bool,
    /// Adapter status.
    pub adapter_status: String,
}

/// Background install job lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolInstallJobStatus {
    /// Queued.
    Queued,
    /// Worker is evaluating the recipe.
    Running,
    /// Installed.
    Installed,
    /// Already configured/present.
    AlreadyPresent,
    /// Requires manual steps.
    Manual,
    /// No install recipe.
    Skipped,
    /// Recipe family is not yet safely supported by Rust.
    Unsupported,
    /// Installation failed.
    Failed,
}

impl ToolInstallJobStatus {
    /// Whether this is a terminal state.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

/// Observable process-local install job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInstallJob {
    /// Job ID.
    pub id: String,
    /// Catalog tool ID.
    pub tool_id: String,
    /// Recipe method wire value.
    pub method: String,
    /// Whether caller requested reinstall.
    pub force: bool,
    /// Current status.
    pub status: ToolInstallJobStatus,
    /// Human-readable result.
    pub message: String,
    /// Resolved executable path.
    pub executable_path: Option<String>,
    /// Queue time.
    pub created_at: Timestamp,
    /// Start time.
    pub started_at: Option<Timestamp>,
    /// Finish time.
    pub finished_at: Option<Timestamp>,
}

/// Module-compatible health response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolHealthResult {
    /// Tool/module ID.
    pub module_id: String,
    /// Health verdict.
    pub ok: bool,
    /// Result status.
    pub status: String,
    /// Bounded explanation.
    pub message: String,
    /// Check time.
    pub checked_at: Timestamp,
    /// Latency.
    pub latency_ms: Option<i64>,
    /// Bounded diagnostic metadata.
    pub raw: Map<String, Value>,
}

/// Tool catalog failure.
#[derive(Debug, thiserror::Error)]
pub enum ToolCatalogError {
    /// Embedded YAML is invalid.
    #[error("curated tool catalog is invalid: {0}")]
    InvalidCatalog(String),
    /// Local configuration I/O failed.
    #[error("local tool configuration at {path} failed: {source}")]
    ConfigIo {
        /// File path.
        path: PathBuf,
        /// I/O source.
        #[source]
        source: std::io::Error,
    },
    /// Local configuration JSON failed to encode.
    #[error("local tool configuration cannot be encoded: {0}")]
    ConfigJson(#[from] serde_json::Error),
    /// Unknown catalog ID.
    #[error("tool {0:?} not found in catalog")]
    NotFound(String),
    /// Configure request violated the declared invocation settings contract
    /// (unknown key, kind mismatch, out-of-range value, undeclared env key,
    /// settings without a local entry, or a tool without an invocation spec).
    #[error("invalid tool invocation settings: {0}")]
    InvalidConfig(String),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogDocument {
    tools: Vec<CatalogTool>,
}

/// Parse and validate the embedded reviewed catalog.
///
/// # Errors
/// Invalid YAML, blank/duplicate IDs, or blank executable candidate.
pub fn load_catalog() -> Result<Vec<CatalogTool>, ToolCatalogError> {
    let document: CatalogDocument = serde_yaml::from_str(EMBEDDED_CATALOG)
        .map_err(|error| ToolCatalogError::InvalidCatalog(error.to_string()))?;
    if document.tools.is_empty() {
        return Err(ToolCatalogError::InvalidCatalog(
            "catalog must contain a non-empty tools list".to_string(),
        ));
    }
    let mut seen = HashSet::new();
    for tool in &document.tools {
        if tool.id.trim().is_empty()
            || tool.name.trim().is_empty()
            || tool.domain.trim().is_empty()
            || tool
                .executable_names
                .iter()
                .any(|name| name.trim().is_empty())
        {
            return Err(ToolCatalogError::InvalidCatalog(format!(
                "tool {:?} has blank required fields",
                tool.id
            )));
        }
        if tool.executable_names.is_empty() {
            return Err(ToolCatalogError::InvalidCatalog(format!(
                "tool {:?} has no executable_names",
                tool.id
            )));
        }
        if !seen.insert(tool.id.to_lowercase()) {
            return Err(ToolCatalogError::InvalidCatalog(format!(
                "duplicate tool id {:?}",
                tool.id
            )));
        }
        if let Some(error) = invocation_validation_error(tool) {
            return Err(ToolCatalogError::InvalidCatalog(error));
        }
    }
    Ok(document.tools)
}

/// Validate one tool's declared invocation surface: keys/names must be
/// non-blank and unique, and integer bounds must be ordered.
fn invocation_validation_error(tool: &CatalogTool) -> Option<String> {
    let invocation = tool.invocation.as_ref()?;
    let mut param_keys = HashSet::new();
    for param in &invocation.params {
        if param.key.trim().is_empty() {
            return Some(format!(
                "tool {:?} declares a blank invocation param key",
                tool.id
            ));
        }
        if let (Some(minimum), Some(maximum)) = (param.minimum, param.maximum)
            && minimum > maximum
        {
            return Some(format!(
                "tool {:?} param {:?} has minimum {minimum} above maximum {maximum}",
                tool.id, param.key
            ));
        }
        if !param_keys.insert(param.key.clone()) {
            return Some(format!(
                "tool {:?} declares duplicate invocation param key {:?}",
                tool.id, param.key
            ));
        }
    }
    let mut env_names = HashSet::new();
    for key in &invocation.env_keys {
        if key.name.trim().is_empty() {
            return Some(format!(
                "tool {:?} declares a blank invocation env key name",
                tool.id
            ));
        }
        if !env_names.insert(key.name.clone()) {
            return Some(format!(
                "tool {:?} declares duplicate invocation env key {:?}",
                tool.id, key.name
            ));
        }
    }
    None
}

/// Default local tool configuration path.
#[must_use]
pub fn local_tools_config_path() -> PathBuf {
    std::env::var_os("LYNCEUS_LOCAL_TOOLS_CONFIG").map_or_else(
        || PathBuf::from("data/config/local-tools.json"),
        PathBuf::from,
    )
}

/// Detect all catalog entries from explicit config and PATH.
///
/// # Errors
/// Embedded catalog validation failure. Missing/malformed local JSON is treated
/// as an empty config, matching the Python control plane.
pub async fn detect_tool_catalog(
    local_tools_path: &Path,
) -> Result<Vec<ToolCatalogEntry>, ToolCatalogError> {
    let configured = load_local_tools(local_tools_path);
    let settings = crate::tool_settings::load_local_tool_settings(local_tools_path);
    let enabled_map = load_local_tool_enabled(local_tools_path);
    let mut entries = Vec::new();
    for tool in load_catalog()? {
        let detection = detect_tool(&tool, &configured).await;
        // Echo rule mirrors `load_local_tools` key matching: entries are
        // indexed by lowercased `tool_name`/`name`, resolved by catalog id
        // first and display name second.
        let configured_settings = settings
            .get(&tool.id.to_lowercase())
            .or_else(|| settings.get(&tool.name.to_lowercase()))
            .and_then(crate::tool_settings::configured_view);
        let mut entry = ToolCatalogEntry::from_tool(tool, detection, configured_settings);
        entry.enabled = enabled_map
            .get(&entry.id.to_lowercase())
            .or_else(|| enabled_map.get(&entry.name.to_lowercase()))
            .copied()
            .unwrap_or(true);
        entries.push(entry);
    }
    Ok(entries)
}

/// 检测缓存：单槽（key = 配置文件路径）+ mtime + TTL 失效。
///
/// 每次 solver 装配都全量扫描 PATH（17 个工具 × 候选名）开销过大；
/// `local-tools.json` 是唯一的可写触发源（`configure_local_tool` 会改
/// mtime），PATH 安装的新工具最多滞后 [`DETECTION_CACHE_TTL`]。缓存只
/// 存最终 entries（Clone 成本远低于重新探测）。
struct CachedDetection {
    key: PathBuf,
    modified: Option<std::time::SystemTime>,
    detected_at: std::time::Instant,
    entries: Vec<ToolCatalogEntry>,
}

static DETECTION_CACHE: StdMutex<Option<CachedDetection>> = StdMutex::new(None);
const DETECTION_CACHE_TTL: Duration = Duration::from_secs(5);

/// [`detect_tool_catalog`] 的缓存版本（Harness 生产装配默认走这里）。
///
/// 锁毒化不 panic：毒化的互斥锁里缓存只是旧值，直接取出来继续用。
///
/// # Errors
/// 与 [`detect_tool_catalog`] 相同（缓存命中时不产生错误）。
pub async fn detect_tool_catalog_cached(
    local_tools_path: &Path,
) -> Result<Vec<ToolCatalogEntry>, ToolCatalogError> {
    let modified = std::fs::metadata(local_tools_path)
        .and_then(|meta| meta.modified())
        .ok();
    {
        let guard = DETECTION_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cached) = guard.as_ref().filter(|cached| {
            cached.key == local_tools_path
                && cached.modified == modified
                && cached.detected_at.elapsed() < DETECTION_CACHE_TTL
        }) {
            return Ok(cached.entries.clone());
        }
    }
    let entries = detect_tool_catalog(local_tools_path).await?;
    let mut guard = DETECTION_CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = Some(CachedDetection {
        key: local_tools_path.to_path_buf(),
        modified,
        detected_at: std::time::Instant::now(),
        entries: entries.clone(),
    });
    Ok(entries)
}

// ---------------------------------------------------------------------------
// 持久化探测快照（list 端点只读快照，绝不现场探测）
// ---------------------------------------------------------------------------

/// 单工具的探测记录（快照文件中的一行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDetectionRecord {
    /// Catalog 工具 ID。
    pub tool_id: String,
    /// 上次真实探测的结果。
    pub detection: ToolDetection,
}

/// 磁盘探测快照：`GET /tool-catalog` 的唯一运行时状态来源。
///
/// 真实探测（PATH 扫描 / 身份校验子进程）只允许由
/// [`refresh_detection_snapshot`] 执行（`POST /tool-catalog/refresh`、
/// startup 后台任务、显式事件），普通 list 读这个文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDetectionSnapshot {
    /// 上次全量探测完成时间（RFC 3339）。
    pub detected_at: String,
    /// 是否包含完整 catalog 的探测结果；单工具事件快照为 false。
    #[serde(default = "default_true")]
    pub complete: bool,
    /// 按工具记录的探测结果。
    pub detections: Vec<ToolDetectionRecord>,
}

/// 快照文件默认路径（`LYNCEUS_TOOL_DETECTION_SNAPSHOT` 可覆盖）。
#[must_use]
pub fn detection_snapshot_path() -> PathBuf {
    std::env::var_os("LYNCEUS_TOOL_DETECTION_SNAPSHOT").map_or_else(
        || PathBuf::from("data/config/tool-detection.json"),
        PathBuf::from,
    )
}

/// Resolve the snapshot paired with a concrete local-tools config. Tests and
/// alternate configs stay isolated unless an explicit snapshot override is set.
#[must_use]
pub fn detection_snapshot_path_for(local_tools_path: &Path) -> PathBuf {
    std::env::var_os("LYNCEUS_TOOL_DETECTION_SNAPSHOT").map_or_else(
        || local_tools_path.with_file_name("tool-detection.json"),
        PathBuf::from,
    )
}

/// Catalog runtime refresh lifecycle exposed by the status endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolDetectionRuntimeState {
    /// No complete snapshot exists yet.
    Unknown,
    /// A bounded background/manual refresh is running.
    Refreshing,
    /// A complete snapshot is available.
    Ready,
    /// The most recent refresh failed.
    Error,
}

fn detection_marker_path(snapshot_path: &Path) -> PathBuf {
    snapshot_path.with_extension("refreshing")
}

fn detection_error_path(snapshot_path: &Path) -> PathBuf {
    snapshot_path.with_extension("error")
}

/// Read the refresh lifecycle without running detection.
#[must_use]
pub fn detection_runtime_state(
    snapshot_path: &Path,
    snapshot: Option<&ToolDetectionSnapshot>,
) -> (ToolDetectionRuntimeState, Option<String>) {
    if detection_marker_path(snapshot_path).exists() {
        return (ToolDetectionRuntimeState::Refreshing, None);
    }
    if let Ok(error) = std::fs::read_to_string(detection_error_path(snapshot_path)) {
        return (
            ToolDetectionRuntimeState::Error,
            Some(error.chars().take(500).collect()),
        );
    }
    if snapshot.is_some_and(|snapshot| snapshot.complete) {
        (ToolDetectionRuntimeState::Ready, None)
    } else {
        (ToolDetectionRuntimeState::Unknown, None)
    }
}

/// 读快照；文件缺失/损坏一律返回 `None`（调用方退化为全 Missing
/// 视图，绝不因此触发探测）。
#[must_use]
pub fn load_detection_snapshot(path: &Path) -> Option<ToolDetectionSnapshot> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// 写快照（先写临时文件再改名，避免读者看到半个 JSON）。
fn write_detection_snapshot(
    path: &Path,
    snapshot: &ToolDetectionSnapshot,
) -> Result<(), ToolCatalogError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ToolCatalogError::ConfigIo {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let body = serde_json::to_string_pretty(snapshot)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).map_err(|source| ToolCatalogError::ConfigIo {
        path: tmp.clone(),
        source,
    })?;
    std::fs::rename(&tmp, path).map_err(|source| ToolCatalogError::ConfigIo {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// 合并视图：内嵌元数据 + live 配置 detection + 快照 PATH detection。
///
/// 这是 `GET /tool-catalog` 的唯一装配路径——**不 spawn 任何进程、
/// 不扫描 PATH**。configured（local-tools.json 显式登记）条目总是
/// 实时准确（configure/install 失效事件因此天然成立）；PATH 探测
/// 结果来自上次 refresh 的快照；快照缺失的条目为 Missing。
///
/// # Errors
/// 内嵌 catalog 解析失败（编译期内嵌，实际不可达）。
pub fn catalog_entries_from_snapshot(
    local_tools_path: &Path,
    snapshot: Option<&ToolDetectionSnapshot>,
) -> Result<Vec<ToolCatalogEntry>, ToolCatalogError> {
    let configured = load_local_tools(local_tools_path);
    let enabled_map = load_local_tool_enabled(local_tools_path);
    let settings = crate::tool_settings::load_local_tool_settings(local_tools_path);
    let snapshot_map: HashMap<&str, &ToolDetection> = snapshot
        .map(|snap| {
            snap.detections
                .iter()
                .map(|record| (record.tool_id.as_str(), &record.detection))
                .collect()
        })
        .unwrap_or_default();
    let mut entries = Vec::new();
    for tool in load_catalog()? {
        let detection = configured_detection(&tool, &configured)
            .or_else(|| {
                snapshot_map
                    .get(tool.id.as_str())
                    .map(|detection| (*detection).clone())
            })
            .unwrap_or_else(|| {
                if snapshot.is_some_and(|snapshot| snapshot.complete) {
                    ToolDetection::missing()
                } else {
                    ToolDetection::unknown()
                }
            });
        let configured_settings = settings
            .get(&tool.id.to_lowercase())
            .or_else(|| settings.get(&tool.name.to_lowercase()))
            .and_then(crate::tool_settings::configured_view);
        let mut entry = ToolCatalogEntry::from_tool(tool, detection, configured_settings);
        entry.enabled = enabled_map
            .get(&entry.id.to_lowercase())
            .or_else(|| enabled_map.get(&entry.name.to_lowercase()))
            .copied()
            .unwrap_or(true);
        entries.push(entry);
    }
    Ok(entries)
}

/// 全量重新探测并刷新快照（唯一允许扫描 PATH / spawn 身份校验的
/// 目录级入口）。成功后同步更新内存缓存与磁盘快照。
///
/// # Errors
/// catalog 解析失败或快照写盘失败。
pub async fn refresh_detection_snapshot(
    local_tools_path: &Path,
    snapshot_path: &Path,
) -> Result<ToolDetectionSnapshot, ToolCatalogError> {
    if let Some(parent) = snapshot_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ToolCatalogError::ConfigIo {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let marker_path = detection_marker_path(snapshot_path);
    let error_path = detection_error_path(snapshot_path);
    std::fs::write(&marker_path, chrono::Utc::now().to_rfc3339()).map_err(|source| {
        ToolCatalogError::ConfigIo {
            path: marker_path.clone(),
            source,
        }
    })?;
    let _ = std::fs::remove_file(&error_path);

    let result: Result<ToolDetectionSnapshot, ToolCatalogError> = async {
        let entries = detect_tool_catalog(local_tools_path).await?;
        let snapshot = ToolDetectionSnapshot {
            detected_at: chrono::Utc::now().to_rfc3339(),
            complete: true,
            detections: entries
                .iter()
                .map(|entry| ToolDetectionRecord {
                    tool_id: entry.id.clone(),
                    detection: entry.detection.clone(),
                })
                .collect(),
        };
        write_detection_snapshot(snapshot_path, &snapshot)?;
        let modified = std::fs::metadata(local_tools_path)
            .and_then(|meta| meta.modified())
            .ok();
        let mut guard = DETECTION_CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(CachedDetection {
            key: local_tools_path.to_path_buf(),
            modified,
            detected_at: std::time::Instant::now(),
            entries,
        });
        Ok(snapshot)
    }
    .await;
    let _ = std::fs::remove_file(&marker_path);
    if let Err(error) = &result {
        let _ = std::fs::write(&error_path, error.to_string());
    }
    result
}

/// 单工具探测（`POST /tool-catalog/{id}/test` 与 configure 失效
/// 刷新用）：只对指定工具跑 `detect_tool`，绝不全量扫描。
///
/// # Errors
/// catalog 解析失败。
pub async fn detect_single_tool(
    tool_id: &str,
    local_tools_path: &Path,
) -> Result<Option<ToolCatalogEntry>, ToolCatalogError> {
    let configured = load_local_tools(local_tools_path);
    let enabled_map = load_local_tool_enabled(local_tools_path);
    let settings = crate::tool_settings::load_local_tool_settings(local_tools_path);
    for tool in load_catalog()? {
        if tool.id.eq_ignore_ascii_case(tool_id) {
            let detection = detect_tool(&tool, &configured).await;
            let configured_settings = settings
                .get(&tool.id.to_lowercase())
                .or_else(|| settings.get(&tool.name.to_lowercase()))
                .and_then(crate::tool_settings::configured_view);
            let mut entry =
                ToolCatalogEntry::from_tool(tool, detection, configured_settings);
            entry.enabled = enabled_map
                .get(&entry.id.to_lowercase())
                .or_else(|| enabled_map.get(&entry.name.to_lowercase()))
                .copied()
                .unwrap_or(true);
            return Ok(Some(entry));
        }
    }
    Ok(None)
}

/// 把单工具的最新探测结果并入快照（configure/install 事件驱动
/// 失效：写配置后立刻重测该工具并落盘，无需全量 refresh）。
pub fn upsert_snapshot_detection(snapshot: &mut ToolDetectionSnapshot, entry: &ToolCatalogEntry) {
    snapshot.detected_at = chrono::Utc::now().to_rfc3339();
    if let Some(record) = snapshot
        .detections
        .iter_mut()
        .find(|record| record.tool_id.eq_ignore_ascii_case(&entry.id))
    {
        record.detection = entry.detection.clone();
        return;
    }
    snapshot.detections.push(ToolDetectionRecord {
        tool_id: entry.id.clone(),
        detection: entry.detection.clone(),
    });
}

/// 单工具重测 + 快照落盘的组合（configure 后调用）；快照文件
/// 缺失时从空快照开始（只含这一条记录，其余条目等下次全量
/// refresh 补全）。
///
/// # Errors
/// catalog 解析或快照写盘失败。
pub async fn redetect_tool_into_snapshot(
    tool_id: &str,
    local_tools_path: &Path,
    snapshot_path: &Path,
) -> Result<Option<ToolCatalogEntry>, ToolCatalogError> {
    let Some(entry) = detect_single_tool(tool_id, local_tools_path).await? else {
        return Ok(None);
    };
    let mut snapshot = load_detection_snapshot(snapshot_path).unwrap_or(ToolDetectionSnapshot {
        detected_at: chrono::Utc::now().to_rfc3339(),
        complete: false,
        detections: Vec::new(),
    });
    upsert_snapshot_detection(&mut snapshot, &entry);
    write_detection_snapshot(snapshot_path, &snapshot)?;
    Ok(Some(entry))
}

/// 纯配置的 detection（不 spawn 任何进程、不碰 PATH）：local-tools.json
/// 显式登记的 path 直接采信。list/快照合并路径用它保证 configure /
/// install 的结果立刻可见，无需等待重新探测。
fn configured_detection(
    tool: &CatalogTool,
    configured: &HashMap<String, String>,
) -> Option<ToolDetection> {
    for name in candidate_names(tool) {
        if let Some(path) = configured.get(&name.to_lowercase()) {
            return Some(ToolDetection {
                available: true,
                availability: ToolAvailability::Configured,
                executable_path: Some(path.clone()),
                source: Some("data/config/local-tools.json".to_string()),
                version: None,
                sha256: None,
                source_url: None,
                integrity_status: "unverified".to_string(),
                integrity_message: None,
            });
        }
    }
    None
}

async fn detect_tool(tool: &CatalogTool, configured: &HashMap<String, String>) -> ToolDetection {
    if let Some(detection) = configured_detection(tool, configured) {
        return detection;
    }
    let mut checked = HashSet::new();
    for name in candidate_names(tool) {
        if let Some(path) = find_on_path(name)
            && checked.insert(path.clone())
            && matches_tool_identity(&tool.id, &path).await
        {
            return ToolDetection {
                available: true,
                availability: ToolAvailability::Path,
                executable_path: Some(path),
                source: Some("PATH".to_string()),
                version: None,
                sha256: None,
                source_url: None,
                integrity_status: "unverified".to_string(),
                integrity_message: None,
            };
        }
    }
    ToolDetection::missing()
}

fn candidate_names(tool: &CatalogTool) -> impl Iterator<Item = &str> {
    std::iter::once(tool.id.as_str())
        .chain(std::iter::once(tool.name.as_str()))
        .chain(tool.executable_names.iter().map(String::as_str))
}

fn find_on_path(name: &str) -> Option<String> {
    let candidate = Path::new(name);
    if candidate.components().count() > 1 && is_usable_executable(candidate) {
        return Some(candidate.to_string_lossy().into_owned());
    }
    let path = std::env::var_os("PATH")?;
    let extensions = if cfg!(windows) && candidate.extension().is_none() {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
            .split(';')
            .filter(|item| !item.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>()
    } else {
        vec![String::new()]
    };
    for directory in std::env::split_paths(&path) {
        for extension in &extensions {
            let resolved = directory.join(format!("{name}{extension}"));
            if is_usable_executable(&resolved) {
                return Some(resolved.to_string_lossy().into_owned());
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_usable_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_usable_executable(path: &Path) -> bool {
    path.is_file()
}

async fn matches_tool_identity(tool_id: &str, executable_path: &str) -> bool {
    if !tool_id.eq_ignore_ascii_case("httpx") {
        return true;
    }
    let cache_key = identity_cache_key(executable_path);
    if let Some(key) = cache_key.as_ref()
        && let Some(cached) = identity_probe_cache()
            .lock()
            .ok()
            .and_then(|cache| cache.get(key).copied())
    {
        return cached;
    }
    let mut command = tokio::process::Command::new(executable_path);
    command.arg("-h").kill_on_drop(true);
    let matches = match tokio::time::timeout(Duration::from_secs(3), command.output()).await {
        Ok(Ok(output)) => identity_output_matches(
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
        ),
        Ok(Err(_)) | Err(_) => false,
    };
    if let Some(key) = cache_key
        && let Ok(mut cache) = identity_probe_cache().lock()
    {
        if cache.len() >= 128 {
            cache.clear();
        }
        cache.insert(key, matches);
    }
    matches
}

/// Decide tool identity from a `-h` probe's combined output.
///
/// `ProjectDiscovery` httpx exposes both flags; the Python HTTPX client CLI,
/// which commonly owns the same command name, does not expose `-silent`.
fn identity_output_matches(stdout: &str, stderr: &str) -> bool {
    let text = format!("{stdout}\n{stderr}").to_lowercase();
    text.contains("-silent") && text.contains("-json")
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct IdentityCacheKey {
    path: PathBuf,
    size: u64,
    modified_nanos: u128,
}

fn identity_probe_cache() -> &'static StdMutex<HashMap<IdentityCacheKey, bool>> {
    static CACHE: OnceLock<StdMutex<HashMap<IdentityCacheKey, bool>>> = OnceLock::new();
    CACHE.get_or_init(|| StdMutex::new(HashMap::new()))
}

fn identity_cache_key(executable_path: &str) -> Option<IdentityCacheKey> {
    let path = Path::new(executable_path);
    let metadata = path.metadata().ok()?;
    let modified_nanos = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(IdentityCacheKey {
        path: path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
        size: metadata.len(),
        modified_nanos,
    })
}

/// Read executable paths from local-tools.json, keyed by lowercased
/// `tool_name`/`name`. Malformed files and entries are treated as absent,
/// matching the Python control plane.
/// local-tools.json 的运行时启停表（key = 小写 tool_name/name；未记录
/// 的工具默认启用）。
pub(crate) fn load_local_tool_enabled(path: &Path) -> HashMap<String, bool> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let Ok(payload) = serde_json::from_str::<Value>(&text) else {
        return HashMap::new();
    };
    let raw = payload
        .as_object()
        .and_then(|object| object.get("local_tools"))
        .unwrap_or(&payload);
    let Some(items) = raw.as_array() else {
        return HashMap::new();
    };
    let mut tools = HashMap::new();
    for item in items {
        let Some(object) = item.as_object() else {
            continue;
        };
        let Some(enabled) = object.get("enabled").and_then(Value::as_bool) else {
            continue;
        };
        for key in ["tool_name", "name"] {
            if let Some(name) = object
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                tools.insert(name.to_lowercase(), enabled);
            }
        }
    }
    tools
}

pub(crate) fn load_local_tools(path: &Path) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let Ok(payload) = serde_json::from_str::<Value>(&text) else {
        return HashMap::new();
    };
    let raw = payload
        .as_object()
        .and_then(|object| object.get("local_tools"))
        .unwrap_or(&payload);
    let Some(items) = raw.as_array() else {
        return HashMap::new();
    };
    let mut tools = HashMap::new();
    for item in items {
        let Some(object) = item.as_object() else {
            continue;
        };
        let Some(executable) = object
            .get("executable_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        for key in ["tool_name", "name"] {
            if let Some(name) = object
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                tools.insert(name.to_lowercase(), executable.to_string());
            }
        }
    }
    tools
}

/// Update one allow-listed tool entry in local-tools.json.
///
/// # Errors
/// Unknown tool ID, directory/write failure, or JSON encoding failure.
pub fn configure_local_tool(
    tool_id: &str,
    executable_path: Option<&str>,
    enabled: bool,
    path: &Path,
) -> Result<(), ToolCatalogError> {
    let catalog = load_catalog()?;
    let tool = catalog
        .iter()
        .find(|tool| tool.id.eq_ignore_ascii_case(tool_id))
        .ok_or_else(|| ToolCatalogError::NotFound(tool_id.to_string()))?;
    let mut payload = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({"local_tools": []}));
    let object = payload.as_object_mut().ok_or_else(|| {
        ToolCatalogError::InvalidCatalog("local config must be an object".to_string())
    })?;
    if !object.get("local_tools").is_some_and(Value::is_array) {
        object.insert("local_tools".to_string(), Value::Array(Vec::new()));
    }
    let items = object
        .get_mut("local_tools")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| {
            ToolCatalogError::InvalidCatalog("local_tools must be an array".to_string())
        })?;
    let normalized = tool.id.to_lowercase();
    let mut found = false;
    for item in items.iter_mut().filter_map(Value::as_object_mut) {
        let matches = item
            .get("tool_name")
            .and_then(Value::as_str)
            .is_some_and(|name| name.trim().eq_ignore_ascii_case(&normalized));
        if matches {
            if let Some(executable) = executable_path
                .map(str::trim)
                .filter(|path| !path.is_empty())
            {
                item.insert(
                    "executable_path".to_string(),
                    Value::String(executable.to_string()),
                );
            }
            item.insert("enabled".to_string(), Value::Bool(enabled));
            found = true;
            break;
        }
    }
    if !found {
        // WP5 运行时启停：无路径也落条目（enabled 切换对未配置工具同样生效；
        // executable_path 缺失保持未配置语义，由检测回退 PATH）。
        if let Some(executable) = executable_path
            .map(str::trim)
            .filter(|path| !path.is_empty())
        {
            items.push(json!({
                "tool_name": tool.id,
                "executable_path": executable,
                "enabled": enabled,
            }));
        } else {
            items.push(json!({
                "tool_name": tool.id,
                "enabled": enabled,
            }));
        }
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|source| ToolCatalogError::ConfigIo {
            path: path.to_path_buf(),
            source,
        })?;
    }
    let text = serde_json::to_string_pretty(&payload)?;
    std::fs::write(path, text).map_err(|source| ToolCatalogError::ConfigIo {
        path: path.to_path_buf(),
        source,
    })
}

/// Deterministically recommend catalog tools without executing them.
#[must_use]
pub fn recommend_tools(
    mission: Option<&Mission>,
    branch_metadata: &Map<String, Value>,
    facts: &[Fact],
    findings: &[Finding],
    evidence: &[Evidence],
    catalog: &[ToolCatalogEntry],
) -> Vec<ToolRecommendation> {
    let branch_kind = branch_metadata
        .get("branch_kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let fact_kinds = facts
        .iter()
        .map(|fact| fact.kind.as_str())
        .collect::<HashSet<_>>();
    let finding_rules = findings
        .iter()
        .filter_map(|item| item.rule_id.as_deref())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let evidence_text = evidence
        .iter()
        .map(|item| item.summary.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let mut scores: Vec<(&str, u8, &str)> = Vec::new();
    if branch_kind.starts_with("url.")
        || fact_kinds.contains("web.endpoint")
        || fact_kinds.contains("asset.http_service")
    {
        add_score(
            &mut scores,
            "ffuf",
            88,
            "Endpoint/service facts make content discovery high-value.",
        );
        add_score(
            &mut scores,
            "wappalyzergo",
            80,
            "HTTP service facts should be fingerprinted.",
        );
    }
    if fact_kinds.contains("asset.technology") || fact_kinds.contains("asset.product") {
        add_score(
            &mut scores,
            "afrog",
            66,
            "Known product facts can guide bounded PoC validation.",
        );
    }
    if finding_rules.contains("secret") || evidence_text.contains("secret") {
        add_score(
            &mut scores,
            "afrog",
            35,
            "Secret findings need manual review before PoC validation.",
        );
    }
    scores.sort_by_key(|item| Reverse(item.1));
    scores
        .into_iter()
        .filter_map(|(id, priority, rationale)| {
            catalog
                .iter()
                .find(|entry| entry.id.eq_ignore_ascii_case(id))
                .map(|entry| ToolRecommendation {
                    tool_id: entry.id.clone(),
                    name: entry.name.clone(),
                    domain: entry.domain.clone(),
                    priority,
                    rationale: rationale.to_string(),
                    installed: entry.detection.available,
                    adapter_status: entry.adapter_status.clone(),
                })
        })
        .collect()
}

fn add_score<'a>(scores: &mut Vec<(&'a str, u8, &'a str)>, id: &'a str, value: u8, why: &'a str) {
    if let Some(existing) = scores.iter_mut().find(|item| item.0 == id) {
        if value > existing.1 {
            *existing = (id, value, why);
        }
    } else {
        scores.push((id, value, why));
    }
}

/// Run a bounded, fixed-argv version probe.
pub async fn test_tool_health(entry: &ToolCatalogEntry) -> ToolHealthResult {
    let checked_at = utcnow();
    let Some(executable) = entry.detection.executable_path.as_ref() else {
        return ToolHealthResult {
            module_id: entry.id.clone(),
            ok: false,
            status: "error".to_string(),
            message: format!(
                "Tool '{}' is not installed or configured on this system.",
                entry.id
            ),
            checked_at,
            latency_ms: None,
            raw: Map::new(),
        };
    };
    let mut request = ToolRequest::new(&entry.id, executable);
    request.args.clone_from(&entry.version_args);
    request.timeout_seconds = 10;
    match ToolGateway.execute(None, None, None, None, request).await {
        Ok(result) => {
            let invocation = result.invocation;
            let ok = invocation.status == ToolStatus::Ok;
            let status = match invocation.status {
                ToolStatus::Ok => "ok",
                ToolStatus::Timeout => "timeout",
                ToolStatus::WaitingForConfirmation | ToolStatus::Error | ToolStatus::Denied => {
                    "error"
                }
            };
            let message = if invocation.output_summary.is_empty() {
                invocation
                    .error
                    .clone()
                    .unwrap_or_else(|| "health check produced no output".to_string())
            } else {
                invocation.output_summary.chars().take(500).collect()
            };
            ToolHealthResult {
                module_id: entry.id.clone(),
                ok,
                status: status.to_string(),
                message,
                checked_at,
                latency_ms: invocation.duration_ms,
                raw: [
                    (
                        "executable_path".to_string(),
                        Value::String(executable.clone()),
                    ),
                    ("version_args".to_string(), json!(entry.version_args)),
                    ("exit_code".to_string(), json!(invocation.exit_code)),
                ]
                .into_iter()
                .collect(),
            }
        }
        Err(error) => ToolHealthResult {
            module_id: entry.id.clone(),
            ok: false,
            status: "error".to_string(),
            message: format!("local tool executable not found: {error}"),
            checked_at,
            latency_ms: None,
            raw: [(
                "executable_path".to_string(),
                Value::String(executable.clone()),
            )]
            .into_iter()
            .collect(),
        },
    }
}

#[derive(Debug, Default)]
struct InstallState {
    jobs: HashMap<String, ToolInstallJob>,
    /// Job ids in insertion order so `list` can keep Python's stable-sort
    /// tie-break (equal `created_at` keeps queue order) despite `HashMap`'s
    /// unordered iteration.
    insertion_order: Vec<String>,
    active_by_tool: HashMap<String, String>,
}

#[derive(Debug)]
struct ProvisionCommand {
    executable: PathBuf,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    cwd: Option<PathBuf>,
}

#[derive(Debug)]
struct ProvisionCommandOutput {
    success: bool,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

#[async_trait]
trait ProvisionCommandRunner: Send + Sync {
    async fn run(&self, command: &ProvisionCommand) -> ProvisionCommandOutput;
}

struct ProcessCommandRunner;

#[async_trait]
impl ProvisionCommandRunner for ProcessCommandRunner {
    async fn run(&self, specification: &ProvisionCommand) -> ProvisionCommandOutput {
        let mut command = tokio::process::Command::new(&specification.executable);
        command
            .args(&specification.args)
            .envs(specification.env.iter().cloned())
            .kill_on_drop(true);
        if let Some(cwd) = &specification.cwd {
            command.current_dir(cwd);
        }
        // Spawn/timeout failures mirror the Python runner's OSError and
        // SubprocessError handling: exit code 127 with the failure text as
        // stderr, so the terminal message keeps the shared "install command
        // failed" shape.
        let output = match tokio::time::timeout(INSTALL_TIMEOUT, command.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                return ProvisionCommandOutput {
                    success: false,
                    exit_code: Some(127),
                    stdout: String::new(),
                    stderr: format!("could not start install command: {error}"),
                };
            }
            Err(_) => {
                return ProvisionCommandOutput {
                    success: false,
                    exit_code: Some(127),
                    stdout: String::new(),
                    stderr: "install command timed out after 1800 seconds".to_string(),
                };
            }
        };
        ProvisionCommandOutput {
            success: output.status.success(),
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

#[derive(Debug)]
struct ProvisionOutcome {
    status: ToolInstallJobStatus,
    message: String,
    executable_path: Option<String>,
}

impl ProvisionOutcome {
    fn terminal(status: ToolInstallJobStatus, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            executable_path: None,
        }
    }

    fn failed(message: impl Into<String>) -> Self {
        Self::terminal(ToolInstallJobStatus::Failed, message)
    }
}

/// Failure modes while deriving an install command from a validated recipe.
#[derive(Debug)]
enum BuildCommandError {
    /// Required toolchain is absent from PATH; maps to a skipped install.
    ToolchainMissing(String),
    /// Recipe or environment defect that must surface as a failed install.
    Fatal(String),
}

type ToolchainResolver = dyn Fn(&str) -> Option<PathBuf> + Send + Sync;

/// Process-local install-job coordinator.
///
/// Only reviewed catalog IDs are accepted. Package names, versions and argv
/// are derived from the embedded catalog and are never supplied by API callers.
pub struct ToolInstallCoordinator {
    catalog: Arc<Vec<CatalogTool>>,
    local_tools_path: PathBuf,
    tools_dir: PathBuf,
    runner: Arc<dyn ProvisionCommandRunner>,
    toolchain_resolver: Arc<ToolchainResolver>,
    release_fetcher: Arc<dyn ReleaseFetcher>,
    state: Mutex<InstallState>,
    install_lock: Mutex<()>,
}

impl ToolInstallCoordinator {
    /// Construct from the reviewed embedded catalog and the default tools root.
    ///
    /// `LYNCEUS_TOOLS_DIR` may override the default workspace-local `tools/`
    /// directory. The value is resolved once, so a later working-directory or
    /// environment change cannot redirect an in-flight install.
    ///
    /// # Errors
    /// Embedded catalog validation failure.
    pub fn new(local_tools_path: PathBuf) -> Result<Self, ToolCatalogError> {
        let tools_dir = std::env::var_os("LYNCEUS_TOOLS_DIR")
            .filter(|value| !value.is_empty())
            .map_or_else(|| PathBuf::from("tools"), PathBuf::from);
        Self::with_tools_dir(local_tools_path, tools_dir)
    }

    /// Construct with an explicit, fixed provisioning root.
    ///
    /// # Errors
    /// Embedded catalog validation failure.
    pub fn with_tools_dir(
        local_tools_path: PathBuf,
        tools_dir: PathBuf,
    ) -> Result<Self, ToolCatalogError> {
        let tools_dir = absolute_once(tools_dir);
        Ok(Self {
            catalog: Arc::new(load_catalog()?),
            local_tools_path,
            tools_dir,
            runner: Arc::new(ProcessCommandRunner),
            toolchain_resolver: Arc::new(|name| find_on_path(name).map(PathBuf::from)),
            release_fetcher: Arc::new(HttpReleaseFetcher),
            state: Mutex::new(InstallState::default()),
            install_lock: Mutex::new(()),
        })
    }

    #[cfg(test)]
    fn with_test_runtime(
        local_tools_path: PathBuf,
        tools_dir: PathBuf,
        runner: Arc<dyn ProvisionCommandRunner>,
        resolver: Arc<ToolchainResolver>,
    ) -> Result<Self, ToolCatalogError> {
        let mut coordinator = Self::with_tools_dir(local_tools_path, tools_dir)?;
        coordinator.runner = runner;
        coordinator.toolchain_resolver = resolver;
        Ok(coordinator)
    }

    #[cfg(test)]
    fn with_test_runtime_and_fetcher(
        local_tools_path: PathBuf,
        tools_dir: PathBuf,
        runner: Arc<dyn ProvisionCommandRunner>,
        resolver: Arc<ToolchainResolver>,
        fetcher: Arc<dyn ReleaseFetcher>,
    ) -> Result<Self, ToolCatalogError> {
        let mut coordinator =
            Self::with_test_runtime(local_tools_path, tools_dir, runner, resolver)?;
        coordinator.release_fetcher = fetcher;
        Ok(coordinator)
    }

    /// Queue an allow-listed install evaluation, deduplicating active jobs.
    ///
    /// The active lookup and job insertion share one lock. Two simultaneous
    /// requests for the same tool therefore cannot both pass the lookup.
    ///
    /// # Errors
    /// Unknown catalog ID.
    pub async fn start(
        self: &Arc<Self>,
        tool_id: &str,
        force: bool,
    ) -> Result<ToolInstallJob, ToolCatalogError> {
        let normalized = tool_id.trim().to_lowercase();
        let tool = self
            .catalog
            .iter()
            .find(|tool| tool.id.to_lowercase() == normalized)
            .cloned()
            .ok_or_else(|| ToolCatalogError::NotFound(tool_id.to_string()))?;
        let method = tool.install.as_ref().map_or_else(
            || "none".to_string(),
            |install| install.method.as_str().to_string(),
        );
        let job = {
            let mut state = self.state.lock().await;
            if let Some(job) = state
                .active_by_tool
                .get(&normalized)
                .and_then(|job_id| state.jobs.get(job_id))
                .filter(|job| !job.status.is_terminal())
            {
                return Ok(job.clone());
            }
            let job = ToolInstallJob {
                id: new_id("toolinstall"),
                tool_id: tool.id.clone(),
                method,
                force,
                status: ToolInstallJobStatus::Queued,
                message: String::new(),
                executable_path: None,
                created_at: utcnow(),
                started_at: None,
                finished_at: None,
            };
            state.jobs.insert(job.id.clone(), job.clone());
            state.insertion_order.push(job.id.clone());
            state.active_by_tool.insert(normalized, job.id.clone());
            job
        };
        let control = Arc::clone(self);
        let job_id = job.id.clone();
        tokio::spawn(async move {
            control.evaluate_install(&job_id, tool).await;
        });
        tokio::task::yield_now().await;
        Ok(self.get(&job.id).await.unwrap_or(job))
    }

    /// Get one job.
    pub async fn get(&self, job_id: &str) -> Option<ToolInstallJob> {
        self.state.lock().await.jobs.get(job_id).cloned()
    }

    /// List newest first, optionally filtered by tool ID.
    pub async fn list(&self, tool_id: Option<&str>) -> Vec<ToolInstallJob> {
        let normalized = tool_id.map(|value| value.trim().to_lowercase());
        let state = self.state.lock().await;
        let mut jobs = state
            .insertion_order
            .iter()
            .filter_map(|job_id| state.jobs.get(job_id))
            .filter(|job| {
                normalized
                    .as_ref()
                    .is_none_or(|id| job.tool_id.to_lowercase() == *id)
            })
            .cloned()
            .collect::<Vec<_>>();
        jobs.sort_by_key(|item| Reverse(item.created_at));
        jobs
    }

    async fn evaluate_install(&self, job_id: &str, tool: CatalogTool) {
        let force = {
            let mut state = self.state.lock().await;
            let Some(job) = state.jobs.get_mut(job_id) else {
                return;
            };
            job.status = ToolInstallJobStatus::Running;
            job.started_at = Some(utcnow());
            job.force
        };
        let outcome = {
            // All recipes can update the same venv and local-tools.json.
            let _install_guard = self.install_lock.lock().await;
            self.provision_tool(&tool, force).await
        };
        let snapshot_error = if matches!(
            outcome.status,
            ToolInstallJobStatus::Installed | ToolInstallJobStatus::AlreadyPresent
        ) {
            redetect_tool_into_snapshot(
                &tool.id,
                &self.local_tools_path,
                &detection_snapshot_path_for(&self.local_tools_path),
            )
            .await
            .err()
        } else {
            None
        };
        let mut state = self.state.lock().await;
        if let Some(job) = state.jobs.get_mut(job_id) {
            job.status = outcome.status;
            job.message = outcome.message;
            if let Some(error) = snapshot_error {
                job.message.push_str("; detection snapshot update failed: ");
                job.message.push_str(&error.to_string());
            }
            job.executable_path = outcome.executable_path;
            job.finished_at = Some(utcnow());
        }
        if state
            .active_by_tool
            .get(&tool.id.to_lowercase())
            .is_some_and(|active| active == job_id)
        {
            state.active_by_tool.remove(&tool.id.to_lowercase());
        }
    }

    async fn provision_tool(&self, tool: &CatalogTool, force: bool) -> ProvisionOutcome {
        let configured = load_local_tools(&self.local_tools_path);
        let detection = detect_tool(tool, &configured).await;
        if detection.available && !force {
            return ProvisionOutcome {
                status: ToolInstallJobStatus::AlreadyPresent,
                message: "tool is already present; use force=true to reinstall".to_string(),
                executable_path: detection.executable_path,
            };
        }
        let Some(recipe) = &tool.install else {
            return ProvisionOutcome::terminal(
                ToolInstallJobStatus::Skipped,
                "catalog tool has no install recipe",
            );
        };
        if recipe.method == ToolInstallMethod::Manual {
            return ProvisionOutcome::terminal(
                ToolInstallJobStatus::Manual,
                recipe
                    .note
                    .clone()
                    .unwrap_or_else(|| format!("Install manually from {}", tool.upstream_url)),
            );
        }
        let platform_key = current_platform_key();
        if let Some(error) = install_recipe_validation_error(tool, &platform_key) {
            return ProvisionOutcome::failed(format!("install recipe rejected: {error}"));
        }
        if recipe.method == ToolInstallMethod::GithubRelease {
            // 进程内下载 + SHA-256 校验 + 安全解压（见 provision_github_release）。
            let root = match ensure_directory(&self.tools_dir, &self.tools_dir) {
                Ok(root) => root,
                Err(error) => return ProvisionOutcome::failed(error),
            };
            return self.provision_github_release(tool, recipe, &root).await;
        }
        self.provision_package(tool, recipe, force).await
    }

    async fn provision_package(
        &self,
        tool: &CatalogTool,
        recipe: &ToolInstall,
        force: bool,
    ) -> ProvisionOutcome {
        let root = match ensure_directory(&self.tools_dir, &self.tools_dir) {
            Ok(root) => root,
            Err(error) => return ProvisionOutcome::failed(error),
        };
        let command = match recipe.method {
            ToolInstallMethod::Go => self.build_go_command(tool, recipe, &root),
            ToolInstallMethod::Pip => self.build_pip_command(tool, recipe, &root).await,
            ToolInstallMethod::Cargo => self.build_cargo_command(tool, recipe, &root, force),
            ToolInstallMethod::GithubRelease | ToolInstallMethod::Manual => {
                return ProvisionOutcome::failed("unsupported package install method");
            }
        };
        let (command, expected) = match command {
            Ok(value) => value,
            // A missing toolchain is an environment state, not an install
            // failure: Python's provisioner maps _ToolchainMissing to SKIPPED.
            Err(BuildCommandError::ToolchainMissing(message)) => {
                return ProvisionOutcome::terminal(ToolInstallJobStatus::Skipped, message);
            }
            Err(BuildCommandError::Fatal(error)) => return ProvisionOutcome::failed(error),
        };
        let result = self.runner.run(&command).await;
        if !result.success {
            return ProvisionOutcome::failed(command_failure_message(&result));
        }
        let executable = match validate_installed_executable(&root, &expected) {
            Ok(executable) => executable,
            Err(error) => return ProvisionOutcome::failed(error),
        };
        let digest = match file_sha256(&executable) {
            Ok(digest) => digest,
            Err(error) => return ProvisionOutcome::failed(error),
        };
        let version = recipe.version.as_deref().unwrap_or_default();
        let integrity = if recipe.method == ToolInstallMethod::Go {
            "sumdb_verified"
        } else {
            "package_index_pinned"
        };
        if let Err(error) = persist_install_record(
            tool,
            &executable,
            version,
            &digest,
            integrity,
            &self.local_tools_path,
        ) {
            return ProvisionOutcome::failed(format!(
                "tool installed but registration failed; refusing installed status: {error}"
            ));
        }
        ProvisionOutcome {
            status: ToolInstallJobStatus::Installed,
            message: format!("installed to {}", expected.display()),
            executable_path: Some(executable.to_string_lossy().into_owned()),
        }
    }

    /// 进程内安装 GitHub release 资产：查 release → 下载 → 校验 SHA-256 →
    /// 安全解压可执行文件 → 落注册记录。
    ///
    /// 与 pip/go/cargo 共用同一收口（`validate_installed_executable` +
    /// `persist_install_record`），失败一律 fail-closed。解压绝不外调 shell，
    /// 只取出目标可执行文件的字节写入固定目标，杜绝路径遍历 / 符号链接逃逸。
    async fn provision_github_release(
        &self,
        tool: &CatalogTool,
        recipe: &ToolInstall,
        root: &Path,
    ) -> ProvisionOutcome {
        let platform_key = current_platform_key();
        let repository = match recipe.repository.as_deref() {
            Some(value) if !value.is_empty() => value.to_string(),
            _ => return ProvisionOutcome::failed("github_release recipe has no repository"),
        };
        let release_tag = match recipe.release_tag.as_deref() {
            Some(value) if !value.is_empty() => value.to_string(),
            _ => return ProvisionOutcome::failed("github_release recipe has no release_tag"),
        };
        let asset_pattern = match recipe.asset_patterns.get(&platform_key) {
            Some(value) if !value.is_empty() => value.clone(),
            _ => {
                return ProvisionOutcome::failed(format!(
                    "no release asset pattern for platform {platform_key}"
                ));
            }
        };
        let expected_sha = match recipe.sha256_by_platform.get(&platform_key) {
            Some(value) if !value.is_empty() => value.clone(),
            _ => {
                return ProvisionOutcome::failed(format!(
                    "no pinned SHA-256 for platform {platform_key}"
                ));
            }
        };
        let asset_regex = match Regex::new(&asset_pattern) {
            Ok(regex) => regex,
            Err(error) => return ProvisionOutcome::failed(format!("invalid asset pattern: {error}")),
        };
        let exe_pattern = recipe
            .archive_executable_patterns
            .get(&platform_key)
            .and_then(|pattern| Regex::new(pattern).ok());

        // 1) 查 release，按当前平台正则挑资产（拿到真实资产名与下载 URL）。
        let (asset_name, download_url) = match self
            .release_fetcher
            .resolve(&repository, &release_tag, &asset_regex)
            .await
        {
            Ok(value) => value,
            Err(error) => return ProvisionOutcome::failed(error),
        };

        // 2) 下载并校验 SHA-256（recipe 固定的是资产整体摘要，先验后解）。
        let bytes = match self.release_fetcher.download(&download_url).await {
            Ok(bytes) => bytes,
            Err(error) => return ProvisionOutcome::failed(error),
        };
        let digest = format!("{:x}", Sha256::digest(&bytes));
        if !digest.eq_ignore_ascii_case(&expected_sha) {
            return ProvisionOutcome::failed(format!(
                "SHA-256 mismatch for {asset_name}: expected {expected_sha}, got {digest}"
            ));
        }

        // 3) 进程内安全解压可执行文件到 tools/bin/<name>。
        let bin = match ensure_directory(&root.join("bin"), root) {
            Ok(bin) => bin,
            Err(error) => return ProvisionOutcome::failed(error),
        };
        let expected_name = match platform_binary_name(tool) {
            Ok(name) => name,
            Err(error) => return ProvisionOutcome::failed(error),
        };
        let dest = bin.join(&expected_name);
        if let Err(error) =
            extract_release_executable(&bytes, &asset_name, &expected_name, exe_pattern.as_ref(), &dest)
        {
            return ProvisionOutcome::failed(error);
        }
        // 额外 Allow-list 归档成员（如 EHole 运行必需的 finger.json），与可执行
        // 文件同目录解出；缺失即 fail-closed，避免报 Installed 却缺文件。
        if !recipe.include_archive_files.is_empty()
            && let Err(error) =
                extract_release_includes(&bytes, &asset_name, &recipe.include_archive_files, &bin)
        {
            return ProvisionOutcome::failed(error);
        }

        // 4) 校验 + 落注册记录（与其它安装方法同一收口）。
        let executable = match validate_installed_executable(root, &dest) {
            Ok(executable) => executable,
            Err(error) => return ProvisionOutcome::failed(error),
        };
        let installed_sha = match file_sha256(&executable) {
            Ok(digest) => digest,
            Err(error) => return ProvisionOutcome::failed(error),
        };
        let version = recipe.version.as_deref().unwrap_or(&release_tag);
        if let Err(error) = persist_install_record(
            tool,
            &executable,
            version,
            &installed_sha,
            "github_release_sha256_verified",
            &self.local_tools_path,
        ) {
            return ProvisionOutcome::failed(format!(
                "tool installed but registration failed; refusing installed status: {error}"
            ));
        }
        ProvisionOutcome {
            status: ToolInstallJobStatus::Installed,
            message: format!("installed {} to {}", asset_name, executable.display()),
            executable_path: Some(executable.to_string_lossy().into_owned()),
        }
    }

    fn build_go_command(
        &self,
        tool: &CatalogTool,
        recipe: &ToolInstall,
        root: &Path,
    ) -> Result<(ProvisionCommand, PathBuf), BuildCommandError> {
        let package = recipe.package.as_deref().ok_or_else(|| {
            BuildCommandError::Fatal("validated Go recipe has no package".to_string())
        })?;
        let go = self.resolve_toolchain("go").ok_or_else(|| {
            BuildCommandError::ToolchainMissing("go toolchain not found on PATH".to_string())
        })?;
        let bin = ensure_directory(&root.join("bin"), root).map_err(BuildCommandError::Fatal)?;
        let go_mod_cache =
            ensure_directory(&root.join("go/pkg/mod"), root).map_err(BuildCommandError::Fatal)?;
        let go_cache =
            ensure_directory(&root.join("go/cache"), root).map_err(BuildCommandError::Fatal)?;
        let expected = bin.join(platform_binary_name(tool).map_err(BuildCommandError::Fatal)?);
        let mut env = vec![
            (OsString::from("GOBIN"), bin.into_os_string()),
            (OsString::from("GOMODCACHE"), go_mod_cache.into_os_string()),
            (OsString::from("GOCACHE"), go_cache.into_os_string()),
            (OsString::from("GO111MODULE"), OsString::from("on")),
            (
                OsString::from("GOPROXY"),
                OsString::from("https://proxy.golang.org,direct"),
            ),
            (OsString::from("GOSUMDB"), OsString::from("sum.golang.org")),
        ];
        if recipe.requires_cgo {
            let compiler = self
                .resolve_toolchain("gcc")
                .or_else(|| self.resolve_toolchain("clang"));
            let compiler = compiler.ok_or_else(|| {
                BuildCommandError::ToolchainMissing(
                    "C compiler required by this pinned Go source; install GCC/Clang ".to_string()
                        + "or bundled LLVM-MinGW under tools/toolchains",
                )
            })?;
            env.push((OsString::from("CGO_ENABLED"), OsString::from("1")));
            env.push((OsString::from("CC"), compiler.clone().into_os_string()));
            let mut path = std::env::var_os("PATH").unwrap_or_default();
            if let Some(parent) = compiler.parent() {
                let mut combined = parent.as_os_str().to_os_string();
                combined.push(if cfg!(windows) { ";" } else { ":" });
                combined.push(path);
                path = combined;
            }
            env.push((OsString::from("PATH"), path));
        }
        Ok((
            ProvisionCommand {
                executable: go,
                args: vec![OsString::from("install"), OsString::from(package)],
                env,
                cwd: None,
            },
            expected,
        ))
    }

    async fn build_pip_command(
        &self,
        tool: &CatalogTool,
        recipe: &ToolInstall,
        root: &Path,
    ) -> Result<(ProvisionCommand, PathBuf), BuildCommandError> {
        let package = recipe.package.as_deref().ok_or_else(|| {
            BuildCommandError::Fatal("validated pip recipe has no package".to_string())
        })?;
        let venv = root.join("pyenv");
        let scripts = venv.join(if cfg!(windows) { "Scripts" } else { "bin" });
        let python = scripts.join(if cfg!(windows) {
            "python.exe"
        } else {
            "python"
        });
        if !is_usable_executable(&python) {
            let (host_python, prefix) = self.resolve_python().ok_or_else(|| {
                BuildCommandError::ToolchainMissing(
                    "Python interpreter not found on PATH".to_string(),
                )
            })?;
            let mut args = prefix;
            args.extend([
                OsString::from("-m"),
                OsString::from("venv"),
                venv.clone().into_os_string(),
            ]);
            let output = self
                .runner
                .run(&ProvisionCommand {
                    executable: host_python,
                    args,
                    env: Vec::new(),
                    cwd: None,
                })
                .await;
            if !output.success {
                return Err(BuildCommandError::Fatal(format!(
                    "Python venv creation failed: {}",
                    command_failure_message(&output)
                )));
            }
        }
        if !is_usable_executable(&python) {
            return Err(BuildCommandError::Fatal(format!(
                "Python venv creation reported success but {} is missing or not executable",
                python.display()
            )));
        }
        let scripts = ensure_directory(&scripts, root).map_err(BuildCommandError::Fatal)?;
        let python =
            validate_installed_executable(root, &python).map_err(BuildCommandError::Fatal)?;
        let expected = scripts.join(platform_binary_name(tool).map_err(BuildCommandError::Fatal)?);
        Ok((
            ProvisionCommand {
                executable: python,
                args: vec![
                    OsString::from("-m"),
                    OsString::from("pip"),
                    OsString::from("install"),
                    OsString::from("--upgrade"),
                    OsString::from(package),
                ],
                env: Vec::new(),
                cwd: None,
            },
            expected,
        ))
    }

    fn build_cargo_command(
        &self,
        tool: &CatalogTool,
        recipe: &ToolInstall,
        root: &Path,
        force: bool,
    ) -> Result<(ProvisionCommand, PathBuf), BuildCommandError> {
        let package = recipe.package.as_deref().ok_or_else(|| {
            BuildCommandError::Fatal("validated Cargo recipe has no package".to_string())
        })?;
        let version = recipe.version.as_deref().ok_or_else(|| {
            BuildCommandError::Fatal("validated Cargo recipe has no version".to_string())
        })?;
        let cargo = self.resolve_toolchain("cargo").ok_or_else(|| {
            BuildCommandError::ToolchainMissing("cargo toolchain not found on PATH".to_string())
        })?;
        let bin = ensure_directory(&root.join("bin"), root).map_err(BuildCommandError::Fatal)?;
        let expected = bin.join(platform_binary_name(tool).map_err(BuildCommandError::Fatal)?);
        let mut args = vec![
            OsString::from("install"),
            OsString::from("--root"),
            root.as_os_str().to_os_string(),
        ];
        if force {
            args.push(OsString::from("--force"));
        }
        args.extend([
            OsString::from("--version"),
            OsString::from(version),
            OsString::from(package),
        ]);
        Ok((
            ProvisionCommand {
                executable: cargo,
                args,
                env: Vec::new(),
                cwd: None,
            },
            expected,
        ))
    }

    fn resolve_toolchain(&self, name: &str) -> Option<PathBuf> {
        (self.toolchain_resolver)(name)
    }

    fn resolve_python(&self) -> Option<(PathBuf, Vec<OsString>)> {
        ["python3", "python", "py"].into_iter().find_map(|name| {
            self.resolve_toolchain(name).map(|path| {
                let prefix = if name == "py" {
                    vec![OsString::from("-3")]
                } else {
                    Vec::new()
                };
                (path, prefix)
            })
        })
    }
}

fn absolute_once(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir().map_or(path.clone(), |current| current.join(path))
    }
}

/// Strip the Windows verbatim prefix that `Path::canonicalize` attaches, so
/// paths recorded in local-tools.json and install jobs use the plain form
/// Python's `Path.resolve()` produces.
fn normalize_recorded_path(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.as_os_str().to_string_lossy();
        if let Some(stripped) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{stripped}"));
        }
        if let Some(stripped) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(stripped);
        }
    }
    path
}

fn ensure_directory(path: &Path, root: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(root)
        .map_err(|error| format!("could not create tools root {}: {error}", root.display()))?;
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("could not resolve tools root {}: {error}", root.display()))?;
    std::fs::create_dir_all(path).map_err(|error| {
        format!(
            "could not create install directory {}: {error}",
            path.display()
        )
    })?;
    let canonical = path.canonicalize().map_err(|error| {
        format!(
            "could not resolve install directory {}: {error}",
            path.display()
        )
    })?;
    if !canonical.starts_with(&canonical_root) {
        return Err(format!(
            "install directory {} escapes configured tools root {}",
            canonical.display(),
            canonical_root.display()
        ));
    }
    Ok(normalize_recorded_path(canonical))
}

fn platform_binary_name(tool: &CatalogTool) -> Result<String, String> {
    let candidate = tool
        .executable_names
        .first()
        .map_or(tool.id.as_str(), String::as_str)
        .trim();
    let base = candidate
        .strip_suffix(".exe")
        .or_else(|| candidate.strip_suffix(".EXE"))
        .unwrap_or(candidate);
    let path = Path::new(base);
    if base.is_empty()
        || path.components().count() != 1
        || matches!(base, "." | "..")
        || base.contains(['/', '\\'])
    {
        return Err(format!("unsafe executable name in catalog: {candidate:?}"));
    }
    Ok(if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    })
}

fn validate_installed_executable(root: &Path, expected: &Path) -> Result<PathBuf, String> {
    if !is_usable_executable(expected) {
        return Err(format!(
            "install reported success but {} is missing or not executable",
            expected.display()
        ));
    }
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("could not resolve tools root: {error}"))?;
    let canonical = expected
        .canonicalize()
        .map_err(|error| format!("could not resolve installed executable: {error}"))?;
    if !canonical.starts_with(canonical_root) {
        return Err(format!(
            "installed executable {} escapes configured tools root",
            canonical.display()
        ));
    }
    Ok(normalize_recorded_path(canonical))
}

fn command_failure_message(output: &ProvisionCommandOutput) -> String {
    // Selection and tail-line extraction mirror the Python provisioner:
    // stderr wins only when it is non-empty (raw truthiness), the detail is
    // stripped and collapsed to its last line, and an empty detail falls back
    // to the exit code. The tail is bounded to keep one runaway toolchain
    // error from inflating the install-job wire response.
    let raw_detail = if output.stderr.is_empty() {
        output.stdout.as_str()
    } else {
        output.stderr.as_str()
    };
    let detail = raw_detail.trim();
    let bounded = detail.lines().next_back().map_or_else(
        || format!("exit code {}", output.exit_code.unwrap_or_default()),
        |line| {
            line.chars()
                .take(MAX_INSTALL_MESSAGE_CHARS)
                .collect::<String>()
        },
    );
    format!("install command failed: {bounded}")
}

fn file_sha256(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|error| {
        format!(
            "could not hash installed executable {}: {error}",
            path.display()
        )
    })?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// GitHub release 抓取抽象：解析资产 + 下载字节。默认走 HTTP（见
/// [`HttpReleaseFetcher`）；测试可注入假实现，免真实网络。
#[async_trait]
trait ReleaseFetcher: Send + Sync {
    /// 按当前平台正则返回 `(资产名, 下载 URL)`。
    async fn resolve(
        &self,
        repository: &str,
        release_tag: &str,
        asset_regex: &Regex,
    ) -> Result<(String, String), String>;
    /// 下载资产字节。
    async fn download(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// 默认实现：经 reqwest 打 GitHub API / 下载重定向。
struct HttpReleaseFetcher;

#[async_trait]
impl ReleaseFetcher for HttpReleaseFetcher {
    async fn resolve(
        &self,
        repository: &str,
        release_tag: &str,
        asset_regex: &Regex,
    ) -> Result<(String, String), String> {
        let client = build_release_client()?;
        resolve_release_asset(&client, repository, release_tag, asset_regex).await
    }

    async fn download(&self, url: &str) -> Result<Vec<u8>, String> {
        let client = build_release_client()?;
        download_release_asset(&client, url).await
    }
}

/// 构建 GitHub release 客户端：rustls + UA（GitHub API 强制要求）+ 可选
/// `GITHUB_TOKEN`/`GH_TOKEN` 提升未认证 60/h 的限额；token 含非法字符则忽略。
fn build_release_client() -> Result<reqwest::Client, String> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("lynceus-tool-installer"));
    if let Some(token) = std::env::var("GITHUB_TOKEN")
        .ok()
        .or_else(|| std::env::var("GH_TOKEN").ok())
        .filter(|value| !value.is_empty())
    {
        if let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}")) {
            headers.insert(AUTHORIZATION, value);
        }
    }
    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|error| format!("could not build release HTTP client: {error}"))
}

/// 查 GitHub release，按当前平台正则返回 `(资产名, 下载 URL)`。
async fn resolve_release_asset(
    client: &reqwest::Client,
    repository: &str,
    release_tag: &str,
    asset_regex: &Regex,
) -> Result<(String, String), String> {
    let api = format!("https://api.github.com/repos/{repository}/releases/tags/{release_tag}");
    let response = client
        .get(&api)
        .send()
        .await
        .map_err(|error| format!("GitHub API request failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "GitHub API returned {} for {repository}@{release_tag}",
            response.status()
        ));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|error| format!("GitHub API response was not JSON: {error}"))?;
    let assets = body
        .get("assets")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("GitHub release {repository}@{release_tag} has no assets"))?;
    for asset in assets {
        let name = asset.get("name").and_then(Value::as_str).unwrap_or_default();
        let url = asset
            .get("browser_download_url")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if asset_regex.is_match(name) {
            if url.is_empty() {
                return Err(format!("release asset {name} has no download URL"));
            }
            return Ok((name.to_string(), url.to_string()));
        }
    }
    Err(format!(
        "no asset of {repository}@{release_tag} matches /{}/",
        asset_regex.as_str()
    ))
}

/// 下载 release 资产字节（reqwest 自动跟随 GitHub 的下载重定向）。
async fn download_release_asset(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| format!("download failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("download returned {}", response.status()));
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|error| format!("could not read download body: {error}"))
}

/// 归档成员名是否安全：非空、非绝对、无盘符、无 `.`/`..`/空段。
fn is_safe_archive_member(name: &str) -> bool {
    let normalized = name.replace('\\', "/");
    if normalized.is_empty() || normalized.starts_with('/') {
        return false;
    }
    // Windows 盘符（如 `C:/...`）。
    if normalized.as_bytes().get(1) == Some(&b':') {
        return false;
    }
    !normalized
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
}

/// 成员 basename（最后一个路径段）。
fn archive_member_basename(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// 该归档成员是否是目标可执行文件：非符号链接 + 路径安全 + basename 命中
/// 期望名，或整路径命中 `archive_executable_patterns` 正则。
fn member_matches_executable(
    member_name: &str,
    expected_name: &str,
    exe_pattern: Option<&Regex>,
    is_symlink: bool,
) -> bool {
    if is_symlink || !is_safe_archive_member(member_name) {
        return false;
    }
    match exe_pattern {
        Some(pattern) => pattern.is_match(member_name),
        None => archive_member_basename(member_name).eq_ignore_ascii_case(expected_name),
    }
}

/// 按资产扩展名分派到 zip / tar.gz 安全解压。
fn extract_release_executable(
    bytes: &[u8],
    asset_name: &str,
    expected_name: &str,
    exe_pattern: Option<&Regex>,
    dest: &Path,
) -> Result<(), String> {
    let lower = asset_name.to_ascii_lowercase();
    if lower.ends_with(".zip") {
        extract_zip_executable(bytes, expected_name, exe_pattern, dest)
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        extract_targz_executable(bytes, expected_name, exe_pattern, dest)
    } else {
        Err(format!("unsupported release archive format: {asset_name}"))
    }
}

/// 从 zip 资产中安全解出可执行文件字节写入 dest（拒符号链接 / 遍历 / 绝对路径）。
fn extract_zip_executable(
    bytes: &[u8],
    expected_name: &str,
    exe_pattern: Option<&Regex>,
    dest: &Path,
) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|error| format!("could not open zip archive: {error}"))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("could not read zip entry {index}: {error}"))?;
        let name = entry.name().to_string();
        // unix_mode 高 4 位是文件类型；0o120000 = 符号链接。
        let is_symlink = (entry.unix_mode().unwrap_or(0) & 0o170000) == 0o120000;
        if !member_matches_executable(&name, expected_name, exe_pattern, is_symlink) {
            continue;
        }
        let buffer = read_archive_member(&mut entry, &name)?;
        return write_executable(dest, &buffer);
    }
    Err(format!("executable {expected_name} not found in zip archive"))
}

/// 从 tar.gz 资产中安全解出可执行文件字节写入 dest（只接受普通/连续文件）。
fn extract_targz_executable(
    bytes: &[u8],
    expected_name: &str,
    exe_pattern: Option<&Regex>,
    dest: &Path,
) -> Result<(), String> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| format!("could not open tar archive: {error}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|error| format!("could not read tar entry: {error}"))?;
        // 只接受普通/连续文件；拒绝符号链接、硬链接、目录、设备等。
        if !matches!(
            entry.header().entry_type(),
            tar::EntryType::Regular | tar::EntryType::Continuous
        ) {
            continue;
        }
        let name = entry
            .header()
            .path()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !member_matches_executable(&name, expected_name, exe_pattern, false) {
            continue;
        }
        let buffer = read_archive_member(&mut entry, &name)?;
        return write_executable(dest, &buffer);
    }
    Err(format!("executable {expected_name} not found in tar archive"))
}

/// 归档成员解压上限：zip-bomb / 伪造 size 字段的纵深防御（pin 的 SHA-256 已
/// 保证归档整体可信；这里防止单个成员自报的 size 被伪造导致内存暴涨）。
const MAX_ARCHIVE_MEMBER_BYTES: u64 = 512 * 1024 * 1024;

/// 有界读取一个归档成员的字节（不信任条目自报 size，不用它预分配）。
fn read_archive_member<R: Read>(mut reader: R, name: &str) -> Result<Vec<u8>, String> {
    let mut buffer = Vec::new();
    reader
        .take(MAX_ARCHIVE_MEMBER_BYTES)
        .read_to_end(&mut buffer)
        .map_err(|error| format!("could not extract {name}: {error}"))?;
    Ok(buffer)
}

/// 解压额外 Allow-list 归档成员（如 EHole 运行必需的 `finger.json`）到 dest_dir。
/// 与可执行文件同套安全约束：只取普通文件、拒符号链接 / 遍历 / 绝对路径，按
/// basename 匹配；全部缺失则报错（fail-closed，避免静默不完整安装）。
fn extract_release_includes(
    bytes: &[u8],
    asset_name: &str,
    includes: &[String],
    dest_dir: &Path,
) -> Result<(), String> {
    let lower = asset_name.to_ascii_lowercase();
    if lower.ends_with(".zip") {
        extract_zip_includes(bytes, includes, dest_dir)
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        extract_targz_includes(bytes, includes, dest_dir)
    } else {
        Err(format!("unsupported release archive format: {asset_name}"))
    }
}

fn extract_zip_includes(bytes: &[u8], includes: &[String], dest_dir: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|error| format!("could not open zip archive: {error}"))?;
    let mut remaining: Vec<&str> = includes.iter().map(String::as_str).collect();
    for index in 0..archive.len() {
        if remaining.is_empty() {
            break;
        }
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("could not read zip entry {index}: {error}"))?;
        let name = entry.name().to_string();
        let is_symlink = (entry.unix_mode().unwrap_or(0) & 0o170000) == 0o120000;
        if is_symlink || !is_safe_archive_member(&name) {
            continue;
        }
        let base = archive_member_basename(&name);
        if let Some(pos) = remaining.iter().position(|want| base.eq_ignore_ascii_case(want)) {
            let buffer = read_archive_member(&mut entry, &name)?;
            let dest = dest_dir.join(base);
            std::fs::write(&dest, &buffer)
                .map_err(|error| format!("could not write {}: {error}", dest.display()))?;
            remaining.remove(pos);
        }
    }
    finish_includes(remaining)
}

fn extract_targz_includes(bytes: &[u8], includes: &[String], dest_dir: &Path) -> Result<(), String> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| format!("could not open tar archive: {error}"))?;
    let mut remaining: Vec<&str> = includes.iter().map(String::as_str).collect();
    for entry in entries {
        if remaining.is_empty() {
            break;
        }
        let mut entry = entry.map_err(|error| format!("could not read tar entry: {error}"))?;
        if !matches!(
            entry.header().entry_type(),
            tar::EntryType::Regular | tar::EntryType::Continuous
        ) {
            continue;
        }
        let name = entry
            .header()
            .path()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !is_safe_archive_member(&name) {
            continue;
        }
        let base = archive_member_basename(&name);
        if let Some(pos) = remaining.iter().position(|want| base.eq_ignore_ascii_case(want)) {
            let buffer = read_archive_member(&mut entry, &name)?;
            let dest = dest_dir.join(base);
            std::fs::write(&dest, &buffer)
                .map_err(|error| format!("could not write {}: {error}", dest.display()))?;
            remaining.remove(pos);
        }
    }
    finish_includes(remaining)
}

fn finish_includes(remaining: Vec<&str>) -> Result<(), String> {
    if remaining.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "archive is missing required file(s): {}",
            remaining.join(", ")
        ))
    }
}

/// 把可执行字节落到固定目标并补执行位（Unix）。
fn write_executable(dest: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(dest, bytes)
        .map_err(|error| format!("could not write {}: {error}", dest.display()))?;
    set_executable_bit(dest);
    Ok(())
}

#[cfg(unix)]
fn set_executable_bit(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(metadata) = std::fs::metadata(path) {
        let mut permissions = metadata.permissions();
        permissions.set_mode(permissions.mode() | 0o111);
        let _ = std::fs::set_permissions(path, permissions);
    }
}

#[cfg(not(unix))]
fn set_executable_bit(_path: &Path) {}

fn persist_install_record(
    tool: &CatalogTool,
    executable: &Path,
    version: &str,
    sha256: &str,
    integrity_status: &str,
    path: &Path,
) -> Result<(), ToolCatalogError> {
    configure_local_tool(
        &tool.id,
        Some(&executable.to_string_lossy()),
        tool.install
            .as_ref()
            .is_none_or(|recipe| recipe.enabled_by_default),
        path,
    )?;
    let mut payload = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({"local_tools": []}));
    let items = payload
        .as_object_mut()
        .and_then(|object| object.get_mut("local_tools"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| ToolCatalogError::InvalidCatalog("local_tools must be an array".into()))?;
    let Some(entry) = items
        .iter_mut()
        .filter_map(Value::as_object_mut)
        .find(|entry| {
            entry
                .get("tool_name")
                .and_then(Value::as_str)
                .is_some_and(|name| name.eq_ignore_ascii_case(&tool.id))
        })
    else {
        return Err(ToolCatalogError::InvalidCatalog(format!(
            "installed tool {:?} was not registered",
            tool.id
        )));
    };
    entry.insert("name".into(), Value::String(tool.name.clone()));
    entry.insert("domain".into(), Value::String(tool.domain.clone()));
    entry.insert("version".into(), Value::String(version.to_string()));
    entry.insert("sha256".into(), Value::String(sha256.to_string()));
    entry.insert(
        "source_url".into(),
        Value::String(tool.upstream_url.clone()),
    );
    entry.insert(
        "integrity_status".into(),
        Value::String(integrity_status.to_string()),
    );
    entry.insert("installed_at".into(), json!(utcnow()));
    if !tool.version_args.is_empty() {
        entry.insert("version_args".into(), json!(tool.version_args));
    }
    let text = serde_json::to_string_pretty(&payload)?;
    std::fs::write(path, format!("{text}\n")).map_err(|source| ToolCatalogError::ConfigIo {
        path: path.to_path_buf(),
        source,
    })
}

fn install_recipe_validation_error(tool: &CatalogTool, platform_key: &str) -> Option<String> {
    let recipe = tool.install.as_ref()?;
    if recipe.method == ToolInstallMethod::Manual {
        return None;
    }
    let repository = github_repository_from_url(&tool.upstream_url)?;
    match recipe.method {
        ToolInstallMethod::GithubRelease => {
            if recipe
                .repository
                .as_deref()
                .is_none_or(|value| !value.eq_ignore_ascii_case(&repository))
            {
                return Some("release repository must exactly match upstream_url".to_string());
            }
            if recipe.release_tag.as_deref().is_none_or(str::is_empty) {
                return Some("release_tag is required".to_string());
            }
            let digest = recipe
                .sha256_by_platform
                .get(platform_key)
                .map_or("", String::as_str);
            if !is_lower_or_upper_hex(digest, 64) {
                return Some(format!("a pinned SHA-256 is required for {platform_key}"));
            }
            if recipe
                .asset_patterns
                .get(platform_key)
                .is_none_or(String::is_empty)
            {
                return Some(format!("an asset pattern is required for {platform_key}"));
            }
            None
        }
        ToolInstallMethod::Go => {
            let package = recipe.package.as_deref().unwrap_or_default();
            let Some((module, pin)) = package.rsplit_once('@') else {
                return Some(
                    "Go module must come from the repository declared by upstream_url".to_string(),
                );
            };
            let prefix = format!("github.com/{repository}/");
            if !module.to_lowercase().starts_with(&prefix.to_lowercase()) {
                return Some(
                    "Go module must come from the repository declared by upstream_url".to_string(),
                );
            }
            if recipe.version.as_deref() != Some(pin) {
                return Some("Go module pin must exactly match the declared version".to_string());
            }
            if !(is_semver_pin(pin) || is_lower_or_upper_hex(pin, 40)) {
                return Some(
                    "Go module must be pinned to a semantic version or full commit SHA".to_string(),
                );
            }
            None
        }
        ToolInstallMethod::Pip => {
            let package = recipe.package.as_deref().unwrap_or_default();
            let Some((name, pin)) = package.split_once("==") else {
                return Some("pip package must use an exact == version pin".to_string());
            };
            if name.is_empty() || pin.is_empty() {
                return Some("pip package must use an exact == version pin".to_string());
            }
            if package.matches("==").count() != 1
                || !safe_package_component(name)
                || !safe_version_component(pin)
                || recipe.version.as_deref() != Some(pin)
            {
                return Some("pip package pin must exactly match the declared version".to_string());
            }
            None
        }
        ToolInstallMethod::Cargo => {
            let package = recipe.package.as_deref().unwrap_or_default();
            let version = recipe.version.as_deref().unwrap_or_default();
            if !safe_package_component(package) || !safe_version_component(version) {
                return Some("Cargo package and exact version are required".to_string());
            }
            None
        }
        ToolInstallMethod::Manual => None,
    }
}

fn github_repository_from_url(value: &str) -> Option<String> {
    let parsed = url::Url::parse(value).ok()?;
    if parsed.scheme() != "https" || parsed.host_str()? != "github.com" {
        return None;
    }
    let parts = parsed
        .path_segments()?
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.len() != 2 {
        return None;
    }
    let repository = parts[1].strip_suffix(".git").unwrap_or(parts[1]);
    if !safe_package_component(parts[0]) || !safe_package_component(repository) {
        return None;
    }
    Some(format!("{}/{}", parts[0], repository))
}

fn current_platform_key() -> String {
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let architecture = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "amd64"
    };
    format!("{os}_{architecture}")
}

fn safe_package_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
        && value.starts_with(char::is_alphanumeric)
}

fn safe_version_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ".+-_".contains(character))
        && value.starts_with(char::is_alphanumeric)
}

fn is_semver_pin(value: &str) -> bool {
    let value = value.strip_prefix('v').unwrap_or(value);
    let core = value.split(['-', '+']).next().unwrap_or_default();
    let parts = core.split('.').collect::<Vec<_>>();
    parts.len() == 3
        && parts.iter().all(|part| {
            !part.is_empty() && part.chars().all(|character| character.is_ascii_digit())
        })
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ".+-".contains(character))
}

fn is_lower_or_upper_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;

    use super::*;
    use models::{MissionId, ProjectId};

    /// Injectable command script mirroring the Python tests' `CommandRunner`.
    /// The closure returns a future so mocks can await (never block the
    /// single-threaded test runtime) while other jobs queue behind the lock.
    type Script = Arc<
        dyn Fn(&ProvisionCommand) -> Pin<Box<dyn Future<Output = ProvisionCommandOutput> + Send>>
            + Send
            + Sync,
    >;

    #[derive(Clone)]
    struct RecordingRunner {
        script: Script,
        calls: Arc<StdMutex<Vec<Vec<String>>>>,
    }

    impl RecordingRunner {
        fn new(
            script: impl Fn(
                &ProvisionCommand,
            ) -> Pin<Box<dyn Future<Output = ProvisionCommandOutput> + Send>>
            + Send
            + Sync
            + 'static,
        ) -> Self {
            Self {
                script: Arc::new(script),
                calls: Arc::new(StdMutex::new(Vec::new())),
            }
        }

        fn argv_log(&self) -> Vec<Vec<String>> {
            self.calls
                .lock()
                .ok()
                .map(|log| log.clone())
                .unwrap_or_default()
        }
    }

    #[async_trait]
    impl ProvisionCommandRunner for RecordingRunner {
        async fn run(&self, command: &ProvisionCommand) -> ProvisionCommandOutput {
            let mut argv = vec![command.executable.to_string_lossy().into_owned()];
            argv.extend(
                command
                    .args
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned()),
            );
            if let Ok(mut log) = self.calls.lock() {
                log.push(argv);
            }
            (self.script)(command).await
        }
    }

    fn write_executable(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mock parent must create");
        }
        std::fs::write(path, contents).expect("mock file must write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
                .expect("mock chmod must succeed");
        }
    }

    fn canonical_plain(path: &Path) -> PathBuf {
        let canonical = path
            .canonicalize()
            .unwrap_or_else(|error| panic!("canonicalize {path:?}: {error}"));
        normalize_recorded_path(canonical)
    }

    /// Runner whose single command always succeeds without touching disk.
    fn ok_runner() -> RecordingRunner {
        RecordingRunner::new(|_| {
            Box::pin(async {
                ProvisionCommandOutput {
                    success: true,
                    exit_code: Some(0),
                    stdout: String::new(),
                    stderr: String::new(),
                }
            })
        })
    }

    fn coordinator_with(
        directory: &Path,
        runner: RecordingRunner,
        toolchains: &[&str],
    ) -> Arc<ToolInstallCoordinator> {
        let names = toolchains
            .iter()
            .map(|name| (*name).to_string())
            .collect::<HashSet<_>>();
        Arc::new(
            ToolInstallCoordinator::with_test_runtime(
                directory.join("config").join("local-tools.json"),
                directory.join("tools"),
                Arc::new(runner),
                Arc::new(move |name| {
                    names
                        .contains(name)
                        .then(|| PathBuf::from(format!("/usr/bin/{name}")))
                }),
            )
            .expect("coordinator must initialize"),
        )
    }

    async fn wait_terminal(coordinator: &ToolInstallCoordinator, job_id: &str) -> ToolInstallJob {
        for _ in 0..500 {
            if let Some(job) = coordinator.get(job_id).await
                && job.status.is_terminal()
            {
                return job;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        panic!("install job {job_id} never reached a terminal state");
    }

    fn configured_tool(catalog: &[CatalogTool], id: &str) -> CatalogTool {
        catalog
            .iter()
            .find(|tool| tool.id == id)
            .unwrap_or_else(|| panic!("catalog must contain {id}"))
            .clone()
    }

    #[test]
    fn embedded_catalog_matches_expected_shape() {
        let tools = load_catalog().expect("catalog must load");
        let ids = tools
            .iter()
            .map(|tool| tool.id.as_str())
            .collect::<HashSet<_>>();
        for required in [
            "ffuf",
            "feroxbuster",
            "gobuster",
            "dnsx",
            "gau",
            "wappalyzergo",
            "EHole",
            "afrog",
        ] {
            assert!(ids.contains(required), "missing catalog tool {required}");
        }
        assert!(tools.iter().all(|tool| !tool.id.trim().is_empty()
            && !tool.domain.trim().is_empty()
            && !tool.executable_names.is_empty()));

        // test_catalog_declares_expected_install_recipes
        let nuclei = configured_tool(&tools, "nuclei");
        let nuclei_install = nuclei.install.clone().expect("nuclei must have a recipe");
        assert_eq!(nuclei_install.method, ToolInstallMethod::GithubRelease);
        assert_eq!(
            nuclei_install.repository.as_deref(),
            Some("projectdiscovery/nuclei")
        );
        assert_eq!(nuclei_install.release_tag.as_deref(), Some("v3.11.1"));
        assert_eq!(
            nuclei_install
                .sha256_by_platform
                .get("windows_amd64")
                .map(String::len),
            Some(64)
        );
        assert_eq!(
            configured_tool(&tools, "semgrep")
                .install
                .as_ref()
                .and_then(|install| install.package.as_deref()),
            Some("semgrep==1.172.0")
        );
        assert_eq!(
            configured_tool(&tools, "feroxbuster")
                .install
                .map(|install| install.method),
            Some(ToolInstallMethod::GithubRelease)
        );
        assert_eq!(
            configured_tool(&tools, "wappalyzergo")
                .install
                .map(|install| install.method),
            Some(ToolInstallMethod::Manual)
        );
        assert!(tools.iter().all(|tool| {
            tool.install.as_ref().is_none_or(|install| {
                !install
                    .package
                    .as_deref()
                    .unwrap_or_default()
                    .contains("@latest")
            })
        }));
        assert!(tools.iter().all(|tool| {
            tool.install.as_ref().is_none_or(|install| {
                install.method == ToolInstallMethod::Manual || !tool.version_args.is_empty()
            })
        }));
    }

    #[test]
    fn recipe_validation_rejects_repository_mismatch_and_mutable_pins() {
        let tools = load_catalog().expect("catalog must load");
        let platform = current_platform_key();

        // test_release_recipe_rejects_repository_mismatch
        let mut nuclei = configured_tool(&tools, "nuclei");
        if let Some(install) = nuclei.install.as_mut() {
            install.repository = Some("lookalike-project/nuclei".to_string());
        }
        assert_eq!(
            install_recipe_validation_error(&nuclei, &platform),
            Some("release repository must exactly match upstream_url".to_string())
        );

        // test_go_recipe_rejects_mutable_latest_pin
        let mut jsluice = configured_tool(&tools, "jsluice");
        if let Some(install) = jsluice.install.as_mut() {
            install.package = Some("github.com/BishopFox/jsluice/cmd/jsluice@latest".to_string());
            install.version = Some("latest".to_string());
        }
        assert_eq!(
            install_recipe_validation_error(&jsluice, &platform),
            Some("Go module must be pinned to a semantic version or full commit SHA".to_string())
        );

        // Version-pin mismatch keeps its own Python message.
        if let Some(install) = jsluice.install.as_mut() {
            install.package = Some("github.com/BishopFox/jsluice/cmd/jsluice@v9.9.9".to_string());
        }
        assert_eq!(
            install_recipe_validation_error(&jsluice, &platform),
            Some("Go module pin must exactly match the declared version".to_string())
        );

        // Pip recipes without an exact pin are rejected with the Python message.
        let mut semgrep = configured_tool(&tools, "semgrep");
        if let Some(install) = semgrep.install.as_mut() {
            install.package = Some("semgrep".to_string());
            install.version = Some("1.172.0".to_string());
        }
        assert_eq!(
            install_recipe_validation_error(&semgrep, &platform),
            Some("pip package must use an exact == version pin".to_string())
        );
    }

    #[tokio::test]
    async fn configured_tool_is_detected_without_executing_it() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("local-tools.json");
        configure_local_tool("ffuf", Some(r"C:\Tools\ffuf.exe"), true, &path)
            .expect("configuration must persist");
        let entries = detect_tool_catalog(&path).await.expect("detection");
        let ffuf = entries
            .iter()
            .find(|entry| entry.id == "ffuf")
            .expect("ffuf must exist");
        assert_eq!(ffuf.detection.availability, ToolAvailability::Configured);
        assert_eq!(
            ffuf.detection.executable_path.as_deref(),
            Some(r"C:\Tools\ffuf.exe")
        );
    }

    #[tokio::test]
    async fn cached_detection_tracks_local_tools_changes() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("local-tools.json");
        configure_local_tool("ffuf", Some(r"C:\Tools\ffuf.exe"), true, &path)
            .expect("configuration must persist");

        // 首次探测与缓存命中结果一致。
        let first = detect_tool_catalog_cached(&path).await.expect("first");
        let second = detect_tool_catalog_cached(&path).await.expect("cached");
        let ffuf = |entries: &[ToolCatalogEntry]| {
            entries
                .iter()
                .find(|entry| entry.id == "ffuf")
                .expect("ffuf must exist")
                .detection
                .executable_path
                .clone()
        };
        assert_eq!(ffuf(&first), Some(r"C:\Tools\ffuf.exe".to_string()));
        assert_eq!(ffuf(&first), ffuf(&second));

        // 配置文件变化后（mtime 失效），缓存不会返回陈旧结果。
        std::thread::sleep(std::time::Duration::from_millis(20));
        configure_local_tool("ffuf", Some(r"C:\Tools\ffuf2.exe"), true, &path)
            .expect("reconfiguration must persist");
        let third = detect_tool_catalog_cached(&path).await.expect("refreshed");
        assert_eq!(ffuf(&third), Some(r"C:\Tools\ffuf2.exe".to_string()));
    }

    /// The Python HTTPX client CLI shares the command name but never prints
    /// `-silent`; `ProjectDiscovery` httpx prints both flags (Python detector
    /// test `test_detector_rejects_unrelated_python_httpx_cli`).
    #[test]
    fn httpx_identity_probe_rejects_unrelated_cli() {
        let python_httpx = "Usage: httpx [OPTIONS] URL\n  --json  --no-verify  --verbose";
        assert!(!identity_output_matches(python_httpx, ""));
        let projectdiscovery = "-silent\n-json\n-v, --verbose";
        assert!(identity_output_matches(projectdiscovery, ""));
    }

    /// 分支类别与事实才是推荐的真实信号源（目标分类已删，不再有按
    /// url/source/binary 预置的工具基线分）。
    #[tokio::test]
    async fn url_branch_kind_recommends_content_discovery_tools() {
        let entries = detect_tool_catalog(Path::new("missing-local-tools.json"))
            .await
            .expect("detection");
        let mission = Mission::new(ProjectId::new("proj_1".to_string()), "audit".to_string());
        let branch_metadata = Map::from_iter([(
            "branch_kind".to_string(),
            Value::String("url.surface_mapping".to_string()),
        )]);
        let facts = [Fact::new(
            ProjectId::new("proj_1".to_string()),
            "asset.technology".to_string(),
            "nginx 1.24".to_string(),
        )];
        let recommendations = recommend_tools(
            Some(&mission),
            &branch_metadata,
            &facts,
            &[],
            &[],
            &entries,
        );
        let ids = recommendations
            .iter()
            .map(|item| item.tool_id.as_str())
            .collect::<Vec<_>>();
        for expected in ["ffuf", "wappalyzergo", "afrog"] {
            assert!(ids.contains(&expected), "missing recommendation {expected}");
        }
        let ffuf = ids.iter().position(|id| *id == "ffuf").expect("ffuf");
        let afrog = ids.iter().position(|id| *id == "afrog").expect("afrog");
        assert!(ffuf < afrog, "url 分支下内容发现优先于 PoC 验证");
    }

    #[tokio::test]
    async fn go_install_writes_bin_and_local_tools_then_is_idempotent() {
        let directory = tempfile::tempdir().expect("tempdir");
        let expected_dir = directory.path().join("tools").join("bin");
        let runner = RecordingRunner::new(move |command: &ProvisionCommand| {
            let gobin = command
                .env
                .iter()
                .find(|(key, _)| key == "GOBIN")
                .map(|(_, value)| value.clone())
                .unwrap_or_default();
            Box::pin(async move {
                let target =
                    PathBuf::from(&gobin).join(format!("jsluice{}", std::env::consts::EXE_SUFFIX));
                write_executable(&target, b"binary");
                ProvisionCommandOutput {
                    success: true,
                    exit_code: Some(0),
                    stdout: String::new(),
                    stderr: String::new(),
                }
            })
        });
        let coordinator =
            coordinator_with(directory.path(), runner.clone(), &["go", "gcc", "cargo"]);
        let config_path = directory.path().join("config").join("local-tools.json");

        let started = coordinator.start("jsluice", false).await.expect("queue");
        let installed = wait_terminal(&coordinator, &started.id).await;
        assert_eq!(installed.status, ToolInstallJobStatus::Installed);
        assert!(installed.started_at.is_some());
        assert!(installed.finished_at.is_some());
        let expected =
            canonical_plain(&expected_dir.join(format!("jsluice{}", std::env::consts::EXE_SUFFIX)));
        assert_eq!(
            installed.executable_path.as_deref(),
            Some(expected.to_string_lossy().as_ref())
        );

        let log = runner.argv_log();
        assert!(!log.is_empty());
        assert_eq!(log[0][1], "install");

        let payload = std::fs::read_to_string(&config_path).expect("config must exist");
        let value: Value = serde_json::from_str(&payload).expect("config must parse");
        let entry = value["local_tools"]
            .as_array()
            .expect("local_tools array")
            .iter()
            .find(|item| item["tool_name"] == "jsluice")
            .expect("jsluice entry")
            .clone();

        let snapshot = load_detection_snapshot(&detection_snapshot_path_for(&config_path))
            .expect("successful install updates detection snapshot");
        assert!(
            snapshot
                .detections
                .iter()
                .any(|record| record.tool_id == "jsluice" && record.detection.available)
        );
        assert_eq!(entry["executable_path"], json!(expected.to_string_lossy()));
        assert_eq!(entry["version_args"], json!(["-h"]));
        assert_eq!(
            entry["sha256"],
            json!(format!("{:x}", Sha256::digest(b"binary")))
        );

        // Second install without force detects the configured binary.
        let again = coordinator.start("jsluice", false).await.expect("queue");
        let present = wait_terminal(&coordinator, &again.id).await;
        assert_eq!(present.status, ToolInstallJobStatus::AlreadyPresent);
        assert_eq!(runner.argv_log().len(), log.len());
    }

    #[tokio::test]
    async fn pip_install_creates_venv_then_installs() {
        let directory = tempfile::tempdir().expect("tempdir");
        let tools_dir = directory.path().join("tools");
        let runner = RecordingRunner::new(|command: &ProvisionCommand| {
            let args = command
                .args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            let venv_requested = args.iter().any(|arg| arg == "venv");
            let venv_dir = args.last().cloned().unwrap_or_default();
            let executable_parent = command
                .executable
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default();
            Box::pin(async move {
                if venv_requested {
                    let scripts = PathBuf::from(&venv_dir).join(if cfg!(windows) {
                        "Scripts"
                    } else {
                        "bin"
                    });
                    let python = scripts.join(if cfg!(windows) {
                        "python.exe"
                    } else {
                        "python"
                    });
                    write_executable(&python, b"python");
                } else if args.iter().any(|arg| arg == "pip") {
                    let target =
                        executable_parent.join(format!("semgrep{}", std::env::consts::EXE_SUFFIX));
                    write_executable(&target, b"semgrep");
                }
                ProvisionCommandOutput {
                    success: true,
                    exit_code: Some(0),
                    stdout: String::new(),
                    stderr: String::new(),
                }
            })
        });
        let coordinator = coordinator_with(directory.path(), runner, &["python3", "go", "gcc"]);
        let started = coordinator.start("semgrep", false).await.expect("queue");
        let installed = wait_terminal(&coordinator, &started.id).await;
        assert_eq!(installed.status, ToolInstallJobStatus::Installed);
        let expected = canonical_plain(
            &tools_dir
                .join("pyenv")
                .join(if cfg!(windows) { "Scripts" } else { "bin" })
                .join(format!("semgrep{}", std::env::consts::EXE_SUFFIX)),
        );
        assert_eq!(
            installed.executable_path.as_deref(),
            Some(expected.to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    async fn cargo_build_command_uses_root_and_exact_version() {
        let directory = tempfile::tempdir().expect("tempdir");
        let tools = load_catalog().expect("catalog must load");
        let mut feroxbuster = configured_tool(&tools, "feroxbuster");
        feroxbuster.install = Some(ToolInstall {
            method: ToolInstallMethod::Cargo,
            package: Some("feroxbuster".to_string()),
            version: Some("2.13.1".to_string()),
            repository: None,
            release_tag: None,
            asset_patterns: IndexMap::new(),
            sha256_by_platform: IndexMap::new(),
             archive_executable_patterns: IndexMap::new(),
             include_archive_files: Vec::new(),
             enabled_by_default: true,
            requires_cgo: false,
            note: None,
        });
        let coordinator = coordinator_with(directory.path(), ok_runner(), &["cargo"]);
        let root = ensure_directory(
            &directory.path().join("tools"),
            &directory.path().join("tools"),
        )
        .expect("tools root");
        let (command, expected) = coordinator
            .build_cargo_command(
                &feroxbuster,
                feroxbuster.install.as_ref().expect("recipe"),
                &root,
                true,
            )
            .expect("command must build");
        let argv = command
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(command.executable, PathBuf::from("/usr/bin/cargo"));
        assert_eq!(argv[0], "install");
        let root_position = argv
            .iter()
            .position(|arg| arg == "--root")
            .expect("--root flag");
        assert_eq!(argv[root_position + 1], root.to_string_lossy());
        assert!(argv.contains(&"--force".to_string()));
        let version_position = argv
            .iter()
            .position(|arg| arg == "--version")
            .expect("--version flag");
        assert_eq!(argv[version_position + 1], "2.13.1");
        assert_eq!(argv[argv.len() - 1], "feroxbuster");
        assert_eq!(
            expected,
            root.join("bin")
                .join(format!("feroxbuster{}", std::env::consts::EXE_SUFFIX))
        );
    }

    #[tokio::test]
    async fn missing_toolchain_is_skipped_without_writing() {
        let directory = tempfile::tempdir().expect("tempdir");
        let coordinator = coordinator_with(directory.path(), ok_runner(), &[]);
        let started = coordinator.start("jsluice", false).await.expect("queue");
        let skipped = wait_terminal(&coordinator, &started.id).await;
        assert_eq!(skipped.status, ToolInstallJobStatus::Skipped);
        assert!(skipped.message.contains("go toolchain"));
        assert!(
            !directory
                .path()
                .join("config")
                .join("local-tools.json")
                .exists()
        );
    }

    #[tokio::test]
    async fn manual_recipe_reports_instructions_without_running_commands() {
        let directory = tempfile::tempdir().expect("tempdir");
        let coordinator = coordinator_with(directory.path(), ok_runner(), &["go", "gcc"]);
        let started = coordinator.start("wappalyzergo", false).await.expect("queue");
        let manual = wait_terminal(&coordinator, &started.id).await;
        assert_eq!(manual.status, ToolInstallJobStatus::Manual);
        assert!(manual.message.contains("github.com/projectdiscovery/wappalyzergo"));
        assert!(
            !directory
                .path()
                .join("config")
                .join("local-tools.json")
                .exists()
        );
    }

    #[tokio::test]
    async fn failed_install_when_command_errors() {
        let directory = tempfile::tempdir().expect("tempdir");
        let runner = RecordingRunner::new(|_| {
            Box::pin(async {
                ProvisionCommandOutput {
                    success: false,
                    exit_code: Some(1),
                    stdout: String::new(),
                    stderr: "go: module lookup disabled".to_string(),
                }
            })
        });
        let coordinator = coordinator_with(directory.path(), runner, &["go", "gcc"]);
        let started = coordinator.start("jsluice", false).await.expect("queue");
        let failed = wait_terminal(&coordinator, &started.id).await;
        assert_eq!(failed.status, ToolInstallJobStatus::Failed);
        assert!(failed.message.contains("module lookup disabled"));
        assert!(failed.message.starts_with("install command failed:"));
        assert!(
            !directory
                .path()
                .join("config")
                .join("local-tools.json")
                .exists()
        );
    }

    #[tokio::test]
    async fn failed_install_when_binary_missing() {
        let directory = tempfile::tempdir().expect("tempdir");
        let coordinator = coordinator_with(directory.path(), ok_runner(), &["go", "gcc"]);
        let started = coordinator.start("jsluice", false).await.expect("queue");
        let failed = wait_terminal(&coordinator, &started.id).await;
        assert_eq!(failed.status, ToolInstallJobStatus::Failed);
        assert!(failed.message.contains("missing"));
    }

    struct FailingFetcher;

    #[async_trait]
    impl ReleaseFetcher for FailingFetcher {
        async fn resolve(
            &self,
            _repository: &str,
            _release_tag: &str,
            _asset_regex: &Regex,
        ) -> Result<(String, String), String> {
            Err("simulated release lookup failure".to_string())
        }

        async fn download(&self, _url: &str) -> Result<Vec<u8>, String> {
            Err("simulated download failure".to_string())
        }
    }

    #[tokio::test]
    async fn github_release_attempts_download_and_fails_closed() {
        let directory = tempfile::tempdir().expect("tempdir");
        let coordinator = Arc::new(
            ToolInstallCoordinator::with_test_runtime_and_fetcher(
                directory.path().join("config").join("local-tools.json"),
                directory.path().join("tools"),
                Arc::new(ok_runner()),
                Arc::new(|_| None),
                Arc::new(FailingFetcher),
            )
            .expect("coordinator must initialize"),
        );
        let started = coordinator.start("nuclei", false).await.expect("queue");
        let job = wait_terminal(&coordinator, &started.id).await;
        // 不再是旧的 Unsupported 桩：recipe 校验通过后进入真实下载路径，
        // 抓取失败即 fail-closed 为 Failed（而非假装支持）。
        assert_eq!(job.status, ToolInstallJobStatus::Failed);
        assert!(
            job.message.contains("simulated"),
            "unexpected message: {}",
            job.message
        );
    }

    #[tokio::test]
    async fn unknown_tool_and_finished_jobs_behave_like_python() {
        let directory = tempfile::tempdir().expect("tempdir");
        let coordinator = coordinator_with(directory.path(), ok_runner(), &[]);
        let error = coordinator
            .start("not-a-tool", false)
            .await
            .expect_err("unknown tool must fail");
        assert!(matches!(error, ToolCatalogError::NotFound(_)));

        // A manual job is terminal after evaluation; listing by tool id is
        // case-insensitive like the Python coordinator.
        let first = coordinator.start("wappalyzergo", false).await.expect("queue");
        let job = wait_terminal(&coordinator, &first.id).await;
        assert_eq!(job.status, ToolInstallJobStatus::Manual);
        let listed = coordinator.list(Some("WappalyzerGo")).await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, job.id);
    }

    /// The install lock serializes recipes; a second request for the same
    /// tool while one job is active must observe the active job instead of
    /// queueing a duplicate (Python `ToolInstallCoordinator.start`). The mock
    /// awaits instead of blocking, so the single-threaded runtime interleaves
    /// the two `start` calls deterministically.
    #[tokio::test]
    async fn active_install_jobs_are_deduplicated() {
        let directory = tempfile::tempdir().expect("tempdir");
        let runner = RecordingRunner::new(|command: &ProvisionCommand| {
            let gobin = command
                .env
                .iter()
                .find(|(key, _)| key == "GOBIN")
                .map(|(_, value)| value.clone())
                .unwrap_or_default();
            Box::pin(async move {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                let target =
                    PathBuf::from(&gobin).join(format!("jsluice{}", std::env::consts::EXE_SUFFIX));
                write_executable(&target, b"binary");
                ProvisionCommandOutput {
                    success: true,
                    exit_code: Some(0),
                    stdout: String::new(),
                    stderr: String::new(),
                }
            })
        });
        let coordinator = coordinator_with(directory.path(), runner, &["go", "gcc"]);
        let first = coordinator.start("jsluice", false).await.expect("queue");
        let second = coordinator.start("jsluice", false).await.expect("queue");
        assert_eq!(
            first.id, second.id,
            "active duplicate must return the running job"
        );
        let finished = wait_terminal(&coordinator, &first.id).await;
        assert_eq!(finished.status, ToolInstallJobStatus::Installed);
    }

    /// configure → 落盘 → detect 回显：`configured_settings` 携带 params 与
    /// env key 名单，env 明文绝不出现在任何序列化输出。
    #[tokio::test]
    async fn detection_echoes_configured_settings_without_env_values() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("local-tools.json");
        configure_local_tool("semgrep", Some(r"C:\Tools\semgrep.exe"), true, &path)
            .expect("configuration must persist");
        let semgrep = configured_tool(&load_catalog().expect("catalog must load"), "semgrep");
        let params = serde_json::from_value(json!({"config": "auto", "timeout_seconds": 60}))
            .expect("params shape");
        let env = BTreeMap::from([(
            "SEMGREP_APP_TOKEN".to_string(),
            Some("secret-token-value".to_string()),
        )]);
        crate::tool_settings::apply_tool_settings(&semgrep, Some(&params), Some(&env), &path)
            .expect("settings must apply");

        let entries = detect_tool_catalog(&path).await.expect("detection");
        let entry = entries
            .iter()
            .find(|entry| entry.id == "semgrep")
            .expect("semgrep entry");
        assert!(entry.invocation.is_some());
        let view = entry
            .configured_settings
            .as_ref()
            .expect("configured settings echo");
        assert_eq!(view.env_set, ["SEMGREP_APP_TOKEN"]);
        assert_eq!(view.params.get("config"), Some(&json!("auto")));
        let serialized = serde_json::to_string(&entry).expect("entry must serialize");
        assert!(
            !serialized.contains("secret-token-value"),
            "env plaintext leaked into catalog entry echo: {serialized}"
        );

        // 无 settings 的工具不携带该字段。
        let gau = entries.iter().find(|entry| entry.id == "gau").expect("gau");
        assert!(gau.configured_settings.is_none());
    }

    // ------------------------------------------------------------------
    // 探测快照（PART 4/7：list 不探测、refresh 探测、事件失效）
    // ------------------------------------------------------------------

    fn write_local_tools(dir: &tempfile::TempDir, tools: &[(&str, &str)]) -> std::path::PathBuf {
        let path = dir.path().join("local-tools.json");
        let configured: Vec<Value> = tools
            .iter()
            .map(|(id, exe)| json!({"tool_name": id, "executable_path": exe}))
            .collect();
        std::fs::write(&path, json!({"local_tools": configured}).to_string())
            .expect("write local tools");
        path
    }

    #[test]
    fn snapshot_view_uses_live_configured_detection_without_any_probe() {
        // list 合并视图：configured 条目实时准确（configure/install 失
        // 效事件天然成立），无快照的条目如实 Missing——全程不探测。
        let dir = tempfile::tempdir().expect("tempdir");
        let local_tools = write_local_tools(&dir, &[("semgrep", "C:/fake/semgrep.exe")]);
        let entries = catalog_entries_from_snapshot(&local_tools, None).expect("entries");
        let semgrep = entries
            .iter()
            .find(|entry| entry.id == "semgrep")
            .expect("semgrep entry");
        assert!(semgrep.detection.available);
        assert_eq!(semgrep.detection.availability, ToolAvailability::Configured);
        assert_eq!(
            semgrep.detection.executable_path.as_deref(),
            Some("C:/fake/semgrep.exe")
        );
        // 未配置且无快照的工具：Missing（绝不因 list 触发 PATH 探测）。
        let nuclei = entries
            .iter()
            .find(|entry| entry.id == "nuclei")
            .expect("nuclei entry");
        assert!(!nuclei.detection.available);
        assert_eq!(nuclei.detection.availability, ToolAvailability::Unknown);
    }

    #[test]
    fn snapshot_view_prefers_snapshot_for_unconfigured_tools() {
        let dir = tempfile::tempdir().expect("tempdir");
        let local_tools = dir.path().join("local-tools.json");
        let snapshot = ToolDetectionSnapshot {
            detected_at: "2026-09-01T00:00:00Z".to_string(),
            complete: true,
            detections: vec![ToolDetectionRecord {
                tool_id: "nuclei".to_string(),
                detection: ToolDetection {
                    available: true,
                    availability: ToolAvailability::Path,
                    executable_path: Some("/usr/local/bin/nuclei".to_string()),
                    source: Some("PATH".to_string()),
                    version: None,
                    sha256: None,
                    source_url: None,
                    integrity_status: "unverified".to_string(),
                    integrity_message: None,
                },
            }],
        };
        let entries =
            catalog_entries_from_snapshot(&local_tools, Some(&snapshot)).expect("entries");
        let nuclei = entries
            .iter()
            .find(|entry| entry.id == "nuclei")
            .expect("nuclei entry");
        assert!(nuclei.detection.available);
        assert_eq!(nuclei.detection.availability, ToolAvailability::Path);
    }

    #[tokio::test]
    async fn refresh_writes_snapshot_and_updates_memory_cache() {
        let dir = tempfile::tempdir().expect("tempdir");
        let local_tools = write_local_tools(&dir, &[("semgrep", "C:/fake/semgrep.exe")]);
        let snapshot_path = dir.path().join("tool-detection.json");

        let snapshot = refresh_detection_snapshot(&local_tools, &snapshot_path)
            .await
            .expect("refresh");
        assert!(!snapshot.detected_at.is_empty());
        assert!(
            snapshot.detections.len() >= 17,
            "refresh covers the whole catalog: {}",
            snapshot.detections.len()
        );
        // 快照文件可读回且一致。
        let reloaded = load_detection_snapshot(&snapshot_path).expect("snapshot file");
        assert_eq!(reloaded.detections.len(), snapshot.detections.len());
        // 内存缓存同步：cached 版本不再重新探测（直接命中）。
        let cached = detect_tool_catalog_cached(&local_tools)
            .await
            .expect("cached entries");
        assert_eq!(cached.len(), snapshot.detections.len());
    }

    #[tokio::test]
    async fn single_tool_redetect_scopes_to_one_tool_and_merges_snapshot() {
        let dir = tempfile::tempdir().expect("tempdir");
        let local_tools = write_local_tools(&dir, &[("semgrep", "C:/fake/semgrep.exe")]);
        let snapshot_path = dir.path().join("tool-detection.json");

        // configure 事件失效：只重测 semgrep 并写入快照，其余工具等
        // 下次全量 refresh（快照允许只含部分条目）。
        let entry = redetect_tool_into_snapshot("semgrep", &local_tools, &snapshot_path)
            .await
            .expect("redetect")
            .expect("semgrep exists");
        assert_eq!(entry.detection.availability, ToolAvailability::Configured);
        let snapshot = load_detection_snapshot(&snapshot_path).expect("snapshot");
        assert_eq!(snapshot.detections.len(), 1);
        assert_eq!(snapshot.detections[0].tool_id, "semgrep");

        // 未知工具：None，且不落盘。
        let unknown = detect_single_tool("not-a-tool", &local_tools)
            .await
            .expect("detect");
        assert!(unknown.is_none());
    }

    #[test]
    fn corrupted_snapshot_is_tolerated_as_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let snapshot_path = dir.path().join("tool-detection.json");
        std::fs::write(&snapshot_path, "{not json").expect("write garbage");
        assert!(load_detection_snapshot(&snapshot_path).is_none());
        let local_tools = dir.path().join("local-tools.json");
        let entries = catalog_entries_from_snapshot(&local_tools, None).expect("entries");
        assert!(
            entries
                .iter()
                .all(|entry| entry.detection.availability != ToolAvailability::Path),
            "corrupt snapshot must degrade to no-PATH-detection view"
        );
    }

    // ---- GitHub release 进程内安全解压 ----

    #[test]
    fn archive_member_safety_rejects_traversal_and_absolute() {
        assert!(is_safe_archive_member("nuclei.exe"));
        assert!(is_safe_archive_member("pkg/nuclei.exe"));
        assert!(is_safe_archive_member("a/b/c/tool"));
        assert!(!is_safe_archive_member("../evil.exe"));
        assert!(!is_safe_archive_member("pkg/../../evil.exe"));
        assert!(!is_safe_archive_member("/etc/passwd"));
        assert!(!is_safe_archive_member("C:/windows/system32/x"));
        assert!(!is_safe_archive_member("./nuclei"));
        assert!(!is_safe_archive_member(""));
    }

    #[test]
    fn member_matches_executable_respects_symlink_and_basename() {
        // 符号链接一律拒绝（即便 basename 命中）。
        assert!(!member_matches_executable("nuclei.exe", "nuclei.exe", None, true));
        // basename 命中，含嵌套路径。
        assert!(member_matches_executable("dist/nuclei.exe", "nuclei.exe", None, false));
        assert!(!member_matches_executable("dist/other.exe", "nuclei.exe", None, false));
        // 遍历路径即使 basename 对也不选。
        assert!(!member_matches_executable("../nuclei.exe", "nuclei.exe", None, false));
        // archive_executable_patterns 正则命中整路径。
        let pattern = Regex::new(r"(^|/)dalfox$").expect("regex");
        assert!(member_matches_executable("bin/dalfox", "x", Some(&pattern), false));
        assert!(!member_matches_executable("bin/dalfox.exe", "x", Some(&pattern), false));
    }

    fn build_zip(entries: &[(&str, Option<u32>, &[u8])]) -> Vec<u8> {
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            for (name, mode, content) in entries {
                let mut options = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored);
                if let Some(mode) = mode {
                    options = options.unix_permissions(*mode);
                }
                writer.start_file(*name, options).expect("start zip entry");
                std::io::Write::write_all(&mut writer, content).expect("write zip entry");
            }
            writer.finish().expect("finish zip");
        }
        buffer
    }

    #[test]
    fn zip_extraction_writes_nested_executable_and_rejects_symlink() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("nuclei.exe");

        // 嵌套普通文件：按 basename 命中，字节落到固定 dest。
        let zip_bytes = build_zip(&[
            ("README.md", None, b"docs"),
            ("nuclei-v3/nuclei.exe", Some(0o644), b"NUCLEI-BINARY"),
        ]);
        extract_release_executable(&zip_bytes, "nuclei_windows_amd64.zip", "nuclei.exe", None, &dest)
            .expect("extract nested executable");
        assert_eq!(std::fs::read(&dest).expect("read"), b"NUCLEI-BINARY");

        // 无匹配项 → fail-closed 返回 not found。（符号链接拒绝由纯函数测试
        // `member_matches_executable` + tar 端到端测试覆盖；zip 的安全写 API
        // 无法 round-trip 符号链接类型位，故此处用「无匹配」验证 fail-closed。）
        let no_match = build_zip(&[("other.exe", None, b"x")]);
        let err = extract_release_executable(
            &no_match,
            "nuclei_windows_amd64.zip",
            "nuclei.exe",
            None,
            &dest,
        )
        .expect_err("no matching executable must fail closed");
        assert!(err.contains("not found"), "unexpected error: {err}");
    }

    #[test]
    fn zip_extraction_pulls_include_files_and_skips_others() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exe_dest = dir.path().join("ehole.exe");
        let zip_bytes = build_zip(&[
            ("README.md", None, b"docs"),
            ("EHole_windows_amd64.exe", Some(0o644), b"EHOLE-BIN"),
            ("finger.json", None, b"{\"finger\":true}"),
        ]);
        let pattern = Regex::new(r"^EHole_windows_amd64\.exe$").expect("regex");
        extract_release_executable(
            &zip_bytes,
            "EHole_windows_amd64.zip",
            "ehole.exe",
            Some(&pattern),
            &exe_dest,
        )
        .expect("extract executable");
        extract_release_includes(
            &zip_bytes,
            "EHole_windows_amd64.zip",
            &["finger.json".to_string()],
            dir.path(),
        )
        .expect("extract include");
        assert_eq!(std::fs::read(&exe_dest).expect("read exe"), b"EHOLE-BIN");
        assert_eq!(
            std::fs::read(dir.path().join("finger.json")).expect("read include"),
            b"{\"finger\":true}"
        );
        assert!(
            !dir.path().join("README.md").exists(),
            "non-include member must not be extracted"
        );
    }

    #[test]
    fn includes_missing_member_fails_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let zip_bytes = build_zip(&[("only.exe", None, b"x")]);
        let err = extract_release_includes(
            &zip_bytes,
            "tool_windows_amd64.zip",
            &["finger.json".to_string()],
            dir.path(),
        )
        .expect_err("missing include must fail closed");
        assert!(err.contains("finger.json"), "unexpected error: {err}");
    }

    fn build_tar_gz(regular: &[(&str, &[u8])], symlink: Option<(&str, &str)>) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(gz);
        for (name, content) in regular {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, name, &content[..])
                .expect("append regular");
        }
        if let Some((name, target)) = symlink {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_mode(0o777);
            header.set_size(0);
            header.set_cksum();
            builder
                .append_link(&mut header, name, target)
                .expect("append symlink");
        }
        gz_finish(builder)
    }

    fn gz_finish(builder: tar::Builder<flate2::write::GzEncoder<Vec<u8>>>) -> Vec<u8> {
        let encoder = builder.into_inner().expect("into inner");
        encoder.finish().expect("gz finish")
    }

    #[test]
    fn targz_extraction_writes_regular_and_rejects_symlink() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("dalfox");

        let good = build_tar_gz(&[("dalfox", b"DALFOX-BINARY")], None);
        extract_release_executable(&good, "dalfox_linux_amd64.tar.gz", "dalfox", None, &dest)
            .expect("extract regular executable");
        assert_eq!(std::fs::read(&dest).expect("read"), b"DALFOX-BINARY");

        // 只有符号链接版 dalfox：entry_type 非 Regular → 拒绝。
        let evil = build_tar_gz(&[], Some(("dalfox", "/etc/passwd")));
        let err = extract_release_executable(
            &evil,
            "dalfox_linux_amd64.tar.gz",
            "dalfox",
            None,
            &dest,
        )
        .expect_err("symlink must be rejected");
        assert!(err.contains("not found"), "unexpected error: {err}");
    }
}
