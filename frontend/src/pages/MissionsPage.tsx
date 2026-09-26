import { useState, useMemo, useEffect, useCallback, type ReactNode } from 'react';
import { Link, useNavigate, useParams, useRouterState } from '@tanstack/react-router';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { formatDistanceToNow } from 'date-fns';
import {
  Activity,
  AlertCircle,
  Bot,
  Cable,
  Clock,
  Database,
  GitBranch,
  Loader2,
  MessageSquarePlus,
  PauseCircle,
  Play,
  Plus,
  RotateCcw,
  Route,
  Target,
  ShieldAlert,
  TerminalSquare,
  Trash2,
  SearchX,
} from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { api, getApiErrorMessage } from '@/lib/api';
import type {
  ApiAuditRun,
  ApiModule,
  Mission,
  MissionCanvas,
  UserDirective,
  DecisionGate,
  DecisionAnswer,
  ApprovalMode,
  GoalOutcomeType,
} from '@/lib/types';
import { formatModuleDomain, formatStatus } from '@/lib/i18n-formatters';
import { getProviderReadiness } from '@/lib/provider-types';
import { useProviderHealthById } from '@/hooks/use-provider-health';
import { useToast } from '@/hooks/use-toast';
import { cn, missionDisplayTitle } from '@/lib/utils';
import { AIIntakeForm } from '@/components/project/AIIntakeForm';
import { ExplorationGraphCanvas } from '@/components/exploration/ExplorationGraph';
import { MissionAssetBoard } from '@/components/mission/MissionAssetBoard';
import { MissionFindingsBoard } from '@/components/mission/MissionFindingsBoard';
import { MissionToolCallsBoard } from '@/components/mission/MissionToolCallsBoard';
import { MissionBroadcastLog } from '@/components/mission/MissionBroadcastLog';
import { MissionCoverageMap } from '@/components/mission/MissionCoverageMap';
import { MissionReportPanel } from '@/components/mission/MissionReportPanel';
import { MissionRetestPanel } from '@/components/mission/MissionRetestPanel';
import { MissionSessionsPanel } from '@/components/mission/MissionSessionsPanel';
import { DecisionGatePanel } from '@/components/decision/DecisionGatePanel';
import { ApprovalModeSelector } from '@/components/mission/ApprovalModeSelector';
import {
  Button,
  Input,
  Textarea,
  Badge,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Drawer,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Tabs,
  TabsList,
  TabsTrigger,
  TabsContent,
  PageHeader,
  PageContainer,
  Section,
  EmptyState,
  ErrorState,
  LoadingState,
  EntityCard,
  DataToolbar,
  MissionStatusBadge,
  type StatusTone,
  missionStatusTone,
  usePageWidthMode,
} from '@/ui/untitled';
import { Label } from '@/ui/untitled';

function emptyMissionForm() {
  return {
    user_goal: '',
    target_key: 'target',
    target_value: '',
    constraints: '',
    success_criteria: '',
    goal_outcome: 'custom' as GoalOutcomeType,
    minimum_count: 1,
    approval_mode: 'ask_for_approval' as ApprovalMode,
  };
}

export function MissionsPage() {
  const { missionId } = useParams({ strict: false }) as { missionId?: string };
  return missionId ? <MissionDetail missionId={missionId} /> : <MissionList />;
}

