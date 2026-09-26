import { useMemo, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { api } from '@/lib/api';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { Input } from '@/ui/untitled';
import { formatDistanceToNow } from 'date-fns';
import {
  Search,
  TerminalSquare,
  FileJson,
  SearchX,
  ShieldOff,
  CheckCircle2,
  XCircle,
  Clock,
  Ban,
  ChevronDown,
} from 'lucide-react';
import { useTranslation } from 'react-i18next';

import { formatStatus } from '@/lib/i18n-formatters';
import type { ApiToolInvocation } from '@/lib/types';
import { ToolInvocationDetailDrawer } from '@/components/dashboard/ToolInvocationDetailDrawer';
import {
  PageContainer,
  PageHeader,
  FilterTabs,
  TableShell,
  Badge,
  Button,
  EmptyState,
  ErrorState,
  toolStatusTone,
  typeScale,
} from '@/ui/untitled';
import { cn } from '@/lib/utils';

const STATUS_FILTERS = ['all', 'ok', 'error', 'timeout', 'denied'] as const;

function getErrorMessage(error: unknown, fallback: string): string {
  if (error instanceof Error) return error.message;
  return fallback;
}

export function ToolInvocationsPage() {
  const { data: invocations, isLoading, isError, error, refetch } = useQuery({
    queryKey: ['tool-invocations'],
    queryFn: api.getToolInvocations,
  });
  const { data: policy } = useQuery({
    queryKey: ['worker-command-policy'],
    queryFn: api.getWorkerCommandPolicy,
    staleTime: 5 * 60_000,
  });

  const { t } = useTranslation();
  const [activeFilter, setActiveFilter] = useState<string>('all');
  const [searchQuery, setSearchQuery] = useState('');
  const [rulesOpen, setRulesOpen] = useState(false);
  const [selected, setSelected] = useState<ApiToolInvocation | null>(null);
  const [drawerOpen, setDrawerOpen] = useState(false);

  const counts = useMemo(() => {
    const map: Record<string, number> = { all: invocations?.length ?? 0 };
    for (const inv of invocations ?? []) {
      const key = inv.status.toLowerCase();
      map[key] = (map[key] ?? 0) + 1;
    }
    return map;
  }, [invocations]);

  const filteredInvocations = useMemo(() => {
    const q = searchQuery.trim().toLowerCase();
    return (invocations ?? []).filter((inv) => {
      const matchesFilter = activeFilter === 'all' || inv.status.toLowerCase() === activeFilter;
      const matchesSearch =
        !q ||
        inv.tool_name?.toLowerCase().includes(q) ||
        inv.module_id?.toLowerCase().includes(q) ||
        inv.input_summary?.toLowerCase().includes(q) ||
        inv.output_summary?.toLowerCase().includes(q) ||
        inv.error?.toLowerCase().includes(q);
      return matchesFilter && matchesSearch;
    });
  }, [invocations, activeFilter, searchQuery]);

  const hasActiveFilters = activeFilter !== 'all' || searchQuery.trim() !== '';
  const clearFilters = () => {
    setActiveFilter('all');
    setSearchQuery('');
  };

  const tabs = STATUS_FILTERS.map((key) => ({
    key,
    label: key === 'all' ? t('findings.all') : formatStatus(t, key),
    count: counts[key] ?? 0,
  }));

  const overview = [
    { key: 'all', icon: TerminalSquare, tone: 'neutral', label: t('toolInvocations.overview.total') },
    { key: 'ok', icon: CheckCircle2, tone: 'success', label: t('toolInvocations.overview.allowed') },
    { key: 'denied', icon: ShieldOff, tone: 'danger', label: t('toolInvocations.overview.denied') },
    { key: 'error', icon: XCircle, tone: 'danger', label: t('toolInvocations.overview.failed') },
    { key: 'timeout', icon: Clock, tone: 'warning', label: t('toolInvocations.overview.timeout') },
  ] as const;

  return (
    <PageContainer>
      <PageHeader
        icon={<TerminalSquare className="h-5 w-5" />}
        title={t('toolInvocations.title')}
        description={t('toolInvocations.description')}
        count={counts.all}
      />

      {isError ? (
        <ErrorState
          title={t('toolInvocations.failedToLoad')}
          description={getErrorMessage(error, t('errors.unknown'))}
          onRetry={() => refetch()}
          retryLabel={t('common.retry')}
        />
      ) : (
        <>
          {/* 执行总览：放行 / 拦截 / 出错 / 超时 一屏看全。 */}
          <div className="grid grid-cols-3 gap-3 sm:grid-cols-5">
            {overview.map(({ key, icon: Icon, label }) => (
              <div key={key} className="rounded-lg border border-border bg-card px-3 py-2.5">
                <div className="flex items-center gap-1.5">
                  <Icon className="size-3.5 shrink-0 text-muted-foreground" />
                  <span className="truncate text-xs text-muted-foreground">{label}</span>
                </div>
                <span className="mt-1 block text-2xl font-semibold tabular-nums text-foreground">
                  {counts[key] ?? 0}
                </span>
              </div>
            ))}
          </div>

          {/* 拦截规则：默认折叠成一行摘要，点开才看细节（避免一堵红 chip 墙压在顶部）。 */}
          <section className="rounded-xl border border-border bg-card">
            <button
              type="button"
              onClick={() => setRulesOpen((open) => !open)}
              aria-expanded={rulesOpen}
              className="flex w-full items-center gap-2 px-4 py-2.5 text-left transition-colors hover:bg-muted/40"
            >
              <Ban className="size-4 shrink-0 text-danger" />
              <span className="text-sm font-semibold text-foreground">{t('toolInvocations.rules.title')}</span>
              {policy && (
                <span className="truncate text-xs text-muted-foreground">
                  {t('toolInvocations.rules.summary', {
                    denied: policy.denied_prefixes.length,
                    prompt: policy.prompt_only_rules.length,
                  })}
                </span>
              )}
              <span className="ml-auto flex shrink-0 items-center gap-1 text-xs text-muted-foreground">
                {t('toolInvocations.rules.subtitle')}
                <ChevronDown className={cn('size-4 transition-transform', rulesOpen && 'rotate-180')} />
              </span>
            </button>
            {rulesOpen && policy && (
              <div className="space-y-3 border-t border-border px-4 py-3">
                <div>
                  <p className="mb-1.5 text-xs font-medium text-muted-foreground">
                    {t('toolInvocations.rules.deniedPrefixes')}
                  </p>
                  <div className="flex flex-wrap gap-1.5">
                    {policy.denied_prefixes.map((prefix) => (
                      <span
                        key={prefix}
                        className="rounded-md border border-danger/30 bg-danger/5 px-2 py-0.5 font-mono text-[11px] text-danger"
                      >
                        {prefix}
                      </span>
                    ))}
                  </div>
                </div>
                <div>
                  <p className="mb-1.5 text-xs font-medium text-muted-foreground">
                    {t('toolInvocations.rules.promptOnly')}
                  </p>
                  <ul className="list-inside list-disc space-y-1 text-xs leading-relaxed text-muted-foreground">
                    {policy.prompt_only_rules.map((rule) => (
                      <li key={rule}>{rule}</li>
                    ))}
                  </ul>
                </div>
              </div>
            )}
          </section>

          <FilterTabs
            tabs={tabs}
            value={activeFilter}
            onChange={setActiveFilter}
            actions={
              <div className="relative w-64">
                <Search className="pointer-events-none absolute left-2.5 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
                <Input
                  value={searchQuery}
                  onChange={(e) => setSearchQuery(e.target.value)}
                  placeholder={t('toolInvocations.searchPlaceholder')}
                  className="h-8 pl-9 text-sm"
                />
              </div>
            }
          />

          <TableShell
            fill
            className="flex-1"
            loading={isLoading}
            empty={
              !isLoading && filteredInvocations.length === 0 ? (
                hasActiveFilters ? (
                  <EmptyState
                    variant="bare"
                    compact
                    icon={<SearchX className="h-5 w-5" />}
                    title={t('common.noMatchingResults')}
                    action={
                      <Button variant="outline" size="sm" onClick={clearFilters}>
                        {t('common.clearFilters')}
                      </Button>
                    }
                  />
                ) : (
                  <EmptyState
                    variant="bare"
                    icon={<TerminalSquare className="h-6 w-6" />}
                    title={t('toolInvocations.emptyTitle')}
                    description={t('toolInvocations.emptyDescription')}
                  />
                )
              ) : undefined
            }
          >
            <Table>
              <TableHeader>
                <TableRow className="hover:bg-transparent">
                  <TableHead className="label-spec h-10 w-[220px] px-4">{t('toolInvocations.toolName')}</TableHead>
                  <TableHead className="label-spec h-10 px-4">{t('toolInvocations.status')}</TableHead>
                  <TableHead className="label-spec h-10 w-[160px] px-4">{t('toolInvocations.missionOrBranch')}</TableHead>
                  <TableHead className="label-spec h-10 w-[90px] px-4">{t('toolInvocations.duration')}</TableHead>
                  <TableHead className="label-spec h-10 w-[140px] px-4">{t('toolInvocations.startedAt')}</TableHead>
                  <TableHead className="label-spec h-10 w-[90px] px-4 text-right">{t('toolInvocations.evidence')}</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {filteredInvocations.map((inv) => (
                  <TableRow
                    key={inv.id}
                    className="h-11 cursor-pointer hover:bg-muted/30"
                    onClick={() => {
                      setSelected(inv);
                      setDrawerOpen(true);
                    }}
                  >
                    <TableCell className={cn('px-4 py-2 font-medium text-foreground', typeScale.listItem)}>
                      <div className="flex items-center gap-2">
                        <span className="truncate">{inv.tool_name}</span>
                        {inv.module_id && (
                          <Badge tone="info" title={`${t('common.moduleId')}: ${inv.module_id}`}>
                            {inv.module_id.startsWith('mod_') ? inv.module_id.substring(4) : inv.module_id}
                          </Badge>
                        )}
                      </div>
                    </TableCell>
                    <TableCell className="px-4 py-2">
                      <Badge tone={toolStatusTone(inv.status)}>
                        {formatStatus(t, inv.status)}
                      </Badge>
                    </TableCell>
                    <TableCell className={cn('px-4 py-2 tabular-nums text-muted-foreground', typeScale.code)}>
                      {inv.mission_id || inv.branch_id
                        ? (inv.mission_id ?? inv.branch_id ?? '').slice(0, 8)
                        : '—'}
                    </TableCell>
                    <TableCell className={cn('px-4 py-2 tabular-nums text-muted-foreground', typeScale.metadata)}>
                      {inv.duration_ms ? `${inv.duration_ms}ms` : '—'}
                    </TableCell>
                    <TableCell className={cn('px-4 py-2 text-muted-foreground', typeScale.metadata)}>
                      {formatDistanceToNow(new Date(inv.started_at), { addSuffix: true })}
                    </TableCell>
                    <TableCell className="px-4 py-2 text-right">
                      {inv.artifact_paths && inv.artifact_paths.length > 0 ? (
                        <span className={cn('inline-flex items-center gap-1 text-muted-foreground', typeScale.metadata)}>
                          <FileJson className="h-3 w-3" /> {inv.artifact_paths.length}
                        </span>
                      ) : (
                        <span className="text-muted-foreground">—</span>
                      )}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </TableShell>
        </>
      )}

      <ToolInvocationDetailDrawer
        invocation={selected}
        isOpen={drawerOpen}
        onClose={() => setDrawerOpen(false)}
      />
    </PageContainer>
  );
}