//! Capability Router —— `server/core/agents/mission_runtime.py` 的
//! `CapabilityRouter` 移植。
//!
//! 把 Branch 假设映射为 solver 派遣配置，不执行任何工具。契约、种类与
//! 优先级全部逐字镜像 Python；分支生成器在 [`crate::branch_generator`]。

use std::collections::HashSet;

use models::common::StrMap;
use models::domain::AuditDomain;
use models::fact::Fact;
use models::ids::BranchId;
use models::mission::Branch;
use models::mission::CapabilityDispatch;
use models::mission::CapabilityGapSeverity;
use models::project::Project;
use serde_json::Map;
use serde_json::Value;

/// `route` 的输入（Python keyword-only 参数镜像）。
pub struct RouteInput<'a> {
    /// 目标 Branch。
    pub branch: &'a Branch,
    /// 所属 Project。
    pub project: &'a Project,
    /// 可用 solver 名集。
    pub available_solvers: &'a [String],
    /// 可用模块 id 集（默认空）。
    pub available_modules: &'a [String],
    /// Run 配置（默认空）。
    pub run_config: Option<&'a Map<String, Value>>,
    /// 全部 Fact（默认空）。
    pub facts: &'a [Fact],
}

impl<'a> RouteInput<'a> {
    /// 以必填参数构造，其余取 Python 默认值。
    #[must_use]
    pub fn new(branch: &'a Branch, project: &'a Project, available_solvers: &'a [String]) -> Self {
        Self {
            branch,
            project,
            available_solvers,
            available_modules: &[],
            run_config: None,
            facts: &[],
        }
    }
}

/// 把 Branch 假设映射为 solver 派遣配置而不执行工具
/// （Python `CapabilityRouter`）。
#[derive(Debug, Default)]
pub struct CapabilityRouter;

