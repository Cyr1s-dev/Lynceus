import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  Bot,
  CheckCircle2,
  ChevronDown,
  ChevronUp,
  HelpCircle,
  KeyRound,
  Loader2,
  MoreHorizontal,
  RefreshCw,
  Terminal,
  Unplug,
} from 'lucide-react';
import { api, getApiErrorMessage } from '@/lib/api';
import type { ProviderConfigResponse } from '@/lib/provider-types';
import type {
  UpsertWorkerRuntimeProfileRequest,
  WorkerAvailability,
  WorkerExecutionEnvironment,
  WorkerProbe,
  WorkerRuntimeProfile,
  WorkerRuntimeType,
} from '@/lib/types';
import { cn } from '@/lib/utils';
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DropdownMenu,
  DropdownMenuItem,
  DropdownMenuSeparator,
  ErrorState,
  Label,
  PageContainer,
  PageHeader,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Switch,
} from '@/ui/untitled';
import { useToast } from '@/hooks/use-toast';
import { useTranslation } from 'react-i18next';

const RUNTIME_ORDER: WorkerRuntimeType[] = [
  'claude_code',
  'codex',
  'pi',

  'deepseek_harness',
];

/** Text color per availability — the status dot inherits it via `border-current`. */
const STATUS_TEXT: Record<WorkerAvailability, string> = {
  available: 'text-emerald-600 dark:text-emerald-400',
  not_installed: 'text-muted-foreground',
  unavailable: 'text-amber-600 dark:text-amber-400',
  unsupported: 'text-sky-600 dark:text-sky-400',
  not_ready: 'text-amber-600 dark:text-amber-400',
  error: 'text-red-600 dark:text-red-500',
  probing: 'text-muted-foreground',
};

function RuntimeIcon({ runtime, className }: { runtime: WorkerRuntimeType; className?: string }) {
  if (runtime === 'claude_code') return <Terminal className={className} />;
  if (runtime === 'codex') return <Bot className={className} />;
  if (runtime === 'pi') return <Bot className={className} />;
  return <HelpCircle className={className} />;
}

interface ProfileForm {
  runtime: WorkerRuntimeType;
  connectionId: string;
  modelOverride: string;
  agentPreset: string;
  executionEnvironment: WorkerExecutionEnvironment;
  maxConcurrency: string;
  timeoutSeconds: string;
  enabled: boolean;
}

function profileToForm(
  runtime: WorkerRuntimeType,
  profile?: WorkerRuntimeProfile,
): ProfileForm {
  return {
    runtime,
    connectionId: profile?.connection_id ?? '',
    modelOverride: profile?.model_override ?? '',
    agentPreset:
      (profile?.runtime_options?.agent_preset as string | undefined) ?? '',
    executionEnvironment: profile?.execution_environment ?? 'local',
    maxConcurrency: profile ? String(profile.max_concurrency) : '',
    timeoutSeconds: profile ? String(profile.timeout_seconds) : '',
    enabled: profile?.enabled ?? true,
  };
}

/** Empty string means "use the backend default"; otherwise a positive integer is required. */
function isPositiveIntegerOrEmpty(value: string): boolean {
  if (value === '') return true;
  return /^[1-9]\d*$/.test(value);
}

/** Compact numeric field with an up/down stepper, mirroring the reference inspector. */
function NumField({
  id,
  value,
  onChange,
  placeholder,
  invalid,
}: {
  id: string;
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  invalid?: boolean;
}) {
  const step = (delta: number) => {
    const current = Number.parseInt(value, 10);
    const base = Number.isFinite(current) && current > 0 ? current : 0;
    onChange(String(Math.max(1, base + delta)));
  };
  return (
    <div
      className={cn(
        'flex h-9 items-stretch overflow-hidden rounded-md border border-input bg-transparent',
        'focus-within:border-primary focus-within:ring-2 focus-within:ring-primary/15',
        invalid && 'border-destructive focus-within:border-destructive focus-within:ring-destructive/15',
      )}
    >
      <input
        id={id}
        inputMode="numeric"
        value={value}
        placeholder={placeholder}
        onChange={(event) => onChange(event.target.value)}
        className="min-w-0 flex-1 bg-transparent px-2.5 text-sm text-foreground outline-none placeholder:text-muted-foreground/60"
      />
      <span className="flex w-6 flex-col border-l border-input">
        <button
          type="button"
          tabIndex={-1}
          aria-label="+1"
          onClick={() => step(1)}
          className="flex flex-1 items-center justify-center text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
        >
          <ChevronUp className="size-3" />
        </button>
        <button
          type="button"
          tabIndex={-1}
          aria-label="-1"
          onClick={() => step(-1)}
          className="flex flex-1 items-center justify-center border-t border-input text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
        >
          <ChevronDown className="size-3" />
        </button>
      </span>
    </div>
  );
}