function MissionList() {
  const { t } = useTranslation();
  const { toast } = useToast();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [form, setForm] = useState(emptyMissionForm());
  const [selectedMissionIds, setSelectedMissionIds] = useState<string[]>([]);
  const [showArchived, setShowArchived] = useState(false);
  const [tagDraft, setTagDraft] = useState('');
  const [categoryDraft, setCategoryDraft] = useState('');
  // 统一创建向导：AI 分析（analyze → plan → start）与手动创建双模式。
  const [isCreateOpen, setIsCreateOpen] = useState(false);
  const [createMode, setCreateMode] = useState<'ai' | 'manual'>('ai');
  const [missionToDelete, setMissionToDelete] = useState<Mission | null>(null);

  const { data: missions = [], isLoading, isError, error, refetch } = useQuery({
    queryKey: ['missions'],
    queryFn: () => api.getMissions(),
  });

  const deleteMissionMutation = useMutation({
    mutationFn: (missionId: string) => api.deleteMission(missionId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['missions'] });
      toast({ title: t('missions.deleteSuccess') });
      setMissionToDelete(null);
    },
    onError: (err) => {
      toast({ title: t('missions.deleteFailed'), description: getApiErrorMessage(err), variant: 'destructive' });
    },
  });

  const createMissionMutation = useMutation({
    mutationFn: () => api.createMission({
      user_goal: form.user_goal,
      target: form.target_value ? { [form.target_key || 'target']: form.target_value } : {},
      constraints: splitLines(form.constraints),
      success_criteria: splitLines(form.success_criteria),
      goal_contract: {
        schema_version: 'goal-contract.v1',
        status: form.goal_outcome === 'custom' ? 'needs_review' : 'resolved',
        outcome_type: form.goal_outcome,
        description: splitLines(form.success_criteria)[0] || form.user_goal,
        minimum_count: form.minimum_count,
        finding_rule_ids: form.goal_outcome === 'flag_capture' ? ['web_exploit.flag_capture'] : [],
        evidence_kinds: [],
        require_confirmed_findings: true,
        require_provenance: true,
        auto_complete: form.goal_outcome !== 'custom',
        source: 'operator',
        confidence: form.goal_outcome === 'custom' ? 0 : 1,
      },
      approval_mode: form.approval_mode,
    }),
    onSuccess: (mission) => {
      queryClient.invalidateQueries({ queryKey: ['missions'] });
      setForm(emptyMissionForm());
      setIsCreateOpen(false);
      toast({ title: t('missions.created'), description: mission.user_goal });
      navigate({ to: '/missions/$missionId', params: { missionId: mission.id } });
    },
    onError: (err) => {
      toast({ title: t('missions.createFailed'), description: getApiErrorMessage(err), variant: 'destructive' });
    },
  });

  const batchMissionMutation = useMutation({
    mutationFn: (input: Parameters<typeof api.batchMissionAction>[0]) => api.batchMissionAction(input),
    onSuccess: () => {
      setSelectedMissionIds([]);
      setTagDraft('');
      setCategoryDraft('');
      queryClient.invalidateQueries({ queryKey: ['missions'] });
      toast({ title: t('missions.batchActionSucceeded') });
    },
    onError: (err) => {
      toast({ title: t('missions.batchActionFailed'), description: getApiErrorMessage(err), variant: 'destructive' });
    },
  });

  const visibleMissions = missions.filter((mission) => showArchived || !mission.archived);
  const [searchQuery, setSearchQuery] = useState('');
  const filteredMissions = useMemo(() => {
    const q = searchQuery.trim().toLowerCase();
    if (!q) return visibleMissions;
    return visibleMissions.filter((mission) =>
      missionDisplayTitle(mission).toLowerCase().includes(q) ||
      mission.user_goal.toLowerCase().includes(q) ||
      mission.tags.some((tag) => tag.toLowerCase().includes(q)) ||
      mission.category?.toLowerCase().includes(q) ||
      Object.values(mission.target ?? {}).some((value) => String(value).toLowerCase().includes(q)),
    );
  }, [visibleMissions, searchQuery]);
  const hasActiveFilters = searchQuery.trim() !== '' || !showArchived;
  const clearFilters = () => {
    setSearchQuery('');
    setShowArchived(true);
  };
  const selectedCount = selectedMissionIds.length;

  const toggleMissionSelection = (missionId: string, selected: boolean) => {
    setSelectedMissionIds((current) => (
      selected
        ? Array.from(new Set([...current, missionId]))
        : current.filter((id) => id !== missionId)
    ));
  };

  const runBatchAction = (action: Parameters<typeof api.batchMissionAction>[0]['action']) => {
    if (selectedMissionIds.length === 0) return;
    batchMissionMutation.mutate({
      mission_ids: selectedMissionIds,
      action,
      tags: splitCommaList(tagDraft),
      category: categoryDraft.trim() || null,
    });
  };

  return (
    <PageContainer>
      <PageHeader
        icon={<Route className="h-5 w-5" />}
        title={t('missions.listTitle')}
        description={t('missions.listDescription')}
        count={filteredMissions.length}
        actions={
          <div className="flex shrink-0 flex-wrap gap-2">
            <Button size="sm" onClick={() => { setCreateMode('ai'); setIsCreateOpen(true); }}>
              <Bot className="h-4 w-4" />
              {t('missions.createMission')}
            </Button>
          </div>
        }
      />
      {isError ? (
        <ErrorState
          title={t('missions.failedToLoad')}
          description={getApiErrorMessage(error)}
          onRetry={() => refetch()}
          retryLabel={t('common.retry')}
        />
      ) : (
        <DataToolbar
          search={searchQuery}
          onSearchChange={setSearchQuery}
          searchPlaceholder={t('missions.searchPlaceholder')}
          count={filteredMissions.length}
          countLabel={t('common.resultsCount', { count: filteredMissions.length })}
          filters={
          <label className="flex cursor-pointer select-none items-center gap-2 text-xs text-muted-foreground">
            <input
              type="checkbox"
              checked={showArchived}
              onChange={(event) => setShowArchived(event.target.checked)}
              className="h-4 w-4 rounded border-border text-primary focus:ring-primary"
            />
            {t('missions.showArchived')}
          </label>
          }
        actions={
          <div className="text-sm text-muted-foreground">
            {t('missions.selectedCount', { count: selectedCount })}
          </div>
        }
        selectionBar={
          <>
            <div className="text-sm font-medium text-foreground">
              {t('missions.selectedCount', { count: selectedCount })}
            </div>
            <div className="flex flex-1 flex-col sm:flex-row gap-2">
              <Input
                value={tagDraft}
                onChange={(event) => setTagDraft(event.target.value)}
                placeholder={t('missions.tagInputPlaceholder')}
                className="h-9"
              />
              <Input
                value={categoryDraft}
                onChange={(event) => setCategoryDraft(event.target.value)}
                placeholder={t('missions.categoryInputPlaceholder')}
                className="h-9"
              />
            </div>
            <div className="flex flex-wrap gap-2">
              <Button variant="outline" size="sm" disabled={batchMissionMutation.isPending} onClick={() => runBatchAction('add_tags')}>{t('missions.addTags')}</Button>
              <Button variant="outline" size="sm" disabled={batchMissionMutation.isPending} onClick={() => runBatchAction('set_category')}>{t('missions.setCategory')}</Button>
              <Button variant="outline" size="sm" disabled={batchMissionMutation.isPending} onClick={() => runBatchAction('archive')}>{t('missions.archiveSelected')}</Button>
              <Button variant="outline" size="sm" disabled={batchMissionMutation.isPending} onClick={() => runBatchAction('restore')}>{t('missions.restoreSelected')}</Button>
              <Button variant="destructive" size="sm" disabled={batchMissionMutation.isPending} onClick={() => runBatchAction('delete')}>{t('missions.deleteSelected')}</Button>
            </div>
          </>
        }
        selectionCount={selectedCount}
      />
      )}

      {/* 加载中 / 空列表 / 有数据 —— 三态都必须在"没报错"的前提下才判断。
          原来这两段是各自独立的条件：后端没起来时 isError 为真渲染报错，
          而下面这段因为 missions 取到空数组、isLoading 为假，照样渲染出
          "还没有归档任务"，于是同一个页面又报错又显示空状态。 */}
      {!isError && (isLoading ? (
        <div className="grid grid-cols-1 gap-4 xl:grid-cols-2">
          {Array.from({ length: 4 }).map((_, i) => (
            <LoadingState key={i} card showHeader lines={3} />
          ))}
        </div>
      ) : filteredMissions.length === 0 ? (
        hasActiveFilters && missions.length > 0 ? (
          <EmptyState
            icon={<SearchX className="h-6 w-6" />}
            title={t('common.noMatchingResults')}
            action={
              <Button variant="outline" size="sm" onClick={clearFilters}>
                {t('common.clearFilters')}
              </Button>
            }
          />
        ) : (
          <EmptyState
            icon={<Route className="h-6 w-6" />}
            title={t('missions.emptyTitle')}
            description={t('missions.emptyDescription')}
            action={
              <div className="flex flex-wrap justify-center gap-2">
                <Button size="sm" onClick={() => { setCreateMode('ai'); setIsCreateOpen(true); }}>
                  <Bot className="h-4 w-4" />
                  {t('missions.aiCreateButton')}
                </Button>
                <Button size="sm" variant="outline" onClick={() => { setCreateMode('manual'); setIsCreateOpen(true); }}>
                  <Plus className="h-4 w-4" />
                  {t('missions.manualCreateButton')}
                </Button>
              </div>
            }
            secondaryAction={
              <Link to="/">
                <Button size="sm" variant="ghost">
                  {t('missions.viewMissionControl')}
                </Button>
              </Link>
            }
          />
        )
      ) : (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
          {filteredMissions.map((mission) => (
            <MissionListCard
              key={mission.id}
              mission={mission}
              selected={selectedMissionIds.includes(mission.id)}
              onSelectedChange={(selected) => toggleMissionSelection(mission.id, selected)}
              onDelete={setMissionToDelete}
            />
          ))}
        </div>
      ))}

      {/* 统一创建向导：AI 分析（/intake/analyze → /intake/start）与手动创建 */}
      <Dialog open={isCreateOpen} onOpenChange={setIsCreateOpen}>
        <DialogContent className="max-h-[90vh] max-w-4xl overflow-y-auto p-0" closeLabel={t('common.close')}>
          <Tabs value={createMode} onValueChange={(value) => setCreateMode(value as 'ai' | 'manual')} className="w-full">
            <div className="border-b border-border px-5 pt-4">
              <TabsList animated indicatorClassName="bg-card" className="h-9 bg-muted/60 p-1">
                <TabsTrigger value="ai" className="gap-1.5">
                  <Bot className="h-3.5 w-3.5" />
                  {t('missions.aiCreateButton')}
                </TabsTrigger>
                <TabsTrigger value="manual" className="gap-1.5">
                  <Plus className="h-3.5 w-3.5" />
                  {t('missions.manualCreateButton')}
                </TabsTrigger>
              </TabsList>
            </div>
            <TabsContent value="ai" className="outline-none">
              <AIIntakeForm />
            </TabsContent>
            <TabsContent value="manual" className="outline-none">
              <div className="p-5">
                <DialogHeader>
                  <DialogTitle>{t('missions.manualCreateTitle')}</DialogTitle>
                  <DialogDescription>{t('missions.manualCreateDescription')}</DialogDescription>
                </DialogHeader>
                <ManualMissionForm
                  form={form}
                  setForm={setForm}
                  isPending={createMissionMutation.isPending}
                  onSubmit={() => createMissionMutation.mutate()}
                />
              </div>
            </TabsContent>
          </Tabs>
        </DialogContent>
      </Dialog>

      {/* Single mission delete confirmation */}
      <Dialog open={missionToDelete !== null} onOpenChange={(open) => { if (!open) setMissionToDelete(null); }}>
        <DialogContent closeLabel={t('common.close')}>
          <DialogHeader>
            <DialogTitle>{t('missions.deleteTitle')}</DialogTitle>
            <DialogDescription>
              {missionToDelete?.status === 'running'
                ? t('missions.deleteRunningWarning')
                : t('missions.deleteDescription')}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setMissionToDelete(null)}>
              {t('common.cancel')}
            </Button>
            <Button
              tone="danger"
              disabled={deleteMissionMutation.isPending || missionToDelete?.status === 'running'}
              onClick={() => {
                if (missionToDelete) {
                  deleteMissionMutation.mutate(missionToDelete.id);
                }
              }}
            >
              {deleteMissionMutation.isPending && <Loader2 className="mr-1 h-4 w-4 animate-spin" />}
              {t('missions.deleteButton')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </PageContainer>
  );
}

