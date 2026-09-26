import { useMemo, useState, type ButtonHTMLAttributes } from 'react';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/ui/untitled';
import { Button, Input } from '@/ui/untitled';
import { Label } from '@/components/ui/label';
import { useToast } from '@/hooks/use-toast';
import {
  PROVIDER_METADATA,
  RUNTIME_SUPPORTED_PROVIDER_TYPES,
} from '@/lib/provider-types';
import type {
  ProviderType,
  ProviderConfigResponse,
  CreateProviderRequest,
  UpdateProviderRequest,
  DiscoverProviderModelsRequest,
  ProviderModelDiscoveryResult,
} from '@/lib/provider-types';
import { api, getApiErrorMessage } from '@/lib/api';
import { Info, RefreshCw, CheckCircle2 } from 'lucide-react';
import { cn } from '@/lib/utils';
import { useMutation } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';

// ---------------------------------------------------------------------------
// 原生 select/switch 封装（自 SettingsPage 迁出共享；任务中心与设置页共用
// 同一套表单，避免两份供应商表单漂移）。
// ---------------------------------------------------------------------------

// Select 族直接复用 untitled 的 Radix 精致版（此前的原生 <select> 包装已
// 移除——两套下拉观感不一致，且原生版在 Radix 关闭回焦时没有统一的
// focus-visible 焦点语义）。导出仅为兼容既有 import 路径。
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/ui/untitled';
export {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
};

type LocalSwitchProps = Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'onChange'> & {
  checked?: boolean;
  onCheckedChange?: (checked: boolean) => void;
};

export function Switch({ checked = false, onCheckedChange, className, disabled, ...props }: LocalSwitchProps) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      disabled={disabled}
      data-state={checked ? 'checked' : 'unchecked'}
      className={cn(
        "inline-flex h-6 w-11 shrink-0 items-center rounded-full border-2 border-transparent transition-colors",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2",
        checked ? "bg-primary" : "bg-input",
        disabled && "cursor-not-allowed opacity-50",
        className,
      )}
      onClick={(event) => {
        props.onClick?.(event);
        if (!event.defaultPrevented) onCheckedChange?.(!checked);
      }}
      {...props}
    >
      <span
        className={cn(
          "pointer-events-none block h-5 w-5 rounded-full bg-background shadow-lg ring-0 transition-transform",
          checked ? "translate-x-5" : "translate-x-0",
        )}
      />
    </button>
  );
}

// ---------------------------------------------------------------------------
// 供应商表单对话框（原 SettingsPage 内联实现，任务中心入口复用）。
// ---------------------------------------------------------------------------

function parseHeadersJson(input: string): { ok: true; value: Record<string, string> } | { ok: false; errorKey: string } {
  let parsed: unknown;
  try {
    parsed = JSON.parse(input || '{}');
  } catch {
    return { ok: false, errorKey: 'settings.validation.invalidJsonFormat' };
  }

  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    return { ok: false, errorKey: 'settings.validation.headersObject' };
  }

  for (const [key, value] of Object.entries(parsed)) {
    if (typeof key !== 'string' || typeof value !== 'string') {
      return { ok: false, errorKey: 'settings.validation.headersStringValues' };
    }
  }

  return { ok: true, value: parsed as Record<string, string> };
}

type DiscoveryErrorCategory =
  | 'runtimeUnavailable'
  | 'network'
  | 'auth'
  | 'notFound'
  | 'timeout'
  | 'invalid'
  | 'unknown';

/**
 * Classify a model-discovery failure into a UI error category so the toast
 * can distinguish runtime unavailable / network / 401-403 / 404 / timeout /
 * invalid payload instead of a single generic "fetch failed" message.
 * The backend detail (already secret-redacted) is always shown as the
 * description; only the title is categorized.
 */
function classifyDiscoveryError(status: string | undefined, message: string): DiscoveryErrorCategory {
  if (status === 'timeout' || /^timed out|request timed out/i.test(message)) {
    return 'timeout';
  }
  if (status === 'denied' || /HTTP (401|403)\b/.test(message)) {
    return 'auth';
  }
  if (/HTTP 404\b/.test(message)) {
    return 'notFound';
  }
  if (/provider runtime is not configured/i.test(message)) {
    return 'runtimeUnavailable';
  }
  if (/network error|transport error|httperror|error sending request|econnrefused|enotfound|dns|failed to connect|connect error/i.test(message)) {
    return 'network';
  }
  if (/invalid|protocol error|non-object|no recognizable model/i.test(message)) {
    return 'invalid';
  }
  return 'unknown';
}