impl CapabilityRouter {
    /// 返回一条派遣建议或一个能力缺口（`route`）。
    // Python 侧 route 是单个 335 行方法（含全部分支决策表），1:1 移植
    // 保持同构；拆分会破坏与源码的逐段对照。
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn route(&self, input: &RouteInput<'_>) -> CapabilityDispatch {
        let solvers: HashSet<&str> = input.available_solvers.iter().map(String::as_str).collect();
        let empty_config = Map::new();
        let config = input.run_config.unwrap_or(&empty_config);
        let branch_kind = input
            .branch
            .metadata
            .get("branch_kind")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let target = &input.project.target;
        let fact_kinds: HashSet<&str> = input.facts.iter().map(|fact| fact.kind.as_str()).collect();
        let fact_count = input.facts.len();
        let branch_id = input.branch.id.clone();

        if branch_kind.starts_with("source.") {
            if has_exposure_inputs(&fact_kinds) && solvers.contains("exposure_intelligence") {
                return CapabilityDispatch {
                    branch_id,
                    solver: Some("exposure_intelligence".to_string()),
                    audit_domain: Some(AuditDomain::ExposureIntelligence.as_str().to_string()),
                    capability_name: Some("exposure_intelligence.rank_candidates".to_string()),
                    config: merge_config(
                        config,
                        &json_config(&[(
                            "audit_domains",
                            Value::Array(vec![Value::String(
                                AuditDomain::ExposureIntelligence.as_str().to_string(),
                            )]),
                        )]),
                    ),
                    rationale: "Source branch already has graph evidence; exposure intelligence \
                                can rank breakthrough candidates without running tools."
                        .to_string(),
                    capability_gap: false,
                    gap_summary: None,
                    gap_severity: CapabilityGapSeverity::Info,
                };
            }
            if solvers.contains("web_sast") {
                let source = target
                    .get("repo_path")
                    .or_else(|| target.get("repo"))
                    .or_else(|| target.get("source_root"))
                    .map(str::to_string);
                let mut semgrep = Map::new();
                if let Some(value) = source {
                    semgrep.insert("target".to_string(), Value::String(value));
                    semgrep.insert("config".to_string(), Value::String("auto".to_string()));
                }
                let mut web_sast = Map::new();
                web_sast.insert("engine".to_string(), Value::String("semgrep".to_string()));
                web_sast.insert("semgrep".to_string(), Value::Object(semgrep));
                return CapabilityDispatch {
                    branch_id,
                    solver: Some("web_sast".to_string()),
                    audit_domain: Some(AuditDomain::WebSast.as_str().to_string()),
                    capability_name: Some("web_sast.semgrep.scan".to_string()),
                    config: merge_config(
                        config,
                        &json_config(&[
                            (
                                "audit_domains",
                                Value::Array(vec![Value::String(
                                    AuditDomain::WebSast.as_str().to_string(),
                                )]),
                            ),
                            ("web_sast", Value::Object(web_sast)),
                        ]),
                    ),
                    rationale: "Source branch needs fast source-sink/security pattern coverage; \
                                web_sast is available and can select Semgrep."
                        .to_string(),
                    capability_gap: false,
                    gap_summary: None,
                    gap_severity: CapabilityGapSeverity::Info,
                };
            }
        }

        if branch_kind == "url.surface_mapping" {
            let url = url_of(target);
            if solvers.contains("web_recon") {
                return web_recon_dispatch(branch_id, config, url);
            }
            if solvers.contains("content_discovery") {
                return content_discovery_dispatch(branch_id, config, url);
            }
        }

        if branch_kind == "url.auth_session" {
            let url = url_of(target);
            if solvers.contains("content_discovery") {
                return content_discovery_dispatch(branch_id, config, url);
            }
            if solvers.contains("web_recon") {
                return CapabilityDispatch {
                    branch_id,
                    solver: Some("web_recon".to_string()),
                    audit_domain: Some(AuditDomain::WebRecon.as_str().to_string()),
                    capability_name: Some("web_recon.surface_map".to_string()),
                    config: web_recon_config(config, url),
                    rationale: "Auth/session branch needs reachable route context; web_recon \
                                is available."
                        .to_string(),
                    capability_gap: false,
                    gap_summary: None,
                    gap_severity: CapabilityGapSeverity::Info,
                };
            }
        }

        if branch_kind == "url.input_validation" || branch_kind == "url.known_exposure" {
            let url = url_of(target);
            if branch_kind == "url.input_validation"
                && url.is_some()
                && solvers.contains("web_exploit")
            {
                return web_exploit_dispatch(branch_id, config, url);
            }
            if branch_kind == "url.known_exposure" && solvers.contains("fingerprint_intelligence") {
                return fingerprint_dispatch(branch_id, config, url);
            }
            if branch_kind == "url.input_validation"
                && solvers.contains("web_validation")
                && config
                    .get("web_validation")
                    .and_then(Value::as_object)
                    .is_some()
            {
                let mut web_validation = Map::new();
                if let Some(value) = url {
                    web_validation.insert("target".to_string(), Value::String(value));
                }
                return CapabilityDispatch {
                    branch_id,
                    solver: Some("web_validation".to_string()),
                    audit_domain: Some(AuditDomain::WebValidation.as_str().to_string()),
                    capability_name: Some("web_validation.targeted".to_string()),
                    config: merge_config(
                        config,
                        &json_config(&[
                            (
                                "audit_domains",
                                Value::Array(vec![Value::String(
                                    AuditDomain::WebValidation.as_str().to_string(),
                                )]),
                            ),
                            ("web_validation", Value::Object(web_validation)),
                        ]),
                    ),
                    rationale: "The intake plan explicitly selected a specialist validator for \
                                this input-validation branch."
                        .to_string(),
                    capability_gap: false,
                    gap_summary: None,
                    gap_severity: CapabilityGapSeverity::Info,
                };
            }
            if solvers.contains("web_dast") {
                let mut nuclei = Map::new();
                if let Some(value) = url {
                    nuclei.insert("target".to_string(), Value::String(value));
                }
                let mut web_dast = Map::new();
                web_dast.insert("engine".to_string(), Value::String("nuclei".to_string()));
                web_dast.insert("nuclei".to_string(), Value::Object(nuclei));
                return CapabilityDispatch {
                    branch_id,
                    solver: Some("web_dast".to_string()),
                    audit_domain: Some(AuditDomain::WebDast.as_str().to_string()),
                    capability_name: Some("web_dast.nuclei.scan".to_string()),
                    config: merge_config(
                        config,
                        &json_config(&[
                            (
                                "audit_domains",
                                Value::Array(vec![Value::String(
                                    AuditDomain::WebDast.as_str().to_string(),
                                )]),
                            ),
                            ("web_dast", Value::Object(web_dast)),
                        ]),
                    ),
                    rationale: "Input validation/exposure branch can be validated by bounded \
                                DAST templates; web_dast is available."
                        .to_string(),
                    capability_gap: false,
                    gap_summary: None,
                    gap_severity: CapabilityGapSeverity::Info,
                };
            }
            if solvers.contains("web_validation") {
                return CapabilityDispatch {
                    branch_id,
                    solver: Some("web_validation".to_string()),
                    audit_domain: Some(AuditDomain::WebValidation.as_str().to_string()),
                    capability_name: Some("web_validation.targeted".to_string()),
                    config: merge_config(
                        config,
                        &json_config(&[(
                            "audit_domains",
                            Value::Array(vec![Value::String(
                                AuditDomain::WebValidation.as_str().to_string(),
                            )]),
                        )]),
                    ),
                    rationale: "Targeted web validators are available for this input branch."
                        .to_string(),
                    capability_gap: false,
                    gap_summary: None,
                    gap_severity: CapabilityGapSeverity::Info,
                };
            }
            if branch_kind == "url.input_validation"
                && !fact_kinds.contains("content_discovery_completed")
                && solvers.contains("content_discovery")
            {
                return content_discovery_dispatch(branch_id, config, url);
            }
            if solvers.contains("exposure_intelligence") && has_exposure_inputs(&fact_kinds) {
                return exposure_dispatch(branch_id, config, "URL graph state can be ranked.");
            }
        }

        if branch_kind.starts_with("traffic.") && solvers.contains("traffic_intelligence") {
            let artifact = target
                .get("artifact_path")
                .or_else(|| target.get("traffic_artifact"))
                .map(str::to_string);
            let mut traffic_intelligence = Map::new();
            if let Some(value) = artifact {
                traffic_intelligence.insert("artifact_path".to_string(), Value::String(value));
            }
            return CapabilityDispatch {
                branch_id,
                solver: Some("traffic_intelligence".to_string()),
                audit_domain: Some(AuditDomain::TrafficIntelligence.as_str().to_string()),
                capability_name: Some("traffic_intelligence.import".to_string()),
                config: merge_config(
                    config,
                    &json_config(&[
                        (
                            "audit_domains",
                            Value::Array(vec![Value::String(
                                AuditDomain::TrafficIntelligence.as_str().to_string(),
                            )]),
                        ),
                        ("traffic_intelligence", Value::Object(traffic_intelligence)),
                    ]),
                ),
                rationale: format!(
                    "Traffic branch needs observed request facts; {fact_count} project fact(s) \
                     are available."
                ),
                capability_gap: false,
                gap_summary: None,
                gap_severity: CapabilityGapSeverity::Info,
            };
        }

        if branch_kind == "mixed.classification" || branch_kind == "mixed.initial_surface" {
            let domain = target
                .get("domain")
                .or_else(|| target.get("target_domain"))
                .or_else(|| target.get("host"))
                .map(str::to_string);
            let url = url_of(target);
            if let Some(domain) = domain
                && solvers.contains("asset_recon")
            {
                return CapabilityDispatch {
                    branch_id,
                    solver: Some("asset_recon".to_string()),
                    audit_domain: Some(AuditDomain::AssetRecon.as_str().to_string()),
                    capability_name: Some("asset_recon.surface_discovery".to_string()),
                    config: merge_config(
                        config,
                        &json_config(&[
                            (
                                "audit_domains",
                                Value::Array(vec![Value::String(
                                    AuditDomain::AssetRecon.as_str().to_string(),
                                )]),
                            ),
                            ("asset_recon", serde_json::json!({"target": domain})),
                        ]),
                    ),
                    rationale: "Classification/surface branch has a domain target; \
                                asset_recon can discover external attack surface without \
                                becoming the Branch."
                        .to_string(),
                    capability_gap: false,
                    gap_summary: None,
                    gap_severity: CapabilityGapSeverity::Info,
                };
            }
            if url.is_some() && solvers.contains("web_recon") {
                return CapabilityDispatch {
                    branch_id,
                    solver: Some("web_recon".to_string()),
                    audit_domain: Some(AuditDomain::WebRecon.as_str().to_string()),
                    capability_name: Some("web_recon.surface_map".to_string()),
                    config: web_recon_config(config, url),
                    rationale: "Classification/surface branch has a URL target; web_recon can \
                                map reachable routes before specialized Branch expansion."
                        .to_string(),
                    capability_gap: false,
                    gap_summary: None,
                    gap_severity: CapabilityGapSeverity::Info,
                };
            }
            if url.is_some() && solvers.contains("fingerprint_intelligence") {
                return fingerprint_dispatch(branch_id, config, url);
            }
            if solvers.contains("exposure_intelligence") && has_exposure_inputs(&fact_kinds) {
                return exposure_dispatch(
                    branch_id,
                    config,
                    "Mixed target already has graph evidence to rank candidates.",
                );
            }
        }

        if branch_kind.starts_with("binary.") && solvers.contains("binary_analysis") {
            let audit_domain = if branch_kind == "binary.heap_stack" {
                AuditDomain::BinaryDynamic
            } else {
                AuditDomain::BinaryStatic
            };
            return CapabilityDispatch {
                branch_id,
                solver: Some("binary_analysis".to_string()),
                audit_domain: Some(audit_domain.as_str().to_string()),
                capability_name: Some("binary_analysis.ida_mcp".to_string()),
                config: merge_config(
                    config,
                    &json_config(&[(
                        "audit_domains",
                        Value::Array(vec![Value::String(audit_domain.as_str().to_string())]),
                    )]),
                ),
                rationale: "Binary branch is handled by the binary_analysis Agent Tool Harness; IDA MCP tools/list supplies the callable tool surface at runtime."
                    .to_string(),
                capability_gap: false,
                gap_summary: None,
                gap_severity: CapabilityGapSeverity::Info,
            };
        }

        // 统一派发兜底：没有领域规则匹配时不再判定能力缺口——通用 Agent
        // 本身就是执行者，工具经 lynceus MCP 自助发现（tool_list /
        // tool_describe / tool_execute）。任选一个可用 solver 作为执行
        // 框架（按 branch_kind 排序取第一个，保持确定性）。
        let fallback_solver = input
            .available_solvers
            .iter()
            .find(|name| name.as_str() == "web_recon")
            .cloned()
            .or_else(|| input.available_solvers.first().cloned());
        match fallback_solver {
            Some(solver) => CapabilityDispatch {
                branch_id,
                solver: Some(solver.clone()),
                audit_domain: Some(AuditDomain::Composite.as_str().to_string()),
                capability_name: None,
                config: config.clone(),
                rationale: format!(
                    "No domain rule matched branch kind '{branch_kind}'; dispatched to the                      generic '{solver}' Agent Tool Harness — tools are discovered via MCP."
                ),
                capability_gap: false,
                gap_summary: None,
                gap_severity: CapabilityGapSeverity::Info,
            },
            None => CapabilityDispatch {
                branch_id,
                solver: None,
                audit_domain: None,
                capability_name: None,
                config: config.clone(),
                rationale: "No solver is registered at all.".to_string(),
                capability_gap: true,
                gap_summary: Some(
                    "No registered solver can advance this branch.".to_string(),
                ),
                gap_severity: CapabilityGapSeverity::Low,
            },
        }
    }
}

