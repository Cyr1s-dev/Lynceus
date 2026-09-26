//! COV：确定性攻击面覆盖检查 —— `server/core/agents/closure.py`
//! `CoverageChecker` 的移植。
//!
//! 覆盖只从真实信号计量：执行过的工具调用、落盘证据、消耗过步数的
//! 分支。相关性来自 Mission 目标类型——盲区的含义是"该 Mission 的
//! 攻击面蕴含该域，而没有任何信号行使过它"。未知工具不产生覆盖信号
//! （不编造覆盖）。

use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::HashSet;

use models::closure::CoverageAssessment;
use models::closure::CoverageCategoryStatus;
use models::closure::CoverageDomainEntry;
use models::domain::AuditDomain;
use models::evidence::Evidence;
use models::lifecycle::EvidenceKind;
use models::mission::Branch;
use models::mission::Mission;
use models::module::ModuleConfig;
use models::module::ModuleDomain;
use models::run::AuditRun;
use models::tool_invocation::ToolInvocation;

/// 一次覆盖检查的输入（Python `check` 的 keyword-only 参数镜像）。
#[derive(Debug)]
pub struct CoverageInput<'a> {
    /// 所属 Mission。
    pub mission: &'a Mission,
    /// 所属 Run。
    pub run: &'a AuditRun,
    /// 全部分支（含未消耗步数的 PROPOSED——它们是意图不是行使面）。
    pub branches: &'a [Branch],
    /// 全部落盘证据。
    pub evidence: &'a [Evidence],
    /// 全部工具调用。
    pub tool_invocations: &'a [ToolInvocation],
    /// 已注册模块（`module_id` → 审计域绑定）。
    pub modules: &'a [ModuleConfig],
    /// 收口轮次。
    pub round_index: i64,
}

impl<'a> CoverageInput<'a> {
    /// 以空信号集构造（mission/run 必填，其余默认空）。
    #[must_use]
    pub fn new(mission: &'a Mission, run: &'a AuditRun) -> Self {
        Self {
            mission,
            run,
            branches: &[],
            evidence: &[],
            tool_invocations: &[],
            modules: &[],
            round_index: 0,
        }
    }
}

/// 单域计数（Python `counters[domain]` 的三键 dict）。
#[derive(Debug, Default, Clone, Copy)]
struct DomainCounts {
    tool_invocation: i64,
    evidence: i64,
    branch: i64,
}

impl DomainCounts {
    fn total(&self) -> i64 {
        self.tool_invocation + self.evidence + self.branch
    }
}

/// 工具名 → 审计域目录（Python `_TOOL_NAME_DOMAINS` 的镜像）。
///
/// 工具名匹配不区分大小写；未收录工具无覆盖信号。
fn tool_name_domain(tool_name: &str) -> Option<AuditDomain> {
    Some(match tool_name {
        "semgrep" => AuditDomain::WebSast,
        "nuclei" => AuditDomain::WebDast,
        "subfinder" | "naabu" | "httpx" | "dnsx" => AuditDomain::AssetRecon,
        "katana" | "jsluice" => AuditDomain::WebRecon,
        "ffuf" | "feroxbuster" | "gobuster" | "gau" => {
            AuditDomain::ContentDiscovery
        }
        "wappalyzergo" | "ehole" => AuditDomain::FingerprintIntelligence,
        "dalfox" => AuditDomain::WebValidation,
        "afrog" => AuditDomain::ExploitabilityValidation,
        _ => return None,
    })
}

/// 证据类型 → 审计域（Python `_EVIDENCE_KIND_DOMAINS`）。
fn evidence_kind_domain(kind: EvidenceKind) -> Option<AuditDomain> {
    Some(match kind {
        EvidenceKind::SourceSnippet | EvidenceKind::SarifLocation => AuditDomain::WebSast,
        EvidenceKind::CallChain | EvidenceKind::TaintPath => AuditDomain::CodeDeepSast,
        EvidenceKind::DecompiledPseudocode => AuditDomain::BinaryStatic,
        EvidenceKind::CrashInput => AuditDomain::Fuzzing,
        EvidenceKind::PocDescription => AuditDomain::ExploitabilityValidation,
        EvidenceKind::ToolOutput => return None,
    })
}

