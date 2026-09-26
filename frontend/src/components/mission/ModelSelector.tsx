import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { Bot, Check, ChevronDown, Loader2, RefreshCw } from 'lucide-react';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { cn } from '@/lib/utils';
import { focusRing } from '@/ui/untitled/tokens';
import { api, getApiErrorMessage } from '@/lib/api';
import {
  PROVIDER_METADATA,
  type ProviderConfigResponse,
  type ProviderModelDiscoveryResult,
  type UpdateProviderRequest,
} from '@/lib/provider-types';
import { useToast } from '@/hooks/use-toast';

interface ModelSelectorProps {
  disabled?: boolean;
  className?: string;
}

/**
 * ModelSelector — pick the model that runs missions.
 *
 * Operates on the default provider: selecting a provider marks it default,
 * selecting a model writes that provider's `model`. The model list merges
 * discovered models (provider /models endpoint), the provider's current model,
 * and per-type suggestions. A free-form input accepts any custom model id.
 */
export function ModelSelector({ disabled = false, className }: ModelSelectorProps) {
  const { t } = useTranslation();
  const { toast } = useToast();
  const queryClient = useQueryClient();

  const [open, setOpen] = useState(false);
  const [activeProviderId, setActiveProviderId] = useState<string | null>(null);
  const [discoveryCache, setDiscoveryCache] = useState<
    Record<string, ProviderModelDiscoveryResult>
  >({});
  const [customModel, setCustomModel] = useState('');

  const { data: providers = [] } = useQuery({
    queryKey: ['providers'],
    queryFn: () => api.getProviders(),
    staleTime: 60_000,
  });

  const defaultProvider = providers.find((p) => p.is_default) ?? providers[0] ?? null;
  const activeProvider =
    providers.find((p) => p.id === activeProviderId) ?? defaultProvider;

  useEffect(() => {
    if (open && activeProvider && activeProviderId === null) {
      setActiveProviderId(activeProvider.id);
    }
  }, [open, activeProvider, activeProviderId]);

  const discoverMutation = useMutation({
    mutationFn: (providerId: string) =>
      api.discoverProviderModels({ provider_id: providerId }),
    onSuccess: (result, providerId) => {
      setDiscoveryCache((prev) => ({ ...prev, [providerId]: result }));
    },
    onError: (err) => {
      toast({
        title: t('modelSelector.discoverFailed'),
        description: getApiErrorMessage(err),
        variant: 'destructive',
      });
    },
  });

  // Discover once per provider when its panel is first opened.
  useEffect(() => {
    if (!open || !activeProvider) return;
    const cached = discoveryCache[activeProvider.id];
    if (!cached && activeProvider.base_url && !discoverMutation.isPending) {
      discoverMutation.mutate(activeProvider.id);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, activeProvider?.id]);

  const updateProviderMutation = useMutation({
    mutationFn: (input: { providerId: string; patch: UpdateProviderRequest }) =>
      api.updateProvider(input.providerId, input.patch),
    onSuccess: (_data, input) => {
      void queryClient.invalidateQueries({ queryKey: ['providers'] });
      if (typeof input.patch.model === 'string') {
        toast({ title: t('modelSelector.updated') });
        setOpen(false);
        setCustomModel('');
      }
    },
    onError: (err) => {
      toast({
        title: t('modelSelector.updateFailed'),
        description: getApiErrorMessage(err),
        variant: 'destructive',
      });
    },
  });

  const triggerModel = defaultProvider?.model?.trim() || t('modelSelector.notSet');

  const { discoveredModels, suggestedModels } = useMemo(() => {
    if (!activeProvider) return { discoveredModels: [], suggestedModels: [] };
    const discovered = Array.from(
      new Set(
        [
          ...(discoveryCache[activeProvider.id]?.models ?? []),
          ...(activeProvider.model?.trim() ? [activeProvider.model.trim()] : []),
        ].filter(Boolean),
      ),
    );
    const suggested = (
      PROVIDER_METADATA[activeProvider.provider_type]?.suggestedModels ?? []
    ).filter((model) => !discovered.includes(model));
    return { discoveredModels: discovered, suggestedModels: suggested };
  }, [activeProvider, discoveryCache]);

  const selectModel = (model: string) => {
    if (!activeProvider) return;
    if (model === activeProvider.model?.trim()) {
      setOpen(false);
      return;
    }
    updateProviderMutation.mutate({
      providerId: activeProvider.id,
      patch: { model },
    });
  };

  const selectProvider = (provider: ProviderConfigResponse) => {
    setActiveProviderId(provider.id);
    setCustomModel('');
    if (!provider.is_default) {
      updateProviderMutation.mutate({
        providerId: provider.id,
        patch: { is_default: true },
      });
    }
  };

  const applyCustomModel = () => {
    const model = customModel.trim();
    if (!model || !activeProvider) return;
    selectModel(model);
  };

  const busy = updateProviderMutation.isPending;
  const triggerIcon =
    defaultProvider?.model?.trim() ? 'text-muted-foreground' : 'text-warning';

  const renderModelRow = (model: string) => {
    const selected = model === activeProvider?.model?.trim();
    return (
      <button
        key={model}
        type="button"
        disabled={busy}
        onClick={() => selectModel(model)}
        className={cn(
          'flex w-full items-center justify-between gap-2 rounded-md px-2.5 py-1.5',
          'text-left text-sm transition-colors disabled:pointer-events-none disabled:opacity-50',
          selected
            ? 'bg-accent/60 text-foreground'
            : 'text-foreground hover:bg-accent/40',
          focusRing.inset,
        )}
      >
        <span className="truncate font-mono text-xs">{model}</span>
        {selected && <Check className="h-3.5 w-3.5 shrink-0 text-primary" aria-hidden />}
      </button>
    );
  };

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          disabled={disabled}
          aria-label={t('modelSelector.label')}
          className={cn(
            'inline-flex h-8 items-center gap-1.5 rounded-md bg-transparent px-2.5',
            'text-xs font-medium text-foreground transition-colors',
            'hover:bg-muted/60 disabled:pointer-events-none disabled:opacity-50',
            focusRing.inset,
            className,
          )}
        >
          <Bot className={cn('h-3.5 w-3.5', triggerIcon)} />
          <span className="max-w-[140px] truncate">{triggerModel}</span>
          <ChevronDown className="h-3 w-3 text-muted-foreground" />
        </button>
      </PopoverTrigger>
      <PopoverContent
        align="end"
        sideOffset={6}
        className={cn(
          'w-[320px] rounded-lg border border-border bg-popover p-2',
          'shadow-elev outline-none',
          'data-[state=open]:animate-in data-[state=closed]:animate-out',
          'data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0',
          'data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95',
        )}
      >
        <div className="px-1.5 pb-2 pt-1">
          <p className="text-xs font-semibold text-foreground">
            {t('modelSelector.panelTitle')}
          </p>
          <p className="mt-0.5 text-xs leading-4 text-muted-foreground">
            {t('modelSelector.panelSubtitle')}
          </p>
        </div>

        {providers.length === 0 ? (
          <p className="px-2.5 pb-2 text-xs text-muted-foreground">
            {t('modelSelector.noProvider')}
          </p>
        ) : (
          <>
            {providers.length > 1 && (
              <div className="mb-2 flex flex-wrap gap-1.5 px-1.5">
                {providers.map((provider) => {
                  const active = provider.id === activeProvider?.id;
                  return (
                    <button
                      key={provider.id}
                      type="button"
                      disabled={busy}
                      onClick={() => selectProvider(provider)}
                      className={cn(
                        'rounded-md border px-2 py-1 text-xs transition-colors',
                        'disabled:pointer-events-none disabled:opacity-50',
                        active
                          ? 'border-primary/30 bg-accent/60 font-medium text-foreground'
                          : 'border-border text-muted-foreground hover:bg-accent/40',
                        focusRing.inset,
                      )}
                    >
                      {provider.name}
                    </button>
                  );
                })}
              </div>
            )}

            <div className="max-h-[220px] overflow-y-auto">
              {discoverMutation.isPending && discoveredModels.length === 0 ? (
                <div className="flex items-center gap-2 px-2.5 py-3 text-xs text-muted-foreground">
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                  {t('modelSelector.discovering')}
                </div>
              ) : (
                <>
                  {discoveredModels.length > 0 && (
                    <div className="mb-1">
                      <p className="px-2.5 py-1 text-[11px] font-medium text-muted-foreground">
                        {t('modelSelector.discovered')}
                      </p>
                      {discoveredModels.map(renderModelRow)}
                    </div>
                  )}
                  {suggestedModels.length > 0 && (
                    <div>
                      <p className="px-2.5 py-1 text-[11px] font-medium text-muted-foreground">
                        {t('modelSelector.suggested')}
                      </p>
                      {suggestedModels.map(renderModelRow)}
                    </div>
                  )}
                  {discoveredModels.length === 0 && suggestedModels.length === 0 && (
                    <p className="px-2.5 py-2 text-xs text-muted-foreground">
                      {t('modelSelector.noModels')}
                    </p>
                  )}
                </>
              )}
            </div>

            <div className="mt-2 flex items-center gap-1.5 border-t border-border/70 px-1.5 pt-2">
              <input
                value={customModel}
                onChange={(e) => setCustomModel(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    e.preventDefault();
                    applyCustomModel();
                  }
                }}
                placeholder={t('modelSelector.customPlaceholder')}
                disabled={busy}
                className={cn(
                  'h-7 min-w-0 flex-1 rounded-md border border-border bg-background px-2',
                  'text-xs text-foreground outline-none placeholder:text-muted-foreground',
                  'focus-visible:border-primary/40 focus-visible:ring-2 focus-visible:ring-primary/15',
                )}
              />
              <button
                type="button"
                onClick={applyCustomModel}
                disabled={busy || !customModel.trim()}
                className={cn(
                  'h-7 shrink-0 rounded-md bg-primary px-2.5 text-xs font-medium',
                  'text-primary-foreground transition-colors hover:bg-primary/90',
                  'disabled:pointer-events-none disabled:opacity-50',
                  focusRing.inset,
                )}
              >
                {busy ? (
                  <Loader2 className="h-3 w-3 animate-spin" />
                ) : (
                  t('modelSelector.apply')
                )}
              </button>
              <button
                type="button"
                onClick={() => activeProvider && discoverMutation.mutate(activeProvider.id)}
                disabled={!activeProvider || discoverMutation.isPending}
                aria-label={t('modelSelector.refresh')}
                title={t('modelSelector.refresh')}
                className={cn(
                  'flex h-7 w-7 shrink-0 items-center justify-center rounded-md',
                  'text-muted-foreground transition-colors hover:bg-accent',
                  'disabled:pointer-events-none disabled:opacity-50',
                  focusRing.inset,
                )}
              >
                <RefreshCw
                  className={cn(
                    'h-3.5 w-3.5',
                    discoverMutation.isPending && 'animate-spin',
                  )}
                />
              </button>
            </div>
          </>
        )}
      </PopoverContent>
    </Popover>
  );
}