function ManualMissionForm({
  form,
  setForm,
  isPending,
  onSubmit,
}: {
  form: ReturnType<typeof emptyMissionForm>;
  setForm: (form: ReturnType<typeof emptyMissionForm>) => void;
  isPending: boolean;
  onSubmit: () => void;
}) {
  const { t } = useTranslation();

  return (
    <div className="section-stack pt-2">
      <div className="space-y-2">
        <Label htmlFor="mission-goal">{t('missions.userGoal')}</Label>
        <Textarea
          id="mission-goal"
          value={form.user_goal}
          onChange={(event) => setForm({ ...form, user_goal: event.target.value })}
          placeholder={t('missions.goalPlaceholder')}
          className="min-h-[112px] resize-none"
        />
      </div>
      <div className="grid grid-cols-1 gap-3">
        <div className="space-y-2">
          <Label htmlFor="target-key">{t('missions.targetKey')}</Label>
          <Input
            id="target-key"
            value={form.target_key}
            onChange={(event) => setForm({ ...form, target_key: event.target.value })}
            placeholder="url"
          />
        </div>
      </div>
      <div className="space-y-2">
        <Label>{t('approvalMode.label')}</Label>
        <ApprovalModeSelector
          value={form.approval_mode}
          onChange={(mode) => setForm({ ...form, approval_mode: mode })}
        />
      </div>
      <div className="space-y-2">
        <Label htmlFor="target-value">{t('missions.targetValue')}</Label>
        <Input
          id="target-value"
          value={form.target_value}
          onChange={(event) => setForm({ ...form, target_value: event.target.value })}
          placeholder="https://app.example.test"
        />
      </div>
      <div className="space-y-2">
        <Label htmlFor="constraints">{t('missions.constraints')}</Label>
        <Textarea
          id="constraints"
          value={form.constraints}
          onChange={(event) => setForm({ ...form, constraints: event.target.value })}
          placeholder={t('missions.constraintsPlaceholder')}
          className="min-h-[72px] resize-none"
        />
      </div>
      <div className="space-y-2">
        <Label>{t('missions.goalContract.outcomeType')}</Label>
        <Select
          value={form.goal_outcome}
          onValueChange={(value) => setForm({
            ...form,
            goal_outcome: value as GoalOutcomeType,
          })}
        >
          <SelectTrigger>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="flag_capture">{t('missions.goalContract.flagCapture')}</SelectItem>
            <SelectItem value="confirmed_finding">{t('missions.goalContract.confirmedFinding')}</SelectItem>
            <SelectItem value="verified_evidence">{t('missions.goalContract.verifiedEvidence')}</SelectItem>
            <SelectItem value="coverage">{t('missions.goalContract.coverage')}</SelectItem>
            <SelectItem value="custom">{t('missions.goalContract.custom')}</SelectItem>
          </SelectContent>
        </Select>
        {form.goal_outcome !== 'coverage' && form.goal_outcome !== 'custom' ? (
          <Input
            type="number"
            min={1}
            max={1000}
            value={form.minimum_count}
            onChange={(event) => setForm({
              ...form,
              minimum_count: Math.max(1, Number(event.target.value) || 1),
            })}
            aria-label={t('missions.goalContract.minimumCount')}
          />
        ) : null}
        <p className="text-xs text-muted-foreground">
          {t('missions.goalContract.description')}
        </p>
      </div>
      <div className="space-y-2">
        <Label htmlFor="success-criteria">{t('missions.successCriteria')}</Label>
        <Textarea
          id="success-criteria"
          value={form.success_criteria}
          onChange={(event) => setForm({ ...form, success_criteria: event.target.value })}
          placeholder={t('missions.successCriteriaPlaceholder')}
          className="min-h-[72px] resize-none"
        />
      </div>
      <Button
        className="w-full"
        disabled={!form.user_goal.trim() || isPending}
        onClick={onSubmit}
      >
        {isPending ? <Loader2 className="h-4 w-4 animate-spin" /> : null}
        {t('missions.createMission')}
      </Button>
    </div>
  );
}

