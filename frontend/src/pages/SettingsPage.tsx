import {
  useEffect,
  useState,
} from 'react';
import { Card, CardHeader, CardTitle, CardDescription, CardContent, CardFooter } from '@/components/ui/card';
import { Button } from '@/ui/untitled';
import { Input } from '@/ui/untitled';
import { Label } from '@/components/ui/label';
import { useToast } from '@/hooks/use-toast';
import { getApiBaseUrl, setApiBaseUrl } from '@/lib/settings';
import {
  PROVIDER_METADATA,
  getProviderHealthQueryKey,
  getProviderReadiness,
  type ProviderReadinessKind,
} from '@/lib/provider-types';
import type {
  ProviderConfigResponse,
  CreateProviderRequest,
  UpdateProviderRequest,
} from '@/lib/provider-types';
import type { ProviderRouteBinding } from '@/lib/types';
import { api, getApiErrorMessage } from '@/lib/api';
import {
  Server, Bot, ShieldAlert, Plus, Trash2, Save, RefreshCw, Settings,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import { ProviderEditDialog } from '@/components/provider/ProviderEditDialog';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/ui/untitled';
import { Badge } from '@/components/ui/badge';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { useSearch } from '@tanstack/react-router';
import { useTranslation } from 'react-i18next';
import { formatStatus } from '@/lib/i18n-formatters';
import { useProviderHealthById } from '@/hooks/use-provider-health';
import {
  PageHeader,
  Section,
  Badge as UntitledBadge,
  EmptyState,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
  type StatusTone,
} from '@/ui/untitled';

function readinessTone(kind: ProviderReadinessKind): StatusTone {
  switch (kind) {
    case 'ready':
      return 'success';
    case 'testing':
      return 'info';
    case 'text_only':
    case 'no_text_generation':
    case 'incomplete':
    case 'untested':
      return 'warning';
    default:
      return 'danger';
  }
}


const EMPTY_PROVIDERS: ProviderConfigResponse[] = [];

export function SettingsPage() {
  const { toast } = useToast();
  const queryClient = useQueryClient();
  const { t } = useTranslation();
  const searchParams = useSearch({ strict: false });
  const [activeTab, setActiveTab] = useState(
    (searchParams as Record<string, unknown>).tab === 'providers'
      ? 'providers'
      : 'api'
  );

  // API Config State
  const [baseUrl, setBaseUrl] = useState(getApiBaseUrl());
  const [healthStatus, setHealthStatus] = useState<'connected' | 'failed' | 'unknown'>('unknown');
  const [isTestingHealth, setIsTestingHealth] = useState(false);

  const [editingProvider, setEditingProvider] = useState<Partial<ProviderConfigResponse> | null>(null);

  const { data: providers = EMPTY_PROVIDERS, isLoading: isLoadingProviders, isError: isProvidersError, error: providersError, refetch: refetchProviders } = useQuery({
    queryKey: ['providers'],
    queryFn: () => api.getProviders(),
  });
  const providerHealthById = useProviderHealthById(providers);

  const createProviderMutation = useMutation({
    mutationFn: (data: CreateProviderRequest) => api.createProvider(data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['providers'] });
      setEditingProvider(null);
      toast({ title: t('settings.toast.providerCreated'), description: t('settings.toast.providerCreatedDescription') });
    },
    onError: (err: Error) => {
      toast({ title: t('common.error'), description: err.message || t('settings.toast.failedToCreateProvider'), variant: 'destructive' });
    }
  });

  const updateProviderMutation = useMutation({
    mutationFn: ({ id, data }: { id: string, data: UpdateProviderRequest }) => api.updateProvider(id, data),
    onSuccess: (provider) => {
      queryClient.invalidateQueries({ queryKey: ['providers'] });
      queryClient.removeQueries({ queryKey: getProviderHealthQueryKey(provider.id) });
      setEditingProvider(null);
      toast({ title: t('settings.toast.providerUpdated'), description: t('settings.toast.providerUpdatedDescription') });
    },
    onError: (err: Error) => {
      toast({ title: t('common.error'), description: err.message || t('settings.toast.failedToUpdateProvider'), variant: 'destructive' });
    }
  });

  const deleteProviderMutation = useMutation({
    mutationFn: (id: string) => api.deleteProvider(id),
    onSuccess: (_data, id) => {
      queryClient.invalidateQueries({ queryKey: ['providers'] });
      queryClient.removeQueries({ queryKey: getProviderHealthQueryKey(id) });
      toast({ title: t('settings.toast.providerDeleted'), description: t('settings.toast.providerDeletedDescription') });
    },
    onError: (err: Error) => {
      toast({ title: t('common.error'), description: err.message || t('settings.toast.failedToDeleteProvider'), variant: 'destructive' });
    }
  });

  const testProviderMutation = useMutation({
    mutationFn: (id: string) => api.testProvider(id),
    onMutate: (id) => {
      queryClient.setQueryData(getProviderHealthQueryKey(id), {
        provider_id: id,
        status: 'testing',
        message: t('common.testing'),
        capabilities: {
          text_generation: false,
          structured_output: false,
        },
      });
    },
    onSuccess: (data, id) => {
      queryClient.setQueryData(getProviderHealthQueryKey(id), data);
      if (data.status === 'ok') {
        toast({ title: t('settings.toast.testSuccessful'), description: t('settings.toast.connectedModel', { model: data.model || t('common.unknown') }), variant: 'default' });
      } else {
        toast({ title: t('settings.toast.testStatus', { status: formatStatus(t, data.status) }), description: data.message || t('settings.toast.failedToConnect'), variant: 'destructive' });
      }
    },
    onError: (err: Error, id) => {
      queryClient.setQueryData(getProviderHealthQueryKey(id), {
        provider_id: id,
        status: 'error',
        message: err.message,
        capabilities: {
          text_generation: false,
          structured_output: false,
        },
      });
      toast({ title: t('common.error'), description: err.message || t('settings.toast.testRequestFailed'), variant: 'destructive' });
    }
  });

  const handleSaveApi = () => {
    setApiBaseUrl(baseUrl);
    queryClient.clear();
    toast({ title: t('settings.apiConfigSaved'), description: t('settings.apiConfigSavedDescription') });
    setTimeout(() => {
      window.location.reload();
    }, 1500);
  };

  const handleTestHealth = async () => {
    setIsTestingHealth(true);
    setHealthStatus('unknown');
    const ok = await api.checkHealth(baseUrl);
    setHealthStatus(ok ? 'connected' : 'failed');
    setIsTestingHealth(false);
    toast({
      title: ok ? t('settings.connectionSuccessful') : t('settings.connectionFailed'),
      description: ok ? t('settings.connectionSuccessDescription', { url: baseUrl }) : t('settings.connectionFailedDescription', { url: baseUrl }),
      variant: ok ? 'default' : 'destructive'
    });
  };

  const handleAddProvider = () => {
    const meta = PROVIDER_METADATA.openai_compatible;
    setEditingProvider({
      name: meta.label,
      provider_type: 'openai_compatible',
      base_url: meta.defaultBaseUrl,
      model: meta.defaultModel,
      enabled: true,
      is_default: providers.length === 0,
      timeout_seconds: 60,
      default_headers: {},
    });
  };

  return (
    <div className="page-stack">
      <PageHeader
        icon={<Settings className="h-5 w-5" />}
        title={t('settings.title')}
        description={t('settings.description')}
        actions={
          activeTab === 'api' && (
            <Button variant="outline" size="sm" onClick={() => window.location.reload()}>
              <RefreshCw className="h-4 w-4" />
              {t('common.forceRefresh')}
            </Button>
          )
        }
      />

      <Tabs value={activeTab} onValueChange={setActiveTab} className="w-full">
        <TabsList animated indicatorClassName="bg-card" className="bg-muted/60 p-1">
          <TabsTrigger value="api" className="gap-2 data-[state=active]:bg-card data-[state=active]:shadow-xs">
            <Server className="h-4 w-4" /> {t('settings.tabs.apiEndpoint')}
          </TabsTrigger>
          <TabsTrigger value="providers" className="gap-2 data-[state=active]:bg-card data-[state=active]:shadow-xs">
            <Bot className="h-4 w-4" /> {t('settings.tabs.agentProviders')}
          </TabsTrigger>
        </TabsList>

        <TabsContent value="api">
          <Card className="shadow-xs">
            <CardHeader>
              <CardTitle>{t('settings.systemConfig')}</CardTitle>
              <CardDescription>{t('settings.configureBackend')}</CardDescription>
            </CardHeader>
            <CardContent className="space-y-6">
              <div className="space-y-2">
                <Label htmlFor="base-url">{t('settings.apiBaseUrl')}</Label>
                <div className="flex gap-2">
                  <Input
                    id="base-url"
                    value={baseUrl}
                    onChange={(e) => setBaseUrl(e.target.value)}
                    placeholder={t('settings.providerDialog.apiEndpointPlaceholder')}
                  />
                  <Button
                    variant="outline"
                    onClick={handleTestHealth}
                    disabled={isTestingHealth}
                  >
                    {isTestingHealth ? t('common.testing') : t('common.test')}
                  </Button>
                </div>
                <div className="flex items-center gap-2 mt-2">
                  <span className="text-xs text-muted-foreground">{t('common.status')}:</span>
                  <HealthBadge status={healthStatus} />
                </div>
                <p className="text-xs text-muted-foreground">{t('settings.updateBaseUrl')}</p>
              </div>

            </CardContent>
            <CardFooter className="bg-muted/40 border-t border-border px-6 py-4 flex justify-between">
              <p className="text-xs text-muted-foreground/70 italic">{t('common.localStoragePriority')}</p>
              <Button onClick={handleSaveApi}>
                <Save className="w-4 h-4 mr-2" /> {t('settings.saveSettings')}
              </Button>
            </CardFooter>
          </Card>
        </TabsContent>

        <TabsContent value="providers">
          <div className="grid grid-cols-1 md:grid-cols-3 gap-6">
            <div className="md:col-span-2 space-y-4">
              <div className="flex justify-between items-center">
                <h3 className="text-lg font-semibold text-foreground flex items-center gap-2">
                  {t('settings.providerConfig')}
                </h3>
                <Button size="sm" onClick={handleAddProvider}>
                  <Plus className="w-4 h-4 mr-2" /> {t('settings.addProvider')}
                </Button>
              </div>

              {isProvidersError ? (
                <EmptyState
                  icon={<ShieldAlert className="h-6 w-6" />}
                  title={t('settings.failedToLoadProviders')}
                  description={providersError?.message || t('errors.unknown')}
                  action={
                    <Button variant="outline" size="sm" onClick={() => refetchProviders()}>
                      <RefreshCw className="h-4 w-4" />
                      {t('common.retry')}
                    </Button>
                  }
                />
              ) : isLoadingProviders ? (
                <div className="flex justify-center py-12">
                  <RefreshCw className="h-6 w-6 animate-spin text-muted-foreground" />
                </div>
              ) : providers.length === 0 ? (
                <EmptyState
                  icon={<Bot className="h-6 w-6" />}
                  title={t('settings.noProviders')}
                  description={t('settings.noProvidersDescription')}
                  action={
                    <Button variant="outline" size="sm" onClick={handleAddProvider}>
                      {t('settings.configureFirstProvider')}
                    </Button>
                  }
                />
              ) : (
                <div className="grid gap-4">
                  {providers.map(p => {
                    const providerHealth = providerHealthById[p.id];
                    const providerReadiness = getProviderReadiness(p, providerHealth);
                    return (
                    <Card key={p.id} className={cn("border-border shadow-xs", p.is_default && "border-primary/50 bg-primary/[0.01]")}>
                      <CardContent className="p-4 flex items-center justify-between">
                        <div className="flex items-center gap-4">
                          <div className="w-10 h-10 rounded-lg bg-muted flex items-center justify-center">
                            <Bot className="w-5 h-5 text-foreground" />
                          </div>
                          <div>
                            <div className="flex items-center gap-2">
                              <span className="font-semibold text-foreground">{p.name}</span>
                              {p.is_default && <Badge variant="secondary" className="text-[10px] uppercase font-bold py-0 h-4">{t('settings.defaultProvider')}</Badge>}
                              {!p.enabled && <Badge variant="outline" className="text-[10px] uppercase font-bold py-0 h-4 text-muted-foreground/70">{t('common.disabled')}</Badge>}
                            </div>
                            <div className="text-xs text-muted-foreground mt-0.5">
                              {PROVIDER_METADATA[p.provider_type]?.label || p.provider_type} • {p.model || t('common.noModel')}
                            </div>
                            <div className="text-xs text-muted-foreground mt-1">
                              {p.base_url ? <div className="truncate max-w-[250px]" title={p.base_url}>{p.base_url}</div> : null}
                              <div className="flex items-center gap-1 mt-1">
                                {p.has_api_key && <Badge variant="outline" className="text-[10px] bg-muted text-foreground uppercase">{t('common.backendKey')}</Badge>}
                                {p.api_key_ref && <Badge variant="outline" className="text-[10px] bg-muted text-foreground uppercase">{p.api_key_ref}</Badge>}
                                {!p.has_api_key && !p.api_key_ref && <Badge variant="outline" className="text-[10px] bg-slate-50 text-muted-foreground/70 uppercase">{t('common.noSecret')}</Badge>}
                              </div>
                              <div className="mt-1 text-muted-foreground/70 text-[10px]">
                                {t('settings.providerDialog.timeoutSeconds')}: {p.timeout_seconds}s
                                {p.max_tokens ? ` · ${t('settings.providerDialog.maxTokens')}: ${p.max_tokens}` : ''}
                                {p.temperature != null ? ` · Temp: ${p.temperature}` : ''}
                              </div>
                              <div className="mt-1 flex flex-wrap gap-1">
                                <UntitledBadge tone={readinessTone(providerReadiness.kind)} className="text-[10px]">
                                  {providerReadiness.kind === 'testing' && <RefreshCw className="mr-1 h-3.5 w-3.5 animate-spin" />}
                                  {t(providerReadiness.labelKey)}
                                </UntitledBadge>
                                {providerHealth?.capabilities && providerHealth.status !== 'testing' && (
                                  <>
                                    <UntitledBadge
                                      tone={providerHealth.capabilities.text_generation ? 'success' : 'danger'}
                                      className="text-[10px]"
                                    >
                                      {t('settings.providerCapabilities.textGeneration')}: {providerHealth.capabilities.text_generation ? t('common.succeeded') : t('common.failed')}
                                    </UntitledBadge>
                                    <UntitledBadge
                                      tone={providerHealth.capabilities.structured_output ? 'success' : 'warning'}
                                      className="text-[10px]"
                                    >
                                      {t('settings.providerCapabilities.structuredOutput')}: {providerHealth.capabilities.structured_output ? t('common.succeeded') : t('status.unknown')}
                                    </UntitledBadge>
                                  </>
                                )}
                              </div>
                            </div>
                          </div>
                        </div>
                        <div className="flex items-center gap-2">
                          <Button 
                            variant="outline" 
                            size="sm" 
                            onClick={() => testProviderMutation.mutate(p.id)}
                            disabled={testProviderMutation.isPending && testProviderMutation.variables === p.id}
                          >
                            {testProviderMutation.isPending && testProviderMutation.variables === p.id ? t('common.testing') : t('common.test')}
                          </Button>
                          <Button variant="ghost" size="sm" onClick={() => setEditingProvider(p)}>{t('common.edit')}</Button>
                          <Button variant="ghost" size="sm" className="text-red-500 hover:text-red-600" onClick={() => {
                            if (confirm(t('settings.toast.deleteProviderConfirm', { name: p.name }))) deleteProviderMutation.mutate(p.id);
                          }}>
                            <Trash2 className="w-4 h-4" />
                          </Button>
                        </div>
                      </CardContent>
                    </Card>
                    );
                  })}
                </div>
              )}
            </div>

            <div className="space-y-6">
              <ProviderRoutesPanel providers={providers} />
            </div>
          </div>
        </TabsContent>

        
      </Tabs>

      {editingProvider && (
        <ProviderEditDialog
          provider={editingProvider}
          isOpen={!!editingProvider}
          onClose={() => setEditingProvider(null)}
          onSave={(data) => {
            if (editingProvider.id) {
              updateProviderMutation.mutate({ id: editingProvider.id, data });
            } else {
              createProviderMutation.mutate(data as CreateProviderRequest);
            }
          }}
          isLoading={createProviderMutation.isPending || updateProviderMutation.isPending}
        />
      )}
    </div>
  );
}