/// Python `_merge_config`：router 值是安全默认；显式 run 配置（例如选定的
/// validator 或 demo baseline）必须赢。
fn merge_config(base: &Map<String, Value>, overlay: &Map<String, Value>) -> Map<String, Value> {
    let mut merged = base.clone();
    for (key, value) in overlay {
        let nested = match (
            value.as_object(),
            merged.get(key).and_then(Value::as_object),
        ) {
            (Some(overlay_map), Some(existing_map)) => {
                let mut nested = overlay_map.clone();
                for (existing_key, existing_value) in existing_map {
                    nested.insert(existing_key.clone(), existing_value.clone());
                }
                Value::Object(nested)
            }
            _ => value.clone(),
        };
        merged.insert(key.clone(), nested);
    }
    merged
}

fn json_config(entries: &[(&str, Value)]) -> Map<String, Value> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_string(), value.clone()))
        .collect()
}

/// Python `_has_exposure_inputs`.
fn has_exposure_inputs(fact_kinds: &HashSet<&str>) -> bool {
    const EXPOSURE_KINDS: [&str; 6] = [
        "web.endpoint",
        "web.directory",
        "web.interesting_route",
        "asset.technology",
        "asset.product",
        "asset.version",
    ];
    EXPOSURE_KINDS.iter().any(|kind| fact_kinds.contains(kind))
}