function MissionListCard({
  mission,
  selected,
  onSelectedChange,
  onDelete,
}: {
  mission: Mission;
  selected: boolean;
  onSelectedChange: (selected: boolean) => void;
  onDelete?: (mission: Mission) => void;
}) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const tone = missionStatusTone(mission.status);
  const isRunning = mission.status === 'running';
  const isArchived = mission.archived;

  const openDetail = () => {
    navigate({ to: '/missions/$missionId', params: { missionId: mission.id } });
  };

  return (
    <div
      className={cn(
        'rounded-lg transition-opacity',
        selected && 'ring-2 ring-primary/25 ring-offset-1',
        isArchived && 'opacity-60',
      )}
    >
      <EntityCard
        interactive
        role="button"
        tabIndex={0}
        onClick={openDetail}
        onKeyDown={(event) => {
          if (event.key === 'Enter' || event.key === ' ') {
            event.preventDefault();
            openDetail();
          }
        }}
        tone={tone}
        icon={<Target className="h-4 w-4" />}
        title={missionDisplayTitle(mission)}
        status={
          <span className="flex items-center gap-2">
            <input
              type="checkbox"
              aria-label={t('missions.selectedCount', { count: 1 })}
              checked={selected}
              onClick={(event) => event.stopPropagation()}
              onChange={(event) => onSelectedChange(event.target.checked)}
              className="h-4 w-4 cursor-pointer rounded border-border text-primary transition-colors focus:ring-2 focus:ring-primary/20 focus:ring-offset-0"
            />
            <MissionStatusBadge
              status={mission.status}
              label={formatStatus(t, mission.status)}
              dot
              size="sm"
            />
          </span>
        }
        meta={
          <span className="flex flex-wrap items-center gap-1.5">
            {mission.category && <Badge tone="neutral">{mission.category}</Badge>}
            {isArchived && <Badge tone="warning">{t('missions.archived')}</Badge>}
          </span>
        }
        description={formatTarget(t, mission.target)}
        metrics={[
          {
            label: '',
            value: (
              <span className="inline-flex items-center gap-1.5 text-xs font-normal text-muted-foreground">
                <Clock className="h-3 w-3" />
                {t('missions.updatedRelative', {
                  time: formatDistanceToNow(new Date(mission.updated_at), { addSuffix: true }),
                })}
              </span>
            ),
          },
        ]}
        actions={
          <>
            {mission.tags.map((tag) => (
              <Badge key={tag} tone="info" variant="outline">
                #{tag}
              </Badge>
            ))}
            {onDelete && (
              <Button
                type="button"
                variant="outline"
                size="icon"
                className="h-8 w-8 rounded-lg border-border bg-background text-muted-foreground shadow-xs transition-colors duration-150 hover:border-danger/40 hover:bg-danger-soft hover:text-danger"
                onClick={(event) => {
                  event.stopPropagation();
                  onDelete(mission);
                }}
                onPointerDown={(event) => event.stopPropagation()}
                aria-label={t('missions.deleteButton')}
                title={isRunning ? t('missions.deleteRunningWarning') : t('missions.deleteButton')}
                disabled={isRunning}
              >
                <Trash2 className="h-4 w-4" />
              </Button>
            )}
          </>
        }
      />
    </div>
  );
}