function FormRow({ label, htmlFor, children }: { label: string; htmlFor?: string; children: ReactNode }) {
  return (
    <div className="grid grid-cols-[88px_minmax(0,1fr)] items-center gap-2">
      <Label htmlFor={htmlFor} className="text-xs text-muted-foreground">
        {label}
      </Label>
      {children}
    </div>
  );
}

/** 模型覆盖：输入 + 模型列表下拉（复用"添加模型"的 discover-models 读取）。 */
function ModelOverrideField({
  value,
  onChange,
  models,
  loading,
  error,
  onRefetch,
  refreshing,
}: {
  value: string;
  onChange: (value: string) => void;
  models: string[];
  loading: boolean;
  error: boolean;
  onRefetch: () => void;
  refreshing: boolean;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const boxRef = useRef<HTMLDivElement | null>(null);

  // 点击下拉外任意处收起。
  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      if (boxRef.current && !boxRef.current.contains(event.target as Node)) {
        setOpen(false);
      }
    };
    window.addEventListener('pointerdown', onPointerDown);
    return () => window.removeEventListener('pointerdown', onPointerDown);
  }, [open]);

  const query = value.trim().toLowerCase();
  const filtered = useMemo(
    () =>
      (query
        ? models.filter((model) => model.toLowerCase().includes(query))
        : models
      ).slice(0, 50),
    [models, query],
  );

  return (
    <div className="relative" ref={boxRef}>
      <div
        className={cn(
          'flex h-9 items-stretch overflow-hidden rounded-md border border-input bg-transparent',
          'focus-within:border-primary focus-within:ring-2 focus-within:ring-primary/15',
        )}
      >
        <input
          value={value}
          placeholder={t('workerRuntimes.editor.modelOverridePlaceholder')}
          onChange={(event) => {
            onChange(event.target.value);
            setOpen(true);
          }}
          onFocus={() => {
            if (models.length > 0) setOpen(true);
          }}
          autoComplete="off"
          className="min-w-0 flex-1 bg-transparent px-2.5 text-sm text-foreground outline-none placeholder:text-muted-foreground/60"
        />
        <button
          type="button"
          onClick={onRefetch}
          disabled={refreshing || loading}
          title={t('workerRuntimes.editor.refreshModels')}
          aria-label={t('workerRuntimes.editor.refreshModels')}
          className="flex w-8 shrink-0 items-center justify-center border-l border-input text-muted-foreground transition-colors hover:bg-muted hover:text-foreground disabled:opacity-60"
        >
          {refreshing || loading ? (
            <Loader2 className="size-3.5 animate-spin" />
          ) : (
            <RefreshCw className="size-3.5" />
          )}
        </button>
      </div>

      {open && filtered.length > 0 && (
        <ul
          role="listbox"
          aria-label={t('workerRuntimes.editor.modelOverride')}
          className="absolute z-30 mt-1 max-h-48 w-full overflow-y-auto rounded-md border border-border bg-card py-1 shadow-md"
        >
          {filtered.map((model) => (
            <li key={model}>
              <button
                type="button"
                onClick={() => {
                  onChange(model);
                  setOpen(false);
                }}
                className="w-full truncate px-2.5 py-1.5 text-left font-mono text-xs text-foreground transition-colors hover:bg-muted"
              >
                {model}
              </button>
            </li>
          ))}
        </ul>
      )}
      {open && !loading && models.length === 0 && !error && (
        <p className="absolute z-30 mt-1 w-full rounded-md border border-border bg-card px-2.5 py-1.5 text-xs text-muted-foreground shadow-md">
          {t('workerRuntimes.editor.modelsEmpty')}
        </p>
      )}
      {error ? (
        <p className="mt-1 text-[11px] leading-relaxed text-muted-foreground/80">
          {t('workerRuntimes.editor.modelsFetchFailed')}
        </p>
      ) : models.length > 0 ? (
        <p className="mt-1 text-[11px] text-muted-foreground/70">
          {t('workerRuntimes.editor.modelsHint', { count: models.length })}
        </p>
      ) : null}
    </div>
  );
}

