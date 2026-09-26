import type {
  ApiFinding,
  ApiProject,
  FindingView,
  ApiAuditRun,
  AuditRunView,
  ApiToolInvocation,
  AgentActivity,
  DashboardSummary,
  EvidenceQuality,
  ToolCoverage,
  NextAction,
  ApiModule,
  ModuleHealthResult,
  ToolCatalogEntry,
} from './types';

export function toFindingView(apiFinding: ApiFinding, project?: ApiProject): FindingView {
  const confidenceMap: Record<string, number> = {
    confirmed: 0.95,
    needs_review: 0.65,
    candidate: 0.5,
    duplicate: 0.2,
    false_positive: 0.1,
    dismissed: 0.1,
  };

  const engine = apiFinding.rule_id
    ? apiFinding.rule_id.split('.')[0]
    : apiFinding.produced_by_task_id ? 'task' : 'unknown';

  return {
    ...apiFinding,
    project_name: project?.name || 'Unknown',
    engine: engine,
    evidence_count: apiFinding.evidence_ids?.length || 0,
    confidence: apiFinding.confidence ?? (confidenceMap[apiFinding.status] || 0.5),
  };
}

export function toAuditRunView(apiRun: ApiAuditRun, project?: ApiProject, projectFindings?: ApiFinding[]): AuditRunView {
  const solver = apiRun.config?.audit_domains?.[0] || 'Unknown';

  return {
    ...apiRun,
    project_name: project?.name || 'Unknown',
    current_solver: solver,
    finding_count: projectFindings?.length || 0,
    steps_used: apiRun.steps_used || 0,
    max_total_steps: apiRun.max_total_steps || 50,
    started_at_display: apiRun.started_at || apiRun.created_at || new Date().toISOString(),
  };
}

export function toolInvocationToActivity(invocation: ApiToolInvocation): AgentActivity {
  return {
    id: invocation.id,
    actor: invocation.tool_name,
    action: invocation.input_summary || 'Invoked tool',
    timestamp: invocation.started_at,
    result: invocation.status === 'ok' ? 'success' : invocation.status === 'error' || invocation.status === 'timeout' || invocation.status === 'denied' ? 'error' : 'info',
    details: invocation.error || invocation.output_summary,
  };
}