function MissionDetail({ missionId }: { missionId: string }) {
  const { t } = useTranslation();
  const { toast } = useToast();
  const queryClient = useQueryClient();

  const routerState = useRouterState();
  const search = routerState.location.search as Record<string, string | undefined>;
  const queryDecisionGateId = search?.decisionGateId;

  const [isDecisionDrawerOpen, setIsDecisionDrawerOpen] = useState(false);
  const [activeGateForDrawer, setActiveGateForDrawer] = useState<DecisionGate | null>(null);
  const [missionTab, setMissionTab] = useState<string>('canvas');

  const missionQuery = useQuery({
    queryKey: ['mission', missionId],
    queryFn: () => api.getMission(missionId),
    refetchInterval: (query) =>
      query.state.data?.status === 'running' ? 3000 : false,
  });

  const canvasQuery = useQuery({
    queryKey: ['mission-canvas', missionId],
    queryFn: () => api.getMissionCanvas(missionId),
    refetchInterval: missionQuery.data && ['running', 'waiting_for_decision'].includes(missionQuery.data.status)
      ? 3000
      : false,
  });

  const providersQuery = useQuery({
    queryKey: ['providers'],
    queryFn: () => api.getProviders(),
  });
  const providerHealthById = useProviderHealthById(providersQuery.data);

  const modulesQuery = useQuery({
    queryKey: ['modules'],
    queryFn: () => api.getModules(),
  });

  const invalidateMission = useCallback(() => {
    queryClient.invalidateQueries({ queryKey: ['missions'] });
    queryClient.invalidateQueries({ queryKey: ['mission', missionId] });
    queryClient.invalidateQueries({ queryKey: ['mission-canvas', missionId] });
    queryClient.invalidateQueries({ queryKey: ['mission-timeline', missionId] });
  }, [queryClient, missionId]);

  const startMutation = useMutation({
    mutationFn: (input: { maxConcurrentBranches: number; swarmWidth: number; maxWorkers: number }) =>
      api.startMission(missionId, {
        auto_start_runtime: true,
        max_concurrent_branches: input.maxConcurrentBranches,
        config: {
          intent_worker_concurrency: input.swarmWidth,
          max_concurrent_workers: input.maxWorkers,
        },
      }),
    onSuccess: () => {
      toast({ title: t('missions.started') });
      invalidateMission();
    },
    onError: (err) => toast({ title: t('missions.startFailed'), description: getApiErrorMessage(err), variant: 'destructive' }),
  });

  const pauseMutation = useMutation({
    mutationFn: () => api.pauseMission(missionId),
    onSuccess: () => {
      toast({ title: t('missions.paused') });
      invalidateMission();
    },
    onError: (err) => toast({ title: t('missions.pauseFailed'), description: getApiErrorMessage(err), variant: 'destructive' }),
  });

  const resumeMutation = useMutation({
    mutationFn: () => api.resumeMission(missionId),
    onSuccess: () => {
      toast({ title: t('missions.resumed') });
      invalidateMission();
    },
    onError: (err) => toast({ title: t('missions.resumeFailed'), description: getApiErrorMessage(err), variant: 'destructive' }),
  });

  const approvalModeMutation = useMutation({
    mutationFn: (approvalMode: ApprovalMode) => api.updateMission(missionId, {
      approval_mode: approvalMode,
    }),
    onSuccess: () => {
      toast({ title: t('approvalMode.updated') });
      invalidateMission();
    },
    onError: (err) => toast({
      title: t('approvalMode.updateFailed'),
      description: getApiErrorMessage(err),
      variant: 'destructive',
    }),
  });

  const mission = missionQuery.data;
  const canvas = canvasQuery.data;
  const currentRun = currentMissionRun(canvas, mission);

  // 头部只留标题/目的/主控按钮，运行与模型信息下沉到「总览」tab。
  const providers = providersQuery.data ?? [];
  const defaultProvider = providers.find((provider) => provider.is_default) ?? providers[0] ?? null;
  const readiness = defaultProvider ? getProviderReadiness(defaultProvider, providerHealthById[defaultProvider.id]) : null;
  const providerSummary = readiness ? `${defaultProvider?.name}: ${t(readiness.labelKey)}` : t('missions.noProvider');

  // 资产详情抽屉里的关联跳转：切到对应 tab（新看板不带行选中态，跳转即定位到列表顶部）。
  const handleViewFinding = useCallback(() => {
    setMissionTab('findings');
  }, []);

  const handleViewToolInvocation = useCallback(() => {
    setMissionTab('tools');
  }, []);

  // triage 写回后端成功后刷新画布：finding 的状态/严重度会带到覆盖图与报告里，
  // 不能只改本地那一行。
  const handleFindingTriaged = useCallback(() => {
    invalidateMission();
    queryClient.invalidateQueries({ queryKey: ['mission-coverage-graph', missionId] });
  }, [invalidateMission, queryClient, missionId]);

  const pendingGates = useMemo(() => {
    return (canvas?.decision_gates || []).filter(g => g.status === 'pending');
  }, [canvas]);

  const activeGate = useMemo(() => {
    if (activeGateForDrawer) return activeGateForDrawer;
    if (queryDecisionGateId) {
      return (canvas?.decision_gates || []).find(g => g.id === queryDecisionGateId) || pendingGates[0];
    }
    return pendingGates[0];
  }, [canvas, queryDecisionGateId, pendingGates, activeGateForDrawer]);

  useEffect(() => {
    if (queryDecisionGateId) {
      setIsDecisionDrawerOpen(true);
    }
  }, [queryDecisionGateId]);

  // 任务详情是宽幅工作区：画布/会话/表格都该铺满，不用外壳那套阅读宽度。
  usePageWidthMode('full');

  if (missionQuery.isError) {
    return (
      <PageContainer maxWidth="full">
        <ErrorState
          title={t('missions.failedToLoad')}
          description={getApiErrorMessage(missionQuery.error)}
          retry={
            <Link to="/missions">
              <Button variant="outline" size="sm">
                <Route className="h-4 w-4" />
                {t('missions.backToMissions')}
              </Button>
            </Link>
          }
        />
      </PageContainer>
    );
  }

  if (missionQuery.isLoading || !mission) {
    return (
      <PageContainer maxWidth="full">
        <LoadingState card showHeader lines={5} />
      </PageContainer>
    );
  }

  return (
    <div className="page-stack">
      <MissionHeader
        mission={mission}
        onStart={(maxConcurrentBranches, swarmWidth, maxWorkers) =>
          startMutation.mutate({ maxConcurrentBranches, swarmWidth, maxWorkers })
        }
        onPause={() => pauseMutation.mutate()}
        onResume={() => resumeMutation.mutate()}
        onOpenDecision={() => setIsDecisionDrawerOpen(true)}
        busy={startMutation.isPending || pauseMutation.isPending || resumeMutation.isPending}
      />

      <Tabs value={missionTab} onValueChange={setMissionTab} className="w-full page-stack">
        {/* tab 条左对齐跟着头部走，超宽时不漂到页面中间 */}
        <div className="min-w-0 overflow-x-auto">
          <TabsList animated indicatorClassName="bg-card" className="h-9 w-max justify-center bg-muted/60 p-1 rounded-lg">
            <TabsTrigger value="canvas">{t('missions.tabCanvas')}</TabsTrigger>
            <TabsTrigger value="sessions">{t('missions.tabSessions')}</TabsTrigger>
            <TabsTrigger value="assets">{t('missions.tabAssets')} ({canvas?.assets?.length || 0})</TabsTrigger>
            <TabsTrigger value="findings">{t('missions.tabFindings')} ({canvas?.findings?.length || 0})</TabsTrigger>
            <TabsTrigger value="tools">{t('missions.tabToolInvocations')} ({canvas?.tool_invocations?.length || 0})</TabsTrigger>
            <TabsTrigger value="logs">{t('missions.tabLogs')}</TabsTrigger>
            <TabsTrigger value="coverage">{t('missions.tabCoverage')}</TabsTrigger>
            <TabsTrigger value="retests">{t('missions.tabRetests')} ({canvas?.findings?.length || 0})</TabsTrigger>
            <TabsTrigger value="report">{t('missions.tabReport')}</TabsTrigger>
            <TabsTrigger value="overview">{t('missions.tabOverview')}</TabsTrigger>
          </TabsList>
        </div>

        <TabsContent value="canvas" className="outline-none">
          {/* 推导链路（探索画布主图）：起点 → 目标 → 意图 → 事实 → 提示 → 漏洞 */}
          <ExplorationGraphCanvas missionId={missionId} className="h-[76vh] min-h-[540px]" />
        </TabsContent>

        <TabsContent value="sessions" className="outline-none">
          <MissionSessionsPanel missionId={mission.id} projectId={mission.project_id || ''} />
        </TabsContent>

        <TabsContent value="assets" className="outline-none">
          {/* 资产页：根域名 / IP / 子域名 / 应用 / 服务 / 接口 六类分桶 */}
          <MissionAssetBoard
            assets={canvas?.assets ?? []}
            onViewFinding={handleViewFinding}
            onViewToolInvocation={handleViewToolInvocation}
          />
        </TabsContent>

        <TabsContent value="findings" className="outline-none">
          <MissionFindingsBoard
            findings={canvas?.findings ?? []}
            projectId={mission.project_id || ''}
            onTriage={handleFindingTriaged}
          />
        </TabsContent>

        <TabsContent value="tools" className="outline-none">
          <MissionToolCallsBoard invocations={canvas?.tool_invocations ?? []} />
        </TabsContent>

        <TabsContent value="logs" className="outline-none">
          {/* 播报板：Agent 蜂群 journal 的时间轴视图 */}
          <MissionBroadcastLog missionId={mission.id} />
        </TabsContent>

        <TabsContent value="coverage" className="outline-none">
          {/* 覆盖图：拓扑由后端 /missions/{id}/coverage-graph 推导，前端只布局 */}
          <MissionCoverageMap missionId={mission.id} />
        </TabsContent>

        <TabsContent value="retests" className="outline-none">
          <MissionRetestPanel
            missionId={mission.id}
            findings={canvas?.findings ?? []}
            onRetestFinished={handleFindingTriaged}
          />
        </TabsContent>

        <TabsContent value="report" className="outline-none">
          {/* 报告：后端 GET /reports/{mission_id} 生成，前端只渲染 */}
          <MissionReportPanel mission={mission} />
        </TabsContent>

        <TabsContent value="overview" className="page-stack outline-none">
          <MissionOverviewTab
            mission={mission}
            canvas={canvas}
            modules={modulesQuery.data ?? []}
            modulesLoading={modulesQuery.isLoading}
            run={currentRun}
            providerSummary={providerSummary}
            approvalModeBusy={approvalModeMutation.isPending}
            onApprovalModeChange={(mode) => approvalModeMutation.mutate(mode)}
          />
        </TabsContent>
      </Tabs>

      {/* Decision slide drawer */}
      {activeGate && (
        <DecisionDrawer
          gate={activeGate}
          projectId={mission.project_id}
          runId={mission.active_run_id || currentRun?.id || ''}
          isOpen={isDecisionDrawerOpen}
          onClose={() => {
            setIsDecisionDrawerOpen(false);
            setActiveGateForDrawer(null);
          }}
          onAnswer={() => {
            invalidateMission();
            setIsDecisionDrawerOpen(false);
            setActiveGateForDrawer(null);
          }}
        />
      )}
    </div>
  );
}

