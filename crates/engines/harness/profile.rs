//! 领域画像：声明允许工具、目标绑定、风险级别与预算。
//!
//! `DomainProfile` 只声明策略（哪些工具可用、目标从哪里来、预算多大），
//! 执行已移交外部 Worker Runtime（`crate::worker`）；执行器代码已删除。
//! 【RETIREMENT CANDIDATE】迁移验证已完成：本表仅为 solver 注册表提供
//! 名/域/风险元数据；不要新增字段。最终形态 = 外部 Worker capability 声明。

use serde_json::{Map, Value};

use agents::solver::SolverContext;
use models::domain::AuditDomain;

/// 工具风险级别（进入模型 prompt 与审计记录；高阶预算收紧）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRisk {
    /// 只读探测类。
    Low,
    /// 常规扫描类（有请求压力）。
    Medium,
    /// 主动利用/侵入类。
    High,
}

impl ToolRisk {
    /// wire/prompt 展示名。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    /// 高风险工具单 run 最多成功调用 1 次。
    #[must_use]
    pub const fn max_calls(self) -> usize {
        match self {
            Self::Low | Self::Medium => 2,
            Self::High => 1,
        }
    }
}

/// 领域画像（静态声明，wire 值与审计域沿用现有契约）。
#[derive(Debug)]
pub struct DomainProfile {
    /// solver wire 名（与 Python 契约一致的注册键）。
    pub solver_name: &'static str,
    /// 声明覆盖的审计域。
    pub audit_domains: &'static [AuditDomain],
    /// 目标回退链：域配置缺 target 时按序取 `context.target` 的键。
    pub target_keys: &'static [&'static str],
    /// 无 provider 确定性路径的工具尝试序（可用性过滤后按序尝试）。
    pub default_tools: &'static [&'static str],
    /// 风险级别。
    pub risk: ToolRisk,
    /// 本域内建的 Rust 原生工具（不在 CLI catalog 中、无需本地检测，
    /// 与 Python 侧 `native_fingerprint` / `page_hints` / `web_exploit_campaign`
    /// 同类；模型 schema 由 adapter 的受信声明构造）。
    pub native_tools: &'static [NativeToolSpec],
    /// 本 profile 挂接的远程工具来源（`binary_analysis` 用 IDA MCP）。
    pub remote_source: RemoteToolSourceKind,
}

/// profile 内建的原生工具声明（无外部可执行文件，可用性与 adapter
/// 同生共死——注册了 adapter 即视为可用）。
#[derive(Debug)]
pub struct NativeToolSpec {
    /// 工具 ID（与 native adapter id 对齐）。
    pub id: &'static str,
    /// 模型可见标题。
    pub title: &'static str,
    /// 模型可见描述。
    pub description: &'static str,
    /// 模型可传的参数面（schema 守卫依据；原生执行没有 argv 概念）。
    pub params: &'static [NativeParamSpec],
}

/// 原生工具参数声明（映射为模型 schema 的
/// [`crate::tool_catalog::ToolParamSpec`]）。
#[derive(Debug)]
pub struct NativeParamSpec {
    /// 参数键。
    pub key: &'static str,
    /// 值类型。
    pub kind: crate::tool_catalog::ToolParamKind,
    /// 是否必填。
    pub required: bool,
}

/// `traffic_artifact_import`：读取并解析 HAR/Burp/Chrome 流量工件（无
/// 外部可执行文件；`artifact_path` 走 mission-first 分层，`format` 白名单）。
static TRAFFIC_ARTIFACT_IMPORT: [NativeToolSpec; 1] = [NativeToolSpec {
    id: "traffic_artifact_import",
    title: "Traffic Artifact Import",
    description: "Import HAR/Burp/Chrome network artifacts into normalized request records (Rust native).",
    params: &[
        NativeParamSpec {
            key: "artifact_path",
            kind: crate::tool_catalog::ToolParamKind::Path,
            required: true,
        },
        NativeParamSpec {
            key: "format",
            kind: crate::tool_catalog::ToolParamKind::String,
            required: false,
        },
    ],
}];