export function buildDashboardData(
  projects: ApiProject[],
  findings: ApiFinding[],
  runs: ApiAuditRun[],
  invocations: ApiToolInvocation[],
  modules: ApiModule[] = [],
  moduleHealth: Record<string, ModuleHealthResult> = {},
  toolCatalog: ToolCatalogEntry[] = []
): DashboardSummary {
  const projectMap = new Map(projects.map(p => [p.id, p]));
  
  const highValueFindings = findings.map(f => toFindingView(f, projectMap.get(f.project_id)));
  
  const activeRuns = runs
    .filter(r => ['pending', 'running', 'reviewing', 'reporting'].includes(r.status))
    .map(r => toAuditRunView(r, projectMap.get(r.project_id), findings.filter(f => f.project_id === r.project_id)));

  const agentActivity = invocations.slice(0, 10).map(toolInvocationToActivity);

  const evidenceQuality: EvidenceQuality = {
    findings_without_evidence: findings.filter(f => !f.evidence_ids || f.evidence_ids.length === 0).length,
    evidence_without_fact: null,
    needs_review_count: findings.filter(f => f.status === 'needs_review').length,
    confirmed_count: findings.filter(f => f.status === 'confirmed').length,
    tool_error_count: invocations.filter(i => i.status === 'error' || i.status === 'timeout').length,
  };

  const catalogById = new Map(toolCatalog.map((tool) => [tool.id.toLowerCase(), tool]));
  const expectedTools = [
    'semgrep',
    'nuclei',
    'subfinder',
    'naabu',
    'httpx',
    'katana',
    'jsluice',
    'dalfox',
    'codeql',
    'ffuf',
    'feroxbuster',
    'gobuster',
    'wappalyzergo',
    'EHole',
    'afrog',
    'fscan',
  ].map((id) => {
    const catalogEntry = catalogById.get(id.toLowerCase());
    return {
      id,
      name: catalogEntry?.name || displayToolName(id),
      catalogEntry,
    };
  });

  const modulesByTool = new Map<string, ApiModule>();
  for (const module of modules) {
    for (const toolName of module.tool_allowlist || []) {
      if (!modulesByTool.has(toolName) || module.enabled) {
        modulesByTool.set(toolName, module);
      }
    }
  }

  const toolCoverage: ToolCoverage[] = expectedTools.map((tool) => {
    if (tool.catalogEntry) {
      const detection = tool.catalogEntry.detection;
      const isPlanned = tool.catalogEntry.adapter_status === 'planned';
      return {
        id: tool.id,
        name: tool.name,
        status: isPlanned ? 'planned' : (detection.available ? 'available' : detection.availability),
        message: detection.available
          ? detection.executable_path || undefined
          : undefined,
        source: detection.source || undefined,
        adapter_status: tool.catalogEntry.adapter_status,
      };
    }

    const module = modulesByTool.get(tool.id);
    if (!module) {
      return {
        id: tool.id,
        name: tool.name,
        status: 'not_cataloged',
        message: 'dashboard.coverage.notCataloged',
      };
    }

    const health = moduleHealth[module.id];
    if (!module.enabled) {
      return {
        id: tool.id,
        name: tool.name,
        status: 'configured',
        module_id: module.id,
        module_enabled: module.enabled,
      };
    }

    if (!health) {
      return {
        id: tool.id,
        name: tool.name,
        status: 'configured',
        module_id: module.id,
        module_enabled: module.enabled,
      };
    }

    return {
      id: tool.id,
      name: tool.name,
      status: health.ok ? 'available' : 'unavailable',
      message: health.message,
      module_id: module.id,
      module_enabled: module.enabled,
    };
  });

  const nextActions: NextAction[] = [];
  if (evidenceQuality.needs_review_count && evidenceQuality.needs_review_count > 0) {
    nextActions.push({ id: 'na1', title: 'reviewFindings', type: 'review', priority: 'high', target_id: 'inbox', count: evidenceQuality.needs_review_count });
  }
  if (evidenceQuality.tool_error_count && evidenceQuality.tool_error_count > 0) {
    nextActions.push({ id: 'na2', title: 'checkToolErrors', type: 'investigate', priority: 'medium', target_id: 'tools', count: evidenceQuality.tool_error_count });
  }

  const projectsWithDomain = projects.filter(p => !runs.some(r => r.project_id === p.id) && (p.target?.domain || p.target?.host || p.target?.target));
  if (projectsWithDomain.length > 0) {
    nextActions.push({ id: 'na3', title: 'Suggest Asset Recon', type: 'investigate', priority: 'high', target_id: projectsWithDomain[0].id, count: projectsWithDomain.length });
  }

  const webProjects = projects.filter(p => p.audit_domain === 'web_dast' || p.audit_domain === 'web_recon' || p.target?.url);
  if (webProjects.length > 0) {
    nextActions.push({ id: 'na4', title: 'Suggest Web Recon / Nuclei', type: 'investigate', priority: 'high', target_id: webProjects[0].id, count: webProjects.length });
  }

  const repoProjects = projects.filter(p => p.target?.repo_path || p.target?.source_root);
  if (repoProjects.length > 0) {
    nextActions.push({ id: 'na5', title: 'Suggest Semgrep / CodeQL', type: 'investigate', priority: 'medium', target_id: repoProjects[0].id, count: repoProjects.length });
  }

  return {
    high_value_findings: highValueFindings,
    active_runs: activeRuns,
    agent_activity: agentActivity,
    evidence_quality: evidenceQuality,
    tool_coverage: toolCoverage,
    next_actions: nextActions,
    projects,
    runs,
    findings,
    invocations,
  };
}

function displayToolName(id: string): string {
  const names: Record<string, string> = {
    semgrep: 'Semgrep',
    nuclei: 'Nuclei',
    subfinder: 'Subfinder',
    naabu: 'Naabu',
    httpx: 'Httpx',
    katana: 'Katana',
    jsluice: 'Jsluice',
    dalfox: 'Dalfox',
    codeql: 'CodeQL',
    ffuf: 'ffuf',
    feroxbuster: 'feroxbuster',
    gobuster: 'gobuster',
    wappalyzergo: 'wappalyzergo',
    EHole: 'EHole',
    afrog: 'afrog',
    fscan: 'fscan',
  };
  return names[id] || id;
}