export function WorkerRuntimesPage() {
  const { t } = useTranslation();
  const { toast } = useToast();
  const queryClient = useQueryClient();
  const [selectedRuntime, setSelectedRuntime] = useState<WorkerRuntimeType | null>(null);
  const [form, setForm] = useState<ProfileForm | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<WorkerRuntimeProfile | null>(null);

  const probesQuery = useQuery({
    queryKey: ['worker-runtimes'],
    queryFn: api.listWorkerRuntimes,
    refetchInterval: 30_000,
  });
  const profilesQuery = useQuery({
    queryKey: ['worker-runtime-profiles'],
    queryFn: api.listWorkerRuntimeProfiles,
  });
  const providersQuery = useQuery({
    queryKey: ['providers'],
    queryFn: () => api.getProviders(),
  });
  const agentPresetsQuery = useQuery({
    queryKey: ['agent-presets'],
    queryFn: api.listAgentPresets,
  });
  const gatewayQuery = useQuery({
    queryKey: ['gateway-status'],
    queryFn: api.getGatewayStatus,
    refetchInterval: 30_000,
  });

  const refresh = useMutation({
    mutationFn: api.refreshWorkerRuntimes,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['worker-runtimes'] });
    },
    onError: (error) => {
      toast({ title: getApiErrorMessage(error), variant: 'destructive' });
    },
  });

  const saveProfile = useMutation({
    mutationFn: (input: UpsertWorkerRuntimeProfileRequest) =>
      api.upsertWorkerRuntimeProfile(input),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['worker-runtime-profiles'] });
      void queryClient.invalidateQueries({ queryKey: ['worker-runtimes'] });
      toast({ title: t('workerRuntimes.editor.saved') });
    },
    onError: (error) => {
      toast({ title: getApiErrorMessage(error), variant: 'destructive' });
    },
  });

  const removeProfile = useMutation({
    mutationFn: (profileId: string) => api.deleteWorkerRuntimeProfile(profileId),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['worker-runtime-profiles'] });
      void queryClient.invalidateQueries({ queryKey: ['worker-runtimes'] });
      setDeleteTarget(null);
      toast({ title: t('workerRuntimes.remove.removed') });
    },
    onError: (error) => {
      toast({ title: getApiErrorMessage(error), variant: 'destructive' });
    },
  });

  const probes = useMemo(() => probesQuery.data ?? [], [probesQuery.data]);
  const profiles = useMemo(() => profilesQuery.data ?? [], [profilesQuery.data]);
  const providers = useMemo(() => providersQuery.data ?? [], [providersQuery.data]);

  // 连接 = 系统设置里的 Provider：下拉直接选，卡片/检查器显示名称而非裸 ID。
  const providersById = useMemo(() => {
    const map = new Map<string, ProviderConfigResponse>();
    for (const provider of providers) {
      map.set(provider.id, provider);
    }
    return map;
  }, [providers]);

  const providerLabel = (connectionId: string | null): string => {
    if (!connectionId) return '';
    const provider = providersById.get(connectionId);
    if (!provider) return connectionId;
    return provider.model
      ? `${provider.name} · ${provider.model}`
      : provider.name;
  };

  // LiteLLM 网关运行中且该 runtime 声明了 agents 绑定时，实际路由经网关
  // 改写（base_url → 127.0.0.1:port，模型 → 别名；上游密钥只进网关进程）。
  // 卡片直接展示这一路由事实，避免「绑定的 Provider ≠ 实际上游」的误读。
  const gatewayRouteByRuntime = useMemo(() => {
    const map = new Map<WorkerRuntimeType, { alias: string; port: number | null }>();
    const status = gatewayQuery.data;
    if (!status?.running) return map;
    for (const [runtime, binding] of Object.entries(status.agents)) {
      if (!(RUNTIME_ORDER as string[]).includes(runtime)) continue;
      map.set(runtime as WorkerRuntimeType, { alias: binding.alias, port: status.port });
    }
    return map;
  }, [gatewayQuery.data]);

  // 选中供应商后自动读取其模型列表（复用"添加模型"的 discover-models）。
  const selectedConnectionId = form?.connectionId ?? '';
  const modelsQuery = useQuery({
    queryKey: ['provider-models', selectedConnectionId],
    queryFn: () => api.discoverProviderModels({ provider_id: selectedConnectionId }),
    enabled: selectedConnectionId !== '' && providersById.has(selectedConnectionId),
    staleTime: 5 * 60_000,
    retry: false,
  });
  const modelOptions =
    modelsQuery.data?.status === 'ok' ? modelsQuery.data.models : [];
  const modelsUnavailable =
    modelsQuery.isError || (modelsQuery.data !== undefined && modelsQuery.data.status !== 'ok');

  // 固定几个 runtime 永远先渲染（Worker 池就这几个），探测结果异步回填。
  // 未探测到的 runtime 用 availability='probing' 占位，避免整页卡在
  // `--version` 探测上（进页面不再加载半天）。后端若返回 RUNTIME_ORDER 之
  // 外的新 runtime，追加在末尾不丢。
  const orderedProbes = useMemo(() => {
    const byRuntime = new Map(probes.map((probe) => [probe.runtime, probe]));
    const rank = (runtime: WorkerRuntimeType) => {
      const index = RUNTIME_ORDER.indexOf(runtime);
      return index === -1 ? RUNTIME_ORDER.length : index;
    };
    const roster: WorkerProbe[] = RUNTIME_ORDER.map(
      (runtime) =>
        byRuntime.get(runtime) ?? {
          runtime,
          availability: 'probing',
          version: null,
          detail: null,
          capabilities: [],
          checked_at: '',
        },
    );
    for (const probe of probes) {
      if (!(RUNTIME_ORDER as string[]).includes(probe.runtime)) {
        roster.push(probe);
      }
    }
    return roster.sort(
      (a, b) => rank(a.runtime) - rank(b.runtime) || a.runtime.localeCompare(b.runtime),
    );
  }, [probes]);

  const profileByRuntime = useMemo(() => {
    const map = new Map<WorkerRuntimeType, WorkerRuntimeProfile>();
    for (const profile of profiles) {
      const existing = map.get(profile.runtime_type);
      if (!existing || (!existing.enabled && profile.enabled)) {
        map.set(profile.runtime_type, profile);
      }
    }
    return map;
  }, [profiles]);

  // Auto-select the first roster entry and drop a stale selection.
  useEffect(() => {
    if (selectedRuntime && orderedProbes.some((probe) => probe.runtime === selectedRuntime)) return;
    setSelectedRuntime(orderedProbes[0]?.runtime ?? null);
  }, [orderedProbes, selectedRuntime]);

  const selectedProbe = selectedRuntime
    ? orderedProbes.find((probe) => probe.runtime === selectedRuntime) ?? null
    : null;
  const selectedProfile = selectedRuntime
    ? profileByRuntime.get(selectedRuntime)
    : undefined;

  // Sync the inspector form when the selection or its profile identity
  // changes. Key on the profile ID (stable across background refetches) so a
  // refetch neither clobbers unsaved edits nor skips the initial population.
  const selectedProfileId = selectedProfile?.id ?? null;
  useEffect(() => {
    setForm(
      selectedRuntime
        ? profileToForm(selectedRuntime, selectedProfile)
        : null,
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedRuntime, selectedProfileId]);

  const statsReady = !probesQuery.isLoading && !profilesQuery.isLoading;
  const enabledCount = profiles.filter((profile) => profile.enabled).length;
  const totalConcurrency = profiles.reduce(
    (sum, profile) => sum + (Number.isFinite(profile.max_concurrency) ? profile.max_concurrency : 0),
    0,
  );
  const pendingConfig = orderedProbes.filter(
    (probe) => !profileByRuntime.has(probe.runtime),
  ).length;

  const availabilityLabel = (availability: WorkerAvailability) =>
    t(`workerRuntimes.availability.${availability}`);

  const hintFor = (probe: WorkerProbe) => {
    if (probe.availability === 'probing') return '';
    if (probe.availability === 'not_installed') return t('workerRuntimes.hints.notInstalled');
    if (probe.availability === 'not_ready') return t('workerRuntimes.hints.notReady');
    if (probe.availability === 'unsupported')
      return probe.detail || t('workerRuntimes.hints.unsupported');
    return probe.detail ?? '';
  };

  const toggleEnabled = (profile: WorkerRuntimeProfile) => {
    saveProfile.mutate({
      runtime_type: profile.runtime_type,
      connection_id: profile.connection_id,
      model_override: profile.model_override,
      execution_environment: profile.execution_environment,
      max_concurrency: profile.max_concurrency,
      timeout_seconds: profile.timeout_seconds,
      enabled: !profile.enabled,
    });
  };

  const formValid =
    form !== null &&
    form.connectionId.trim().length > 0 &&
    isPositiveIntegerOrEmpty(form.maxConcurrency) &&
    isPositiveIntegerOrEmpty(form.timeoutSeconds);

  const saveFromInspector = () => {
    if (!form || !formValid) return;
    saveProfile.mutate({
      runtime_type: form.runtime,
      connection_id: form.connectionId.trim(),
      model_override: form.modelOverride.trim() ? form.modelOverride.trim() : null,
      agent_preset: form.agentPreset.trim() ? form.agentPreset.trim() : null,
      execution_environment: form.executionEnvironment,
      ...(form.maxConcurrency !== '' ? { max_concurrency: Number(form.maxConcurrency) } : {}),
      ...(form.timeoutSeconds !== '' ? { timeout_seconds: Number(form.timeoutSeconds) } : {}),
      enabled: form.enabled,
    });
  };

  const inspectorOpen = form !== null && selectedProbe !== null;

  return (
    <PageContainer>
      <PageHeader
        icon={<Bot className="h-5 w-5" />}
        title={t('workerRuntimes.title')}
        description={t('workerRuntimes.description')}
        count={orderedProbes.length}
        actions={
          <Button
            variant="outline"
            size="sm"
            onClick={() => refresh.mutate()}
            disabled={refresh.isPending}
          >
            {refresh.isPending ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <RefreshCw className="h-4 w-4" />
            )}
            {refresh.isPending ? t('workerRuntimes.checking') : t('workerRuntimes.checkAll')}
          </Button>
        }
      />

      {refresh.isError && (
        <ErrorState
          title={t('workerRuntimes.checkFailed')}
          description={getApiErrorMessage(refresh.error)}
          onRetry={() => refresh.mutate()}
          retryLabel={t('common.retry')}
        />
      )}

      {profilesQuery.isError && (
        <ErrorState
          title={t('workerRuntimes.profilesLoadFailed')}
          description={getApiErrorMessage(profilesQuery.error)}
          onRetry={() => profilesQuery.refetch()}
          retryLabel={t('common.retry')}
        />
      )}

      <div className="grid grid-cols-1 items-start gap-4 lg:grid-cols-[minmax(0,1fr)_360px]">
        {/* 左：Worker 阵容。外层 muted 面板给内部 bg-card 卡片提供对比。 */}
        <section className="min-w-0 rounded-xl border border-border bg-muted/30 p-4 sm:p-5">
          <p className="mb-3 font-mono text-xs tabular-nums text-muted-foreground">
            {statsReady
              ? t('workerRuntimes.statsSummary', {
                  enabled: enabledCount,
                  configured: profiles.length,
                  concurrency: totalConcurrency,
                  pending: pendingConfig,
                })
              : '—'}
          </p>

          {probesQuery.isError && (
            <ErrorState
              title={t('workerRuntimes.subsystemUnavailable')}
              description={getApiErrorMessage(probesQuery.error)}
              onRetry={() => probesQuery.refetch()}
              retryLabel={t('common.retry')}
            />
          )}

          <div className="grid grid-cols-1 gap-2.5 md:grid-cols-2">
              {orderedProbes.map((probe, index) => {
                const profile = profileByRuntime.get(probe.runtime);
                const displayName = t(`workerRuntimes.display.${probe.runtime}`);
                const reason = probe.availability === 'available' ? '' : hintFor(probe);
                const isSelected = selectedRuntime === probe.runtime;
                const gatewayRoute = gatewayRouteByRuntime.get(probe.runtime);
                return (
                  <article
                    key={probe.runtime}
                    role="button"
                    tabIndex={0}
                    aria-pressed={isSelected}
                    onClick={() => setSelectedRuntime(probe.runtime)}
                    onKeyDown={(event) => {
                      if (event.key === 'Enter' || event.key === ' ') {
                        event.preventDefault();
                        setSelectedRuntime(probe.runtime);
                      }
                    }}
                    className={cn(
                      'group relative cursor-pointer overflow-hidden rounded-lg border bg-card shadow-xs',
                      'transition-all hover:-translate-y-0.5 hover:shadow-md',
                      'focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary',
                      profile && !profile.enabled && 'opacity-60',
                      isSelected
                        ? 'border-primary/50 bg-primary/[0.04] shadow-md ring-1 ring-primary/15'
                        : 'border-border',
                    )}
                  >
                    {/* 卡片头：序号 / 图标 / 名称 / 开关 / 菜单（参考 wroster-card-head） */}
                    <div className="flex items-center gap-2 px-3 pb-2 pt-3">
                      <span className="w-5 shrink-0 font-mono text-[10px] leading-none text-muted-foreground/70">
                        {String(index + 1).padStart(2, '0')}
                      </span>
                      <span className="flex size-8 shrink-0 items-center justify-center rounded-lg border border-border bg-muted/40 text-foreground">
                        <RuntimeIcon runtime={probe.runtime} className="size-4" />
                      </span>
                      <span className="min-w-0 flex-1">
                        <span className="block truncate text-sm font-bold text-foreground">
                          {displayName}
                        </span>
                        <span className="block truncate text-[11px] leading-tight text-muted-foreground/70">
                          {probe.runtime}
                        </span>
                      </span>
                      <Switch
                        checked={profile ? profile.enabled : false}
                        disabled={!profile || saveProfile.isPending}
                        onCheckedChange={() => profile && toggleEnabled(profile)}
                        aria-label={`${t('workerRuntimes.card.enabled')} — ${displayName}`}
                        onClick={(event) => event.stopPropagation()}
                      />
                      <DropdownMenu
                        align="end"
                        items={
                          profile ? (
                            <>
                              <DropdownMenuItem
                                icon={<CheckCircle2 className="size-4" />}
                                onSelect={() => setSelectedRuntime(probe.runtime)}
                              >
                                {t('workerRuntimes.card.edit')}
                              </DropdownMenuItem>
                              <DropdownMenuSeparator />
                              <DropdownMenuItem
                                destructive
                                icon={<Unplug className="size-4" />}
                                onSelect={() => setDeleteTarget(profile)}
                              >
                                {t('workerRuntimes.card.remove')}
                              </DropdownMenuItem>
                            </>
                          ) : (
                            <DropdownMenuItem
                              icon={<KeyRound className="size-4" />}
                              onSelect={() => setSelectedRuntime(probe.runtime)}
                            >
                              {t('workerRuntimes.card.bind')}
                            </DropdownMenuItem>
                          )
                        }
                      >
                        <Button
                          variant="ghost"
                          size="icon"
                          aria-label={`${t('workerRuntimes.card.moreActions')} — ${displayName}`}
                          onClick={(event) => event.stopPropagation()}
                        >
                          <MoreHorizontal className="size-4" />
                        </Button>
                      </DropdownMenu>
                    </div>

                    {/* 元信息：连接 / 网关路由 / 模型 / 执行环境 / 并发 / 超时（参考 wroster-meta） */}
                    <dl className="space-y-1 px-3 pb-2.5">
                      {(
                        [
                          {
                            label: t('workerRuntimes.card.connection'),
                            value: profile ? (
                              providerLabel(profile.connection_id) || (
                                <span className="italic">{t('workerRuntimes.card.notBound')}</span>
                              )
                            ) : (
                              <span className="italic">{t('workerRuntimes.card.notBound')}</span>
                            ),
                          },
                          ...(gatewayRoute
                            ? [
                                {
                                  label: t('workerRuntimes.card.gateway'),
                                  value: (
                                    <span>
                                      {t('workerRuntimes.card.gatewayRoute', {
                                        alias: gatewayRoute.alias,
                                      })}
                                      {gatewayRoute.port != null ? ` :${gatewayRoute.port}` : ''}
                                    </span>
                                  ),
                                },
                              ]
                            : []),
                          {
                            label: t('workerRuntimes.card.model'),
                            value: profile
                              ? profile.model_override ?? t('workerRuntimes.card.modelFromConnection')
                              : '—',
                          },
                          {
                            label: t('workerRuntimes.card.environment'),
                            value: profile
                              ? t(`workerRuntimes.environment.${profile.execution_environment}`)
                              : '—',
                          },
                          {
                            label: t('workerRuntimes.card.concurrency'),
                            value: profile ? String(profile.max_concurrency) : '—',
                          },
                          {
                            label: t('workerRuntimes.card.timeout'),
                            value: profile ? `${profile.timeout_seconds}s` : '—',
                          },
                        ]
                      ).map((row) => (
                        <div
                          key={row.label}
                          className="grid grid-cols-[56px_minmax(0,1fr)] items-baseline gap-1.5"
                        >
                          <dt className="text-[11px] text-muted-foreground/70">{row.label}</dt>
                          <dd className="truncate font-mono text-[11px] leading-4 text-muted-foreground">
                            {row.value}
                          </dd>
                        </div>
                      ))}
                    </dl>

                    {/* 底部状态条：状态圆点 + 文字 + 最近检查（参考 wroster-card-foot） */}
                    <div className="flex min-h-[38px] items-center gap-2.5 border-t border-border px-3 py-2">
                      <span
                        className={cn(
                          'size-2 shrink-0 rounded-full border-2 border-current',
                          STATUS_TEXT[probe.availability],
                        )}
                      />
                      <span className="min-w-0 flex-1">
                        <span
                          className={cn(
                            'block truncate text-[11px] font-semibold leading-tight',
                            STATUS_TEXT[probe.availability],
                          )}
                        >
                          {availabilityLabel(probe.availability)}
                        </span>
                        {reason ? (
                          <span className="block truncate text-[11px] leading-tight text-muted-foreground/70">
                            {reason}
                          </span>
                        ) : null}
                      </span>
                      <span
                        className="shrink-0 font-mono text-[11px] text-muted-foreground"
                        title={probe.checked_at}
                      >
                        {t('workerRuntimes.card.lastCheckedShort', { time: probe.checked_at })}
                      </span>
                    </div>
                  </article>
                );
              })}
            </div>

          {/* 执行策略说明（保留既有约束信息） */}
          <div className="mt-4 rounded-lg border border-border bg-muted/40 p-3.5 text-[11px] leading-relaxed text-muted-foreground">
            <ul className="list-inside list-disc space-y-1">
              <li>{t('workerRuntimes.policy.executionOnly')}</li>
              <li>{t('workerRuntimes.policy.connectionAuth')}</li>
              <li>{t('workerRuntimes.policy.gatewayRoute')}</li>
              <li>{t('workerRuntimes.policy.noLocalLogin')}</li>
              <li>{t('workerRuntimes.policy.noFabrication')}</li>
            </ul>
          </div>
        </section>

        {/* 右侧：Worker 配置检查器（参考 muteki wset-inspector） */}
        <aside className="flex max-h-[calc(100vh-2rem)] flex-col overflow-hidden rounded-xl border border-border bg-card lg:sticky lg:top-6">
          {inspectorOpen && form ? (
            <>
              <header className="flex-none border-b border-border px-4 py-3.5">
                <span className="label-spec">
                  {t('workerRuntimes.inspector.title')}
                </span>
                <strong className="mt-0.5 block truncate text-[15px] font-semibold text-foreground">
                  {t(`workerRuntimes.display.${form.runtime}`)}
                </strong>
                <p className="mt-0.5 truncate text-xs text-muted-foreground">
                  {form.runtime} ·{' '}
                  {selectedProfile
                    ? `${t('workerRuntimes.card.connection')} ${providerLabel(selectedProfile.connection_id)}`
                    : t('workerRuntimes.card.notBound')}{' '}
                  ·{' '}
                  {selectedProbe
                    ? availabilityLabel(selectedProbe.availability)
                    : ''}
                </p>
              </header>

              <div className="min-h-0 flex-1 overflow-y-auto">
                <section className="space-y-2.5 border-b border-border px-4 py-3">
                  <h3 className="text-sm font-semibold text-foreground">
                    {t('workerRuntimes.inspector.sectionIdentity')}
                  </h3>
                  <FormRow label={t('workerRuntimes.inspector.workerProgram')}>
                    <div className="flex items-center gap-2 rounded-md border border-input bg-muted/30 px-2.5 py-1.5 text-sm">
                      <RuntimeIcon runtime={form.runtime} className="size-4 shrink-0 text-primary" />
                      <span className="min-w-0 flex-1 truncate font-medium">
                        {t(`workerRuntimes.display.${form.runtime}`)}
                      </span>
                      <span className="shrink-0 font-mono text-[11px] text-muted-foreground">
                        {form.runtime}
                      </span>
                    </div>
                  </FormRow>
                </section>

                <section className="space-y-2.5 border-b border-border px-4 py-3">
                  <h3 className="text-sm font-semibold text-foreground">
                    {t('workerRuntimes.inspector.sectionConnection')}
                  </h3>
                  <FormRow label={t('workerRuntimes.editor.connectionId')} htmlFor="worker-connection-id">
                    <Select
                      value={form.connectionId}
                      onValueChange={(value) => setForm({ ...form, connectionId: value })}
                    >
                      <SelectTrigger
                        id="worker-connection-id"
                        className="h-9 w-full justify-start text-left [&>span]:min-w-0 [&>span]:flex-1 [&>span]:truncate"
                      >
                        <SelectValue placeholder={t('workerRuntimes.editor.selectConnection')} />
                      </SelectTrigger>
                      <SelectContent>
                        {/* 档案引用的供应商已被删除时保留原值，避免静默丢配置。 */}
                        {form.connectionId && !providersById.has(form.connectionId) && (
                          <SelectItem value={form.connectionId}>
                            {form.connectionId} · {t('workerRuntimes.editor.missingProvider')}
                          </SelectItem>
                        )}
                        {providers.map((provider) => (
                          <SelectItem key={provider.id} value={provider.id}>
                            {provider.name}
                            {provider.model ? ` · ${provider.model}` : ''}
                            {!provider.enabled ? ` · ${t('workerRuntimes.editor.disabledProvider')}` : ''}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </FormRow>
                  <p className="pl-[96px] text-[11px] leading-relaxed text-muted-foreground/70">
                    {t('workerRuntimes.editor.connectionIdHint')}
                  </p>
                  <FormRow
                    label={t('workerRuntimes.editor.modelOverride')}
                    htmlFor="worker-model-override"
                  >
                    <ModelOverrideField
                      value={form.modelOverride}
                      onChange={(value) => setForm({ ...form, modelOverride: value })}
                      models={modelOptions}
                      loading={modelsQuery.isLoading}
                      error={modelsUnavailable}
                      onRefetch={() => modelsQuery.refetch()}
                      refreshing={modelsQuery.isFetching}
                    />
                  </FormRow>
                  <FormRow
                    label={t('workerRuntimes.editor.agentPreset')}
                    htmlFor="worker-agent-preset"
                  >
                    <Select
                      value={form.agentPreset || '__none__'}
                      onValueChange={(value) =>
                        setForm({ ...form, agentPreset: value === '__none__' ? '' : value })
                      }
                    >
                      <SelectTrigger id="worker-agent-preset" className="h-9">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="__none__">
                          {t('workerRuntimes.editor.agentPresetNone')}
                        </SelectItem>
                        {(agentPresetsQuery.data ?? []).map((preset) => (
                          <SelectItem key={preset.key} value={preset.key} disabled={!preset.enabled}>
                            {preset.name} ({preset.key})
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </FormRow>
                  <p className="pl-[96px] text-[11px] leading-relaxed text-muted-foreground/70">
                    {t('workerRuntimes.editor.agentPresetHint')}
                  </p>
                </section>

                <section className="space-y-2.5 border-b border-border px-4 py-3">
                  <h3 className="text-sm font-semibold text-foreground">
                    {t('workerRuntimes.inspector.sectionScheduling')}
                  </h3>
                  <FormRow
                    label={t('workerRuntimes.editor.executionEnvironment')}
                    htmlFor="worker-execution-environment"
                  >
                    <Select
                      value={form.executionEnvironment}
                      onValueChange={(value) =>
                        setForm({
                          ...form,
                          executionEnvironment: value as WorkerExecutionEnvironment,
                        })
                      }
                    >
                      <SelectTrigger id="worker-execution-environment" className="h-9 w-full">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="local">{t('workerRuntimes.environment.local')}</SelectItem>
                        <SelectItem value="container">
                          {t('workerRuntimes.environment.container')}
                        </SelectItem>
                      </SelectContent>
                    </Select>
                  </FormRow>
                  <FormRow
                    label={t('workerRuntimes.editor.maxConcurrency')}
                    htmlFor="worker-max-concurrency"
                  >
                    <NumField
                      id="worker-max-concurrency"
                      value={form.maxConcurrency}
                      onChange={(value) => setForm({ ...form, maxConcurrency: value })}
                      placeholder={t('workerRuntimes.editor.backendDefault')}
                      invalid={!isPositiveIntegerOrEmpty(form.maxConcurrency)}
                    />
                  </FormRow>
                  {!isPositiveIntegerOrEmpty(form.maxConcurrency) && (
                    <p className="pl-[96px] text-[11px] text-destructive">
                      {t('workerRuntimes.editor.invalidInteger')}
                    </p>
                  )}
                  <FormRow
                    label={t('workerRuntimes.editor.timeoutSeconds')}
                    htmlFor="worker-timeout-seconds"
                  >
                    <NumField
                      id="worker-timeout-seconds"
                      value={form.timeoutSeconds}
                      onChange={(value) => setForm({ ...form, timeoutSeconds: value })}
                      placeholder={t('workerRuntimes.editor.backendDefault')}
                      invalid={!isPositiveIntegerOrEmpty(form.timeoutSeconds)}
                    />
                  </FormRow>
                  {!isPositiveIntegerOrEmpty(form.timeoutSeconds) && (
                    <p className="pl-[96px] text-[11px] text-destructive">
                      {t('workerRuntimes.editor.invalidInteger')}
                    </p>
                  )}
                  <div className="flex min-h-[33px] items-center justify-between gap-3">
                    <span className="min-w-0">
                      <b className="block text-sm font-semibold text-foreground">
                        {t('workerRuntimes.editor.enabled')}
                      </b>
                      <small className="block text-[11px] leading-tight text-muted-foreground/70">
                        {t('workerRuntimes.editor.enabledHint')}
                      </small>
                    </span>
                    <Switch
                      id="worker-enabled"
                      checked={form.enabled}
                      onCheckedChange={(checked) => setForm({ ...form, enabled: checked })}
                    />
                  </div>
                </section>
              </div>

              {/* 底部操作坞（参考 wset-inspector-dock） */}
              <footer className="flex-none space-y-1.5 border-t border-border bg-muted/30 p-3">
                <Button
                  className="w-full"
                  onClick={saveFromInspector}
                  disabled={!formValid || saveProfile.isPending}
                >
                  {saveProfile.isPending ? (
                    <Loader2 className="mr-2 size-4 animate-spin" />
                  ) : (
                    <CheckCircle2 className="mr-2 size-4" />
                  )}
                  {selectedProfile
                    ? t('workerRuntimes.inspector.save')
                    : t('workerRuntimes.inspector.bind')}
                </Button>
                {selectedProfile && (
                  <Button
                    variant="outline"
                    className="w-full text-destructive hover:bg-destructive/10 hover:text-destructive"
                    onClick={() => setDeleteTarget(selectedProfile)}
                    disabled={removeProfile.isPending}
                  >
                    <Unplug className="mr-2 size-4" />
                    {t('workerRuntimes.card.remove')}
                  </Button>
                )}
              </footer>
            </>
          ) : (
            <div className="flex flex-1 flex-col items-center justify-center gap-1.5 p-6 text-center">
              <strong className="text-sm text-foreground">
                {t('workerRuntimes.inspector.emptyTitle')}
              </strong>
              <span className="max-w-[240px] text-xs leading-relaxed text-muted-foreground">
                {t('workerRuntimes.inspector.emptyDescription')}
              </span>
            </div>
          )}
        </aside>

      </div>

      {/* 解除绑定 / 删除确认 */}
      <Dialog
        open={deleteTarget !== null}
        onOpenChange={(open) => {
          if (!open) setDeleteTarget(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('workerRuntimes.remove.title')}</DialogTitle>
            <DialogDescription>
              {deleteTarget
                ? t('workerRuntimes.remove.description', {
                    runtime: t(`workerRuntimes.display.${deleteTarget.runtime_type}`),
                  })
                : ''}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setDeleteTarget(null)}>
              {t('common.cancel')}
            </Button>
            <Button
              variant="destructive"
              onClick={() => deleteTarget && removeProfile.mutate(deleteTarget.id)}
              disabled={removeProfile.isPending}
            >
              {removeProfile.isPending ? (
                <Loader2 className="mr-2 size-4 animate-spin" />
              ) : (
                <Unplug className="mr-2 size-4" />
              )}
              {t('workerRuntimes.remove.confirm')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </PageContainer>
  );
}