/// 远程工具来源声明：profile 除本地 CLI/native 工具外可挂接的远程面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteToolSourceKind {
    /// 无远程来源（全部工具来自 catalog/native）。
    None,
    /// IDA MCP（`binary_analysis` 专用；host/port 来自 mission config）。
    IdaMcp,
}

impl DomainProfile {
    /// 目标回退链解析（域配置 `target` 优先，然后 `context.target` 按序）。
    ///
    /// 兼容 Python `_target_fallback`：域配置形如
    /// `config[section][section].target` 或 `config[section].target`。
    #[must_use]
    pub fn resolve_target(&self, context: &SolverContext) -> Option<String> {
        let section = context
            .config
            .get(self.solver_name)
            .and_then(Value::as_object);
        if let Some(target) = section
            .and_then(|outer| outer.get(self.solver_name))
            .and_then(Value::as_object)
            .and_then(|inner| inner.get("target"))
            .and_then(Value::as_str)
            .filter(|target| !target.trim().is_empty())
            .or_else(|| {
                section
                    .and_then(|outer| outer.get("target"))
                    .and_then(Value::as_str)
                    .filter(|target| !target.trim().is_empty())
            })
        {
            return Some(target.to_string());
        }
        self.target_keys.iter().find_map(|key| {
            context
                .target
                .get(key)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
        })
    }

    /// 读取域配置段（不存在时为空对象语义）。
    #[must_use]
    pub fn config_section(&self, context: &SolverContext) -> Map<String, Value> {
        context
            .config
            .get(self.solver_name)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    }

    /// 全部工具不可用时的 fail-closed 提示（措辞逐域对齐 Python
    /// solver 的无配置返回；H4：`web_recon` / `asset_recon` /
    /// `fingerprint_intelligence` / `traffic_intelligence` /
    /// `web_validation` / `exploitability_validation` 与
    /// `content_discovery` 同为 "no concrete config provided"）。
    #[must_use]
    pub fn no_tool_note(&self) -> String {
        const CONFIG_NOTE_DOMAINS: [&str; 7] = [
            "asset_recon",
            "content_discovery",
            "exploitability_validation",
            "fingerprint_intelligence",
            "traffic_intelligence",
            "web_recon",
            "web_validation",
        ];
        if self.solver_name == "binary_analysis" {
            return format!(
                "{}: no IDA MCP endpoint configured (set {}.ida_host and {}.ida_port)",
                self.solver_name, self.solver_name, self.solver_name
            );
        }
        if CONFIG_NOTE_DOMAINS.contains(&self.solver_name) {
            return format!("{}: no concrete config provided", self.solver_name);
        }
        format!("{}: no concrete engine configured", self.solver_name)
    }
}

static ASSET_RECON: [AuditDomain; 1] = [AuditDomain::AssetRecon];
static WEB_RECON: [AuditDomain; 1] = [AuditDomain::WebRecon];
static CONTENT_DISCOVERY: [AuditDomain; 1] = [AuditDomain::ContentDiscovery];
static FINGERPRINT: [AuditDomain; 1] = [AuditDomain::FingerprintIntelligence];
static EXPOSURE: [AuditDomain; 1] = [AuditDomain::ExposureIntelligence];
static WEB_SAST: [AuditDomain; 1] = [AuditDomain::WebSast];
static WEB_DAST: [AuditDomain; 1] = [AuditDomain::WebDast];
static WEB_VALIDATION: [AuditDomain; 1] = [AuditDomain::WebValidation];
static EXPLOITABILITY: [AuditDomain; 1] = [AuditDomain::Exploitability];
static EXPLOITABILITY_VALIDATION: [AuditDomain; 1] = [AuditDomain::ExploitabilityValidation];
static INTERNAL_SURFACE: [AuditDomain; 1] = [AuditDomain::InternalSurface];
static TRAFFIC: [AuditDomain; 1] = [AuditDomain::TrafficIntelligence];
static CODE_DEEP_SAST: [AuditDomain; 1] = [AuditDomain::CodeDeepSast];
static BINARY_ANALYSIS: [AuditDomain; 3] = [
    AuditDomain::BinaryStatic,
    AuditDomain::BinaryDynamic,
    AuditDomain::Exploitability,
];