/// 分支种类 → 审计域（Python `_BRANCH_KIND_DOMAINS`）。
fn branch_kind_domain(branch_kind: &str) -> Option<AuditDomain> {
    Some(match branch_kind {
        "url.surface_mapping" | "url.auth_session" => AuditDomain::WebRecon,
        "url.input_validation" | "traffic.replay_validation" => AuditDomain::WebValidation,
        "url.known_exposure" => AuditDomain::FingerprintIntelligence,
        "source.source_sink" | "source.framework_route" | "source.secret_exposure" => {
            AuditDomain::WebSast
        }
        "source.dependency_config" => AuditDomain::SupplyChain,
        "binary.parser_surface" | "binary.protocol_surface" | "binary.dangerous_api" => {
            AuditDomain::BinaryStatic
        }
        "binary.heap_stack" => AuditDomain::BinaryDynamic,
        "traffic.parameter_analysis" | "traffic.auth_context" | "traffic.interesting_endpoint" => {
            AuditDomain::TrafficIntelligence
        }
        _ => return None,
    })
}

/// 相关域集合 = Mission 自己声明的那一个审计域。
///
/// 目标分类已删除：不再按 url/source/binary/traffic 把域分组。摄入阶段
/// 由模型写下的 `audit_domain` 是唯一被期待的域；其余域只要被真实信号
/// 触达就算 covered，未被触达是 NotRelevant 而不是盲区——不编造期待。
fn relevant_domains(mission: &Mission) -> HashSet<AuditDomain> {
    mission
        .metadata
        .get("audit_domain")
        .and_then(serde_json::Value::as_str)
        .and_then(|raw| {
            serde_json::from_value::<AuditDomain>(serde_json::Value::String(raw.to_string())).ok()
        })
        .into_iter()
        .collect()
}

/// COV：从真实信号计量攻击面覆盖的确定性检查器。
#[derive(Debug, Default)]
pub struct CoverageChecker;

impl CoverageChecker {
    /// 执行一轮覆盖检查（纯计算，无 I/O）。
    #[must_use]
    pub fn check(&self, input: &CoverageInput<'_>) -> CoverageAssessment {
        let (counters, sources) = collect_signals(input, &module_domain_map(input.modules));
        let relevant = relevant_domains(input.mission);
        let declared_domain = relevant.iter().next().copied();
        let (entries, covered, blind_spots) =
            build_entries(&relevant, &counters, &sources, declared_domain);
        let summary = summarize(&relevant, &covered, &blind_spots);

        // Python：covered/blind_spots/relevant 按 wire 值字典序排序
        // （str 混入枚举的 sorted() 语义），entries 保持枚举定义序。
        let mut covered = covered;
        covered.sort_unstable_by_key(|domain| domain.as_str());
        let mut blind_spots = blind_spots;
        blind_spots.sort_unstable_by_key(|domain| domain.as_str());
        let mut relevant_sorted: Vec<AuditDomain> = relevant.iter().copied().collect();
        relevant_sorted.sort_unstable_by_key(|domain| domain.as_str());

        let mut assessment =
            CoverageAssessment::new(input.run.project_id.clone(), input.run.id.clone());
        assessment.mission_id = Some(input.mission.id.clone());
        assessment.round_index = input.round_index;
        assessment.entries = entries;
        assessment.relevant_domains = relevant_sorted;
        assessment.covered_domains = covered;
        assessment.blind_spots = blind_spots;
        assessment.summary = summary;
        if let Some(domain) = declared_domain {
            assessment.metadata.insert(
                "audit_domain".to_string(),
                serde_json::Value::String(domain.as_str().to_string()),
            );
        }
        assessment.metadata.insert(
            "tool_invocation_count".to_string(),
            json_count(input.tool_invocations.len()),
        );
        assessment.metadata.insert(
            "evidence_count".to_string(),
            json_count(input.evidence.len()),
        );
        assessment
            .metadata
            .insert("branch_count".to_string(), json_count(input.branches.len()));
        assessment
    }
}

/// module.id → `AuditDomain(ModuleDomain(domain).value)`。
///
/// Python 侧非法 domain 值被 `except ValueError: continue` 跳过；
/// Rust 侧 `ModuleDomain` 已是枚举、`TryFrom` 穷尽映射，不存在非法值。
fn module_domain_map(modules: &[ModuleConfig]) -> HashMap<&str, AuditDomain> {
    modules
        .iter()
        .map(|module| {
            let domain = AuditDomain::try_from(module.domain)
                .unwrap_or_else(|domain| unreachable_domain(domain));
            (module.id.as_str(), domain)
        })
        .collect()
}