function MissionHeader({
  mission,
  onStart,
  onPause,
  onResume,
  onOpenDecision,
  busy,
}: {
  mission: Mission;
  onStart: (maxConcurrentBranches: number, swarmWidth: number, maxWorkers: number) => void;
  onPause: () => void;
  onResume: () => void;
  onOpenDecision: () => void;
  busy: boolean;
}) {
  const { t } = useTranslation();
  const [concurrency, setConcurrency] = useState(1);
  const [swarmWidth, setSwarmWidth] = useState(4);
  const [maxWorkers, setMaxWorkers] = useState(4);
  const isRunning = mission.status === 'running';
  const isPaused = mission.status === 'paused';
  const needsDecision = mission.status === 'waiting_for_decision';
  const terminal = mission.status === 'completed' || mission.status === 'failed' || mission.status === 'cancelled';
  // 只有"待启动 / 可重启"时才显示并发滑杆；运行中 / 暂停 / 等待决策时不可改。
  const showConcurrency = !isRunning && !isPaused && !needsDecision;

  // 右上角单个主控按钮：未执行过 → 启动；运行中 → 暂停；暂停/等待决策 → 继续；终态 → 重新启动。
  let controlIcon = <Play className="h-4 w-4" />;
  let controlLabel = t('missions.start');
  let controlDisabled = busy;
  if (isRunning) {
    controlIcon = <PauseCircle className="h-4 w-4" />;
    controlLabel = t('missions.pause');
    // 运行中要允许暂停：只在其它的 start/pause/resume mutation 进行中禁用。
    // （曾经的 `busy || isRunning` 在 isRunning 恒真，暂停按钮永远灰着。）
    controlDisabled = busy;
  } else if (isPaused || needsDecision) {
    controlIcon = <Play className="h-4 w-4" />;
    controlLabel = t('missions.resume');
    controlDisabled = busy || isRunning;
  } else if (mission.active_run_id || terminal) {
    controlIcon = <RotateCcw className="h-4 w-4" />;
    controlLabel = t('missions.restart');
  }
  const handleControl = () => {
    if (isRunning) return onPause();
    if (isPaused || needsDecision) return onResume();
    return onStart(concurrency, swarmWidth, maxWorkers);
  };

  return (
    <Section className="p-4" bodyClassName="p-0">
      {/* 第一行：返回 + 标题 + 状态 + 主控按钮（紧凑头部） */}
      <div className="flex items-center gap-2">
        <Link to="/missions" aria-label={t('missions.backToMissions')} title={t('missions.backToMissions')}>
          <Button variant="ghost" size="icon" className="h-8 w-8 shrink-0 text-muted-foreground">
            <Route className="h-4 w-4" />
          </Button>
        </Link>
        <h1
          className="min-w-0 flex-1 truncate text-sm font-semibold text-foreground"
          title={missionDisplayTitle(mission)}
        >
          {missionDisplayTitle(mission)}
        </h1>
        <code className="hidden max-w-[168px] truncate rounded bg-muted px-1.5 py-0.5 font-mono text-xs text-muted-foreground lg:inline">
          {mission.id}
        </code>
        <MissionStatusBadge status={mission.status} label={formatStatus(t, mission.status)} dot size="sm" />
        {needsDecision && (
          <Button size="sm" onClick={onOpenDecision} className="shrink-0">
            <AlertCircle className="h-4 w-4" />
            {t('missions.openDecision')}
          </Button>
        )}
        <Button size="sm" onClick={handleControl} disabled={controlDisabled} className="shrink-0">
          {busy ? <Loader2 className="h-4 w-4 animate-spin" /> : controlIcon}
          {controlLabel}
        </Button>
      </div>
      {/* 第二行：一行目的 */}
      <p className="mt-1.5 truncate text-xs text-muted-foreground" title={mission.user_goal}>
        {mission.user_goal}
      </p>
      {/* 并发探索数：仅待启动 / 可重启时可调。这是分支级并发（一波同时跑几条
          branch），与 Worker 池里单个 profile 的 max_concurrency 是两层、互不冲突。 */}
      {showConcurrency && (
        <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1">
          <Label htmlFor="mission-concurrency" className="whitespace-nowrap text-xs font-medium text-foreground">
            {t('missions.concurrency')}
          </Label>
          <input
            id="mission-concurrency"
            type="range"
            min={1}
            max={8}
            step={1}
            value={concurrency}
            onChange={(event) => setConcurrency(Number(event.target.value))}
            className="h-1.5 w-40 cursor-pointer accent-primary"
            aria-label={t('missions.concurrency')}
          />
          <span className="w-6 text-right text-xs font-semibold tabular-nums text-foreground">{concurrency}</span>
          <span className="min-w-0 flex-1 text-xs text-muted-foreground">{t('missions.concurrencyHint')}</span>
        </div>
      )}
      {showConcurrency && (
        <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1">
          <Label htmlFor="mission-swarm" className="whitespace-nowrap text-xs font-medium text-foreground">
            {t('missions.swarmConcurrency')}
          </Label>
          <input
            id="mission-swarm"
            type="range"
            min={1}
            max={4}
            step={1}
            value={swarmWidth}
            onChange={(event) => setSwarmWidth(Number(event.target.value))}
            className="h-1.5 w-40 cursor-pointer accent-primary"
            aria-label={t('missions.swarmConcurrency')}
          />
          <span className="w-6 text-right text-xs font-semibold tabular-nums text-foreground">{swarmWidth}</span>
          <span className="min-w-0 flex-1 text-xs text-muted-foreground">{t('missions.swarmConcurrencyHint')}</span>
        </div>
      )}
      {showConcurrency && (
        <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1">
          <Label htmlFor="mission-max-workers" className="whitespace-nowrap text-xs font-medium text-foreground">
            {t('missions.maxWorkers')}
          </Label>
          <input
            id="mission-max-workers"
            type="range"
            min={1}
            max={16}
            step={1}
            value={maxWorkers}
            onChange={(event) => setMaxWorkers(Number(event.target.value))}
            className="h-1.5 w-40 cursor-pointer accent-primary"
            aria-label={t('missions.maxWorkers')}
          />
          <span className="w-6 text-right text-xs font-semibold tabular-nums text-foreground">{maxWorkers}</span>
          <span className="min-w-0 flex-1 text-xs text-muted-foreground">{t('missions.maxWorkersHint')}</span>
        </div>
      )}
    </Section>
  );
}