function HealthBadge({ status }: { status: string }) {
  const { t } = useTranslation();
  if (status === 'connected') return <Badge className="bg-success-soft text-success-foreground border-success-border text-[10px]">{t('common.connected')}</Badge>;
  if (status === 'failed') return <Badge className="bg-danger-soft text-danger-foreground border-danger-border text-[10px]">{t('common.failed')}</Badge>;
  return <Badge className="bg-neutral-soft text-neutral-foreground border-neutral-border text-[10px]">{t('common.unknown')}</Badge>;
}

// 已接入模型调用的用途清单（与后端 purpose 字面量一一对应；顺序即展示序）。
//
// 2026-09-22：删除复杂度分档后移除 `task_profile`。原 intake 分类角色改为
// Goal 拆解——同一个 purpose 字面量 `natural_language_intake` 保留（DB 里已有
// 路由绑定按它索引，改名会让存量绑定失效），语义在 i18n 文案里改。
// `planner` 角色等 Planner 落地后再加，不预先摆一个空壳用途。
const KNOWN_ROUTE_PURPOSES = [
  'natural_language_intake',
  'agent_tool_harness',
  'strategy_board_maintainer',
  'metacognition_divergence',
] as const;

type KnownRoutePurpose = (typeof KNOWN_ROUTE_PURPOSES)[number];