fn url_of(target: &StrMap) -> Option<String> {
    target
        .get("url")
        .or_else(|| target.get("target"))
        .or_else(|| target.get("base_url"))
        .map(str::to_string)
}

fn web_recon_config(config: &Map<String, Value>, url: Option<String>) -> Map<String, Value> {
    let mut web_recon = Map::new();
    if let Some(value) = url {
        web_recon.insert("target".to_string(), Value::String(value));
        web_recon.insert(
            "engines".to_string(),
            Value::Array(vec![
                Value::String("katana".to_string()),
                Value::String("jsluice".to_string()),
            ]),
        );
    }
    merge_config(
        config,
        &json_config(&[
            (
                "audit_domains",
                Value::Array(vec![Value::String(
                    AuditDomain::WebRecon.as_str().to_string(),
                )]),
            ),
            ("web_recon", Value::Object(web_recon)),
        ]),
    )
}

fn web_recon_dispatch(
    branch_id: BranchId,
    config: &Map<String, Value>,
    url: Option<String>,
) -> CapabilityDispatch {
    CapabilityDispatch {
        branch_id,
        solver: Some("web_recon".to_string()),
        audit_domain: Some(AuditDomain::WebRecon.as_str().to_string()),
        capability_name: Some("web_recon.surface_map".to_string()),
        config: web_recon_config(config, url),
        rationale: "URL surface branch should first discover endpoints and parameters; \
                    web_recon is available."
            .to_string(),
        capability_gap: false,
        gap_summary: None,
        gap_severity: CapabilityGapSeverity::Info,
    }
}