function containsMaskedHeaderValue(headers: Record<string, string>): boolean {
  return Object.values(headers).some(value => value === '********');
}

interface ProviderEditDialogProps {
  provider: Partial<ProviderConfigResponse>;
  isOpen: boolean;
  onClose: () => void;
  onSave: (data: UpdateProviderRequest | CreateProviderRequest) => void;
  isLoading?: boolean;
  /** 任务中心入口显示类型预设卡（设置页保持原样不显示）。 */
  showPresets?: boolean;
}

export function ProviderEditDialog({ provider, isOpen, onClose, onSave, isLoading, showPresets = false }: ProviderEditDialogProps) {
  const { t } = useTranslation();
  const { toast } = useToast();
  const [name, setName] = useState(provider.name || '');
  const [providerType, setProviderType] = useState<ProviderType>(provider.provider_type || 'openai_compatible');
  const [baseUrl, setBaseUrl] = useState(provider.base_url || '');
  const [model, setModel] = useState(provider.model || '');
  const [apiKey, setApiKey] = useState('');
  const [apiKeyRef, setApiKeyRef] = useState(provider.api_key_ref || '');
  const [isDefault, setIsDefault] = useState(provider.is_default || false);
  const [enabled, setEnabled] = useState(provider.enabled !== false);
  const [timeoutSeconds, setTimeoutSeconds] = useState(provider.timeout_seconds || 60);
  const [maxTokens, setMaxTokens] = useState<number | ''>(provider.max_tokens || '');
  const [temperature, setTemperature] = useState<number | ''>(provider.temperature || '');

  const initialHeadersJson = useMemo(
    () => JSON.stringify(provider.default_headers || {}, null, 2),
    [provider.default_headers]
  );
  const [headersJson, setHeadersJson] = useState(initialHeadersJson);
  const [headerError, setHeaderError] = useState('');
  const [discoveredModels, setDiscoveredModels] = useState<string[]>([]);
  const [discoveryResult, setDiscoveryResult] = useState<ProviderModelDiscoveryResult | null>(null);

  const isEditing = Boolean(provider.id);
  const hasMaskedHeaders = containsMaskedHeaderValue(provider.default_headers || {});

  const isCLI = providerType === 'codex_cli' || providerType === 'claude_code';
  const isMCP = providerType === 'mcp_remote';
  const providerMeta = PROVIDER_METADATA[providerType];

  const discoverModelsMutation = useMutation({
    mutationFn: async () => {
      const parsedResult = parseHeadersJson(headersJson);
      if (!parsedResult.ok) {
        throw new Error(t(parsedResult.errorKey));
      }
      const headersChanged = headersJson !== initialHeadersJson;
      if (isEditing && headersChanged && containsMaskedHeaderValue(parsedResult.value)) {
        throw new Error(t('settings.validation.maskedHeaderValues'));
      }

      const payload: DiscoverProviderModelsRequest = {
        provider_id: provider.id || null,
        provider_type: providerType,
        base_url: baseUrl.trim() || null,
        model: model.trim() || null,
        api_key_ref: apiKeyRef.trim() || null,
        timeout_seconds: timeoutSeconds,
      };
      if (apiKey) payload.api_key = apiKey;
      if (!isEditing || headersChanged) payload.default_headers = parsedResult.value;
      return api.discoverProviderModels(payload);
    },
    onSuccess: (result) => {
      setDiscoveryResult(result);
      if (result.status !== 'ok') {
        setDiscoveredModels([]);
        toast({
          title: t(`settings.providerDialog.discoveryError.${classifyDiscoveryError(result.status, result.message)}`),
          description: result.message,
          variant: 'destructive',
        });
        return;
      }

      setDiscoveredModels(result.models);
      const caseMatchedModel = model
        ? result.models.find(candidate => candidate.toLowerCase() === model.toLowerCase())
        : undefined;
      if (caseMatchedModel && caseMatchedModel !== model) {
        setModel(caseMatchedModel);
        setDiscoveryResult({
          ...result,
          configured_model: caseMatchedModel,
          configured_model_available: true,
        });
        toast({
          title: t('settings.toast.modelCaseAdjusted'),
          description: t('settings.toast.modelCaseAdjustedDescription', {
            previous: model,
            model: caseMatchedModel,
          }),
        });
      } else {
        toast({
          title: t('settings.toast.modelsFetched'),
          description: result.configured_model && result.configured_model_available === false
            ? t('settings.providerDialog.configuredModelNotFound', {
                model: result.configured_model,
              })
            : t('settings.toast.modelsFetchedDescription', {
                count: result.models.length,
              }),
        });
      }
    },
    onError: (error: unknown) => {
      const message = getApiErrorMessage(error);
      setDiscoveryResult({
        status: 'error',
        message,
        endpoint: '',
        models: [],
      });
      setDiscoveredModels([]);
      toast({
        title: t(`settings.providerDialog.discoveryError.${classifyDiscoveryError(undefined, message)}`),
        description: message,
        variant: 'destructive',
      });
    },
  });

  const handleSave = () => {
    const parsedResult = parseHeadersJson(headersJson);
    if (!parsedResult.ok) {
      setHeaderError(t(parsedResult.errorKey));
      return;
    }

    const parsedHeaders = parsedResult.value;
    const headersChanged = headersJson !== initialHeadersJson;

    if (isEditing && headersChanged && containsMaskedHeaderValue(parsedHeaders)) {
      setHeaderError(t('settings.validation.maskedHeaderValues'));
      return;
    }

    setHeaderError('');

    const payload: UpdateProviderRequest | CreateProviderRequest = {
      name,
      provider_type: providerType,
      base_url: baseUrl || null,
      model: model || null,
      api_key_ref: apiKeyRef || null,
      timeout_seconds: timeoutSeconds,
      max_tokens: maxTokens !== '' ? Number(maxTokens) : null,
      temperature: temperature !== '' ? Number(temperature) : null,
      is_default: isDefault,
      enabled: enabled,
    };

    // Only send api_key if user typed it
    if (apiKey) {
      payload.api_key = apiKey;
    }

    onSave(payload);
  };

  return (
    <Dialog open={isOpen} onOpenChange={(open: boolean) => !open && onClose()}>
      <DialogContent className="w-[calc(100vw-2rem)] max-w-2xl overflow-hidden p-0" closeLabel={t('common.close')}>
        <DialogHeader className="border-b px-6 pb-4 pt-6 text-left">
          <DialogTitle>{provider.id ? t('settings.providerDialog.editTitle') : t('settings.providerDialog.addTitle')}</DialogTitle>
          <DialogDescription>{t('settings.providerDialog.description')}</DialogDescription>
        </DialogHeader>
        <div className="space-y-4 max-h-[70vh] overflow-y-auto px-6 py-4 pr-8 text-left">
            {showPresets && (
              <div className="space-y-2">
                <Label>{t('intake.providerPreset')}</Label>
                <div className="grid grid-cols-3 gap-2">
                  {(['openai', 'anthropic', 'gemini', 'openai_compatible', 'ollama', 'lm_studio'] as ProviderType[]).map((type) => {
                    const meta = PROVIDER_METADATA[type];
                    const active = providerType === type;
                    return (
                      <button
                        key={type}
                        type="button"
                        className={`relative rounded-lg border p-2.5 text-left transition-colors ${active ? 'border-primary bg-primary/5' : 'border-border bg-card hover:bg-accent'}`}
                        onClick={() => {
                          setProviderType(type);
                          setName(meta.label);
                          setBaseUrl(meta.defaultBaseUrl || '');
                          setModel(meta.defaultModel || '');
                          setApiKey('');
                          setDiscoveredModels([]);
                          setDiscoveryResult(null);
                        }}
                      >
                        <div className="text-sm font-semibold text-foreground">{meta.label}</div>
                        <div className="mt-1 truncate text-[11px] text-muted-foreground">{meta.defaultModel || t('common.noModel')}</div>
                        {active && <CheckCircle2 className="absolute right-1.5 top-1.5 h-4 w-4 text-primary" />}
                      </button>
                    );
                  })}
                </div>
              </div>
            )}

            <div className="grid grid-cols-2 gap-4">
              <div className="space-y-2">
                <Label>{t('settings.providerDialog.providerName')}</Label>
                <Input value={name} onChange={(e) => setName(e.target.value)} placeholder={t('settings.providerDialog.namePlaceholder')} />
              </div>
              <div className="space-y-2">
                <Label>{t('settings.providerDialog.type')}</Label>
                <Select value={providerType} onValueChange={(val) => {
                  const meta = PROVIDER_METADATA[val as ProviderType];
                  setProviderType(val as ProviderType);
                  setBaseUrl(meta?.defaultBaseUrl || '');
                  setModel(meta?.defaultModel || '');
                  setDiscoveredModels([]);
                  setDiscoveryResult(null);
                }}>
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {Object.entries(PROVIDER_METADATA)
                      .filter(([key]) => RUNTIME_SUPPORTED_PROVIDER_TYPES.has(key as ProviderType))
                      .map(([key, meta]) => (
                      <SelectItem key={key} value={key as ProviderType}>{meta.label}</SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            </div>

            {!isCLI && !isMCP ? (
              <>
                <div className="space-y-2">
                  <Label>{t('settings.providerDialog.baseUrl')}</Label>
                  <Input
                    value={baseUrl}
                    onChange={(e) => {
                      setBaseUrl(e.target.value);
                      setDiscoveredModels([]);
                      setDiscoveryResult(null);
                    }}
                    placeholder={t('settings.providerDialog.baseUrlPlaceholder')}
                  />
                </div>
                <div className="grid grid-cols-2 gap-4">
                  <div className="space-y-2">
                    <Label>{t('settings.providerDialog.model')}</Label>
                    <div className="flex gap-2">
                      <Input
                        value={model}
                        onChange={(e) => setModel(e.target.value)}
                        placeholder={providerMeta?.defaultModel || t('settings.providerDialog.modelPlaceholder')}
                      />
                      <Button
                        type="button"
                        variant="outline"
                        className="shrink-0"
                        onClick={() => discoverModelsMutation.mutate()}
                        disabled={discoverModelsMutation.isPending}
                      >
                        <RefreshCw className={cn('h-4 w-4', discoverModelsMutation.isPending && 'animate-spin')} />
                        {discoverModelsMutation.isPending
                          ? t('settings.providerDialog.fetchingModels')
                          : t('settings.providerDialog.fetchModels')}
                      </Button>
                    </div>
                    {discoveredModels.length > 0 && (
                      <Select
                        value={discoveredModels.includes(model) ? model : ''}
                        onValueChange={setModel}
                      >
                        <SelectTrigger>
                          <SelectValue placeholder={t('settings.providerDialog.selectDiscoveredModel')} />
                        </SelectTrigger>
                        <SelectContent>
                          {discoveredModels.map(discoveredModel => (
                            <SelectItem key={discoveredModel} value={discoveredModel}>
                              {discoveredModel}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    )}
                    {discoveryResult && (
                      <p className={cn(
                        'text-[11px]',
                        discoveryResult.status === 'ok' ? 'text-muted-foreground' : 'text-red-600',
                      )}>
                        {discoveryResult.status === 'ok'
                          ? discoveryResult.configured_model && discoveryResult.configured_model_available === false
                            ? t('settings.providerDialog.configuredModelNotFound', {
                                model: discoveryResult.configured_model,
                              })
                            : t('settings.providerDialog.modelsFound', { count: discoveryResult.models.length })
                          : discoveryResult.message}
                      </p>
                    )}
                    {discoveredModels.length === 0 && providerMeta?.suggestedModels && providerMeta.suggestedModels.length > 0 && (
                      <div className="flex flex-wrap gap-1.5">
                        {providerMeta.suggestedModels.map((suggestedModel) => (
                          <button
                            key={suggestedModel}
                            type="button"
                            className="rounded border border-border bg-card px-2 py-0.5 text-[11px] text-muted-foreground hover:border-primary/40 hover:text-primary"
                            onClick={() => setModel(suggestedModel)}
                          >
                            {suggestedModel}
                          </button>
                        ))}
                      </div>
                    )}
                  </div>
                  <div className="space-y-2">
                    <Label>{t('settings.providerDialog.apiKey')}</Label>
                    <Input
                      type="password"
                      value={apiKey}
                      onChange={(e) => {
                        setApiKey(e.target.value);
                        setDiscoveredModels([]);
                        setDiscoveryResult(null);
                      }}
                      placeholder={provider.has_api_key ? t('settings.providerDialog.storedOnBackend') : "sk-..."}
                    />
                  </div>
                </div>
                <div className="grid grid-cols-2 gap-4">
                   <div className="space-y-2">
                      <Label>{t('settings.providerDialog.apiKeyRef')}</Label>
                      <Input value={apiKeyRef} onChange={(e) => setApiKeyRef(e.target.value)} placeholder={t('settings.providerDialog.apiKeyRefPlaceholder')} />
                      <p className="text-[10px] text-muted-foreground">{t('settings.providerDialog.apiKeyRefHelp')}</p>
                   </div>
                   <div className="space-y-2">
                      <Label>{t('settings.providerDialog.timeoutSeconds')}</Label>
                      <Input type="number" value={timeoutSeconds} onChange={(e) => setTimeoutSeconds(Number(e.target.value))} />
                   </div>
                </div>
                <div className="grid grid-cols-2 gap-4">
                   <div className="space-y-2">
                      <Label>{t('settings.providerDialog.maxTokens')}</Label>
                      <Input type="number" value={maxTokens} onChange={(e) => setMaxTokens(e.target.value ? Number(e.target.value) : '')} placeholder={t('common.optional')} />
                   </div>
                   <div className="space-y-2">
                      <Label>{t('settings.providerDialog.temperature')}</Label>
                      <Input type="number" step="0.1" value={temperature} onChange={(e) => setTemperature(e.target.value ? Number(e.target.value) : '')} placeholder={t('common.optional')} />
                   </div>
                </div>
                <div className="space-y-2">
                  <Label>{t('settings.providerDialog.defaultHeaders')}</Label>
                  <textarea
                    value={headersJson}
                    onChange={(e: React.ChangeEvent<HTMLTextAreaElement>) => setHeadersJson(e.target.value)}
                    placeholder={'{\n  "X-Custom-Auth": "token"\n}'}
                    className="flex min-h-[80px] w-full rounded-md border border-slate-200 bg-white px-3 py-2 text-sm ring-offset-white placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-slate-950 focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50 font-mono text-xs"
                  />
                  {headerError && <p className="text-xs text-red-500">{headerError}</p>}
                  {isEditing && hasMaskedHeaders && !headerError && (
                    <p className="text-[10px] text-amber-600">{t('settings.providerDialog.maskedHeadersHelp')}</p>
                  )}
                </div>
              </>
            ) : isCLI ? (
              <div className="space-y-4">
                <div className="flex items-start gap-2 rounded-lg border border-info-border bg-info-soft p-3 text-xs text-info-foreground">
                  <Info className="h-4 w-4 shrink-0" />
                  <p>{t('settings.providerDialog.localCliNotice')}</p>
                </div>
              </div>
            ) : (
              <div className="space-y-4">
                <div className="flex items-start gap-2 rounded-lg border border-info-border bg-info-soft p-3 text-xs text-info-foreground">
                  <Info className="h-4 w-4 shrink-0" />
                  <p>{t('settings.providerDialog.mcpRemoteNotice')}</p>
                </div>
                <div className="space-y-2">
                  <Label>{t('settings.providerDialog.serverUrl')}</Label>
                  <Input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} placeholder={t('settings.providerDialog.mcpServerUrlPlaceholder')} />
                </div>
                <div className="space-y-2">
                  <Label>{t('settings.providerDialog.authTokenOptional')}</Label>
                  <Input type="password" value={apiKey} onChange={(e) => setApiKey(e.target.value)} placeholder={provider.has_api_key ? t('settings.providerDialog.storedOnBackend') : "mcp-token-..."} />
                </div>
              </div>
            )}

            <div className="grid grid-cols-2 gap-4 pt-2">
              <div className="flex items-center justify-between p-3 border rounded-lg">
                <div className="space-y-0.5">
                  <Label className="text-sm">{t('common.enabled')}</Label>
                  <p className="text-[10px] text-muted-foreground">{t('settings.providerDialog.allowProvider')}</p>
                </div>
                <Switch checked={enabled} onCheckedChange={setEnabled} />
              </div>
              <div className="flex items-center justify-between p-3 border rounded-lg">
                <div className="space-y-0.5">
                  <Label className="text-sm">{t('settings.providerDialog.setAsDefault')}</Label>
                  <p className="text-[10px] text-muted-foreground">{t('settings.providerDialog.setAsDefaultHelp')}</p>
                </div>
                <Switch checked={isDefault} onCheckedChange={setIsDefault} />
              </div>
            </div>
        </div>
        <DialogFooter className="gap-3 border-t px-6 py-4 sm:space-x-0">
          <Button variant="ghost" onClick={onClose} disabled={isLoading}>{t('common.cancel')}</Button>
          <Button onClick={handleSave} disabled={isLoading}>
            {isLoading ? t('common.saving') : t('settings.providerDialog.saveProvider')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
