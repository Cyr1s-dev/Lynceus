import { useState, useCallback, useEffect, type ReactNode } from 'react';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { Link, useNavigate } from '@tanstack/react-router';
import {
  Crosshair,
  Plus,
  ChevronRight,
  ShieldAlert,
  Wrench,
  Network,
  Coins,
  Archive,
  RotateCw,
  Activity,
  CircleDot,
  Loader2,
} from 'lucide-react';
import { formatDistanceToNow } from 'date-fns';
import { api, getApiErrorMessage } from '@/lib/api';
import { useToast } from '@/hooks/use-toast';
import { useProviderHealthById } from '@/hooks/use-provider-health';
import {
  getProviderHealthQueryKey,
  getProviderReadiness,
  isProviderConfigComplete,
} from '@/lib/provider-types';
import type { ApprovalMode, DecisionGate, Mission, MissionStatus, UploadArtifactResponse } from '@/lib/types';
import type { CreateProviderRequest } from '@/lib/provider-types';
import { formatStatus } from '@/lib/i18n-formatters';
import { cn, missionDisplayTitle } from '@/lib/utils';
import {
  PageHeader,
  PageContainer,
  EmptyState,
  LoadingState,
  ErrorState,
  Button,
  Badge,
  MissionStatusBadge,
} from '@/ui/untitled';
import { ProviderEditDialog } from '@/components/provider/ProviderEditDialog';
import { MissionComposer, type PendingFile } from '@/components/mission/MissionComposer';
const ACTIVE_STATUS_SET = new Set([
  'running',
  'paused',
  'waiting_for_decision',
  'reviewing',
  'reporting',
]);

const MISSION_DRAFT_STORAGE_KEY = 'lynceus.missionComposer.draft.v1';
const MISSION_APPROVAL_STORAGE_KEY = 'lynceus.missionComposer.approvalMode.v1';

function readMissionDraft(): string {
  if (typeof window === 'undefined') return '';
  return window.sessionStorage.getItem(MISSION_DRAFT_STORAGE_KEY) ?? '';
}

function readMissionApprovalMode(): ApprovalMode {
  if (typeof window === 'undefined') return 'ask_for_approval';
  const stored = window.sessionStorage.getItem(MISSION_APPROVAL_STORAGE_KEY);
  if (stored === 'ask_for_approval' || stored === 'approve_for_me' || stored === 'full_access') {
    return stored;
  }
  return 'ask_for_approval';
}

function clearMissionDraft(): void {
  if (typeof window === 'undefined') return;
  window.sessionStorage.removeItem(MISSION_DRAFT_STORAGE_KEY);
}

/** 紧凑数字格式（与网关页同款 Intl 记法）。 */
function formatCompact(value: number): string {
  return new Intl.NumberFormat(undefined, {
    notation: 'compact',
    maximumFractionDigits: 1,
  }).format(value);
}

