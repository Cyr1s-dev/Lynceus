import { useEffect, useMemo, useRef, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import {
  Cable, CheckCircle2, XCircle, Clock, Settings2,
  Search, ExternalLink, Play, RefreshCw, AlertTriangle,
  ChevronDown, ChevronRight, Download,
} from 'lucide-react';
import { api, getApiErrorMessage } from '@/lib/api';
import { cn } from '@/lib/utils';
import { Card } from '@/components/ui/card';
import { useToast } from '@/hooks/use-toast';
import type {
  ModuleHealthResult,
  ApiToolInvocation,
  ToolInstallJob,
} from '@/lib/types';
import {
  Button,
  Input,
  Badge,
  Switch,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
  Drawer,
  PageHeader,
  MetricCard,
  DataToolbar,
  EngineStatusBadge,
  EntityCard,
  EmptyState,
  type StatusTone,
} from '@/ui/untitled';

type EngineStatusKind =
  | 'checking'
  | 'installed'
  | 'configured'
  | 'ready'
  | 'failed'
  | 'missing'
  | 'disabled';

type CatalogTool = NonNullable<Awaited<ReturnType<typeof api.getToolCatalog>>>[number];

type DomainGroupKey = 'webSast' | 'webDast' | 'recon' | 'binary' | 'exploitability' | 'cloud' | 'other';

const DOMAIN_GROUP_MAP: Record<string, DomainGroupKey> = {
  web_sast: 'webSast',
  code_deep_sast: 'webSast',
  web_dast: 'webDast',
  asset_recon: 'recon',
  web_recon: 'recon',
  content_discovery: 'recon',
  fingerprint_intelligence: 'recon',
  exposure_intelligence: 'recon',
  internal_surface: 'recon',
  binary_static: 'binary',
  binary_dynamic: 'binary',
  exploitability: 'exploitability',
  exploitability_validation: 'exploitability',
  cloud_native: 'cloud',
};

function getDomainGroup(domain: string): DomainGroupKey {
  return DOMAIN_GROUP_MAP[domain] || 'other';
}

const DOMAIN_GROUP_ORDER: DomainGroupKey[] = ['webSast', 'webDast', 'recon', 'binary', 'exploitability', 'cloud', 'other'];

export function EngineCatalogPage() {
  const { t } = useTranslation();
  const { toast } = useToast();
  const queryClient = useQueryClient();

  const { data: tools = [], isLoading, isRefetching } = useQuery({
    queryKey: ['tool-catalog'],
    queryFn: api.getToolCatalog,
  });

  // 探测快照元信息（最后检测时间）：与 list 一样只读快照，不触发探测。
  const { data: detectionStatus } = useQuery({
    queryKey: ['tool-catalog-status'],
    queryFn: api.getToolCatalogStatus,
    refetchInterval: (query) => {
      const state = query.state.data?.state;
      return state === 'unknown' || state === 'refreshing' ? 1500 : false;
    },
  });
  const appliedSnapshot = useRef<string | null>(null);

  useEffect(() => {
    if (
      detectionStatus?.state === 'ready' &&
      detectionStatus.detected_at &&
      appliedSnapshot.current !== detectionStatus.detected_at
    ) {
      appliedSnapshot.current = detectionStatus.detected_at;
      void queryClient.invalidateQueries({ queryKey: ['tool-catalog'] });
    }
  }, [detectionStatus?.detected_at, detectionStatus?.state, queryClient]);
  const [isRefreshing, setIsRefreshing] = useState(false);

  const { data: modules = [] } = useQuery({
    queryKey: ['modules'],
    queryFn: api.getModules,
  });

  const { data: toolInvocations = [] } = useQuery({
    queryKey: ['tool-invocations'],
    queryFn: api.getToolInvocations,
  });

  const { data: installationJobs = [] } = useQuery({
    queryKey: ['tool-installations'],
    queryFn: () => api.getToolInstallations(),
    refetchInterval: (query) => {
      const jobs = query.state.data as ToolInstallJob[] | undefined;
      return jobs?.some((job) => job.status === 'queued' || job.status === 'running')
        ? 1500
        : false;
    },
  });

  const [searchQuery, setSearchQuery] = useState('');
  const [statusFilter, setStatusFilter] = useState('all');
  const [domainFilter, setDomainFilter] = useState('all');

  const [isPathDialogOpen, setIsPathDialogOpen] = useState(false);
  const [selectedTool, setSelectedTool] = useState<CatalogTool | null>(null);
  const [dialogPath, setDialogPath] = useState('');
  const [dialogEnabled, setDialogEnabled] = useState(true);
  const [isSavingPath, setIsSavingPath] = useState(false);
  // 调用参数编辑态：文本型（string/path/integer/string_list）存文本，
  // boolean 存开关值；保存时按 invocation 声明重建整体替换集。
  const [dialogParams, setDialogParams] = useState<Record<string, string>>({});
  const [dialogBooleans, setDialogBooleans] = useState<Record<string, boolean>>({});
  // env 编辑态：只允许写入新值（已保存值不回显）；envCleared 标记显式清除。
  const [dialogEnv, setDialogEnv] = useState<Record<string, string>>({});
  const [envCleared, setEnvCleared] = useState<Record<string, boolean>>({});

  /** 打开对话框时用已存 settings（缺省用声明默认值）预填参数编辑态。 */
  const initializeDialogSettings = (tool: CatalogTool) => {
    const configured = tool.configured_settings?.params ?? {};
    const textState: Record<string, string> = {};
    const boolState: Record<string, boolean> = {};
    for (const param of tool.invocation?.params ?? []) {
      const hasStored = Object.prototype.hasOwnProperty.call(configured, param.key);
      const value = hasStored ? configured[param.key] : param.default;
      if (param.kind === 'boolean') {
        boolState[param.key] = typeof value === 'boolean' ? value : false;
      } else if (param.kind === 'string_list') {
        textState[param.key] = Array.isArray(value) ? value.map(String).join(', ') : '';
      } else if (param.kind === 'integer') {
        textState[param.key] = value === null || value === undefined ? '' : String(value);
      } else {
        textState[param.key] = typeof value === 'string' ? value : '';
      }
    }
    setDialogParams(textState);
    setDialogBooleans(boolState);
    setDialogEnv({});
    setEnvCleared({});
  };

  const [isLogsDrawerOpen, setIsLogsDrawerOpen] = useState(false);
  const [selectedLogsTool, setSelectedLogsTool] = useState<CatalogTool | null>(null);

  const [healthResults, setHealthResults] = useState<Record<string, ModuleHealthResult>>({});
  const [testingTools, setTestingTools] = useState<Record<string, boolean>>({});
  const [startingInstalls, setStartingInstalls] = useState<Record<string, boolean>>({});
  const [collapsedGroups, setCollapsedGroups] = useState<Set<DomainGroupKey>>(new Set());
  const seenTerminalJobs = useRef<Set<string>>(new Set());
  const pageOpenedAt = useRef(0);

  useEffect(() => {
    pageOpenedAt.current = Date.now();
  }, []);

  const latestInstallByTool = useMemo(() => {
    const latest = new Map<string, ToolInstallJob>();
    for (const job of installationJobs) {
      const key = job.tool_id.toLowerCase();
      if (!latest.has(key)) latest.set(key, job);
    }
    return latest;
  }, [installationJobs]);

  useEffect(() => {
    const terminal = new Set([
      'installed',
      'already_present',
      'manual',
      'skipped',
      'unsupported',
      'failed',
    ]);
    for (const job of installationJobs) {
      if (!terminal.has(job.status) || seenTerminalJobs.current.has(job.id)) continue;
      seenTerminalJobs.current.add(job.id);
      // Historical process-local jobs remain visible after navigation, but
      // should not replay completion notifications when the page is reopened.
      if (new Date(job.created_at).getTime() < pageOpenedAt.current - 1000) continue;
      const successful = job.status === 'installed' || job.status === 'already_present';
      toast({
        title: successful
          ? t('engineCatalog.install.completed')
          : t('engineCatalog.install.attention'),
        description: job.message || `${job.tool_id}: ${job.status}`,
        variant: job.status === 'failed' ? 'destructive' : undefined,
      });
      void queryClient.invalidateQueries({ queryKey: ['tool-catalog'] });
      void queryClient.invalidateQueries({ queryKey: ['modules'] });
    }
  }, [installationJobs, queryClient, t, toast]);

  const toggleGroup = (key: DomainGroupKey) => {
    setCollapsedGroups(prev => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  const handleRedetect = async () => {
    // 真实探测只走 POST /tool-catalog/refresh（PATH 扫描 + 身份校验）；
    // 普通 list 永远只读快照，不会同步探测任何 binary。
    setIsRefreshing(true);
    try {
      await api.refreshToolCatalog();
      await queryClient.invalidateQueries({ queryKey: ['tool-catalog'] });
      await queryClient.invalidateQueries({ queryKey: ['tool-catalog-status'] });
      await queryClient.invalidateQueries({ queryKey: ['modules'] });
      toast({
        title: t('common.success'),
        description: t('engineCatalog.toast.redetectSuccess', { defaultValue: '检测状态已刷新' }),
      });
    } catch (err) {
      toast({
        title: t('common.error'),
        description: String(err),
        variant: 'destructive',
      });
    } finally {
      setIsRefreshing(false);
    }
  };

  const getExamplePath = (tool: CatalogTool) => {
    const execName = tool.executable_names?.[0] || `${tool.id}.exe`;
    return `C:\\Tools\\${tool.name}\\${execName}`;
  };

  /** 卡片图标底色随引擎状态变化，便于整页扫读。 */
  const engineTone = (status: EngineStatusKind): StatusTone => {
    switch (status) {
      case 'ready':
        return 'success';
      case 'installed':
      case 'configured':
        return 'info';
      case 'failed':
        return 'danger';
      case 'missing':
        return 'warning';
      case 'checking':
        return 'neutral';
      default:
        return 'neutral';
    }
  };

  const getToolStatus = (tool: CatalogTool): EngineStatusKind => {
    // 1. Check matching module status in database
    const matchingModule = modules.find((m) =>
      m.module_type === 'local_tool' &&
      typeof m.metadata?.tool_name === 'string' &&
      m.metadata.tool_name.toLowerCase() === tool.id.toLowerCase(),
    );
    if (matchingModule && matchingModule.enabled === false) {
      return 'disabled';
    }

    // 2. Check health test results from manual trigger
    const health = healthResults[tool.id];
    if (health && health.ok === false) {
      return 'failed';
    }

    if (tool.detection?.availability === 'unknown') {
      return 'checking';
    }

    // 3. Distinguish installed (configured + detected) from configured (path set but not detected)
    const hasConfiguredPath = !!matchingModule || !!tool.detection?.executable_path;
    if (tool.detection?.available === true) {
      return hasConfiguredPath && matchingModule ? 'installed' : 'ready';
    }

    if (hasConfiguredPath) {
      return 'configured';
    }

    return 'missing';
  };

  // WP5：内置工具运行时启停（写 local-tools.json 的 enabled，无需路径）。
  const toggleEnabledMutation = useMutation({
    mutationFn: async ({ toolId, enabled }: { toolId: string; enabled: boolean }) =>
      api.configureToolPath(toolId, { executable_path: null, enabled }),
    onSuccess: (_result, variables) => {
      queryClient.invalidateQueries({ queryKey: ['tool-catalog'] });
      toast({
        title: variables.enabled
          ? t('engineCatalog.toggleEnabledTitle')
          : t('engineCatalog.toggleDisabledTitle'),
        description: variables.toolId,
      });
    },
    onError: (err) => {
      toast({
        title: t('common.failed', { defaultValue: '操作失败' }),
        description: getApiErrorMessage(err),
        variant: 'destructive',
      });
    },
  });

  const handleTestTool = async (toolId: string) => {
    setTestingTools(prev => ({ ...prev, [toolId]: true }));
    try {
      const res = await api.testToolCatalog(toolId);
      setHealthResults(prev => ({ ...prev, [toolId]: res }));
      if (res.ok) {
        toast({
          title: t('common.success'),
          description: t('engineCatalog.healthCheckPassed', { tool: toolId }),
        });
      } else {
        toast({
          title: t('common.failed', { defaultValue: '检测失败' }),
          description: res.message || t('engineCatalog.healthCheckFailed', { tool: toolId }),
          variant: 'destructive',
        });
      }
    } catch (err) {
      const errorResult: ModuleHealthResult = {
        module_id: toolId,
        ok: false,
        status: 'error',
        message: String(err),
        checked_at: new Date().toISOString(),
        latency_ms: 0,
        raw: { error: String(err) }
      };
      setHealthResults(prev => ({ ...prev, [toolId]: errorResult }));
      toast({
        title: t('common.error'),
        description: String(err),
        variant: 'destructive',
      });
    } finally {
      setTestingTools(prev => ({ ...prev, [toolId]: false }));
      queryClient.invalidateQueries({ queryKey: ['tool-catalog'] });
      queryClient.invalidateQueries({ queryKey: ['modules'] });
    }
  };

  const handleInstallTool = async (tool: CatalogTool) => {
    setStartingInstalls(prev => ({ ...prev, [tool.id]: true }));
    try {
      const force = tool.detection?.available === true;
      const job = await api.installToolCatalog(tool.id, { force });
      toast({
        title: t('engineCatalog.install.started'),
        description: t('engineCatalog.install.startedHint', {
          tool: tool.name,
          method: job.method,
        }),
      });
      await queryClient.invalidateQueries({ queryKey: ['tool-installations'] });
    } catch (err) {
      toast({
        title: t('common.error'),
        description: String(err),
        variant: 'destructive',
      });
    } finally {
      setStartingInstalls(prev => ({ ...prev, [tool.id]: false }));
    }
  };

  /** 按声明把编辑态重建为 params（整体替换）/ env（逐 key 合并）请求体。 */
  const buildInvocationSettings = (
    tool: CatalogTool,
  ): { params: Record<string, unknown>; env: Record<string, string | null> } | { error: string } => {
    const params: Record<string, unknown> = {};
    for (const param of tool.invocation?.params ?? []) {
      if (param.kind === 'boolean') {
        params[param.key] = dialogBooleans[param.key] ?? false;
        continue;
      }
      const text = (dialogParams[param.key] ?? '').trim();
      if (!text) continue; // 清空 = 从整体替换集中移除该参数
      if (param.kind === 'integer') {
        const parsed = Number(text);
        if (!Number.isInteger(parsed)) {
          return { error: t('engineCatalog.configDialog.invalidInteger', { key: param.key }) };
        }
        const min = param.minimum ?? null;
        const max = param.maximum ?? null;
        if ((min !== null && parsed < min) || (max !== null && parsed > max)) {
          return {
            error: t('engineCatalog.configDialog.integerOutOfRange', {
              key: param.key,
              min: min ?? '-∞',
              max: max ?? '+∞',
            }),
          };
        }
        params[param.key] = parsed;
      } else if (param.kind === 'string_list') {
        const items = text.split(',').map((item) => item.trim()).filter(Boolean);
        if (items.length) params[param.key] = items;
      } else {
        params[param.key] = text;
      }
    }
    const env: Record<string, string | null> = {};
    for (const keySpec of tool.invocation?.env_keys ?? []) {
      const value = dialogEnv[keySpec.name];
      if (typeof value === 'string' && value.length > 0) {
        env[keySpec.name] = value;
      } else if (envCleared[keySpec.name]) {
        env[keySpec.name] = null;
      }
    }
    return { params, env };
  };

  const handleSavePath = async () => {
    if (!selectedTool) return;
    setIsSavingPath(true);
    try {
      // 仅当工具声明了 invocation 才提交 params/env；无声明面工具保持旧语义。
      let settings: { params: Record<string, unknown>; env: Record<string, string | null> } | null = null;
      if (selectedTool.invocation) {
        const built = buildInvocationSettings(selectedTool);
        if ('error' in built) {
          toast({ title: t('common.error'), description: built.error, variant: 'destructive' });
          setIsSavingPath(false);
          return;
        }
        settings = built;
      }
      await api.configureToolPath(selectedTool.id, {
        executable_path: dialogPath ? dialogPath.trim() : null,
        enabled: dialogEnabled,
        ...(settings ? { params: settings.params, env: settings.env } : {}),
      });
      toast({
        title: t('common.success'),
        description: t('engineCatalog.toast.saveReloadSuccess'),
      });
      setIsPathDialogOpen(false);
      queryClient.invalidateQueries({ queryKey: ['tool-catalog'] });
      queryClient.invalidateQueries({ queryKey: ['modules'] });
    } catch (err) {
      toast({
        title: t('common.error'),
        description: String(err),
        variant: 'destructive',
      });
    } finally {
      setIsSavingPath(false);
    }
  };

  // Filter tools locally — computed before early return so groupedTools useMemo is not conditional
  const filteredTools = tools.filter((tool) => {
    const matchesSearch =
      !searchQuery ||
      tool.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      tool.id.toLowerCase().includes(searchQuery.toLowerCase()) ||
      tool.domain.toLowerCase().includes(searchQuery.toLowerCase()) ||
      (tool.description && tool.description.toLowerCase().includes(searchQuery.toLowerCase()));

    const status = getToolStatus(tool);
    let matchesStatus = true;
    if (statusFilter !== 'all') {
      matchesStatus = status === statusFilter;
    }

    let matchesDomain = true;
    if (domainFilter !== 'all') {
      matchesDomain = tool.domain === domainFilter;
    }

    return matchesSearch && matchesStatus && matchesDomain;
  });

  const groupedTools = useMemo(() => {
    const groups: Map<DomainGroupKey, CatalogTool[]> = new Map();
    for (const key of DOMAIN_GROUP_ORDER) {
      groups.set(key, []);
    }
    for (const tool of filteredTools) {
      const groupKey = getDomainGroup(tool.domain);
      groups.get(groupKey)?.push(tool);
    }
    return groups;
  }, [filteredTools]);

  if (isLoading) {
    return <div className="p-8 text-sm text-muted-foreground">{t('common.loading')}</div>;
  }

  // Statistics Calculation using getToolStatus
  const totalCount = tools.length;
  const installedCount = tools.filter((tool) => getToolStatus(tool) === 'installed').length;
  const configuredCount = tools.filter((tool) => getToolStatus(tool) === 'configured').length;
  const readyCount = tools.filter((tool) => getToolStatus(tool) === 'ready').length;
  const failedCount = tools.filter((tool) => getToolStatus(tool) === 'failed').length;
  const missingCount = tools.filter((tool) => getToolStatus(tool) === 'missing').length;
  const disabledCount = tools.filter((tool) => getToolStatus(tool) === 'disabled').length;
  const checkingCount = tools.filter((tool) => getToolStatus(tool) === 'checking').length;

  const engineStatusLabel = (kind: EngineStatusKind): string => {
    switch (kind) {
      case 'checking':
        return t('engineCatalog.status.checking');
      case 'installed':
        return t('engineCatalog.status.installed');
      case 'configured':
        return t('engineCatalog.status.configured');
      case 'ready':
        return t('engineCatalog.status.ready');
      case 'failed':
        return t('engineCatalog.status.failed');
      case 'missing':
        return t('engineCatalog.status.missing');
      case 'disabled':
        return t('engineCatalog.status.disabled');
      default:
        return kind;
    }
  };

  return (
    <div className="page-stack">
      <PageHeader
        icon={<Cable className="h-5 w-5" />}
        title={t('engineCatalog.title')}
        description={t('engineCatalog.description')}
        actions={
          <div className="flex items-center gap-3">
            <span className="text-xs text-muted-foreground">
              {detectionStatus?.state === 'refreshing'
                ? t('engineCatalog.detection.refreshing')
                : detectionStatus?.state === 'error'
                  ? t('engineCatalog.detection.error', {
                      error: detectionStatus.last_error || '',
                    })
                  : detectionStatus?.detected_at
                    ? t('engineCatalog.detection.lastDetectedAt', {
                        time: new Date(detectionStatus.detected_at).toLocaleString(),
                      })
                    : t('engineCatalog.detection.neverDetected')}
            </span>
            <Button
              variant="outline"
              size="sm"
              onClick={handleRedetect}
              disabled={isRefreshing || isRefetching}
            >
              <RefreshCw className={isRefreshing || isRefetching ? 'h-4 w-4 animate-spin' : 'h-4 w-4'} />
              {t('engineCatalog.actions.redetect')}
            </Button>
          </div>
        }
      />

      {/* Engine health metric row */}
      <div className="grid grid-cols-2 gap-4 md:grid-cols-4 xl:grid-cols-7">
        <MetricCard
          label={t('engineCatalog.managerStats.total')}
          value={totalCount}
          icon={<Cable className="h-4 w-4" />}
          tone="neutral"
        />
        <MetricCard
          label={t('engineCatalog.status.checking')}
          value={checkingCount}
          icon={<RefreshCw className="h-4 w-4" />}
          tone="neutral"
        />
        <MetricCard
          label={t('engineCatalog.status.installed')}
          value={installedCount}
          icon={<CheckCircle2 className="h-4 w-4" />}
          tone="success"
        />
        <MetricCard
          label={t('engineCatalog.status.configured')}
          value={configuredCount}
          icon={<Settings2 className="h-4 w-4" />}
          tone="info"
        />
        <MetricCard
          label={t('engineCatalog.status.ready')}
          value={readyCount}
          icon={<CheckCircle2 className="h-4 w-4" />}
          tone="success"
        />
        <MetricCard
          label={t('engineCatalog.status.failed')}
          value={failedCount}
          icon={<AlertTriangle className="h-4 w-4" />}
          tone="danger"
        />
        <MetricCard
          label={t('engineCatalog.status.missing')}
          value={missingCount + disabledCount}
          icon={<XCircle className="h-4 w-4" />}
          tone="warning"
        />
      </div>

      {/* Filter toolbar */}
      <DataToolbar
        filters={
          <>
            <div className="relative w-full md:max-w-xs">
              <Search className="pointer-events-none absolute left-3 top-2.5 h-4 w-4 text-muted-foreground" />
              <Input
                placeholder={t('engineCatalog.filters.searchPlaceholder')}
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                className="h-9 pl-9"
              />
            </div>
          </>
        }
        actions={
          <>
            <div className="flex items-center gap-2">
              <span className="label-spec shrink-0">{t('engineCatalog.filters.status')}</span>
              <Select value={statusFilter} onValueChange={setStatusFilter}>
                <SelectTrigger className="h-9 w-[150px]">
                  <SelectValue placeholder={t('engineCatalog.filters.all')} />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">{t('engineCatalog.filters.all')}</SelectItem>
                  <SelectItem value="checking">{engineStatusLabel('checking')}</SelectItem>
                  <SelectItem value="installed">{engineStatusLabel('installed')}</SelectItem>
                  <SelectItem value="configured">{engineStatusLabel('configured')}</SelectItem>
                  <SelectItem value="ready">{engineStatusLabel('ready')}</SelectItem>
                  <SelectItem value="failed">{engineStatusLabel('failed')}</SelectItem>
                  <SelectItem value="missing">{engineStatusLabel('missing')}</SelectItem>
                  <SelectItem value="disabled">{engineStatusLabel('disabled')}</SelectItem>
                </SelectContent>
              </Select>
            </div>
            <div className="flex items-center gap-2">
              <span className="label-spec shrink-0">{t('engineCatalog.filters.domain')}</span>
              <Select value={domainFilter} onValueChange={setDomainFilter}>
                <SelectTrigger className="h-9 w-[180px]">
                  <SelectValue placeholder={t('engineCatalog.filters.all')} />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="all">{t('engineCatalog.filters.domains.all')}</SelectItem>
                  <SelectItem value="web_sast">{t('engineCatalog.filters.domains.web_sast')}</SelectItem>
                  <SelectItem value="web_dast">{t('engineCatalog.filters.domains.web_dast')}</SelectItem>
                  <SelectItem value="asset_recon">{t('engineCatalog.filters.domains.asset_recon')}</SelectItem>
                  <SelectItem value="web_recon">{t('engineCatalog.filters.domains.web_recon')}</SelectItem>
                  <SelectItem value="content_discovery">{t('engineCatalog.filters.domains.content_discovery')}</SelectItem>
                  <SelectItem value="fingerprint_intelligence">{t('engineCatalog.filters.domains.fingerprint_intelligence')}</SelectItem>
                  <SelectItem value="exploitability_validation">{t('engineCatalog.filters.domains.exploitability_validation')}</SelectItem>
                  <SelectItem value="internal_surface">{t('engineCatalog.filters.domains.internal_surface')}</SelectItem>
                  <SelectItem value="code_deep_sast">{t('engineCatalog.filters.domains.code_deep_sast')}</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </>
        }
      />

      {/* Engine table — grouped by domain */}
      {filteredTools.length === 0 ? (
        <EmptyState
          variant="card"
          icon={<Search className="h-6 w-6" />}
          title={t('common.noData')}
        />
      ) : (
        <div className="page-stack">
          {DOMAIN_GROUP_ORDER.map((groupKey) => {
            const groupTools = groupedTools.get(groupKey) ?? [];
            if (groupTools.length === 0) return null;
            const isCollapsed = collapsedGroups.has(groupKey);
            return (
              <Card key={groupKey} className="overflow-hidden shadow-xs">
                <button
                  type="button"
                  onClick={() => toggleGroup(groupKey)}
                  className="flex w-full items-center justify-between gap-3 border-b border-border bg-muted/30 px-4 py-2.5 text-left hover:bg-muted/50 transition-colors"
                >
                  <div className="flex items-center gap-2">
                    {isCollapsed ? (
                      <ChevronRight className="h-4 w-4 text-muted-foreground" />
                    ) : (
                      <ChevronDown className="h-4 w-4 text-muted-foreground" />
                    )}
                    <span className="text-sm font-semibold text-foreground">
                      {t(`engineCatalog.domainGroups.${groupKey}`)}
                    </span>
                    <Badge tone="neutral" size="sm">{groupTools.length}</Badge>
                  </div>
                </button>
                {!isCollapsed && (
                  <div className="grid grid-cols-1 gap-3 p-4 lg:grid-cols-2 2xl:grid-cols-3">
                    {groupTools.map((tool) => {
                      const status = getToolStatus(tool);
                      const health = healthResults[tool.id];
                      const installJob = latestInstallByTool.get(tool.id.toLowerCase());
                      const installActive =
                        startingInstalls[tool.id] ||
                        installJob?.status === 'queued' ||
                        installJob?.status === 'running';
                      const canAutoInstall =
                        !!tool.install && tool.install.method !== 'manual';
                      const matchingModule = modules.find((m) =>
                        m.module_type === 'local_tool' &&
                        typeof m.metadata?.tool_name === 'string' &&
                        m.metadata.tool_name.toLowerCase() === tool.id.toLowerCase(),
                      );
                      const installLabel = installActive
                        ? t('engineCatalog.install.installing')
                        : tool.install?.method === 'manual'
                          ? t('engineCatalog.install.instructions')
                          : tool.detection?.available
                            ? t('engineCatalog.install.reinstall')
                            : t('engineCatalog.install.action');
                      const hint =
                        status === 'missing'
                          ? t('engineCatalog.positiveFraming.notDetectedHint')
                          : status === 'failed'
                            ? t('engineCatalog.positiveFraming.notConfiguredHint')
                            : tool.adapter_status !== 'implemented'
                              ? t('engineCatalog.positiveFraming.adapterPlannedHint')
                              : '';
                      const detail = [tool.description, hint].filter(Boolean).join(' ') || undefined;

                      return (
                        <EntityCard
                          key={tool.id}
                          tone={engineTone(status)}
                          icon={<Cable className="h-4 w-4" />}
                          title={tool.name}
                          meta={
                            <div className="space-y-0.5">
                              <div className="flex items-center gap-1.5">
                                <span className="font-mono text-[11px]">{tool.id}</span>
                                {tool.output_format && (
                                  <span className="rounded border border-border bg-muted/50 px-1 text-[10px] uppercase tracking-wide">
                                    {tool.output_format}
                                  </span>
                                )}
                              </div>
                              <div
                                className="truncate font-mono text-[11px]"
                                title={tool.detection?.executable_path || undefined}
                              >
                                {tool.detection?.executable_path || t('engineCatalog.unconfigured')}
                              </div>
                            </div>
                          }
                          status={
                            <>
                              <Badge
                                tone={tool.adapter_status === 'implemented' ? 'info' : 'neutral'}
                                size="sm"
                              >
                                {tool.adapter_status === 'implemented'
                                  ? t('engineCatalog.status.available')
                                  : t('engineCatalog.status.planned')}
                              </Badge>
                              <EngineStatusBadge
                                status={status}
                                label={engineStatusLabel(status)}
                                tooltip={health?.message || (health?.raw?.stderr as string) || undefined}
                              />
                              <button
                                type="button"
                                role="switch"
                                aria-checked={tool.enabled}
                                title={tool.enabled ? t('engineCatalog.toggleDisableHint') : t('engineCatalog.toggleEnableHint')}
                                onClick={(event) => {
                                  event.stopPropagation();
                                  toggleEnabledMutation.mutate({
                                    toolId: tool.id,
                                    enabled: !tool.enabled,
                                  });
                                }}
                                className={cn(
                                  'relative inline-flex h-4 w-7 shrink-0 items-center rounded-full transition-colors',
                                  tool.enabled ? 'bg-success' : 'bg-muted-foreground/40',
                                  toggleEnabledMutation.isPending && 'opacity-60',
                                )}
                              >
                                <span
                                  className={cn(
                                    'inline-block h-3 w-3 transform rounded-full bg-background shadow transition-transform',
                                    tool.enabled ? 'translate-x-3.5' : 'translate-x-0.5',
                                  )}
                                />
                              </button>
                            </>
                          }
                          description={detail}
                          actions={
                            <div className="flex items-center gap-1.5">
                              {installJob?.message && !installActive && (
                                <span
                                  className={`max-w-20 truncate text-[10px] leading-tight ${
                                    installJob.status === 'failed'
                                      ? 'text-danger-foreground'
                                      : 'text-muted-foreground'
                                  }`}
                                  title={installJob.message}
                                >
                                  {installJob.message}
                                </span>
                              )}
                              <Button
                                variant={canAutoInstall ? 'default' : 'outline'}
                                size="sm"
                                onClick={() => handleInstallTool(tool)}
                                disabled={!tool.install || installActive}
                                className="h-7 gap-1 px-2.5 text-xs font-medium"
                              >
                                {installActive ? (
                                  <RefreshCw className="h-3 w-3 animate-spin" />
                                ) : (
                                  <Download className="h-3 w-3" />
                                )}
                                {installLabel}
                              </Button>
                              <Button
                                variant="outline"
                                size="sm"
                                onClick={() => {
                                  setSelectedTool(tool);
                                  setDialogPath(tool.detection?.executable_path || '');
                                  setDialogEnabled(matchingModule ? matchingModule.enabled : true);
                                  initializeDialogSettings(tool);
                                  setIsPathDialogOpen(true);
                                }}
                                className="h-7 gap-1 px-2.5 text-xs font-medium"
                              >
                                <Settings2 className="h-3 w-3" />
                                {t('engineCatalog.actions.configure')}
                              </Button>
                              <Button
                                variant="outline"
                                size="sm"
                                onClick={() => handleTestTool(tool.id)}
                                disabled={tool.adapter_status !== 'implemented' || testingTools[tool.id]}
                                className="h-7 gap-1 px-2.5 text-xs font-medium"
                              >
                                {testingTools[tool.id] ? (
                                  <RefreshCw className="h-3 w-3 animate-spin text-muted-foreground" />
                                ) : (
                                  <Play className="h-3 w-3 text-success" />
                                )}
                                {testingTools[tool.id] ? t('common.testing') : t('engineCatalog.actions.test')}
                              </Button>
                              <Button
                                variant="ghost"
                                size="sm"
                                onClick={() => {
                                  setSelectedLogsTool(tool);
                                  setIsLogsDrawerOpen(true);
                                }}
                                className="h-7 w-7 p-0"
                                title={t('common.recent', { defaultValue: 'Recent' })}
                              >
                                <Clock className="h-3.5 w-3.5 text-muted-foreground" />
                              </Button>
                              {tool.upstream_url && (
                                <Button
                                  variant="ghost"
                                  size="sm"
                                  onClick={() => window.open(tool.upstream_url, '_blank', 'noopener,noreferrer')}
                                  className="h-7 w-7 p-0"
                                  title={t('engineCatalog.actions.upstream')}
                                >
                                  <ExternalLink className="h-3.5 w-3.5 text-muted-foreground" />
                                </Button>
                              )}
                            </div>
                          }
                        />
                      );
                    })}
                  </div>
                )}
              </Card>
            );
          })}
        </div>
      )}

      {/* Configure Engine Dialog */}
      <Dialog open={isPathDialogOpen} onOpenChange={setIsPathDialogOpen}>
        {selectedTool && (
          <DialogContent className="max-w-2xl max-h-[90vh] overflow-y-auto flex flex-col">
            <DialogHeader>
              <DialogTitle className="flex items-center gap-2 font-bold text-foreground">
                <Settings2 className="h-5 w-5 text-primary" />
                {t('engineCatalog.configDialog.title', { name: selectedTool.name })}
              </DialogTitle>
              <DialogDescription>
                {t('engineCatalog.configDialog.manualConfigExample')}
              </DialogDescription>
            </DialogHeader>

            <div className="flex-1 space-y-4 my-2">
              {/* Form Input for Path */}
              <div className="space-y-2">
                <label className="text-sm font-semibold text-foreground block">
                  {t('engineCatalog.configDialog.currentPath')}
                </label>
                <Input
                  value={dialogPath}
                  onChange={(e) => setDialogPath(e.target.value)}
                  placeholder={getExamplePath(selectedTool)}
                  className="font-mono text-sm w-full"
                />
              </div>

              {/* Form Switch for Enabled */}
              <div className="flex items-center justify-between border-t border-border pt-4">
                <div className="space-y-0.5">
                  <label className="text-sm font-semibold text-foreground block">
                    {t('common.enabled', { defaultValue: '启用状态' })}
                  </label>
                  <span className="text-xs text-muted-foreground">
                    {t('engineCatalog.configDialog.enabledHint', {
                      defaultValue: '是否允许 Agent 在审计任务中自动调用此引擎',
                    })}
                  </span>
                </div>
                <Switch checked={dialogEnabled} onCheckedChange={setDialogEnabled} />
              </div>

              {/* 调用参数（catalog invocation 声明；mission config 显式值永远优先） */}
              {(selectedTool.invocation?.params?.length ?? 0) > 0 && (
                <div className="space-y-3 border-t border-border pt-4">
                  <div>
                    <label className="text-sm font-semibold text-foreground block">
                      {t('engineCatalog.configDialog.invocationParams')}
                    </label>
                    <span className="text-xs text-muted-foreground">
                      {t('engineCatalog.configDialog.invocationParamsHint')}
                    </span>
                  </div>
                  {(selectedTool.invocation?.params ?? []).map((param) => {
                    const isConfigured = Object.prototype.hasOwnProperty.call(
                      selectedTool.configured_settings?.params ?? {},
                      param.key,
                    );
                    return (
                      <div key={param.key} className="space-y-1">
                        <div className="flex items-center justify-between gap-2">
                          <label className="font-mono text-xs text-foreground">
                            {param.key}
                            {param.flag ? (
                              <span className="ml-1 font-sans text-muted-foreground">({param.flag})</span>
                            ) : null}
                          </label>
                          <div className="flex items-center gap-1">
                            {param.required ? (
                              <Badge tone="danger" size="sm">
                                {t('engineCatalog.configDialog.paramRequired')}
                              </Badge>
                            ) : null}
                            {isConfigured ? (
                              <Badge tone="success" size="sm">
                                {t('engineCatalog.configDialog.configured')}
                              </Badge>
                            ) : null}
                          </div>
                        </div>
                        {param.description ? (
                          <p className="text-[11px] text-muted-foreground">{param.description}</p>
                        ) : null}
                        {param.kind === 'boolean' ? (
                          <div className="flex justify-end py-1">
                            <Switch
                              checked={dialogBooleans[param.key] ?? false}
                              onCheckedChange={(checked) =>
                                setDialogBooleans((prev) => ({ ...prev, [param.key]: checked }))
                              }
                            />
                          </div>
                        ) : (
                          <Input
                            value={dialogParams[param.key] ?? ''}
                            onChange={(e) =>
                              setDialogParams((prev) => ({ ...prev, [param.key]: e.target.value }))
                            }
                            placeholder={
                              param.kind === 'integer'
                                ? t('engineCatalog.configDialog.integerPlaceholder', {
                                    min: param.minimum ?? '-∞',
                                    max: param.maximum ?? '+∞',
                                  })
                                : param.kind === 'string_list'
                                  ? t('engineCatalog.configDialog.listPlaceholder')
                                  : undefined
                            }
                            inputMode={param.kind === 'integer' ? 'numeric' : undefined}
                            className="font-mono text-sm"
                          />
                        )}
                      </div>
                    );
                  })}
                </div>
              )}

              {/* API 密钥（env）：值写入本地文件并注入子进程环境，保存后不回显 */}
              {(selectedTool.invocation?.env_keys?.length ?? 0) > 0 && (
                <div className="space-y-3 border-t border-border pt-4">
                  <div>
                    <label className="text-sm font-semibold text-foreground block">
                      {t('engineCatalog.configDialog.apiKeys')}
                    </label>
                    <span className="text-xs text-muted-foreground">
                      {t('engineCatalog.configDialog.apiKeysHint')}
                    </span>
                  </div>
                  {(selectedTool.invocation?.env_keys ?? []).map((keySpec) => {
                    const isConfigured =
                      (selectedTool.configured_settings?.env_set ?? []).includes(keySpec.name) &&
                      !envCleared[keySpec.name];
                    const willClear = envCleared[keySpec.name] === true;
                    return (
                      <div key={keySpec.name} className="space-y-1">
                        <div className="flex items-center justify-between gap-2">
                          <label className="font-mono text-xs text-foreground">{keySpec.name}</label>
                          <div className="flex items-center gap-1">
                            {isConfigured ? (
                              <Badge tone="success" size="sm">
                                {t('engineCatalog.configDialog.configured')}
                              </Badge>
                            ) : null}
                            {isConfigured ? (
                              <Button
                                variant="ghost"
                                size="sm"
                                className="h-5 px-2 text-[11px]"
                                onClick={() => {
                                  setDialogEnv((prev) => ({ ...prev, [keySpec.name]: '' }));
                                  setEnvCleared((prev) => ({ ...prev, [keySpec.name]: true }));
                                }}
                              >
                                {t('engineCatalog.configDialog.clearValue')}
                              </Button>
                            ) : null}
                            {willClear ? (
                              <Badge tone="warning" size="sm">
                                {t('engineCatalog.configDialog.willClear')}
                              </Badge>
                            ) : null}
                          </div>
                        </div>
                        {keySpec.description ? (
                          <p className="text-[11px] text-muted-foreground">{keySpec.description}</p>
                        ) : null}
                        <Input
                          type="password"
                          value={dialogEnv[keySpec.name] ?? ''}
                          onChange={(e) => {
                            const next = e.target.value;
                            setDialogEnv((prev) => ({ ...prev, [keySpec.name]: next }));
                            if (next) {
                              setEnvCleared((prev) => {
                                const rest = { ...prev };
                                delete rest[keySpec.name];
                                return rest;
                              });
                            }
                          }}
                          placeholder={
                            isConfigured
                              ? t('engineCatalog.configDialog.envConfiguredPlaceholder')
                              : t('engineCatalog.configDialog.envPlaceholder')
                          }
                          autoComplete="off"
                          className="font-mono text-sm"
                        />
                      </div>
                    );
                  })}
                </div>
              )}

              {/* Tool properties */}
              <div className="grid grid-cols-2 gap-4 surface-inset p-4 text-sm">
                <div>
                  <span className="label-spec">{t('engineCatalog.configDialog.executableNames')}</span>
                  <div className="mt-1 font-mono text-foreground">
                    {selectedTool.executable_names?.join(', ') || '—'}
                  </div>
                </div>
                <div>
                  <span className="label-spec">{t('engineCatalog.configDialog.detectionSource', { defaultValue: '可执行文件检测源' })}</span>
                  <div className="mt-1 break-all font-mono text-foreground">
                    {selectedTool.detection?.source || t('engineCatalog.notDetected', { defaultValue: '未检测到' })}
                  </div>
                </div>
              </div>

              {/* Dynamic example path */}
              <div className="space-y-1.5">
                <span className="label-spec">{t('engineCatalog.configDialog.examplePath')}</span>
                <div className="surface-inset break-all rounded border p-2.5 font-mono text-xs text-foreground/80">
                  <div>Windows: {getExamplePath(selectedTool)}</div>
                  <div className="mt-1">Linux/macOS: /usr/local/bin/{selectedTool.executable_names?.[0] || selectedTool.id}</div>
                </div>
              </div>

              {/* JSON Template */}
              <div className="space-y-1.5">
                <span className="label-spec">
                  data/config/local-tools.json — {t('engineCatalog.configDialog.jsonExample', { defaultValue: 'JSON 示例' })}
                </span>
                <pre className="overflow-x-auto rounded-lg bg-foreground p-4 font-mono text-xs text-background">
                  {JSON.stringify(
                    {
                      local_tools: [
                        {
                          tool_name: selectedTool.id,
                          executable_path: dialogPath || getExamplePath(selectedTool),
                          enabled: dialogEnabled,
                        },
                      ],
                    },
                    null,
                    2,
                  )}
                </pre>
              </div>
            </div>

            <DialogFooter className="mt-4 flex justify-between items-center border-t border-border pt-4">
              <Button variant="outline" onClick={() => setIsPathDialogOpen(false)}>
                {t('common.cancel')}
              </Button>
              <Button onClick={handleSavePath} disabled={isSavingPath} className="px-5">
                {isSavingPath ? t('common.saving') : t('common.save')}
              </Button>
            </DialogFooter>
          </DialogContent>
        )}
      </Dialog>

      {/* Recent Invocation Logs Drawer */}
      <RecentInvocationLogsDrawer
        open={isLogsDrawerOpen}
        onOpenChange={setIsLogsDrawerOpen}
        tool={selectedLogsTool}
        invocations={toolInvocations}
      />
    </div>
  );
}

function invocationTone(status: string): 'success' | 'danger' | 'warning' | 'neutral' {
  if (status === 'error' || status === 'timeout') return 'danger';
  if (status === 'denied') return 'warning';
  if (status === 'success' || status === 'completed') return 'success';
  return 'neutral';
}

function RecentInvocationLogsDrawer({
  open,
  onOpenChange,
  tool,
  invocations,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  tool: CatalogTool | null;
  invocations: ApiToolInvocation[];
}) {
  const { t } = useTranslation();
  const logs = tool
    ? invocations
        .filter(
          (inv) =>
            inv.tool_name.toLowerCase() === tool.id.toLowerCase() ||
            inv.module_id?.toLowerCase() === tool.id.toLowerCase(),
        )
        .sort((a, b) => new Date(b.started_at).getTime() - new Date(a.started_at).getTime())
        .slice(0, 10)
    : [];

  return (
    <Drawer
      open={open}
      onOpenChange={onOpenChange}
      icon={<Clock className="h-5 w-5" />}
      title={tool ? `${tool.name} — ${t('engineCatalog.recentInvocations', { defaultValue: '最近调用日志' })}` : t('engineCatalog.recentInvocations', { defaultValue: '最近调用日志' })}
      description={t('engineCatalog.recentInvocationsHint', {
        defaultValue: '该引擎最近在当前工作区被 Agent 调用的 10 次记录。',
      })}
      width="lg"
    >
      {logs.length === 0 ? (
        <div className="py-12 text-center text-sm text-muted-foreground">{t('common.noData')}</div>
      ) : (
        <div className="section-stack">
          {logs.map((log) => {
            const tone = invocationTone(log.status);
            return (
              <div key={log.id} className="surface-card p-4 section-stack">
                <div className="flex items-center justify-between gap-2">
                  <div className="flex items-center gap-2">
                    <span className="rounded border border-border bg-muted/50 px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground">
                      ID: {log.id}
                    </span>
                    <Badge tone={tone}>{log.status}</Badge>
                  </div>
                  <span className="text-[11px] text-muted-foreground">
                    {new Date(log.started_at).toLocaleString()}
                  </span>
                </div>

                <div className="grid grid-cols-3 gap-2 border-y border-border py-2 text-xs">
                  <div>
                    <span className="text-muted-foreground">{t('engineCatalog.duration', { defaultValue: '耗时' })}:</span>{' '}
                    <span className="font-mono">{log.duration_ms ? `${log.duration_ms}ms` : '—'}</span>
                  </div>
                  <div>
                    <span className="text-muted-foreground">{t('engineCatalog.exitCode', { defaultValue: '退出码' })}:</span>{' '}
                    <span className="font-mono">{log.exit_code !== undefined ? log.exit_code : '—'}</span>
                  </div>
                  <div>
                    <span className="text-muted-foreground">{t('engineCatalog.taskId', { defaultValue: '任务 ID' })}:</span>{' '}
                    <span className="inline-block max-w-[120px] truncate font-mono" title={log.task_id || ''}>
                      {log.task_id || '—'}
                    </span>
                  </div>
                </div>

                {log.input_summary && (
                  <div className="space-y-1">
                    <span className="text-xs font-medium text-foreground">{t('engineCatalog.inputParams', { defaultValue: '输入参数' })}</span>
                    <pre className="max-h-[120px] overflow-x-auto whitespace-pre-wrap rounded bg-foreground p-3 font-mono text-xs text-background select-all">
                      {log.input_summary}
                    </pre>
                  </div>
                )}

                {(log.output_summary || log.error) && (
                  <div className="space-y-1">
                    <span className="text-xs font-medium text-foreground">
                      {log.error ? t('engineCatalog.stderr', { defaultValue: '异常/错误输出 (stderr)' }) : t('engineCatalog.stdout', { defaultValue: '响应/结果 (stdout)' })}
                    </span>
                    <pre
                      className={`max-h-[150px] overflow-x-auto whitespace-pre-wrap rounded p-3 font-mono text-xs select-all ${
                        log.error
                          ? 'border border-danger-border bg-danger-soft text-danger-foreground'
                          : 'bg-foreground text-background'
                      }`}
                    >
                      {log.error || log.output_summary}
                    </pre>
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </Drawer>
  );
}
