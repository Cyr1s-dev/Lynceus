import { useMemo, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { Input } from '@/ui/untitled';
import { formatDistanceToNow } from 'date-fns';
import { ShieldAlert, Search, AlertCircle, ListTree, Network, Rows3 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { formatFindingStatus, formatSeverity } from '@/lib/i18n-formatters';
import { FindingDetailDrawer } from '@/components/dashboard/FindingDetailDrawer';
import { FindingAssetTree } from '@/components/dashboard/FindingAssetTree';
import { api, getApiErrorMessage } from '@/lib/api';
import type { ApiFinding, ApiProject, FindingView, Mission } from '@/lib/types';
import { cn } from '@/lib/utils';
import {
  PageHeader,
  TableShell,
  Badge,
  EmptyState,
  EmptyStateAction,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  severityTone,
  getStatusToken,
  SEVERITY_ORDER,
  Tabs,
  TabsList,
  TabsTrigger,
} from '@/ui/untitled';

const STATUSES = ['candidate', 'needs_review', 'confirmed', 'false_positive', 'duplicate', 'fixed'] as const;
type View = 'flat' | 'grouped' | 'asset';

function cweOf(finding: ApiFinding): string {
  const cwe = finding.cwe;
  if (Array.isArray(cwe)) return cwe[0] ?? '';
  return cwe ?? '';
}

/** ApiFinding → FindingView（抽屉需要 project_name/engine/evidence_count/confidence）。 */
function toView(finding: ApiFinding, projectName: string): FindingView {
  return {
    ...finding,
    project_name: projectName,
    engine: finding.rule_id ?? '—',
    evidence_count: finding.evidence_ids?.length ?? 0,
    confidence: finding.confidence ?? 0.8,
  };
}

export function FindingsInboxPage() {
  const { t } = useTranslation();
  const [view, setView] = useState<View>('flat');
  const [severity, setSeverity] = useState<string>('all');
  const [status, setStatus] = useState<string>('all');
  const [typeFilter, setTypeFilter] = useState<string>('all');
  const [missionFilter, setMissionFilter] = useState<string>('all');
  const [assetKey, setAssetKey] = useState<string | null>(null);
  const [search, setSearch] = useState('');
  const [selectedFinding, setSelectedFinding] = useState<FindingView | null>(null);
  const [isDrawerOpen, setIsDrawerOpen] = useState(false);

  const projectsQuery = useQuery({ queryKey: ['projects'], queryFn: api.getProjects });
  const projects = projectsQuery.data ?? [];

  // 每个项目的 finding 数：默认落到「有数据的」项目，而非第一个（常是删空的历史项目）。
  const countsQuery = useQuery({
    queryKey: ['project-finding-counts'],
    queryFn: async () => {
      const entries = await Promise.all(
        projects.map(async (p) => {
          const n = await api.getProjectFindings(p.id).then((r) => r.length).catch(() => 0);
          return [p.id, n] as const;
        }),
      );
      return Object.fromEntries(entries) as Record<string, number>;
    },
    enabled: projects.length > 0,
  });
  const defaultProjectId = useMemo(() => {
    const counts = countsQuery.data ?? {};
    return (
      [...projects].sort((a, b) => (counts[b.id] ?? 0) - (counts[a.id] ?? 0))[0]?.id ??
      projects[0]?.id ??
      null
    );
  }, [projects, countsQuery.data]);
  const activeProjectId = defaultProjectId;
  const activeProject = projects.find((p: ApiProject) => p.id === activeProjectId) ?? null;

  const findingsQuery = useQuery({
    queryKey: ['project-findings', activeProjectId],
    queryFn: () => api.getProjectFindings(activeProjectId as string),
    enabled: activeProjectId !== null,
  });
  const missionsQuery = useQuery({
    queryKey: ['missions', activeProjectId],
    queryFn: () => api.getMissions(activeProjectId ?? undefined),
    enabled: activeProjectId !== null,
  });
  const treeQuery = useQuery({
    queryKey: ['finding-asset-tree', activeProjectId],
    queryFn: () => api.getFindingAssetTree(activeProjectId as string),
    enabled: activeProjectId !== null && view === 'asset',
  });

  const findings = useMemo(() => findingsQuery.data ?? [], [findingsQuery.data]);
  const missions = missionsQuery.data ?? [];
  // 存活 mission 集合：finding 是项目级、删 mission 不级联删，mission_id 会悬空；
  // 这些「已删任务的 finding」不该出现在风险看板，按是否存在对应 mission 过滤掉。
  const liveMissionIds = useMemo(() => new Set(missions.map((m: Mission) => m.id)), [missions]);
  const missionTitle = (id: string | null | undefined): string => {
    if (!id) return t('riskBoard.unassigned');
    return missions.find((m: Mission) => m.id === id)?.title || id.slice(0, 12);
  };

  const severityCounts = useMemo(() => {
    const map: Record<string, number> = {};
    for (const f of findings) map[(f.severity || 'info').toLowerCase()] = (map[(f.severity || 'info').toLowerCase()] ?? 0) + 1;
    return map;
  }, [findings]);
  const totalFindings = findings.length;

  const typeOptions = useMemo(() => {
    const set = new Set<string>();
    for (const f of findings) {
      const cwe = cweOf(f);
      if (cwe) set.add(cwe);
    }
    return [...set].sort();
  }, [findings]);

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    return findings.filter((f) => {
      if (severity !== 'all' && (f.severity || '').toLowerCase() !== severity) return false;
      if (status !== 'all' && f.status !== status) return false;
      if (typeFilter !== 'all' && cweOf(f) !== typeFilter) return false;
      if (missionFilter !== 'all' && (f.mission_id ?? '') !== missionFilter) return false;
      if (f.mission_id && !liveMissionIds.has(f.mission_id)) return false;
      if (assetKey) {
        const node = (treeQuery.data?.nodes ?? []).find((n) => n.key === assetKey);
        const needle = (node?.value ?? '').toLowerCase();
        if (needle) {
          const hay = `${f.title} ${f.source_label ?? ''} ${f.sink_label ?? ''} ${f.description ?? ''}`.toLowerCase();
          if (!hay.includes(needle)) return false;
        }
      }
      if (!q) return true;
      return (
        f.title?.toLowerCase().includes(q) ||
        (f.source_label ?? '').toLowerCase().includes(q) ||
        (f.sink_label ?? '').toLowerCase().includes(q) ||
        (f.description ?? '').toLowerCase().includes(q)
      );
    });
  }, [findings, severity, status, typeFilter, missionFilter, search, assetKey, treeQuery.data, liveMissionIds]);

  const grouped = useMemo(() => {
    const map = new Map<string, ApiFinding[]>();
    for (const f of filtered) {
      const key = f.mission_id ?? '__none__';
      const list = map.get(key);
      if (list) list.push(f);
      else map.set(key, [f]);
    }
    return [...map.entries()].sort((a, b) => b[1].length - a[1].length);
  }, [filtered]);

  const projectName = activeProject?.name ?? '';

  const severityCard = (sev: string) => {
    const token = getStatusToken(severityTone(sev));
    const active = severity === sev;
    return (
      <button
        key={sev}
        type="button"
        onClick={() => setSeverity(active ? 'all' : sev)}
        className={cn(
          'rounded-lg border bg-card px-3 py-2.5 text-left transition-colors',
          active ? 'border-primary/50 ring-1 ring-primary/20' : 'border-border hover:border-primary/30',
        )}
      >
        <div className="flex items-center gap-1.5">
          <span className={cn('size-2 shrink-0 rounded-full', token.bar)} />
          <span className="truncate text-xs text-muted-foreground">{formatSeverity(t, sev)}</span>
        </div>
        <span className="mt-1 block text-2xl font-semibold tabular-nums text-foreground">
          {severityCounts[sev] ?? 0}
        </span>
      </button>
    );
  };

  const filterBar = (
    <div className="flex flex-wrap items-center gap-2">
      <div className="relative w-56">
        <Search className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
        <Input
          value={search}
          onChange={(event) => setSearch(event.target.value)}
          placeholder={t('riskBoard.searchPlaceholder')}
          className="h-9 pl-9 text-sm"
        />
      </div>
      <Select value={status} onValueChange={setStatus}>
        <SelectTrigger className="h-9 w-36"><SelectValue /></SelectTrigger>
        <SelectContent>
          <SelectItem value="all">{t('riskBoard.allStatus')}</SelectItem>
          {STATUSES.map((s) => (
            <SelectItem key={s} value={s}>{formatFindingStatus(t, s)}</SelectItem>
          ))}
        </SelectContent>
      </Select>
      <Select value={typeFilter} onValueChange={setTypeFilter}>
        <SelectTrigger className="h-9 w-36"><SelectValue /></SelectTrigger>
        <SelectContent>
          <SelectItem value="all">{t('riskBoard.allTypes')}</SelectItem>
          {typeOptions.map((c) => (
            <SelectItem key={c} value={c}>{c}</SelectItem>
          ))}
        </SelectContent>
      </Select>
      <Select value={missionFilter} onValueChange={setMissionFilter}>
        <SelectTrigger className="h-9 w-44"><SelectValue /></SelectTrigger>
        <SelectContent>
          <SelectItem value="all">{t('riskBoard.allMissions')}</SelectItem>
          {missions.map((m: Mission) => (
            <SelectItem key={m.id} value={m.id}>{m.title || m.id.slice(0, 12)}</SelectItem>
          ))}
        </SelectContent>
      </Select>
    </div>
  );

  const viewTabs = (
    <Tabs
      value={view}
      onValueChange={(value) => setView(value as View)}
      className="w-max"
    >
      <TabsList animated indicatorClassName="bg-card" className="h-9 bg-muted/60 p-1">
        <TabsTrigger value="flat" className="gap-1.5">
          <Rows3 className="size-4" />
          {t('riskBoard.viewFlat')}
        </TabsTrigger>
        <TabsTrigger value="grouped" className="gap-1.5">
          <ListTree className="size-4" />
          {t('riskBoard.viewGrouped')}
        </TabsTrigger>
        <TabsTrigger value="asset" className="gap-1.5">
          <Network className="size-4" />
          {t('riskBoard.viewAsset')}
        </TabsTrigger>
      </TabsList>
    </Tabs>
  );

  const findingsTable = (rows: ApiFinding[]) => (
    <TableShell fill className="flex-1" loading={findingsQuery.isLoading}
      empty={!findingsQuery.isLoading && rows.length === 0 ? (
        <EmptyState variant="bare" compact icon={<ShieldAlert className="size-5" />}
          title={t('findings.emptyTitle')} description={t('riskBoard.emptyFiltered')} />
      ) : undefined}
    >
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead className="w-[300px]">{t('findings.titleColumn')}</TableHead>
            <TableHead>{t('findings.severity')}</TableHead>
            <TableHead>{t('riskBoard.mission')}</TableHead>
            <TableHead>{t('findings.engine')}</TableHead>
            <TableHead>{t('findings.evidence')}</TableHead>
            <TableHead className="text-right">{t('findings.updatedAt')}</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {rows.map((finding) => (
            <TableRow key={finding.id} className="cursor-pointer"
              onClick={() => { setSelectedFinding(toView(finding, projectName)); setIsDrawerOpen(true); }}
            >
              <TableCell className="font-medium text-foreground">{finding.title}</TableCell>
              <TableCell><Badge tone={severityTone(finding.severity)}>{formatSeverity(t, finding.severity)}</Badge></TableCell>
              <TableCell className="truncate text-muted-foreground">{missionTitle(finding.mission_id)}</TableCell>
              <TableCell className="text-muted-foreground">{finding.rule_id ?? '—'}</TableCell>
              <TableCell>
                <span className="inline-flex items-center justify-center rounded bg-muted px-2 py-0.5 text-xs font-medium text-muted-foreground">
                  {t('findings.evidenceItems', { count: finding.evidence_ids?.length ?? 0 })}
                </span>
              </TableCell>
              <TableCell className="text-right text-xs text-muted-foreground">
                {formatDistanceToNow(new Date(finding.updated_at), { addSuffix: true })}
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </TableShell>
  );

  if (projectsQuery.isError) {
    return (
      <EmptyState variant="card" icon={<AlertCircle className="size-6 text-danger" />}
        title={t('findings.failedToLoad')} description={getApiErrorMessage(projectsQuery.error)}
        action={<EmptyStateAction onClick={() => projectsQuery.refetch()}>{t('common.retry')}</EmptyStateAction>} />
    );
  }

  return (
    <div className="page-stack h-full">
      <PageHeader
        icon={<ShieldAlert className="size-5" />}
        title={t('findings.title')}
        description={t('findings.description')}
      />

      {/* 跨任务汇总：按严重程度分档（点击卡片即按该严重度筛选）。 */}
      <div className="grid grid-cols-3 gap-3 sm:grid-cols-6">
        <button type="button" onClick={() => setSeverity('all')}
          className={cn('rounded-lg border bg-card px-3 py-2.5 text-left transition-colors',
            severity === 'all' ? 'border-primary/50 ring-1 ring-primary/20' : 'border-border hover:border-primary/30')}
        >
          <div className="flex items-center gap-1.5">
            <span className="size-2 shrink-0 rounded-full bg-neutral" />
            <span className="truncate text-xs text-muted-foreground">{t('findings.statTotal')}</span>
          </div>
          <span className="mt-1 block text-2xl font-semibold tabular-nums text-foreground">{totalFindings}</span>
        </button>
        {SEVERITY_ORDER.map((sev) => severityCard(sev))}
      </div>

      <div className="flex flex-wrap items-center justify-between gap-2">
        {viewTabs}
        {filterBar}
      </div>

      {activeProjectId === null ? (
        <EmptyState variant="bare" compact icon={<ShieldAlert className="size-5" />} title={t('riskBoard.noProject')} />
      ) : view === 'flat' ? (
        findingsTable(filtered)
      ) : view === 'grouped' ? (
        <div className="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto">
          {grouped.length === 0 && (
            <EmptyState variant="bare" compact icon={<ShieldAlert className="size-5" />} title={t('findings.emptyTitle')} description={t('riskBoard.emptyFiltered')} />
          )}
          {grouped.map(([mid, rows]) => (
            <div key={mid} className="rounded-xl border border-border bg-card">
              <div className="flex items-center justify-between border-b border-border px-3 py-2">
                <span className="text-sm font-semibold text-foreground">{missionTitle(mid === '__none__' ? null : mid)}</span>
                <Badge tone="neutral" size="sm" className="tabular-nums">{rows.length}</Badge>
              </div>
              {findingsTable(rows)}
            </div>
          ))}
        </div>
      ) : (
        <div className="grid min-h-0 flex-1 gap-4 lg:grid-cols-[280px_minmax(0,1fr)]">
          <div className="rounded-xl border border-border bg-card p-3">
            <FindingAssetTree
              nodes={treeQuery.data?.nodes ?? []}
              selected={assetKey}
              onSelect={setAssetKey}
              loading={treeQuery.isLoading}
              findingTotal={treeQuery.data?.finding_total ?? 0}
              onRefresh={() => treeQuery.refetch()}
            />
          </div>
          {findingsTable(filtered)}
        </div>
      )}

      <FindingDetailDrawer finding={selectedFinding} isOpen={isDrawerOpen} onClose={() => setIsDrawerOpen(false)} />
    </div>
  );
}