export function DashboardPage() {
  const { t } = useTranslation();
  const { toast } = useToast();
  const navigate = useNavigate();
  const queryClient = useQueryClient();

  /* ── Mission Intake state ── */
  const [intakeGoal, setIntakeGoal] = useState(readMissionDraft);
  const [showProviderSetup, setShowProviderSetup] = useState(false);
  const createProviderMutation = useMutation({
    mutationFn: async (data: CreateProviderRequest) => {
      const created = await api.createProvider(data);
      try {
        const health = await api.testProvider(created.id);
        queryClient.setQueryData(getProviderHealthQueryKey(created.id), health);
        toast({ title: t('intake.providerConfigured'), description: data.name });
      } catch (err) {
        queryClient.setQueryData(getProviderHealthQueryKey(created.id), {
          provider_id: created.id,
          status: 'error',
          message: err instanceof Error ? err.message : t('intake.providerTestFailed'),
          capabilities: {
            text_generation: false,
            structured_output: false,
          },
        });
        toast({ title: t('intake.providerConfigured'), description: t('intake.providerTestFailed'), variant: 'destructive' });
      }
      return created.id;
    },
    onSuccess: () => {
      setShowProviderSetup(false);
      queryClient.invalidateQueries({ queryKey: ['providers'] });
      queryClient.invalidateQueries({ queryKey: ['providers', 'health'] });
    },
    onError: (err: Error) => {
      toast({ title: t('common.error'), description: err.message, variant: 'destructive' });
    },
  });
  const [pendingSubmitAfterProviderSetup, setPendingSubmitAfterProviderSetup] = useState(false);
  const [pendingFiles, setPendingFiles] = useState<PendingFile[]>([]);
  const [approvalMode, setApprovalMode] = useState<ApprovalMode>(readMissionApprovalMode);
  const [isPreparingMission, setIsPreparingMission] = useState(false);
  const [isCheckingProvider, setIsCheckingProvider] = useState(false);
  /**
   * 刚提交、后台摄入分析还没下结论的 mission id。
   *
   * `null` = 没有待判定的任务；非空 = 主 Agent 正等它出结论，此时不跳转，
   * 由 `clarifyingMission` 查询 + 下面的 effect 决定去向。
   */
  const [clarifyingMissionId, setClarifyingMissionId] = useState<string | null>(null);

  useEffect(() => {
    if (typeof window === 'undefined') return;
    if (intakeGoal) {
      window.sessionStorage.setItem(MISSION_DRAFT_STORAGE_KEY, intakeGoal);
    } else {
      clearMissionDraft();
    }
  }, [intakeGoal]);

  useEffect(() => {
    if (typeof window === 'undefined') return;
    window.sessionStorage.setItem(MISSION_APPROVAL_STORAGE_KEY, approvalMode);
  }, [approvalMode]);

  /* ── Data queries ── */
  const {
    data: missions = [],
    isLoading: isLoadingMissions,
    isError: isErrorMissions,
    error: errorMissions,
    refetch: refetchMissions,
  } = useQuery({
    queryKey: ['missions'],
    queryFn: () => api.getMissions(),
  });

  const { data: providers = [] } = useQuery({
    queryKey: ['providers', 'mission-control'],
    queryFn: () => api.getProviders(),
    staleTime: 60_000,
  });

  const { data: toolCatalog = [] } = useQuery({
    queryKey: ['tool-catalog', 'mission-control'],
    queryFn: () => api.getToolCatalog(),
    staleTime: 60_000,
  });

  const { data: decisionGates = [] } = useQuery({
    queryKey: ['decision-gates', 'pending'],
    queryFn: () => api.getDecisionGates({ status: 'pending' }),
    refetchInterval: 10000,
  });

  // 五卡聚合（确认发现 / 资产节点 / 工具调用 / Token 用量）。
  // 活跃任务沿用 missions 客户端过滤，保证与下方活跃列表同一口径。
  const { data: dashboardStats } = useQuery({
    queryKey: ['dashboard-stats'],
    queryFn: api.getDashboardStats,
    refetchInterval: 15000,
  });

  /* ── Provider readiness ── */
  const providerHealthById = useProviderHealthById(providers, { autoTest: true });
  const configuredProviders = providers.filter(isProviderConfigComplete);
  const verifiedProviders = providers.filter(
    (p) =>
      getProviderReadiness(p, providerHealthById[p.id]).usableForTextGeneration,
  );
  const hasProviderReady = verifiedProviders.length > 0;
  const availableEngines = toolCatalog.filter((e) => e.detection.available).length;
  const totalEngines = toolCatalog.length;

  /* ── Mission creation：提交即建任务并进入任务页，模型分析与派发在后台 ── */
  const createMissionMutation = useMutation({
    mutationFn: async (input: {
      goal: string;
      artifactRecordIds?: string[];
      approvalMode: ApprovalMode;
    }) => {
      // 异步 intake：后端同步建草稿 Mission（毫秒级），analyze→start 在
      // 后台跑（分类/派发完成后任务页自动转为 running）。
      const result = await api.asyncIntake({
        prompt: input.goal,
        artifact_record_ids: input.artifactRecordIds ?? [],
      });
      return { kind: 'mission' as const, mission: result.mission };
    },
    onSuccess: (result) => {
      setIsPreparingMission(false);
      clearMissionDraft();
      setPendingFiles([]);
      queryClient.invalidateQueries({ queryKey: ['missions'] });
      queryClient.invalidateQueries({ queryKey: ['projects'] });
      // 不立刻跳任务页。后台 analyze→start 要几秒（模型分析 + 建 run），
      // 先拿着 mission id 轮询，等 `intake_plan` 落库（后台分析已跑完的
      // 可靠标记）再进任务详情页。
      setClarifyingMissionId(result.mission.id);
    },
    onError: (err) => {
      setIsPreparingMission(false);
      toast({
        title: t('missions.startFailed'),
        description: getApiErrorMessage(err),
        variant: 'destructive',
      });
    },
  });

  /**
   * 轮询刚提交的 mission，等后台摄入分析下结论。
   *
   * 只有仍处于 `draft` 时才继续轮询——一旦转起来（running）结论已出，
   * 停轮询交给下面的 effect 处理。
   */
  const clarifyingMissionQuery = useQuery({
    queryKey: ['mission', clarifyingMissionId],
    queryFn: () => api.getMission(clarifyingMissionId as string),
    enabled: clarifyingMissionId !== null,
    refetchInterval: (query) => {
      const status = query.state.data?.status;
      return status === 'draft' ? 2000 : false;
    },
  });
  const clarifyingMission = clarifyingMissionQuery.data;
  const isAnalyzingIntake =
    clarifyingMissionId !== null && clarifyingMission?.status === 'draft';

  // 摄入结论分流：分析跑完（`intake_plan` 落库）就进任务详情页。
  useEffect(() => {
    if (!clarifyingMissionId || !clarifyingMission) return;
    // 分析还没跑完，继续等轮询。
    if (!clarifyingMission.metadata?.intake_plan) return;
    navigate({ to: '/missions/$missionId', params: { missionId: clarifyingMissionId } });
    setClarifyingMissionId(null);
  }, [clarifyingMission, clarifyingMissionId, navigate]);

  /* ── Derived data ── */
  const activeMissions = missions.filter((m) => ACTIVE_STATUS_SET.has(m.status));
  const waitingMissions = activeMissions.filter((m) => m.status === 'waiting_for_decision');
  const runningMissions = activeMissions.filter((m) => m.status === 'running');
  const pausedMissions = activeMissions.filter((m) => m.status === 'paused');
  const recentMissions = missions
    .filter((m) => !ACTIVE_STATUS_SET.has(m.status))
    .slice(0, 5);
  const pendingGates = decisionGates.filter((g) => g.status === 'pending');
  const confirmedFindings = dashboardStats?.confirmed_findings;
  const assetNodes = dashboardStats?.asset_nodes;
  const toolCalls = dashboardStats?.tool_calls;
  const tokenUsage = dashboardStats?.token_usage;

  const systemReady = hasProviderReady && availableEngines > 0;
  const attentionCount = pendingGates.length + waitingMissions.length;

  /* ── Handlers ── */
  const handleCreateMission = useCallback(async () => {
    if (
      (!intakeGoal.trim() && pendingFiles.length === 0) ||
      isPreparingMission ||
      isCheckingProvider ||
      createMissionMutation.isPending
    ) return;
    if (configuredProviders.length === 0) {
      setPendingSubmitAfterProviderSetup(true);
      setShowProviderSetup(true);
      return;
    }

    if (!hasProviderReady) {
      setIsCheckingProvider(true);
      let ready = false;
      let failureMessage = t('missionControl.noProviderReady');
      try {
        for (const provider of configuredProviders) {
          try {
            const health = await queryClient.fetchQuery({
              queryKey: getProviderHealthQueryKey(provider.id),
              queryFn: () => api.testProvider(provider.id),
              staleTime: 0,
            });
            if (health.status === 'ok' && health.capabilities?.text_generation === true) {
              ready = true;
              break;
            }
            failureMessage = health.message || failureMessage;
          } catch (err) {
            failureMessage = getApiErrorMessage(err);
          }
        }
      } finally {
        setIsCheckingProvider(false);
      }
      if (!ready) {
        toast({
          title: t('missionControl.noProviderReady'),
          description: failureMessage,
          variant: 'destructive',
        });
        return;
      }
    }

    const goal = intakeGoal.trim() || t('uploads.analyzeWithFiles');
    setIsPreparingMission(true);

    // 先上传工件且不绑定 mission：是否创建 Mission 由 analyze 复杂度判定
    // 决定（S0 直答不建任何状态）；intake/start 会把 artifact_record_ids
    // 引用的工件移入 mission 工作区并绑定资产。
    const artifactIds: string[] = [];
    const filesToUpload = pendingFiles.filter((f) => f.status === 'pending' || f.status === 'failed');
    if (filesToUpload.length > 0) {
      for (const pf of filesToUpload) {
        setPendingFiles((prev) =>
          prev.map((p) => (p.file === pf.file ? { ...p, status: 'uploading' as const, error: undefined } : p)),
        );
        try {
          const res: UploadArtifactResponse = await api.uploadArtifact({
            file: pf.file,
            purpose: 'mission_intake',
          });
          artifactIds.push(res.artifact.id);
          setPendingFiles((prev) =>
            prev.map((p) =>
              p.file === pf.file
                ? { ...p, status: 'uploaded' as const, artifactId: res.artifact.id, detectedType: res.detected_input_type }
                : p,
            ),
          );
        } catch (err) {
          setPendingFiles((prev) =>
            prev.map((p) =>
              p.file === pf.file
                ? { ...p, status: 'failed' as const, error: getApiErrorMessage(err) }
                : p,
            ),
          );
          toast({
            title: t('uploads.failed'),
            description: `${pf.file.name}: ${getApiErrorMessage(err)}`,
            variant: 'destructive',
          });
          setIsPreparingMission(false);
          return;
        }
      }
    }

    for (const pf of pendingFiles) {
      if (pf.status === 'uploaded' && pf.artifactId) {
        artifactIds.push(pf.artifactId);
      }
    }

    createMissionMutation.mutate({
      goal,
      artifactRecordIds: artifactIds.length > 0 ? artifactIds : undefined,
      approvalMode,
    });
  }, [
    createMissionMutation,
    approvalMode,
    configuredProviders,
    hasProviderReady,
    intakeGoal,
    isCheckingProvider,
    isPreparingMission,
    pendingFiles,
    queryClient,
    t,
    toast,
  ]);

  const handleFilesSelected = useCallback((files: FileList | File[]) => {
    const arr = Array.from(files);
    if (arr.length === 0) return;
    const newFiles: PendingFile[] = arr.map((file) => ({
      file,
      status: 'pending' as const,
    }));
    setPendingFiles((prev) => [...prev, ...newFiles]);
  }, []);

  const handleRemoveFile = (index: number) => {
    setPendingFiles((prev) => prev.filter((_, i) => i !== index));
  };

  const handleRetryUpload = async (index: number) => {
    const pf = pendingFiles[index];
    if (!pf) return;
    setPendingFiles((prev) =>
      prev.map((p, i) =>
        i === index
          ? { file: p.file, status: 'pending' as const }
          : p,
      ),
    );
  };

  const openDecision = useCallback(
    (gate: DecisionGate) => {
      const targetMission = missions.find(
        (m) =>
          m.active_run_id === gate.audit_run_id ||
          m.project_id === gate.project_id,
      );
      if (targetMission) {
        navigate({
          to: '/missions/$missionId',
          params: { missionId: targetMission.id },
          search: { decisionGateId: gate.id },
        });
      } else {
        // 找不到对应任务时回任务列表（资产空间 UI 已删除）。
        navigate({ to: '/missions' });
      }
    },
    [missions, navigate],
  );

  useEffect(() => {
    if (!hasProviderReady || !pendingSubmitAfterProviderSetup || createMissionMutation.isPending) {
      return;
    }
    setPendingSubmitAfterProviderSetup(false);
    void handleCreateMission();
  }, [
    createMissionMutation.isPending,
    handleCreateMission,
    hasProviderReady,
    pendingSubmitAfterProviderSetup,
  ]);

  /* ── Loading / Error states ── */
  if (isLoadingMissions) {
    return (
      <PageContainer>
        <LoadingState card lines={4} />
      </PageContainer>
    );
  }

  if (isErrorMissions) {
    return (
      <PageContainer>
        <ErrorState
          title={t('missions.failedToLoad')}
          description={getApiErrorMessage(errorMissions)}
          onRetry={() => refetchMissions()}
        />
      </PageContainer>
    );
  }

  return (
    <PageContainer gap="lg">
      <PageHeader
        icon={<Crosshair className="h-5 w-5" />}
        title={t('nav.missionControl')}
        description={t('dashboard.missionControlDescription')}
        actions={
          <div className="flex items-center gap-2">
            <SystemPulse
              ready={systemReady}
              attention={attentionCount > 0}
              label={
                attentionCount > 0
                  ? t('dashboard.attentionNeeded', {
                      defaultValue: '需要关注',
                      count: attentionCount,
                    })
                  : systemReady
                    ? t('dashboard.systemReady', { defaultValue: '系统就绪' })
                    : t('dashboard.systemWarming', { defaultValue: '待配置' })
              }
            />
            {!hasProviderReady && (
              <Button
                variant="outline"
                size="sm"
                onClick={() => setShowProviderSetup(true)}
              >
                <Plus className="h-4 w-4" />
                {t('missionControl.setupProvider')}
              </Button>
            )}
            <Button variant="outline" size="sm" onClick={() => refetchMissions()}>
              <RotateCw className="h-4 w-4" />
              {t('common.refresh')}
            </Button>
          </div>
        }
      />

      {/* Metrics: separate cards, never fused into one shell.
          语义对齐审计仪表盘：活跃任务 / 确认发现 / 资产节点 / 工具调用 / Token 用量。
          Token 卡遵守成本红线：reported_runs === 0 时显示缺失（—），不伪造 0。 */}
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-5">
        <HeroMetric
          label={t('dashboard.cards.activeMissions')}
          value={activeMissions.length}
          hint={
            activeMissions.length > 0
              ? t('dashboard.cards.missionSplit', {
                  running: runningMissions.length,
                  paused: pausedMissions.length,
                  waiting: waitingMissions.length,
                })
              : t('dashboard.cards.noActiveMissions')
          }
          icon={<Activity className="h-3.5 w-3.5" />}
          tone={activeMissions.length > 0 ? 'info' : 'neutral'}
          emphasize
        />
        <HeroMetric
          label={t('dashboard.cards.confirmedFindings')}
          value={confirmedFindings?.total ?? '—'}
          hint={
            confirmedFindings && confirmedFindings.total > 0
              ? t('dashboard.cards.findingSplit', {
                  critical: confirmedFindings.critical,
                  high: confirmedFindings.high,
                  medium: confirmedFindings.medium,
                  low: confirmedFindings.low,
                })
              : t('dashboard.cards.noConfirmedFindings')
          }
          icon={<ShieldAlert className="h-3.5 w-3.5" />}
          tone={
            confirmedFindings && confirmedFindings.critical + confirmedFindings.high > 0
              ? 'warning'
              : 'neutral'
          }
          emphasize
        />
        <HeroMetric
          label={t('dashboard.cards.assetNodes')}
          value={assetNodes?.distinct_count ?? '—'}
          hint={
            assetNodes && assetNodes.by_type.length > 0
              ? assetNodes.by_type
                  .slice(0, 2)
                  .map(
                    (bucket) =>
                      `${t(`assets.assetTypes.${bucket.asset_type}`, { defaultValue: bucket.asset_type })} ${bucket.count}`,
                  )
                  .concat(
                    assetNodes.by_type.length > 2
                      ? [t('dashboard.cards.moreTypes', { count: assetNodes.by_type.length - 2 })]
                      : [],
                  )
                  .join(' · ')
              : t('dashboard.cards.noAssetNodes')
          }
          icon={<Network className="h-3.5 w-3.5" />}
          tone={assetNodes && assetNodes.distinct_count > 0 ? 'success' : 'neutral'}
        />
        <HeroMetric
          label={t('dashboard.cards.toolCalls')}
          value={toolCalls?.total_invocations ?? '—'}
          hint={
            toolCalls && toolCalls.active_worker_runs > 0
              ? t('dashboard.cards.workerInFlight', { count: toolCalls.active_worker_runs })
              : t('dashboard.cards.toolCallsIdle')
          }
          icon={<Wrench className="h-3.5 w-3.5" />}
          tone={toolCalls && toolCalls.active_worker_runs > 0 ? 'info' : 'neutral'}
        />
        <HeroMetric
          label={t('dashboard.cards.tokenUsage')}
          value={
            tokenUsage && tokenUsage.reported_runs > 0
              ? formatCompact(
                  tokenUsage.input_tokens +
                    tokenUsage.cached_input_tokens +
                    tokenUsage.output_tokens,
                )
              : '—'
          }
          hint={
            tokenUsage && tokenUsage.reported_runs > 0
              ? t('dashboard.cards.tokenSplit', {
                  input: formatCompact(tokenUsage.input_tokens),
                  cached: formatCompact(tokenUsage.cached_input_tokens),
                  output: formatCompact(tokenUsage.output_tokens),
                })
              : t('dashboard.cards.tokenNoData')
          }
          icon={<Coins className="h-3.5 w-3.5" />}
          tone={tokenUsage && tokenUsage.reported_runs > 0 ? 'success' : 'neutral'}
          className="col-span-2 sm:col-span-1"
        />
      </div>

      {/* Intake: bare composer only — no outer shell, no title chrome */}
      <MissionComposer
        value={intakeGoal}
        onChange={setIntakeGoal}
        pendingFiles={pendingFiles}
        onFilesSelected={handleFilesSelected}
        onRemoveFile={handleRemoveFile}
        onRetryFile={handleRetryUpload}
        onSubmit={handleCreateMission}
        isSubmitting={isPreparingMission || isCheckingProvider || createMissionMutation.isPending || isAnalyzingIntake}
        approvalMode={approvalMode}
        onApprovalModeChange={setApprovalMode}
      />

      {/* 摄入结论区：后台分析中 → 一句话说明 + 直达任务详情的链接。 */}
      {isAnalyzingIntake && (
        <div className="flex items-center gap-2 rounded-xl border border-border bg-card px-4 py-3 text-xs text-muted-foreground shadow-card">
          <Loader2 className="h-3.5 w-3.5 animate-spin" />
          <span>{t('intake.analyzing')}</span>
          {clarifyingMissionId && (
            <Link
              to="/missions/$missionId"
              params={{ missionId: clarifyingMissionId }}
              className="ml-auto shrink-0 text-primary hover:underline"
            >
              {t('missions.intakeClarificationOpenMission')}
            </Link>
          )}
        </div>
      )}

      {/* ── Main workspace ── */}
      <div className="relative grid grid-cols-1 gap-5 xl:grid-cols-12 xl:gap-6">
        {/* Left: active work stream */}
        <div className="min-w-0 xl:col-span-8">
          <section className="overflow-hidden rounded-2xl border border-border bg-card shadow-card">
            <div className="flex items-center justify-between gap-3 border-b border-border px-4 py-3.5 sm:px-5">
              <div className="min-w-0">
                <div className="flex items-center gap-2.5">
                  <h2 className="text-sm font-semibold tracking-tight text-foreground">
                    {t('missionControl.activeMissions')}
                  </h2>
                  {activeMissions.length > 0 && (
                    <Badge tone="info" variant="soft" size="sm" dot pulse>
                      {activeMissions.length}
                    </Badge>
                  )}
                </div>
                <p className="mt-0.5 text-xs text-muted-foreground">
                  {t('missionControl.activeMissionsHint')}
                </p>
              </div>
              <Link
                to="/missions"
                className="flex shrink-0 items-center gap-1 text-xs font-medium text-primary hover:underline"
              >
                {t('missionControl.viewAll')}
                <ChevronRight className="h-3 w-3" />
              </Link>
            </div>

            {activeMissions.length === 0 ? (
              <div className="px-5 py-10">
                <EmptyState
                  variant="bare"
                  compact
                  icon={<Crosshair className="h-5 w-5" />}
                  title={t('missionControl.noActiveMissions')}
                  description={t('missionControl.noActiveMissionsHint')}
                />
              </div>
            ) : (
              <div className="divide-y divide-border">
                {activeMissions.slice(0, 8).map((mission) => (
                  <ActiveMissionRow key={mission.id} mission={mission} />
                ))}
                {activeMissions.length > 8 && (
                  <Link
                    to="/missions"
                    className="flex items-center justify-center gap-1 px-4 py-3 text-xs font-medium text-muted-foreground transition-colors hover:bg-muted/40 hover:text-foreground"
                  >
                    {t('missionControl.moreActive', {
                      count: activeMissions.length - 8,
                    })}
                    <ChevronRight className="h-3 w-3" />
                  </Link>
                )}
              </div>
            )}
          </section>
        </div>

        {/* Right: live context rail */}
        <div className="flex min-w-0 flex-col gap-4 xl:col-span-4">
          {/* Decision queue */}
          <RailPanel
            icon={<ShieldAlert className="h-3.5 w-3.5" />}
            title={t('missionControl.decisionQueue')}
            description={t('missionControl.decisionQueueHint')}
            actions={
              pendingGates.length > 0 ? (
                <Badge tone="warning" variant="soft" size="sm" dot pulse>
                  {pendingGates.length}
                </Badge>
              ) : (
                <Badge tone="success" variant="soft" size="sm" dot>
                  {t('dashboard.clear', { defaultValue: '空闲' })}
                </Badge>
              )
            }
            bodyClassName={pendingGates.length === 0 ? 'px-4 py-3.5' : 'p-0'}
          >
            {pendingGates.length === 0 ? (
              <div className="flex items-start gap-2.5">
                <span className="mt-0.5 flex h-6 w-6 shrink-0 items-center justify-center rounded-md bg-success-soft text-success">
                  <CircleDot className="h-3.5 w-3.5" />
                </span>
                <div className="min-w-0">
                  <p className="text-sm font-medium text-foreground">
                    {t('decisionGate.noPendingDecisions')}
                  </p>
                  <p className="mt-0.5 text-xs leading-5 text-muted-foreground">
                    {t('missionControl.decisionQueueIdleHint', {
                      defaultValue: '新的阻塞或顾问决策会实时出现在这里。',
                    })}
                  </p>
                </div>
              </div>
            ) : (
              <div className="divide-y divide-border">
                {pendingGates.slice(0, 4).map((gate) => (
                  <DecisionQueueRow
                    key={gate.id}
                    gate={gate}
                    onView={() => openDecision(gate)}
                  />
                ))}
                {pendingGates.length > 4 && (
                  <Link
                    to="/missions"
                    className="flex items-center justify-center gap-1 px-4 py-2.5 text-xs font-medium text-muted-foreground transition-colors hover:bg-muted/40 hover:text-foreground"
                  >
                    {t('missionControl.moreDecisions', {
                      count: pendingGates.length - 4,
                    })}
                  </Link>
                )}
              </div>
            )}
          </RailPanel>

          {/* System status: engines + providers merged into one compact card
              （原「引擎 / 可用供应方」两张 Hero 卡合并至此，一行一个状态点） */}
          <RailPanel
            icon={<Activity className="h-3.5 w-3.5" />}
            title={t('dashboard.systemStatus.title')}
            description={t('dashboard.systemStatus.description')}
            bodyClassName="p-0"
          >
            <SystemStatusRow
              label={t('dashboard.systemStatus.engines')}
              value={
                totalEngines > 0
                  ? t('dashboard.systemStatus.enginesValue', {
                      available: availableEngines,
                      total: totalEngines,
                    })
                  : t('dashboard.systemStatus.enginesMissing')
              }
              ok={availableEngines > 0}
              to="/modules"
            />
            <SystemStatusRow
              label={t('dashboard.systemStatus.providers')}
              value={
                providers.length > 0
                  ? t('dashboard.systemStatus.providersValue', {
                      ready: verifiedProviders.length,
                      total: providers.length,
                    })
                  : t('dashboard.systemStatus.providersMissing')
              }
              ok={hasProviderReady}
              to="/settings"
            />
          </RailPanel>

          {/* Recent archive */}
          {recentMissions.length > 0 && (
            <RailPanel
              icon={<Archive className="h-3.5 w-3.5" />}
              title={t('missionControl.recentlyCompleted')}
              actions={
                <Link
                  to="/missions"
                  className="text-xs font-medium text-primary hover:underline"
                >
                  {t('missionControl.archive')}
                </Link>
              }
              bodyClassName="p-0"
            >
              <div className="divide-y divide-border">
                {recentMissions.slice(0, 4).map((mission) => (
                  <Link
                    key={mission.id}
                    to="/missions/$missionId"
                    params={{ missionId: mission.id }}
                    className="flex items-center justify-between gap-2 px-4 py-2.5 transition-colors hover:bg-muted/40"
                  >
                    <div className="min-w-0 flex-1">
                      <p className="truncate text-sm font-medium text-foreground">
                        {missionDisplayTitle(mission)}
                      </p>
                      <p className="text-[11px] text-muted-foreground">
                        {formatDistanceToNow(new Date(mission.updated_at), {
                          addSuffix: true,
                        })}
                      </p>
                    </div>
                    <MissionStatusBadge
                      status={mission.status}
                      label={formatStatus(t, mission.status)}
                      dot
                      size="sm"
                    />
                  </Link>
                ))}
              </div>
            </RailPanel>
          )}
        </div>
      </div>

      <ProviderEditDialog
        provider={{}}
        isOpen={showProviderSetup}
        onClose={() => setShowProviderSetup(false)}
        onSave={(data) => createProviderMutation.mutate(data as CreateProviderRequest)}
        isLoading={createProviderMutation.isPending}
        showPresets
      />
    </PageContainer>
  );
}