function CapabilityPool({ modules, isLoading }: { modules: ApiModule[]; isLoading: boolean }) {
  const { t } = useTranslation();
  const planned = ['fuzzing', 'supply_chain', 'cloud_native', 'binary_dynamic', 'exploitability'];
  const enabled = modules.filter((module) => module.enabled);
  const disabled = modules.filter((module) => !module.enabled);

  return (
    <Section
      icon={<Cable className="h-4 w-4" />}
      title={t('missions.capabilityPool')}
    >
      {isLoading ? (
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <Loader2 className="h-4 w-4 animate-spin" />
          {t('missions.loadingCapabilities')}
        </div>
      ) : (
        <div className="section-stack">
          <CapabilityGroup title={t('missions.availableCapabilities')} modules={enabled} empty={t('missions.noAvailableCapabilities')} status="available" />
          <CapabilityGroup title={t('missions.unavailableCapabilities')} modules={disabled} empty={t('missions.noUnavailableCapabilities')} status="unavailable" />
          <div>
            <div className="label-spec mb-2">{t('missions.plannedCapabilities')}</div>
            <div className="flex flex-wrap gap-2">
              {planned.map((item) => (
                <Badge key={item} tone="neutral">{formatModuleDomain(t, item)}</Badge>
              ))}
            </div>
          </div>
        </div>
      )}
    </Section>
  );
}