/// 信号收集：三类真实信号 → 域计数与各域信号来源集合。
fn collect_signals(
    input: &CoverageInput<'_>,
    module_domains: &HashMap<&str, AuditDomain>,
) -> (
    HashMap<AuditDomain, DomainCounts>,
    HashMap<AuditDomain, BTreeSet<String>>,
) {
    let mut counters: HashMap<AuditDomain, DomainCounts> = HashMap::new();
    let mut sources: HashMap<AuditDomain, BTreeSet<String>> = HashMap::new();

    for invocation in input.tool_invocations {
        if let Some(domain) = invocation_domain(invocation, module_domains) {
            counters.entry(domain).or_default().tool_invocation += 1;
            sources
                .entry(domain)
                .or_default()
                .insert(invocation.tool_name.clone());
        }
    }
    for item in input.evidence {
        if let Some(domain) = evidence_kind_domain(item.kind) {
            counters.entry(domain).or_default().evidence += 1;
            sources
                .entry(domain)
                .or_default()
                .insert(item.kind.as_str().to_string());
        }
    }
    for branch in input.branches {
        let branch_kind = branch
            .metadata
            .get("branch_kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        // 分支只有真实消耗过步数才计入覆盖：从未派发的 PROPOSED 分支
        // 是意图，不是行使过的攻击面。
        if branch.steps_used > 0
            && let Some(domain) = branch_kind_domain(branch_kind)
        {
            counters.entry(domain).or_default().branch += 1;
            sources
                .entry(domain)
                .or_default()
                .insert(branch_kind.to_string());
        }
    }

    (counters, sources)
}

/// entries 构建：遍历全部审计域，同时产出已覆盖与盲区列表
/// （均保持 `AuditDomain::ALL` 枚举定义序）。
fn build_entries(
    relevant: &HashSet<AuditDomain>,
    counters: &HashMap<AuditDomain, DomainCounts>,
    sources: &HashMap<AuditDomain, BTreeSet<String>>,
    declared_domain: Option<AuditDomain>,
) -> (Vec<CoverageDomainEntry>, Vec<AuditDomain>, Vec<AuditDomain>) {
    let mut entries: Vec<CoverageDomainEntry> = Vec::new();
    let mut covered: Vec<AuditDomain> = Vec::new();
    let mut blind_spots: Vec<AuditDomain> = Vec::new();
    for domain in AuditDomain::ALL {
        let counts = counters.get(&domain);
        let is_relevant = relevant.contains(&domain);
        // Python：`counts is not None and (tool+evidence+branch > 0)`。
        let exercised = counts.is_some_and(|counts| counts.total() > 0);
        if !is_relevant && !exercised {
            continue;
        }
        let status = if exercised {
            covered.push(domain);
            CoverageCategoryStatus::Covered
        } else if is_relevant {
            blind_spots.push(domain);
            CoverageCategoryStatus::NotCovered
        } else {
            CoverageCategoryStatus::NotRelevant
        };
        let counts = counts.copied().unwrap_or_default();
        entries.push(CoverageDomainEntry {
            domain,
            status,
            relevance_reason: if is_relevant {
                match declared_domain {
                    Some(domain) => format!("mission audit domain is {}", domain.as_str()),
                    None => String::new(),
                }
            } else {
                String::new()
            },
            tool_invocation_count: counts.tool_invocation,
            evidence_count: counts.evidence,
            branch_count: counts.branch,
            // Python 侧 check() 从不填充 finding_count（恒为模型默认 0）。
            finding_count: 0,
            signal_sources: sources
                .get(&domain)
                .map(|set| set.iter().cloned().collect())
                .unwrap_or_default(),
        });
    }
    (entries, covered, blind_spots)
}

/// 摘要文案：有盲区时点名盲区，否则宣告全部相关域已行使。
fn summarize(
    relevant: &HashSet<AuditDomain>,
    covered: &[AuditDomain],
    blind_spots: &[AuditDomain],
) -> String {
    if blind_spots.is_empty() {
        format!(
            "all {} domain(s) relevant to this Mission were exercised ({} covered in total)",
            relevant.len(),
            covered.len()
        )
    } else {
        format!(
            "{} domain(s) exercised; blind spots remain in: {}",
            covered.len(),
            blind_spots
                .iter()
                .map(|domain| domain.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

/// `usize` → JSON 数值（集合尺寸远小于 `i64::MAX`，饱和转换即精确转换）。
fn json_count(len: usize) -> serde_json::Value {
    serde_json::Value::from(i64::try_from(len).unwrap_or(i64::MAX))
}

/// `TryFrom<ModuleDomain> for AuditDomain` 的映射穷尽两侧枚举，Err 分支
/// 在类型上不可达；仅在两侧枚举新增值而映射未同步时到达，属编程错误，
/// 立即失败优于静默编造覆盖信号。
fn unreachable_domain(domain: ModuleDomain) -> ! {
    panic!("ModuleDomain→AuditDomain 映射必须穷尽：{domain:?} 未映射（两侧枚举漂移）");
}

/// Python `CoverageChecker._invocation_domain`：`module_id` 绑定优先，
/// 工具名目录兜底（trim + lowercase 匹配）。
fn invocation_domain(
    invocation: &ToolInvocation,
    module_domains: &HashMap<&str, AuditDomain>,
) -> Option<AuditDomain> {
    if let Some(module_id) = &invocation.module_id
        && let Some(domain) = module_domains.get(module_id.as_str())
    {
        return Some(*domain);
    }
    tool_name_domain(&invocation.tool_name.trim().to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::ids::MissionId;
    use models::ids::ModuleId;
    use models::ids::ProjectId;

    const PROJECT_ID_VALUE: &str = "proj_test";

    fn project_id() -> ProjectId {
        ProjectId::new(PROJECT_ID_VALUE.to_string())
    }

    /// Python `_mission(domain)`：写入 mission 声明的审计域。
    fn mission(domain: AuditDomain) -> Mission {
        let mut mission = Mission::new(project_id(), "Assess the target".to_string());
        mission
            .target
            .insert("url".to_string(), "https://example.test".to_string());
        mission.metadata.insert(
            "audit_domain".to_string(),
            serde_json::Value::String(domain.as_str().to_string()),
        );
        mission
    }

    /// Python `_run(mission_id)`：`max_total_steps=64`。
    fn run(mission_id: Option<&str>) -> AuditRun {
        let mut run = AuditRun::new(project_id());
        run.mission_id = mission_id.map(|id| MissionId::new(id.to_string()));
        run.max_total_steps = 64;
        run
    }

    /// Python `_tool(name, mission_id)`。
    fn tool(name: &str, mission_id: Option<&str>) -> ToolInvocation {
        let mut invocation = ToolInvocation::new(name.to_string(), "scan".to_string());
        invocation.project_id = Some(project_id());
        invocation.mission_id = mission_id.map(|id| MissionId::new(id.to_string()));
        invocation
    }

    /// Python `_evidence(kind)`。
    fn evidence(kind: EvidenceKind) -> Evidence {
        Evidence::new(project_id(), kind, "unit evidence".to_string())
    }

    /// Python `_branch(kind, steps_used=…)`。
    fn branch(kind: &str, steps_used: i64) -> Branch {
        let mut branch = Branch::new(
            project_id(),
            MissionId::new("mission_x".to_string()),
            format!("Branch {kind}"),
            "some untested falsifiable hypothesis about the surface".to_string(),
        );
        branch.steps_used = steps_used;
        branch.metadata.insert(
            "branch_kind".to_string(),
            serde_json::Value::String(kind.to_string()),
        );
        branch
    }

    /// Python `test_coverage_checker_counts_real_signals_and_blind_spots`。
    #[test]
    fn counts_real_signals_and_blind_spots() {
        let url_mission = mission(AuditDomain::ContentDiscovery);
        let audit_run = run(Some("mission_x"));
        let branches = [branch("url.surface_mapping", 2)];
        let evidence_list = [evidence(EvidenceKind::SourceSnippet)];
        let invocations = [tool("nuclei", Some("mission_x"))];
        let coverage = CoverageChecker.check(&CoverageInput {
            branches: &branches,
            evidence: &evidence_list,
            tool_invocations: &invocations,
            ..CoverageInput::new(&url_mission, &audit_run)
        });

        // nuclei 调用 → WEB_DAST；消耗过步数的分支 → WEB_RECON；
        // 证据类型 → WEB_SAST（已行使即计入）。
        assert!(coverage.covered_domains.contains(&AuditDomain::WebDast));
        assert!(coverage.covered_domains.contains(&AuditDomain::WebRecon));
        assert!(coverage.covered_domains.contains(&AuditDomain::WebSast));
        // 声明过的域没有任何信号 → 盲区。
        assert!(coverage.blind_spots.contains(&AuditDomain::ContentDiscovery));
        // 未声明也未触达的域是 NotRelevant，不是盲区。
        assert!(!coverage.blind_spots.contains(&AuditDomain::AssetRecon));
        let entry = coverage
            .entries
            .iter()
            .find(|entry| entry.domain == AuditDomain::WebDast)
            .unwrap_or_else(|| panic!("WEB_DAST 条目必须存在"));
        assert_eq!(entry.tool_invocation_count, 1);
        assert_eq!(entry.signal_sources, vec!["nuclei".to_string()]);
        assert!(coverage.summary.contains("blind spots remain"));
    }

    /// Python `test_coverage_checker_ignores_intent_only_and_unknown_signals`。
    #[test]
    fn ignores_intent_only_and_unknown_signals() {
        let url_mission = mission(AuditDomain::WebRecon);
        let audit_run = run(None);
        let branches = [branch("url.surface_mapping", 0)];
        let invocations = [tool("totally_unknown_tool", None)];
        let coverage = CoverageChecker.check(&CoverageInput {
            branches: &branches,
            tool_invocations: &invocations,
            ..CoverageInput::new(&url_mission, &audit_run)
        });

        // 声明过的域只有未派发的分支 = 意图，不是行使过的攻击面。
        assert!(coverage.blind_spots.contains(&AuditDomain::WebRecon));
        // 未知工具不编造覆盖信号。
        assert!(coverage.covered_domains.is_empty());
        let web_recon = coverage
            .entries
            .iter()
            .find(|entry| entry.domain == AuditDomain::WebRecon)
            .unwrap_or_else(|| panic!("WEB_RECON 条目必须存在"));
        assert_eq!(web_recon.branch_count, 0);
    }

    /// Python `test_coverage_checker_respects_module_domain_binding`。
    #[test]
    fn respects_module_domain_binding() {
        let url_mission = mission(AuditDomain::WebRecon);
        let audit_run = run(None);
        let mut module = ModuleConfig::new("Custom DAST module".to_string());
        module.id = ModuleId::new("mod_x".to_string());
        module.domain = ModuleDomain::WebDast;
        let mut invocation =
            ToolInvocation::new("custom_dast_runner".to_string(), "scan".to_string());
        invocation.project_id = Some(project_id());
        invocation.module_id = Some("mod_x".to_string());
        let modules = [module];
        let invocations = [invocation];

        let coverage = CoverageChecker.check(&CoverageInput {
            tool_invocations: &invocations,
            modules: &modules,
            ..CoverageInput::new(&url_mission, &audit_run)
        });

        // module_id 绑定优先于工具名目录：自定义工具按模块域计覆盖。
        assert!(coverage.covered_domains.contains(&AuditDomain::WebDast));
        assert!(!coverage.blind_spots.contains(&AuditDomain::WebDast));
    }

    /// Python `test_coverage_checker_full_source_coverage_has_no_blind_spots`。
    #[test]
    fn full_source_coverage_has_no_blind_spots() {
        let source_mission = mission(AuditDomain::WebSast);
        let audit_run = run(None);
        let branches = [branch("source.dependency_config", 1)];
        let invocations = [tool("semgrep", None)];
        let coverage = CoverageChecker.check(&CoverageInput {
            branches: &branches,
            tool_invocations: &invocations,
            ..CoverageInput::new(&source_mission, &audit_run)
        });

        // 声明的 WEB_SAST 已被 semgrep 证据覆盖 → 无盲区。
        assert!(coverage.blind_spots.is_empty());
        assert!(coverage.covered_domains.contains(&AuditDomain::WebSast));
        assert!(coverage.summary.contains("were exercised"));
    }
}