/* ───────────────────────── Sub-components ───────────────────────── */

function SystemPulse({
  ready,
  attention,
  label,
}: {
  ready: boolean;
  attention: boolean;
  label: ReactNode;
}) {
  return (
    <div className="hidden items-center gap-2 rounded-full border border-border bg-card px-2.5 py-1 text-xs font-medium text-muted-foreground shadow-xs sm:flex">
      <span className="relative flex h-2 w-2">
        {(attention || ready) && (
          <span
            className={cn(
              'absolute inline-flex h-full w-full animate-ping rounded-full opacity-40',
              attention ? 'bg-warning' : 'bg-success',
            )}
          />
        )}
        <span
          className={cn(
            'relative inline-flex h-2 w-2 rounded-full',
            attention ? 'bg-warning' : ready ? 'bg-success' : 'bg-muted-foreground/50',
          )}
        />
      </span>
      <span className="text-foreground">{label}</span>
    </div>
  );
}

function HeroMetric({
  label,
  value,
  hint,
  icon,
  tone = 'neutral',
  emphasize = false,
  className,
}: {
  label: ReactNode;
  value: ReactNode;
  hint?: ReactNode;
  icon?: ReactNode;
  tone?: 'neutral' | 'info' | 'warning' | 'success';
  emphasize?: boolean;
  className?: string;
}) {
  const valueTone =
    tone === 'info'
      ? 'text-info'
      : tone === 'warning'
        ? 'text-warning'
        : tone === 'success'
          ? 'text-success'
          : 'text-foreground';

  const iconTone =
    tone === 'info'
      ? 'text-info'
      : tone === 'warning'
        ? 'text-warning'
        : tone === 'success'
          ? 'text-success'
          : 'text-muted-foreground';

  return (
    <div
      className={cn(
        'lift flex min-h-[88px] flex-col justify-between gap-2 rounded-xl border border-border bg-card px-4 py-3.5 shadow-xs',
        className,
      )}
    >
      <div className="flex items-center justify-between gap-2">
        <p className="text-[11px] font-medium leading-4 text-muted-foreground">{label}</p>
        <span className={cn('opacity-70', iconTone)}>{icon}</span>
      </div>
      <div>
        <p
          className={cn(
            'font-semibold tabular-nums tracking-tight',
            emphasize ? 'text-[1.75rem] leading-none' : 'text-xl leading-none',
            valueTone,
          )}
        >
          {value}
        </p>
        {hint && (
          <p className="mt-1.5 line-clamp-1 text-[11px] leading-4 text-muted-foreground">
            {hint}
          </p>
        )}
      </div>
    </div>
  );
}