function CapabilityGroup({ title, modules, empty, status }: { title: string; modules: ApiModule[]; empty: string; status: 'available' | 'unavailable' }) {
  const { t } = useTranslation();
  const tone: StatusTone = status === 'available' ? 'success' : 'neutral';
  return (
    <div>
      <div className="label-spec mb-2">{title}</div>
      {modules.length === 0 ? (
        <div className="text-xs text-muted-foreground">{empty}</div>
      ) : (
        <div className="section-stack">
          {modules.map((module) => (
            <div key={module.id} className="surface-inset p-3">
              <div className="flex items-start justify-between gap-2">
                <div className="min-w-0">
                  <div className="truncate text-sm font-medium text-foreground">{module.name}</div>
                  <div className="mt-1 text-xs text-muted-foreground">{formatModuleDomain(t, module.domain)}</div>
                </div>
                <Badge tone={tone}>{t(`status.${status}`)}</Badge>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function DirectivesList({ directives }: { directives: UserDirective[] }) {
  const { t } = useTranslation();
  return (
    <Section
      icon={<MessageSquarePlus className="h-4 w-4" />}
      title={t('missions.directives')}
      flush
    >
      {directives.length === 0 ? (
        <div className="p-4 text-sm text-muted-foreground">{t('missions.noDirectives')}</div>
      ) : (
        <ol className="divide-y divide-border">
          {directives.slice(0, 12).map((directive) => {
            const tone = missionStatusTone(directive.status);
            return (
              <li key={directive.id} className="p-4">
                <div className="flex items-start justify-between gap-2">
                  <Badge tone={tone}>{formatStatus(t, directive.status)}</Badge>
                  <span className="text-xs text-muted-foreground">{formatDistanceToNow(new Date(directive.created_at), { addSuffix: true })}</span>
                </div>
                <div className="mt-2 text-sm font-medium text-foreground">{t(`missions.directiveTypes.${directive.directive_type}`, directive.directive_type)}</div>
                <div className="mt-1 break-words text-xs text-muted-foreground">{directive.content}</div>
              </li>
            );
          })}
        </ol>
      )}
    </Section>
  );
}

function currentMissionRun(canvas: MissionCanvas | undefined, mission: Mission | undefined): ApiAuditRun | null {
  if (!canvas || !mission) return null;
  if (mission.active_run_id) {
    return canvas.runs.find((run) => run.id === mission.active_run_id) ?? null;
  }
  return canvas.runs.at(-1) ?? null;
}

function splitLines(value: string): string[] {
  return value.split('\n').map((item) => item.trim()).filter(Boolean);
}

function splitCommaList(value: string): string[] {
  return value.split(',').map((item) => item.trim()).filter(Boolean);
}

function formatTarget(t: ReturnType<typeof useTranslation>['t'], target: Record<string, string>): string {
  const entries = Object.entries(target);
  if (entries.length === 0) return t('missions.targetNotSpecified');
  return entries.map(([key, value]) => `${key}: ${value}`).join(', ');
}

function MissionOverviewTab({
  mission,
  canvas,
  modules,
  modulesLoading,
  run,
  providerSummary,
  approvalModeBusy,
  onApprovalModeChange,
}: {
  mission: Mission;
  canvas: MissionCanvas | undefined;
  modules: ApiModule[];
  modulesLoading: boolean;
  run: ApiAuditRun | null;
  providerSummary: string;
  approvalModeBusy: boolean;
  onApprovalModeChange: (mode: ApprovalMode) => void;
}) {
  const { t } = useTranslation();
  const branchCount = canvas?.branches?.length ?? 0;
  const findingCount = canvas?.findings?.length ?? 0;
  const evidenceCount = canvas?.evidence?.length ?? 0;
  const toolCount = canvas?.tool_invocations?.length ?? 0;
  const runCount = (canvas?.run_history ?? canvas?.runs)?.length ?? 0;
  const pendingGateCount = (canvas?.decision_gates ?? []).filter(g => g.status === 'pending').length;

  return (
    <div className="grid grid-cols-1 xl:grid-cols-12 gap-6">
      <div className="xl:col-span-8 page-stack">
        <Section icon={<Target className="h-4 w-4" />} title={t('missions.overview.target')}>
          <div className="space-y-3">
            <div>
              <div className="label-spec mb-1">{t('missions.overview.target')}</div>
              <p className="text-sm text-foreground">{formatTarget(t, mission.target)}</p>
            </div>
            <div>
              <div className="label-spec mb-1">{t('missions.overview.constraints')}</div>
              {mission.constraints.length > 0 ? (
                <ul className="list-inside list-disc space-y-0.5 text-sm text-muted-foreground">
                  {mission.constraints.map((c, i) => <li key={i}>{c}</li>)}
                </ul>
              ) : (
                <p className="text-sm text-muted-foreground">{t('missions.overview.noConstraints')}</p>
              )}
            </div>
            <div>
              <div className="label-spec mb-1">{t('missions.overview.successCriteria')}</div>
              {mission.success_criteria.length > 0 ? (
                <ul className="list-inside list-disc space-y-0.5 text-sm text-muted-foreground">
                  {mission.success_criteria.map((c, i) => <li key={i}>{c}</li>)}
                </ul>
              ) : (
                <p className="text-sm text-muted-foreground">{t('missions.overview.noSuccessCriteria')}</p>
              )}
            </div>
          </div>
        </Section>

        <Section title={t('missions.overview.progress')}>
          <div className="grid grid-cols-2 gap-4 sm:grid-cols-3">
            <OverviewMetric label={t('missions.overview.branches')} value={branchCount} icon={<GitBranch className="h-4 w-4" />} />
            <OverviewMetric label={t('missions.overview.findings')} value={findingCount} icon={<ShieldAlert className="h-4 w-4" />} tone={findingCount > 0 ? 'warning' : 'neutral'} />
            <OverviewMetric label={t('missions.overview.evidence')} value={evidenceCount} icon={<Database className="h-4 w-4" />} />
            <OverviewMetric label={t('missions.overview.toolCalls')} value={toolCount} icon={<TerminalSquare className="h-4 w-4" />} />
            <OverviewMetric label={t('missions.overview.decisionGates')} value={pendingGateCount} icon={<AlertCircle className="h-4 w-4" />} tone={pendingGateCount > 0 ? 'danger' : 'neutral'} />
            <OverviewMetric label={t('missions.overview.runs')} value={runCount} icon={<Activity className="h-4 w-4" />} />
          </div>
        </Section>
      </div>

      <div className="xl:col-span-4 page-stack">
        {/* 头部精炼后，运行与模型配置信息统一放在总览里 */}
        <Section icon={<Activity className="h-4 w-4" />} title={t('missions.overview.runtime')}>
          <div className="space-y-2 text-sm">
            <div className="flex items-center justify-between gap-2">
              <span className="shrink-0 text-muted-foreground">{t('missions.activeRun')}</span>
              <span className="flex min-w-0 items-center gap-2">
                {mission.active_run_id ? (
                  <>
                    <code className="min-w-0 truncate font-mono text-xs text-foreground" title={mission.active_run_id}>
                      {mission.active_run_id}
                    </code>
                    {run && <Badge tone="neutral">{formatStatus(t, run.status)}</Badge>}
                  </>
                ) : (
                  <span className="text-muted-foreground">{t('missions.noActiveRun')}</span>
                )}
              </span>
            </div>
            <div className="flex items-center justify-between gap-2">
              <span className="shrink-0 text-muted-foreground">{t('missions.providerReadiness')}</span>
              <span className="min-w-0 truncate text-foreground" title={providerSummary}>{providerSummary}</span>
            </div>
            <div className="flex items-center justify-between gap-2">
              <span className="shrink-0 text-muted-foreground">{t('approvalMode.label')}</span>
              <ApprovalModeSelector
                value={mission.approval_mode}
                onChange={onApprovalModeChange}
                disabled={approvalModeBusy}
                align="end"
              />
            </div>
            {mission.archived && (
              <div className="flex items-center justify-between gap-2">
                <span className="shrink-0 text-muted-foreground">{t('missions.overview.archiveStatus')}</span>
                <Badge tone="warning">{t('missions.archived')}</Badge>
              </div>
            )}
          </div>
        </Section>

        <Section title={t('missions.overview.scope')}>
          <div className="space-y-2 text-sm">
            <div className="flex items-center justify-between">
              <span className="text-muted-foreground">{t('missions.overview.createdAt')}</span>
              <span className="text-foreground">{formatDistanceToNow(new Date(mission.created_at), { addSuffix: true })}</span>
            </div>
            <div className="flex items-center justify-between">
              <span className="text-muted-foreground">{t('missions.overview.updatedAt')}</span>
              <span className="text-foreground">{formatDistanceToNow(new Date(mission.updated_at), { addSuffix: true })}</span>
            </div>
            {mission.finished_at && (
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">{t('missions.overview.finishedAt')}</span>
                <span className="text-foreground">{formatDistanceToNow(new Date(mission.finished_at), { addSuffix: true })}</span>
              </div>
            )}
            <div className="flex items-center justify-between">
              <span className="text-muted-foreground">{t('missions.overview.category')}</span>
              <span className="text-foreground">{mission.category || t('missions.uncategorized')}</span>
            </div>
            <div className="flex items-start justify-between gap-2">
              <span className="shrink-0 text-muted-foreground">{t('missions.overview.tags')}</span>
              <div className="flex flex-wrap justify-end gap-1">
                {mission.tags.length > 0 ? (
                  mission.tags.map((tag) => <Badge key={tag} tone="info">#{tag}</Badge>)
                ) : (
                  <span className="text-foreground">{t('missions.overview.noTags')}</span>
                )}
              </div>
            </div>
          </div>
        </Section>

        <CapabilityPool modules={modules} isLoading={modulesLoading} />
        <DirectivesList directives={canvas?.directives ?? []} />
      </div>
    </div>
  );
}

function OverviewMetric({ label, value, icon, tone = 'neutral' }: { label: string; value: number; icon: ReactNode; tone?: StatusTone }) {
  return (
    <div className="rounded-lg border border-border p-3">
      <div className="flex items-center gap-2 text-muted-foreground">
        {icon}
        <span className="text-xs">{label}</span>
      </div>
      <div className="mt-1 flex items-center gap-2">
        <span className="text-xl font-semibold tabular-nums text-foreground">{value}</span>
        {tone !== 'neutral' && <Badge tone={tone} size="sm" dot />}
      </div>
    </div>
  );
}

interface DecisionDrawerProps {
  gate: DecisionGate;
  projectId: string;
  runId: string;
  isOpen: boolean;
  onClose: () => void;
  onAnswer: () => void;
}

function DecisionDrawer({ gate, projectId, runId, isOpen, onClose, onAnswer }: DecisionDrawerProps) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { toast } = useToast();

  const answerMutation = useMutation({
    mutationFn: async (answer: DecisionAnswer) => {
      await api.answerDecisionGate(gate.id, answer);
      if (gate.kind === 'blocking') {
        await api.resumeAuditRun(projectId, runId);
      }
    },
    onSuccess: () => {
      toast({ title: t('decisionGate.answeredSuccessfully') });
      queryClient.invalidateQueries({ queryKey: ['mission'] });
      queryClient.invalidateQueries({ queryKey: ['mission-canvas'] });
      queryClient.invalidateQueries({ queryKey: ['decision-gates'] });
      onAnswer();
    },
    onError: (err) => {
      toast({ title: t('decisionGate.errorSubmitting'), description: getApiErrorMessage(err), variant: 'destructive' });
    }
  });

  const cancelMutation = useMutation({
    mutationFn: async () => {
      await api.cancelDecisionGate(gate.id);
    },
    onSuccess: () => {
      toast({ title: t('decisionGate.cancelledSuccessfully') });
      queryClient.invalidateQueries({ queryKey: ['mission'] });
      queryClient.invalidateQueries({ queryKey: ['mission-canvas'] });
      onClose();
    },
    onError: (err) => {
      toast({ title: t('decisionGate.errorCancelling'), description: getApiErrorMessage(err), variant: 'destructive' });
    }
  });

  if (!isOpen) return null;

  return (
    <Drawer
      open={isOpen}
      onOpenChange={(open) => !open && onClose()}
      icon={<AlertCircle className="h-4 w-4" />}
      title={t('missions.decisionControl')}
      description={t('missions.decisionControlHint')}
      width="lg"
    >
      <DecisionGatePanel
        gate={gate}
        onSubmit={(ans) => answerMutation.mutateAsync(ans)}
        onCancel={async () => { await cancelMutation.mutateAsync(); }}
        isSubmitting={answerMutation.isPending || cancelMutation.isPending}
      />
    </Drawer>
  );
}