function ProviderRoutesPanel({ providers }: { providers: ProviderConfigResponse[] }) {
  const { t } = useTranslation();
  const { toast } = useToast();
  const queryClient = useQueryClient();
  const [customPurpose, setCustomPurpose] = useState('');
  const [customProviderId, setCustomProviderId] = useState('');
  const [customModelOverride, setCustomModelOverride] = useState('');

  const { data: routes = [] } = useQuery({
    queryKey: ['provider-routes'],
    queryFn: () => api.getProviderRoutes(),
  });

  const assignMutation = useMutation({
    mutationFn: async (input: {
      purpose: string;
      providerId: string; // '' = 跟随默认 provider
      modelOverride: string;
    }) => {
      const existing = routes
        .filter((route) => route.purpose === input.purpose)
        .sort((a, b) => b.priority - a.priority)[0];
      if (!input.providerId) {
        // 跟随默认：清除该用途的全部路由绑定。
        await Promise.all(
          routes
            .filter((route) => route.purpose === input.purpose)
            .map((route) => api.deleteProviderRoute(route.id)),
        );
        return null;
      }
      const payload = {
        purpose: input.purpose,
        provider_id: input.providerId,
        model_override: input.modelOverride || null,
      };
      if (existing) {
        return api.updateProviderRoute(existing.id, {
          ...payload,
          priority: existing.priority,
          weight: existing.weight,
          enabled: true,
          fallback_group: existing.fallback_group,
          required_capabilities: existing.required_capabilities,
          max_failures: existing.max_failures,
          cooldown_seconds: existing.cooldown_seconds,
          metadata: existing.metadata,
        });
      }
      return api.createProviderRoute({
        ...payload,
        priority: 100,
        weight: 100,
        enabled: true,
        fallback_group: 'default',
        required_capabilities: {},
        max_failures: 3,
        cooldown_seconds: 60,
        metadata: {},
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['provider-routes'] });
      toast({ title: t('settings.providerRouting.routeSaved') });
    },
    onError: (error: unknown) => {
      toast({ title: t('settings.providerRouting.routeSaveFailed'), description: getApiErrorMessage(error), variant: 'destructive' });
    },
  });

  const createCustomMutation = useMutation({
    mutationFn: () => api.createProviderRoute({
      purpose: customPurpose.trim(),
      provider_id: customProviderId,
      model_override: customModelOverride || null,
      priority: 100,
      weight: 100,
      enabled: true,
      fallback_group: 'default',
      required_capabilities: {},
      max_failures: 3,
      cooldown_seconds: 60,
      metadata: {},
    }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['provider-routes'] });
      setCustomPurpose('');
      setCustomProviderId('');
      setCustomModelOverride('');
      toast({ title: t('settings.providerRouting.routeSaved') });
    },
    onError: (error: unknown) => {
      toast({ title: t('settings.providerRouting.routeSaveFailed'), description: getApiErrorMessage(error), variant: 'destructive' });
    },
  });

  const deleteRouteMutation = useMutation({
    mutationFn: (id: string) => api.deleteProviderRoute(id),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['provider-routes'] }),
  });

  const knownPurposes = new Set<string>(KNOWN_ROUTE_PURPOSES);
  const customRoutes = routes.filter((route) => !knownPurposes.has(route.purpose));

  return (
    <Section
      title={t('settings.providerRouting.title')}
      description={t('settings.providerRouting.description')}
    >
      <div className="section-stack">
        {KNOWN_ROUTE_PURPOSES.map((purpose) => (
          <PurposeRouteRow
            key={purpose}
            purpose={purpose}
            providers={providers}
            routes={routes}
            pending={assignMutation.isPending}
            onAssign={(providerId, modelOverride) =>
              assignMutation.mutate({ purpose, providerId, modelOverride })
            }
          />
        ))}

        <details className="rounded border border-border bg-card p-3 text-xs">
          <summary className="cursor-pointer font-medium text-foreground">
            {t('settings.providerRouting.advanced')}
          </summary>
          <div className="mt-2 grid grid-cols-1 gap-2">
            <Input value={customPurpose} onChange={(event) => setCustomPurpose(event.target.value)} placeholder={t('settings.providerRouting.purposePlaceholder')} />
            <Select value={customProviderId} onValueChange={setCustomProviderId}>
              <SelectTrigger><SelectValue placeholder={t('settings.providerRouting.selectProvider')} /></SelectTrigger>
              <SelectContent>
                {providers.map((provider) => (
                  <SelectItem key={provider.id} value={provider.id}>{provider.name}</SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Input value={customModelOverride} onChange={(event) => setCustomModelOverride(event.target.value)} placeholder={t('settings.providerRouting.modelOverridePlaceholder')} />
            <Button size="sm" disabled={!customPurpose.trim() || !customProviderId || createCustomMutation.isPending} onClick={() => createCustomMutation.mutate()}>
              <Plus className="mr-2 h-4 w-4" /> {t('settings.providerRouting.addRoute')}
            </Button>
          </div>
          <div className="section-stack pt-2">
            {customRoutes.length === 0 ? (
              <div className="rounded border border-border bg-muted/30 p-3 text-center text-xs text-muted-foreground">
                {t('settings.providerRouting.noRoutes')}
              </div>
            ) : customRoutes.map((route) => {
              const provider = providers.find((item) => item.id === route.provider_id);
              return (
                <div key={route.id} className="space-y-1 rounded border border-border bg-card p-3 text-xs">
                  <div className="flex items-center justify-between gap-2">
                    <span className="font-semibold text-foreground">{route.purpose}</span>
                    <Button variant="ghost" size="sm" onClick={() => deleteRouteMutation.mutate(route.id)}>
                      <Trash2 className="h-3.5 w-3.5 text-danger" />
                    </Button>
                  </div>
                  <div className="text-muted-foreground">{provider?.name || route.provider_id}</div>
                  <div className="flex flex-wrap gap-1">
                    <UntitledBadge tone="neutral">{t('settings.providerRouting.priority', { value: route.priority })}</UntitledBadge>
                    {route.model_override && <UntitledBadge tone="info">{route.model_override}</UntitledBadge>}
                    {route.circuit_open_until && <UntitledBadge tone="danger">{t('settings.providerRouting.circuitOpen')}</UntitledBadge>}
                  </div>
                </div>
              );
            })}
          </div>
        </details>
      </div>
    </Section>
  );
}

function PurposeRouteRow({
  purpose,
  providers,
  routes,
  pending,
  onAssign,
}: {
  purpose: KnownRoutePurpose;
  providers: ProviderConfigResponse[];
  routes: ProviderRouteBinding[];
  pending: boolean;
  onAssign: (providerId: string, modelOverride: string) => void;
}) {
  const { t } = useTranslation();
  const labelKey = `settings.providerRouting.purposes.${purpose}` as const;
  const descriptionKey = `settings.providerRouting.purposes.${purpose}_description` as const;
  // 生效绑定 = 该用途下优先级最高的路由。
  const binding = routes
    .filter((route) => route.purpose === purpose)
    .sort((a, b) => b.priority - a.priority)[0];
  const [providerId, setProviderId] = useState(binding?.provider_id ?? '');
  const [modelOverride, setModelOverride] = useState(binding?.model_override ?? '');

  // 路由数据异步到达 / 保存后回读时同步本地状态。
  useEffect(() => {
    setProviderId(binding?.provider_id ?? '');
    setModelOverride(binding?.model_override ?? '');
  }, [binding?.provider_id, binding?.model_override]);

  return (
    <div className="space-y-2 rounded border border-border bg-card p-3">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
        <div className="min-w-0 flex-1 space-y-0.5">
          <div className="flex items-center gap-2 text-sm font-semibold text-foreground">
            {t(labelKey)}
            {binding?.circuit_open_until && (
              <UntitledBadge tone="danger">{t('settings.providerRouting.circuitOpen')}</UntitledBadge>
            )}
          </div>
          <div className="text-xs text-muted-foreground">{t(descriptionKey)}</div>
        </div>
        <div className="w-full sm:w-52">
          <Select
            value={providerId || '__default__'}
            onValueChange={(value) => {
              const next = value === '__default__' ? '' : value;
              setProviderId(next);
              onAssign(next, modelOverride);
            }}
          >
            <SelectTrigger><SelectValue placeholder={t('settings.providerRouting.selectProvider')} /></SelectTrigger>
            <SelectContent>
              <SelectItem value="__default__">{t('settings.providerRouting.followDefault')}</SelectItem>
              {providers.map((provider) => (
                <SelectItem key={provider.id} value={provider.id}>{provider.name}</SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      </div>
      {providerId && (
        <div className="flex items-center gap-2">
          <Input
            value={modelOverride}
            onChange={(event) => setModelOverride(event.target.value)}
            placeholder={t('settings.providerRouting.modelOverridePlaceholder')}
            className="h-8 text-xs"
            onBlur={() => {
              if (modelOverride !== (binding?.model_override ?? '')) {
                onAssign(providerId, modelOverride);
              }
            }}
            onKeyDown={(event) => {
              if (event.key === 'Enter') {
                event.currentTarget.blur();
              }
            }}
          />
          <Button
            size="sm"
            variant="ghost"
            disabled={pending || modelOverride === (binding?.model_override ?? '')}
            onClick={() => onAssign(providerId, modelOverride)}
          >
            {t('common.save')}
          </Button>
        </div>
      )}
    </div>
  );
}