/// `web_exploit_campaign`：Rust 原生的一次有界同源利用活动（无外部
/// 可执行文件；参数面为空——目标只来自 profile 目标链）。
/// `native_fingerprint`：抓取一个有界页面并按受信 eHole 规则包匹配
/// （无外部可执行文件；packs 目录走 mission config 或
/// `LYNCEUS_FINGERPRINT_PACKS_DIR`）。
/// `web_recon_pipeline`：`katana`（可选 CLI）+ `page_hints`（原生）+ `jsluice`
/// （可选 CLI）的单次编排——与 Python solver 的执行顺序与合并归一化同构。
static WEB_RECON_PIPELINE: [NativeToolSpec; 1] = [NativeToolSpec {
    id: "web_recon_pipeline",
    title: "Web Recon Pipeline",
    description: "Katana crawl with native page hints and optional jsluice JS analysis (Rust native orchestration).",
    params: &[
        NativeParamSpec {
            key: "depth",
            kind: crate::tool_catalog::ToolParamKind::Integer,
            required: false,
        },
        NativeParamSpec {
            key: "js_crawl",
            kind: crate::tool_catalog::ToolParamKind::Boolean,
            required: false,
        },
        NativeParamSpec {
            key: "headless",
            kind: crate::tool_catalog::ToolParamKind::Boolean,
            required: false,
        },
        NativeParamSpec {
            key: "extract_secrets",
            kind: crate::tool_catalog::ToolParamKind::Boolean,
            required: false,
        },
        NativeParamSpec {
            key: "timeout_seconds",
            kind: crate::tool_catalog::ToolParamKind::Integer,
            required: false,
        },
    ],
}];

static NATIVE_FINGERPRINT: [NativeToolSpec; 1] = [NativeToolSpec {
    id: "native_fingerprint",
    title: "Native Fingerprint",
    description: "Bounded HTTP fingerprinting against a verified eHole rule pack (Rust native).",
    params: &[
        NativeParamSpec {
            key: "pack_id",
            kind: crate::tool_catalog::ToolParamKind::String,
            required: false,
        },
        NativeParamSpec {
            key: "timeout_seconds",
            kind: crate::tool_catalog::ToolParamKind::Integer,
            required: false,
        },
        NativeParamSpec {
            key: "max_body_bytes",
            kind: crate::tool_catalog::ToolParamKind::Integer,
            required: false,
        },
        NativeParamSpec {
            key: "verify_tls",
            kind: crate::tool_catalog::ToolParamKind::Boolean,
            required: false,
        },
    ],
}];

static WEB_EXPLOIT_CAMPAIGN: [NativeToolSpec; 1] = [NativeToolSpec {
    id: "web_exploit_campaign",
    title: "Web Exploit Campaign",
    description: "Bounded same-origin flag capture campaign (Rust native).",
    params: &[],
}];

