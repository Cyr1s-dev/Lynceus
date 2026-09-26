import { useQuery } from '@tanstack/react-query';
import { api } from '@/lib/api';
import { buildDashboardData } from '@/lib/mappers';

export function useDashboardData() {
  return useQuery({
    queryKey: ['dashboard', 'summary'],
    queryFn: async () => {
      const projects = await api.getProjects();
      const modules = await api.getModules();
      const moduleHealth = await api.getModuleHealth().catch(() => ({}));
      const toolCatalog = await api.getToolCatalog().catch(() => []);
      
      const findingsPromises = projects.map(p => api.getProjectFindings(p.id));
      const runsPromises = projects.map(p => api.getProjectAuditRuns(p.id));
      const invocationsPromises = projects.map(p => api.getProjectToolInvocations(p.id));
      
      const findings = await Promise.all(findingsPromises).then(arr => arr.flat());
      const runs = await Promise.all(runsPromises).then(arr => arr.flat());
      const invocations = await Promise.all(invocationsPromises).then(arr => arr.flat());
      
      invocations.sort((a, b) => new Date(b.started_at).getTime() - new Date(a.started_at).getTime());
      
      return buildDashboardData(projects, findings, runs, invocations, modules, moduleHealth, toolCatalog);
    },
    retry: 1,
  });
}

// Aliases for backward compatibility in components

export function useHighValueFindings() {
  const { data, isLoading, isError, error, refetch } = useDashboardData();
  return { data: data?.high_value_findings, isLoading, isError, error, refetch };
}

export function useActiveAuditRuns() {
  const { data, isLoading, isError, error, refetch } = useDashboardData();
  return { data: data?.active_runs, isLoading, isError, error, refetch };
}

export function useAgentActivity() {
  const { data, isLoading, isError, error, refetch } = useDashboardData();
  return { data: data?.agent_activity, isLoading, isError, error, refetch };
}

export function useEvidenceQuality() {
  const { data, isLoading, isError, error, refetch } = useDashboardData();
  return { data: data?.evidence_quality, isLoading, isError, error, refetch };
}

export function useToolCoverage() {
  const { data, isLoading, isError, error, refetch } = useDashboardData();
  return { data: data?.tool_coverage, isLoading, isError, error, refetch };
}

export function useNextActions() {
  const { data, isLoading, isError, error, refetch } = useDashboardData();
  return { data: data?.next_actions, isLoading, isError, error, refetch };
}