fn content_discovery_dispatch(
    branch_id: BranchId,
    config: &Map<String, Value>,
    url: Option<String>,
) -> CapabilityDispatch {
    let mut content_discovery = Map::new();
    if let Some(value) = url {
        content_discovery.insert("target".to_string(), Value::String(value));
    }
    CapabilityDispatch {
        branch_id,
        solver: Some("content_discovery".to_string()),
        audit_domain: Some(AuditDomain::ContentDiscovery.as_str().to_string()),
        capability_name: Some("content_discovery.route_discovery".to_string()),
        config: merge_config(
            config,
            &json_config(&[
                (
                    "audit_domains",
                    Value::Array(vec![Value::String(
                        AuditDomain::ContentDiscovery.as_str().to_string(),
                    )]),
                ),
                ("content_discovery", Value::Object(content_discovery)),
            ]),
        ),
        rationale: "URL branch needs bounded directory/API discovery before deeper validation."
            .to_string(),
        capability_gap: false,
        gap_summary: None,
        gap_severity: CapabilityGapSeverity::Info,
    }
}

fn web_exploit_dispatch(
    branch_id: BranchId,
    config: &Map<String, Value>,
    url: Option<String>,
) -> CapabilityDispatch {
    let mut web_exploit = Map::new();
    if let Some(value) = url {
        web_exploit.insert("target".to_string(), Value::String(value));
    }
    CapabilityDispatch {
        branch_id,
        solver: Some("web_exploit".to_string()),
        audit_domain: Some(AuditDomain::Exploitability.as_str().to_string()),
        capability_name: Some("web_exploit.flag_capture".to_string()),
        config: merge_config(
            config,
            &json_config(&[
                (
                    "audit_domains",
                    Value::Array(vec![Value::String(
                        AuditDomain::Exploitability.as_str().to_string(),
                    )]),
                ),
                ("web_exploit", Value::Object(web_exploit)),
            ]),
        ),
        rationale: "Input-validation branch on a URL target should act on reachable \
                    inputs: harvest disclosed source/hints and run bounded LFI/parameter \
                    probes to capture a flag or secret."
            .to_string(),
        capability_gap: false,
        gap_summary: None,
        gap_severity: CapabilityGapSeverity::Info,
    }
}