/// 14 个已注册 solver 名的领域画像（wire 名与审计域与
/// `default_solver_registry` 完全一致）。
static PROFILES: [DomainProfile; 14] = [
    DomainProfile {
        solver_name: "asset_recon",
        audit_domains: &ASSET_RECON,
        target_keys: &["domain", "target", "url"],
        default_tools: &["subfinder", "httpx", "naabu"],
        risk: ToolRisk::Low,
        native_tools: &[],
        remote_source: RemoteToolSourceKind::None,
    },
    DomainProfile {
        solver_name: "web_recon",
        audit_domains: &WEB_RECON,
        target_keys: &["url", "target"],
        default_tools: &["web_recon_pipeline"],
        risk: ToolRisk::Low,
        native_tools: &WEB_RECON_PIPELINE,
        remote_source: RemoteToolSourceKind::None,
    },
    DomainProfile {
        solver_name: "content_discovery",
        audit_domains: &CONTENT_DISCOVERY,
        target_keys: &["url", "target"],
        default_tools: &["ffuf", "feroxbuster", "gobuster"],
        risk: ToolRisk::Medium,
        native_tools: &[],
        remote_source: RemoteToolSourceKind::None,
    },
    DomainProfile {
        solver_name: "fingerprint_intelligence",
        audit_domains: &FINGERPRINT,
        target_keys: &["url", "target"],
        default_tools: &["native_fingerprint", "wappalyzergo"],
        risk: ToolRisk::Low,
        native_tools: &NATIVE_FINGERPRINT,
        remote_source: RemoteToolSourceKind::None,
    },
    DomainProfile {
        solver_name: "exposure_intelligence",
        audit_domains: &EXPOSURE,
        target_keys: &["url", "target"],
        default_tools: &["afrog", "ehole"],
        risk: ToolRisk::Medium,
        native_tools: &[],
        remote_source: RemoteToolSourceKind::None,
    },
    DomainProfile {
        solver_name: "web_sast",
        audit_domains: &WEB_SAST,
        target_keys: &["repo_path", "repo"],
        default_tools: &["semgrep"],
        risk: ToolRisk::Low,
        native_tools: &[],
        remote_source: RemoteToolSourceKind::None,
    },
    DomainProfile {
        solver_name: "web_dast",
        audit_domains: &WEB_DAST,
        target_keys: &["url", "target"],
        default_tools: &["nuclei"],
        risk: ToolRisk::Medium,
        native_tools: &[],
        remote_source: RemoteToolSourceKind::None,
    },
    DomainProfile {
        solver_name: "web_validation",
        audit_domains: &WEB_VALIDATION,
        target_keys: &["url", "target"],
        default_tools: &["dalfox"],
        risk: ToolRisk::Medium,
        native_tools: &[],
        remote_source: RemoteToolSourceKind::None,
    },
    DomainProfile {
        solver_name: "web_exploit",
        audit_domains: &EXPLOITABILITY,
        target_keys: &["url", "target", "base_url"],
        default_tools: &["web_exploit_campaign"],
        risk: ToolRisk::High,
        native_tools: &WEB_EXPLOIT_CAMPAIGN,
        remote_source: RemoteToolSourceKind::None,
    },
    DomainProfile {
        solver_name: "exploitability_validation",
        audit_domains: &EXPLOITABILITY_VALIDATION,
        target_keys: &["url", "target"],
        default_tools: &["web_exploit_campaign"],
        risk: ToolRisk::High,
        native_tools: &WEB_EXPLOIT_CAMPAIGN,
        remote_source: RemoteToolSourceKind::None,
    },
    DomainProfile {
        solver_name: "internal_surface",
        audit_domains: &INTERNAL_SURFACE,
        target_keys: &["url", "target", "repo"],
        // 曾有 fscan 作为默认工具，但 fscan 许可含糊、捆绑 PoC，不随开源项目
        // 分发；internal_surface 现无内置默认工具，靠 worker 经 MCP 自行发现。
        default_tools: &[],
        risk: ToolRisk::Low,
        native_tools: &[],
        remote_source: RemoteToolSourceKind::None,
    },
    DomainProfile {
        solver_name: "traffic_intelligence",
        audit_domains: &TRAFFIC,
        target_keys: &["pcap", "target"],
        default_tools: &["traffic_artifact_import"],
        risk: ToolRisk::Low,
        native_tools: &TRAFFIC_ARTIFACT_IMPORT,
        remote_source: RemoteToolSourceKind::None,
    },
    DomainProfile {
        solver_name: "code_deep_sast",
        audit_domains: &CODE_DEEP_SAST,
        target_keys: &["repo_path", "repo"],
        default_tools: &["semgrep"],
        risk: ToolRisk::Low,
        native_tools: &[],
        remote_source: RemoteToolSourceKind::None,
    },
    DomainProfile {
        solver_name: "binary_analysis",
        audit_domains: &BINARY_ANALYSIS,
        target_keys: &["binary", "target"],
        default_tools: &[],
        risk: ToolRisk::Low,
        native_tools: &[],
        remote_source: RemoteToolSourceKind::IdaMcp,
    },
];

