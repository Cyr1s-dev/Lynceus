import { useMemo, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { Progress } from '@/components/ui/progress';
import { Input } from '@/ui/untitled';
import { formatDistanceToNow } from 'date-fns';
import { enUS, zhCN } from 'date-fns/locale';
import {
  CircleDashed,
  PauseCircle,
  CheckCircle2,
  XCircle,
  Search,
  AlertCircle,
  ScanSearch,
  UserCheck,
  type LucideIcon,
} from 'lucide-react';
import { Button } from '@/ui/untitled';
import { useTranslation } from 'react-i18next';
import { useLocale } from '@/hooks/useLocale';
import { formatStatus } from '@/lib/i18n-formatters';
import { DecisionGateDialog } from '@/components/decision/DecisionGateDialog';
import { api } from '@/lib/api';
import { toAuditRunView } from '@/lib/mappers';
import type { AuditRunView, RunStatus } from '@/lib/types';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/ui/untitled';
import {
  PageHeader,
  Section,
  Badge,
  EmptyState,
  EmptyStateAction,
  getStatusToken,
  missionStatusTone,
  type StatusTone,
} from '@/ui/untitled';

const STATUS_ICONS: Partial<Record<string, LucideIcon>> = {
  running: ScanSearch,
  paused: PauseCircle,
  waiting_for_decision: UserCheck,
  completed: CheckCircle2,
  failed: XCircle,
};

function getErrorMessage(error: unknown, fallback: string): string {
  if (error instanceof Error) return error.message;
  return fallback;
}

export function AuditRunsPage() {
  const { t } = useTranslation();
  const { locale } = useLocale();
  const [statusFilter, setStatusFilter] = useState<RunStatus | 'all'>('all');
  const [search, setSearch] = useState('');
  const [selectedRun, setSelectedRun] = useState<{ id: string; projectId: string } | null>(null);
  const { data: runs, isLoading, isError, error, refetch } = useQuery({
    queryKey: ['audit-runs', 'all'],
    queryFn: async (): Promise<AuditRunView[]> => {
      const projects = await api.getProjects();
      const runsByProject = await Promise.all(
        projects.map(async (project) => ({
          project,
          runs: await api.getProjectAuditRuns(project.id),
          findings: await api.getProjectFindings(project.id),
        })),
      );
      return runsByProject
        .flatMap(({ project, runs, findings }) => runs.map((run) => toAuditRunView(run, project, findings)))
        .sort((a, b) => new Date(b.started_at_display).getTime() - new Date(a.started_at_display).getTime());
    },
  });

  const filteredRuns = useMemo(() => {
    const q = search.trim().toLowerCase();
    return (runs ?? []).filter((run) => {
      if (statusFilter !== 'all' && run.status !== statusFilter) return false;
      if (!q) return true;
      return (
        run.project_name?.toLowerCase().includes(q) ||
        run.current_solver?.toLowerCase().includes(q) ||
        run.id.toLowerCase().includes(q)
      );
    });
  }, [runs, statusFilter, search]);

  const dateLocale = locale === 'zh-CN' ? zhCN : enUS;

  if (isError) {
    return (
      <EmptyState
        variant="card"
        icon={<AlertCircle className="h-6 w-6 text-danger" />}
        title={t('auditRuns.failedToLoad')}
        description={getErrorMessage(error, t('errors.unknown'))}
        action={<EmptyStateAction onClick={() => refetch()}>{t('common.retry')}</EmptyStateAction>}
      />
    );
  }

  return (
    <div className="page-stack">
      <PageHeader
        icon={<ScanSearch className="h-5 w-5" />}
        title={t('auditRuns.title')}
        description={t('auditRuns.description')}
        actions={
          <div className="flex w-full flex-col gap-3 sm:w-auto sm:flex-row">
            <Select value={statusFilter} onValueChange={(value) => setStatusFilter(value as RunStatus | 'all')}>
              <SelectTrigger className="w-full sm:w-52">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">{t('auditRuns.allStatuses')}</SelectItem>
                {(
                  [
                    'pending',
                    'running',
                    'paused',
                    'waiting_for_decision',
                    'reviewing',
                    'reporting',
                    'completed',
                    'failed',
                  ] as RunStatus[]
                ).map((status) => (
                  <SelectItem key={status} value={status}>
                    {formatStatus(t, status)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <div className="relative w-full sm:w-72">
              <Search className="pointer-events-none absolute left-2.5 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                placeholder={t('auditRuns.searchPlaceholder')}
                className="h-9 pl-9 text-sm"
              />
            </div>
          </div>
        }
      />

      {isLoading ? (
        <EmptyState variant="bare" compact title={t('auditRuns.loading')} />
      ) : filteredRuns.length === 0 ? (
        <EmptyState
          variant="card"
          icon={<ScanSearch className="h-5 w-5" />}
          title={t('auditRuns.noRuns')}
          description={t('auditRuns.noRunsDescription')}
        />
      ) : (
        <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
          {filteredRuns.map((run) => {
            const tone: StatusTone = missionStatusTone(run.status);
            const token = getStatusToken(tone);
            const Icon = STATUS_ICONS[run.status.toLowerCase()] ?? CircleDashed;
            const progress = run.max_total_steps
              ? Math.min(100, Math.round((run.steps_used / run.max_total_steps) * 100))
              : 0;
            return (
              <Section
                key={run.id}
                icon={<Icon className={`h-4 w-4 ${token.dot}`} />}
                title={t('auditRuns.runName', { name: run.project_name })}
                description={
                  <span className="inline-flex items-center gap-1.5">
                    <span className="font-medium text-foreground">
                      {run.current_solver || t('common.initializing')}
                    </span>
                    <span className="text-muted-foreground">•</span>
                    <span>
                      {t('auditRuns.startedRelative', {
                        time: formatDistanceToNow(new Date(run.started_at_display), {
                          addSuffix: true,
                          locale: dateLocale,
                        }),
                      })}
                    </span>
                  </span>
                }
                actions={<Badge tone={tone} dot>{formatStatus(t, run.status)}</Badge>}
              >
                <div className="section-stack">
                  <div className="space-y-2">
                    <div className="flex items-center justify-between text-sm">
                      <span className="font-medium text-foreground">{t('common.progress')}</span>
                      <span className="text-muted-foreground">
                        {t('auditRuns.stepsProgress', {
                          used: run.steps_used,
                          total: run.max_total_steps,
                          percent: progress,
                        })}
                      </span>
                    </div>
                    <Progress value={progress} className="h-1.5" indicatorClassName={token.bar} />
                  </div>

                  <div className="grid grid-cols-2 gap-4 border-t border-border pt-4">
                    <div>
                      <p className="mb-1 text-xs text-muted-foreground">{t('auditRuns.findings')}</p>
                      <p className="text-lg font-semibold tabular-nums text-foreground">{run.finding_count}</p>
                    </div>
                    <div>
                      <p className="mb-1 text-xs text-muted-foreground">{t('common.engine')}</p>
                      <p className="text-sm font-medium text-foreground">{t('auditRuns.multiAgentEngine')}</p>
                    </div>
                  </div>

                  {run.status === 'waiting_for_decision' && (
                    <div className="flex items-center justify-between gap-3 border-t border-border pt-4">
                      <span className="flex items-center gap-2 text-sm font-medium text-warning-foreground">
                        <AlertCircle className="h-4 w-4" />
                        {t('decisionGate.requiresHumanConfirmation')}
                      </span>
                      <Button
                        size="sm"
                        variant="outline"
                        onClick={() => setSelectedRun({ id: run.id, projectId: run.project_id })}
                      >
                        {t('decisionGate.viewDecision')}
                      </Button>
                    </div>
                  )}
                </div>
              </Section>
            );
          })}
        </div>
      )}

      {selectedRun && (
        <DecisionGateDialog
          runId={selectedRun.id}
          projectId={selectedRun.projectId}
          open={!!selectedRun}
          onOpenChange={(open) => {
            if (!open) setSelectedRun(null);
          }}
        />
      )}
    </div>
  );
}