fn fingerprint_dispatch(
    branch_id: BranchId,
    config: &Map<String, Value>,
    url: Option<String>,
) -> CapabilityDispatch {
    let mut fingerprint_intelligence = Map::new();
    if let Some(value) = url {
        fingerprint_intelligence.insert("target".to_string(), Value::String(value));
    }
    CapabilityDispatch {
        branch_id,
        solver: Some("fingerprint_intelligence".to_string()),
        audit_domain: Some(AuditDomain::FingerprintIntelligence.as_str().to_string()),
        capability_name: Some("fingerprint_intelligence.identify_stack".to_string()),
        config: merge_config(
            config,
            &json_config(&[
                (
                    "audit_domains",
                    Value::Array(vec![Value::String(
                        AuditDomain::FingerprintIntelligence.as_str().to_string(),
                    )]),
                ),
                (
                    "fingerprint_intelligence",
                    Value::Object(fingerprint_intelligence),
                ),
            ]),
        ),
        rationale: "Known-exposure branch benefits from technology/product \
                    fingerprints before exploitability decisions."
            .to_string(),
        capability_gap: false,
        gap_summary: None,
        gap_severity: CapabilityGapSeverity::Info,
    }
}

fn exposure_dispatch(
    branch_id: BranchId,
    config: &Map<String, Value>,
    rationale: &str,
) -> CapabilityDispatch {
    CapabilityDispatch {
        branch_id,
        solver: Some("exposure_intelligence".to_string()),
        audit_domain: Some(AuditDomain::ExposureIntelligence.as_str().to_string()),
        capability_name: Some("exposure_intelligence.rank_candidates".to_string()),
        config: merge_config(
            config,
            &json_config(&[(
                "audit_domains",
                Value::Array(vec![Value::String(
                    AuditDomain::ExposureIntelligence.as_str().to_string(),
                )]),
            )]),
        ),
        rationale: rationale.to_string(),
        capability_gap: false,
        gap_summary: None,
        gap_severity: CapabilityGapSeverity::Info,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::ids::MissionId;
    use models::ids::ProjectId;
    use models::mission::Branch;

    fn project_with_target(entries: &[(&str, &str)]) -> Project {
        let mut project = Project::new("route".to_string(), AuditDomain::WebSast);
        let mut target = StrMap::new();
        for (key, value) in entries {
            target.insert((*key).to_string(), (*value).to_string());
        }
        project.target = target;
        project
    }

    fn branch_of_kind(kind: &str) -> Branch {
        let mut branch = Branch::new(
            ProjectId::new("proj_route".to_string()),
            MissionId::new("mission_route".to_string()),
            "title".to_string(),
            "hypothesis".to_string(),
        );
        branch
            .metadata
            .insert("branch_kind".to_string(), Value::String(kind.to_string()));
        branch
    }

    fn route<'a>(
        branch: &'a Branch,
        project: &'a Project,
        solvers: &'a [String],
        config: Option<&'a Map<String, Value>>,
    ) -> CapabilityDispatch {
        let mut input = RouteInput::new(branch, project, solvers);
        input.run_config = config;
        CapabilityRouter.route(&input)
    }

    #[test]
    fn routes_source_branch_to_web_sast_with_semgrep_target() {
        let project = project_with_target(&[("repo_path", "D:/src/app")]);
        let branch = branch_of_kind("source.source_sink");
        let solvers = vec!["web_sast".to_string(), "web_recon".to_string()];
        let dispatch = route(&branch, &project, &solvers, None);
        assert_eq!(dispatch.solver.as_deref(), Some("web_sast"));
        assert!(!dispatch.capability_gap);
        let web_sast = dispatch
            .config
            .get("web_sast")
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("web_sast 配置必须存在"));
        assert_eq!(web_sast["engine"], Value::String("semgrep".to_string()));
        let semgrep = web_sast["semgrep"]
            .as_object()
            .unwrap_or_else(|| panic!("semgrep 配置必须是对象"));
        assert_eq!(semgrep["target"], Value::String("D:/src/app".to_string()));
        assert_eq!(semgrep["config"], Value::String("auto".to_string()));
    }

    #[test]
    fn explicit_run_config_wins_over_router_defaults() {
        let project = project_with_target(&[("repo_path", "D:/src/app")]);
        let branch = branch_of_kind("source.source_sink");
        let solvers = vec!["web_sast".to_string()];
        let mut config = Map::new();
        config.insert(
            "web_sast".to_string(),
            serde_json::json!({"engine": "baseline"}),
        );
        let dispatch = route(&branch, &project, &solvers, Some(&config));
        let web_sast = dispatch.config["web_sast"].as_object().unwrap();
        // 显式 engine=baseline 覆盖 router 默认 semgrep。
        assert_eq!(web_sast["engine"], Value::String("baseline".to_string()));
        // router 补充的嵌套默认仍在。
        assert!(web_sast.get("semgrep").is_some());
    }

    #[test]
    fn routes_url_surface_to_web_recon() {
        let project = project_with_target(&[("url", "https://example.test")]);
        let branch = branch_of_kind("url.surface_mapping");
        let solvers = vec!["web_recon".to_string()];
        let dispatch = route(&branch, &project, &solvers, None);
        assert_eq!(dispatch.solver.as_deref(), Some("web_recon"));
        let web_recon = dispatch.config["web_recon"].as_object().unwrap();
        assert_eq!(
            web_recon["target"],
            Value::String("https://example.test".to_string())
        );
        assert_eq!(
            web_recon["engines"],
            serde_json::json!(["katana", "jsluice"])
        );
    }

    #[test]
    fn every_binary_branch_routes_to_binary_analysis() {
        let project = project_with_target(&[("binary", "C:/chal.exe")]);
        let solvers = vec!["binary_analysis".to_string()];
        for kind in [
            "binary.parser_surface",
            "binary.dangerous_api",
            "binary.heap_stack",
            "binary.protocol_surface",
        ] {
            let branch = branch_of_kind(kind);
            let dispatch = route(&branch, &project, &solvers, None);
            assert!(!dispatch.capability_gap, "{kind}");
            assert_eq!(dispatch.solver.as_deref(), Some("binary_analysis"));
            assert_eq!(
                dispatch.capability_name.as_deref(),
                Some("binary_analysis.ida_mcp")
            );
            assert!(dispatch.gap_summary.is_none());
        }
    }

    #[test]
    fn unknown_kind_dispatches_generic_agent() {
        let project = project_with_target(&[]);
        let branch = branch_of_kind("mystery.kind");
        // 统一派发：未知 branch_kind 不再判定能力缺口，走通用 Agent。
        let dispatch = route(&branch, &project, &["web_recon".to_string()], None);
        assert!(!dispatch.capability_gap);
        assert_eq!(dispatch.solver.as_deref(), Some("web_recon"));
        assert_eq!(
            dispatch.audit_domain.as_deref(),
            Some(AuditDomain::Composite.as_str())
        );
        // 完全没有 solver 注册时才保留缺口语义。
        let none = route(&branch, &project, &[], None);
        assert!(none.capability_gap);
    }

    #[test]
    fn explicit_internal_surface_no_longer_routes_to_fscan() {
        // fscan 因许可问题已从工具目录移除，internal_surface 不再有显式的
        // fscan 导入路由：显式 internal_cidr 配置现落在能力缺口（无内置工具），
        // 由 worker 经 MCP 自行发现，而不是路由到一个已不存在的工具。
        let project = project_with_target(&[("internal_cidr", "10.0.0.0/8")]);
        let branch = branch_of_kind("mixed.initial_surface");
        let solvers = vec!["internal_surface".to_string()];
        let mut config = Map::new();
        config.insert(
            "audit_domains".to_string(),
            Value::Array(vec![Value::String("internal_surface".to_string())]),
        );
        let dispatch = route(&branch, &project, &solvers, Some(&config));
        // fscan 专用导入路径已移除：不再产出 internal_surface.fscan.discovery_import。
        // （internal_surface 作为唯一可用 solver 仍可能被 fallback 路由到，但那是
        // 无内置工具的 worker 自主发现，不再是钉死 fscan 的受限导入。）
        assert_ne!(
            dispatch.capability_name.as_deref(),
            Some("internal_surface.fscan.discovery_import")
        );
    }

    #[test]
    fn no_solver_for_branch_kind_returns_gap() {
        let project = project_with_target(&[]);
        let branch = branch_of_kind("source.source_sink");
        let dispatch = route(&branch, &project, &[], None);
        assert!(dispatch.capability_gap);
        assert_eq!(dispatch.solver, None);
    }
}