/// 按 solver wire 名取领域画像。
#[must_use]
pub fn profile_for(solver_name: &str) -> Option<&'static DomainProfile> {
    PROFILES
        .iter()
        .find(|profile| profile.solver_name == solver_name)
}

/// 全部领域画像，供 solver 注册和 Harness 路由共享。
#[must_use]
pub fn profiles() -> &'static [DomainProfile] {
    &PROFILES
}

/// 全部画像的 solver 名（升序，与 `SolverRegistry::names()` 同序）。
#[must_use]
pub fn profile_names() -> Vec<String> {
    let mut names: Vec<String> = PROFILES
        .iter()
        .map(|profile| profile.solver_name.to_string())
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    #![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

    use super::*;
    use models::ids::{ProjectId, RunId, TaskId};

    #[test]
    fn profiles_cover_all_registered_solvers() {
        let registry = crate::solvers::default_solver_registry();
        let registered = registry.names();
        assert_eq!(registered.len(), 14);
        for name in &registered {
            let profile = profile_for(name).unwrap_or_else(|| panic!("profile missing: {name}"));
            assert_eq!(profile.solver_name, name.as_str());
            assert!(!profile.no_tool_note().is_empty());
        }
        assert_eq!(profile_names(), registered);
        assert!(profile_for("cloud_native").is_none());
    }

    #[test]
    fn content_discovery_keeps_legacy_note_wording() {
        let profile = profile_for("content_discovery").expect("content profile");
        assert_eq!(
            profile.no_tool_note(),
            "content_discovery: no concrete config provided"
        );
        let other = profile_for("web_sast").expect("web_sast profile");
        assert_eq!(
            other.no_tool_note(),
            "web_sast: no concrete engine configured"
        );
    }

    #[test]
    fn target_resolution_prefers_domain_config_then_fallback_chain() {
        let profile = profile_for("web_sast").expect("web_sast profile");
        let mut context = SolverContext::new(
            ProjectId::new("proj_profile".to_string()),
            RunId::new("run_profile".to_string()),
            TaskId::new("task_profile".to_string()),
        );
        context
            .target
            .insert("repo".to_string(), "fallback-repo".to_string());
        assert_eq!(
            profile.resolve_target(&context).as_deref(),
            Some("fallback-repo")
        );
        context.config.insert(
            "web_sast".to_string(),
            serde_json::json!({"target": "domain-config-repo"}),
        );
        assert_eq!(
            profile.resolve_target(&context).as_deref(),
            Some("domain-config-repo")
        );
        context.config.insert(
            "web_sast".to_string(),
            serde_json::json!({"web_sast": {"target": "nested-domain-config-repo"}}),
        );
        assert_eq!(
            profile.resolve_target(&context).as_deref(),
            Some("nested-domain-config-repo")
        );
    }

    #[test]
    fn risk_levels_are_stable() {
        assert_eq!(ToolRisk::High.max_calls(), 1);
        assert_eq!(ToolRisk::Medium.max_calls(), 2);
        let exploit = profile_for("web_exploit").expect("web_exploit profile");
        assert_eq!(exploit.risk, ToolRisk::High);
    }
}
