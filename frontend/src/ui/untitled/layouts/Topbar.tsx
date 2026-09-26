import { Link, useNavigate } from '@tanstack/react-router';
import { useQuery } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { Bell, Settings, Languages } from 'lucide-react';
import { api } from '@/lib/api';
import { useLocale } from '@/hooks/useLocale';
import { useProviderHealthById } from '@/hooks/use-provider-health';
import {
  getProviderReadiness,
  isProviderConfigComplete,
} from '@/lib/provider-types';
import { DecisionGateBadge } from '@/components/decision/DecisionGateBadge';
import { CommandSearch } from '@/components/layout/CommandSearch';
import { ScrollArea } from '@/components/ui/scroll-area';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { Badge } from '@/ui/untitled/primitives/Badge';
import { Button } from '@/ui/untitled/primitives/Button';

/**
 * Untitled UI Topbar — global command surface.
 *
 * Search is a command palette trigger (no flashy focus fill animation).
 */
export function Topbar() {
  const { t } = useTranslation();
  const { label, nextLabel, toggleLocale } = useLocale();
  const navigate = useNavigate();

  const { data: decisionGates = [] } = useQuery({
    queryKey: ['decision-gates', 'pending'],
    queryFn: () => api.getDecisionGates({ status: 'pending' }),
    refetchInterval: 10000,
  });

  const { data: providers = [] } = useQuery({
    queryKey: ['providers', 'topbar'],
    queryFn: () => api.getProviders(),
    staleTime: 60_000,
    refetchInterval: 60_000,
  });

  const pendingGates = decisionGates.filter((g) => g.status === 'pending');
  const alertCount = pendingGates.length;

  const providerHealthById = useProviderHealthById(providers);
  const configuredProviders = providers.filter(isProviderConfigComplete).length;
  const verifiedProviders = providers.filter(
    (provider) =>
      getProviderReadiness(
        provider,
        providerHealthById[provider.id],
      ).usableForTextGeneration,
  ).length;

  const readinessTone =
    verifiedProviders > 0
      ? 'success'
      : configuredProviders > 0
        ? 'warning'
        : 'neutral';
  const readinessLabel =
    verifiedProviders > 0
      ? t('topbar.providersReady', { count: verifiedProviders })
      : configuredProviders > 0
        ? t('topbar.providersConfigured', { count: configuredProviders })
        : t('topbar.noProvidersConfigured');

  return (
    <header className="relative z-40 flex h-16 shrink-0 items-center justify-between gap-4 bg-card px-6">
      <div className="relative z-40 min-w-0 flex-1">
        <CommandSearch />
      </div>

      <div className="flex items-center gap-1.5">
        <div className="mr-1.5 hidden items-center gap-2 border-r border-border pr-3 md:flex">
          <Badge tone={readinessTone} variant="soft" dot size="sm">
            {readinessLabel}
          </Badge>
        </div>

        <Button
          variant="ghost"
          size="sm"
          className="h-8 gap-1.5 font-medium text-muted-foreground hover:text-foreground"
          onClick={toggleLocale}
          aria-label={t('topbar.languageSwitch', { next: nextLabel })}
          title={t('topbar.languageSwitch', { next: nextLabel })}
        >
          <Languages className="h-4 w-4" />
          <span className="hidden sm:inline">{label}</span>
        </Button>

        <Popover>
          <PopoverTrigger asChild>
            <Button
              variant="ghost"
              size="icon"
              className="relative h-9 w-9 text-muted-foreground hover:text-foreground"
              aria-label={t('decisionGate.pendingDecisions')}
            >
              <Bell className="h-4.5 w-4.5 h-[18px] w-[18px]" />
              {alertCount > 0 && (
                <span className="absolute right-1.5 top-1.5 flex h-4 min-w-4 items-center justify-center rounded-full bg-danger px-1 text-[10px] font-semibold text-white ring-2 ring-card">
                  {alertCount > 9 ? '9+' : alertCount}
                </span>
              )}
            </Button>
          </PopoverTrigger>
          <PopoverContent className="w-80 overflow-hidden rounded-xl p-0 shadow-float" align="end">
            <div className="flex items-center justify-between border-b border-border px-4 py-3">
              <span className="text-sm font-semibold tracking-tight">
                {t('decisionGate.pendingDecisions')}
              </span>
              <span className="text-xs text-muted-foreground">
                {t('topbar.pendingCount', { count: alertCount })}
              </span>
            </div>
            <ScrollArea className="max-h-[300px]">
              {alertCount === 0 ? (
                <div className="p-6 text-center text-sm text-muted-foreground">
                  {t('decisionGate.noPendingDecisions')}
                </div>
              ) : (
                <div className="flex flex-col">
                  {pendingGates.map((gate) => (
                    <div
                      key={gate.id}
                      className="cursor-pointer border-b border-border p-4 transition-colors last:border-0 hover:bg-muted/50"
                      onClick={() =>
                        void navigate({ to: `/projects/${gate.project_id}` })
                      }
                    >
                      <div className="mb-1 flex items-start justify-between gap-2">
                        <span className="line-clamp-2 pr-1 text-sm font-medium">
                          {gate.question}
                        </span>
                        <div className="flex shrink-0 flex-col items-end gap-1">
                          <DecisionGateBadge
                            kind={gate.kind}
                            className="px-1.5 py-0 text-[10px]"
                          />
                          <DecisionGateBadge
                            severity={gate.severity}
                            className="px-1.5 py-0 text-[10px]"
                          />
                        </div>
                      </div>
                      <div className="mt-1.5 flex items-center text-xs text-muted-foreground">
                        <span className="truncate font-mono text-[11px]">
                          {t('common.runId')}: {gate.audit_run_id}
                        </span>
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </ScrollArea>
          </PopoverContent>
        </Popover>

        <Button
          asChild
          variant="ghost"
          size="icon"
          className="h-9 w-9 text-muted-foreground hover:text-foreground"
        >
          <Link to="/settings" aria-label={t('topbar.openSettings')} title={t('nav.settings')}>
            <Settings className="h-[18px] w-[18px]" />
          </Link>
        </Button>

        <div className="ml-1 flex h-8 w-8 items-center justify-center rounded-full bg-primary/10 text-xs font-semibold text-primary ring-1 ring-primary/15">
          A
        </div>
      </div>
    </header>
  );
}