function RailPanel({
  icon,
  title,
  description,
  actions,
  children,
  bodyClassName,
}: {
  icon?: ReactNode;
  title: ReactNode;
  description?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  bodyClassName?: string;
}) {
  return (
    <section className="overflow-hidden rounded-2xl border border-border bg-card shadow-card">
      <div className="flex items-start justify-between gap-3 border-b border-border px-4 py-3">
        <div className="flex min-w-0 items-start gap-2.5">
          {icon && (
            <div className="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-lg bg-muted text-primary">
              {icon}
            </div>
          )}
          <div className="min-w-0">
            <h3 className="text-sm font-semibold leading-5 tracking-tight text-foreground">
              {title}
            </h3>
            {description && (
              <p className="mt-0.5 text-[11px] leading-4 text-muted-foreground">
                {description}
              </p>
            )}
          </div>
        </div>
        {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
      </div>
      <div className={cn(bodyClassName)}>{children}</div>
    </section>
  );
}

/** 系统状态卡的单行：状态点 + 名称 + 数值，整行可点进对应页面。 */
function SystemStatusRow({
  label,
  value,
  ok,
  to,
}: {
  label: string;
  value: string;
  ok: boolean;
  to: string;
}) {
  return (
    <Link
      to={to}
      className="flex items-center justify-between gap-2 px-4 py-2.5 transition-colors hover:bg-muted/40"
    >
      <span className="flex min-w-0 items-center gap-2">
        <span
          className={cn(
            'h-2 w-2 shrink-0 rounded-full',
            ok ? 'bg-success' : 'bg-warning',
          )}
          aria-hidden
        />
        <span className="truncate text-sm font-medium text-foreground">{label}</span>
      </span>
      <span className="shrink-0 text-xs tabular-nums text-muted-foreground">{value}</span>
    </Link>
  );
}

function DecisionQueueRow({
  gate,
  onView,
}: {
  gate: DecisionGate;
  onView: () => void;
}) {
  const { t } = useTranslation();
  const isBlocking = gate.kind === 'blocking';

  return (
    <button
      type="button"
      onClick={onView}
      className="group flex w-full items-start gap-3 px-4 py-3 text-left transition-colors hover:bg-muted/40"
    >
      <span
        className={cn(
          'mt-1 h-8 w-0.5 shrink-0 rounded-full',
          isBlocking ? 'bg-warning' : 'bg-info',
        )}
        aria-hidden
      />
      <div className="min-w-0 flex-1">
        <div className="flex items-start justify-between gap-2">
          <p className="line-clamp-2 text-sm font-medium leading-5 text-foreground group-hover:text-primary">
            {gate.question}
          </p>
          <Badge
            tone={isBlocking ? 'warning' : 'info'}
            variant="soft"
            size="sm"
            className="shrink-0"
          >
            {isBlocking
              ? t('decisionGate.kind.blocking', { defaultValue: '阻塞' })
              : t('decisionGate.kind.advisory', { defaultValue: '顾问' })}
          </Badge>
        </div>
        <div className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[11px] text-muted-foreground">
          <span className="capitalize">{gate.severity}</span>
          <span className="text-border">·</span>
          <span>
            {formatDistanceToNow(new Date(gate.created_at), { addSuffix: true })}
          </span>
        </div>
      </div>
    </button>
  );
}

function ActiveMissionRow({ mission }: { mission: Mission }) {
  const { t } = useTranslation();
  const target = formatTargetSummary(mission.target);
  const stage = getMissionStage(mission.status);
  const accent = statusAccent(mission.status);

  return (
    <Link
      to="/missions/$missionId"
      params={{ missionId: mission.id }}
      className="group relative flex items-start gap-3.5 px-4 py-3.5 transition-colors hover:bg-muted/35 sm:px-5"
    >
      {/* Left status accent */}
      <span
        className={cn('absolute inset-y-3 left-0 w-0.5 rounded-full', accent.bar)}
        aria-hidden
      />

      <div
        className={cn(
          'mt-0.5 flex h-9 w-9 shrink-0 items-center justify-center rounded-xl border text-sm',
          accent.chip,
        )}
      >
        <Crosshair className="h-3.5 w-3.5" />
      </div>

      <div className="min-w-0 flex-1">
        <div className="flex items-start justify-between gap-3">
          <div className="min-w-0">
            <p className="truncate text-sm font-medium leading-5 text-foreground group-hover:text-primary">
              {missionDisplayTitle(mission)}
            </p>
            <div className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[11px] text-muted-foreground">
              <span>
                {t('missions.updatedRelative', {
                  time: formatDistanceToNow(new Date(mission.updated_at), {
                    addSuffix: true,
                  }),
                })}
              </span>
              {target && (
                <>
                  <span className="text-border">·</span>
                  <span className="max-w-[240px] truncate">{target}</span>
                </>
              )}
            </div>
          </div>
          <MissionStatusBadge
            status={mission.status}
            label={formatStatus(t, mission.status)}
            dot
            size="sm"
          />
        </div>

        {/* Stage progress — scannable density without fake data */}
        <div className="mt-2.5 flex items-center gap-1.5">
          {(['intake', 'execute', 'decide', 'report'] as const).map((key, idx) => {
            const filled = idx <= stage.index;
            const current = idx === stage.index;
            return (
              <div key={key} className="flex min-w-0 flex-1 items-center gap-1.5">
                <div
                  className={cn(
                    'h-1 flex-1 rounded-full transition-colors',
                    filled
                      ? current
                        ? accent.fill
                        : 'bg-foreground/15'
                      : 'bg-muted',
                  )}
                />
              </div>
            );
          })}
          <span className="ml-1 shrink-0 text-[10px] font-medium text-muted-foreground">
            {stage.label}
          </span>
        </div>
      </div>
    </Link>
  );
}

function statusAccent(status: MissionStatus | string) {
  switch (status) {
    case 'running':
      return {
        bar: 'bg-info',
        chip: 'border-info-border bg-info-soft text-info',
        fill: 'bg-info',
      };
    case 'waiting_for_decision':
      return {
        bar: 'bg-warning',
        chip: 'border-warning-border bg-warning-soft text-warning',
        fill: 'bg-warning',
      };
    case 'paused':
      return {
        bar: 'bg-neutral',
        chip: 'border-border bg-muted text-muted-foreground',
        fill: 'bg-neutral',
      };
    case 'failed':
      return {
        bar: 'bg-danger',
        chip: 'border-danger-border bg-danger-soft text-danger',
        fill: 'bg-danger',
      };
    default:
      return {
        bar: 'bg-primary/50',
        chip: 'border-border bg-muted/60 text-primary',
        fill: 'bg-primary/70',
      };
  }
}

function getMissionStage(status: MissionStatus | string): { index: number; label: string } {
  switch (status) {
    case 'draft':
      return { index: 0, label: 'Intake' };
    case 'running':
    case 'reviewing':
      return { index: 1, label: 'Execute' };
    case 'paused':
      return { index: 1, label: 'Paused' };
    case 'waiting_for_decision':
      return { index: 2, label: 'Decide' };
    case 'reporting':
      return { index: 3, label: 'Report' };
    case 'completed':
      return { index: 3, label: 'Done' };
    case 'failed':
    case 'cancelled':
      return { index: 1, label: 'Stopped' };
    default:
      return { index: 1, label: 'Active' };
  }
}

function formatTargetSummary(target: Record<string, string>): string {
  const entries = Object.entries(target).filter(([key]) => key !== 'raw_prompt');
  if (entries.length === 0) return '';
  return entries.map(([key, value]) => `${key}: ${value}`).join(', ');
}
