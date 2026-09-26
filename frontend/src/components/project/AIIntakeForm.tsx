import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { useNavigate } from '@tanstack/react-router';
import { Bot, Loader2, Play, Plus, AlertCircle } from 'lucide-react';
import { api, getApiErrorMessage } from '@/lib/api';
import { useToast } from '@/hooks/use-toast';
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from '@/components/ui/card';
import { Button } from '@/components/ui/button';
import { Textarea } from '@/components/ui/textarea';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Badge } from '@/components/ui/badge';
import type { IntakeAnalyzeResponse } from '@/lib/types';
import { formatModuleDomain } from '@/lib/i18n-formatters';

export function AIIntakeForm() {
  const { t } = useTranslation();
  const { toast } = useToast();
  const navigate = useNavigate();
  const queryClient = useQueryClient();

  const [prompt, setPrompt] = useState('');
  const [draft, setDraft] = useState<IntakeAnalyzeResponse | null>(null);
  const [concurrency, setConcurrency] = useState(1);

  const analyzeMutation = useMutation({
    mutationFn: async () => {
      return api.analyzeIntake({ prompt });
    },
    onSuccess: (data) => {
      setDraft(data);
      toast({ title: t('intake.analyzeSuccess', '分析完成') });
    },
    onError: (err) => {
      toast({ title: t('intake.analyzeFailed', '分析失败'), description: getApiErrorMessage(err), variant: 'destructive' });
    },
  });

  const startMutation = useMutation({
    mutationFn: async (startAgent: boolean) => {
      if (!draft) return;
      // 并发探索数经 pipeline.config 透传进 run config（后端 audit_config 原样
      // 搬迁，branch_concurrency_limit 读取）——与 Worker 池单 profile 的
      // max_concurrency 是两层，互不冲突。
      const plan = {
        ...draft.plan,
        pipeline: {
          ...draft.plan.pipeline,
          config: { ...draft.plan.pipeline.config, max_concurrent_branches: concurrency },
        },
      };
      return api.startIntake({
        plan,
        artifact_record_ids: draft.plan.artifact_record_ids ?? [],
        start_pipeline: startAgent,
        start_audit: startAgent,
      });
    },
    onSuccess: (response) => {
      queryClient.invalidateQueries({ queryKey: ['missions'] });
      queryClient.invalidateQueries({ queryKey: ['projects'] });
      setDraft(null);
      setPrompt('');
      if (response?.mission) {
        toast({ title: t('missions.created'), description: response.mission.user_goal });
        navigate({ to: '/missions/$missionId', params: { missionId: response.mission.id } });
      } else {
        // 无 mission 的极端情况：回任务列表（资产空间 UI 已删除）。
        toast({ title: t('intake.quickTodoCreated') });
        navigate({ to: '/missions' });
      }
    },
    onError: (err) => {
      toast({ title: t('missions.createFailed'), description: getApiErrorMessage(err), variant: 'destructive' });
    },
  });

  const updateDraft = (updater: (prev: IntakeAnalyzeResponse) => IntakeAnalyzeResponse) => {
    if (draft) setDraft(updater(draft));
  };

  return (
    <Card className="w-full border-0 shadow-none">
      <CardHeader className="border-b border-border bg-muted/30 pb-4">
        <CardTitle className="flex items-center gap-2 text-xl font-semibold text-foreground">
          <Bot className="w-5 h-5 text-primary" />
          {t('intake.title')}
        </CardTitle>
        <CardDescription className="mt-1 max-w-2xl text-sm text-muted-foreground">
          {t('intake.description')}
        </CardDescription>
      </CardHeader>
      <CardContent className="p-5 space-y-4">
        {!draft ? (
          <div className="space-y-4">
            <Textarea
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              placeholder={t('intake.promptPlaceholder')}
              className="min-h-[120px] resize-none text-base p-4 focus-visible:ring-primary/20"
              autoComplete="off"
            />
            <p className="text-xs text-muted-foreground">
              {t('intake.rawInputHint')}
            </p>
            <Button
              size="lg"
              className="w-full sm:w-auto"
              disabled={!prompt.trim() || analyzeMutation.isPending}
              onClick={() => analyzeMutation.mutate()}
            >
              {analyzeMutation.isPending ? <Loader2 className="w-5 h-5 mr-2 animate-spin" /> : <Bot className="w-5 h-5 mr-2" />}
              {t('intake.analyzeButton')}
            </Button>
          </div>
        ) : (
          <div className="space-y-6 animate-in fade-in slide-in-from-bottom-2">
            <div className="flex items-center justify-between">
              <h3 className="font-semibold text-foreground">{t('intake.draftTitle')}</h3>
              <div className="flex gap-2">
                {draft.used_fallback && (
                  <Badge variant="secondary" className="bg-orange-50 text-orange-700 hover:bg-orange-50">
                    {t('intake.fallbackUsed', 'Deterministic Fallback')}
                  </Badge>
                )}
                <Badge variant="outline" className="border-info-border bg-info-soft text-info-foreground">
                  {t('common.confidence')}: {Math.round(draft.plan.confidence * 100)}%
                </Badge>
                <Badge variant="outline" className="border-border bg-muted text-foreground">
                  {t(`project.auditDomains.${draft.plan.project.audit_domain}`, { defaultValue: draft.plan.project.audit_domain })}
                </Badge>
              </div>
            </div>


            {draft.used_fallback && draft.fallback_reason && (
              <div className="flex gap-2 rounded-md border border-warning-border bg-warning-soft p-3 text-sm text-warning-foreground">
                <AlertCircle className="w-4 h-4 mt-0.5 shrink-0" />
                <span>{draft.fallback_reason}</span>
              </div>
            )}

            {draft.plan.rationale && (
              <div className="rounded-md border border-border bg-muted/40 p-3 text-sm italic text-muted-foreground">
                {draft.plan.rationale}
              </div>
            )}

            <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
              <div className="space-y-2">
                <Label>{t('missions.userGoal')}</Label>
                <Input
                  value={draft.plan.project.goal}
                  onChange={(e) => updateDraft((prev) => ({ ...prev, plan: { ...prev.plan, project: { ...prev.plan.project, goal: e.target.value } } }))}
                  autoComplete="off"
                />
              </div>
              <div className="space-y-2 md:col-span-2">
                <Label>{t('missions.targetValue')}</Label>
                <Input
                  value={Object.entries(draft.plan.project.target).map(([k,v]) => `${k}:${v}`).join(', ')}
                  onChange={(e) => {
                    const parts = e.target.value.split(':');
                    const key = parts[0]?.trim() || 'url';
                    const val = parts.slice(1).join(':')?.trim() || '';
                    updateDraft((prev) => ({ ...prev, plan: { ...prev.plan, project: { ...prev.plan.project, target: { [key]: val } } } }));
                  }}
                  autoComplete="off"
                />
              </div>
              <div className="space-y-2 md:col-span-2">
                <Label>{t('missions.constraints')}</Label>
                <Textarea
                  value={draft.mission_draft?.constraints?.join('\n') || ''}
                  onChange={(e) => updateDraft((prev) => ({ ...prev, mission_draft: { ...prev.mission_draft, constraints: e.target.value.split('\n') } }))}
                  className="min-h-[60px]"
                  autoComplete="off"
                />
              </div>
              <div className="space-y-2 md:col-span-2">
                <Label>{t('missions.successCriteria')}</Label>
                <Textarea
                  value={draft.mission_draft?.success_criteria?.join('\n') || ''}
                  onChange={(e) => updateDraft((prev) => ({ ...prev, mission_draft: { ...prev.mission_draft, success_criteria: e.target.value.split('\n') } }))}
                  className="min-h-[60px]"
                  autoComplete="off"
                />
              </div>
            </div>

            <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
              <div className="space-y-2">
                <Label className="label-spec">{t('intake.recommendedDomains')}</Label>
                <div className="flex flex-wrap gap-2">
                  {draft.plan.pipeline.audit_domains.map(domain => (
                    <Badge key={domain} variant="secondary" className="font-normal">
                      {formatModuleDomain(t, domain)}
                    </Badge>
                  ))}
                  {draft.plan.pipeline.audit_domains.length === 0 && <span className="text-sm text-muted-foreground">-</span>}
                </div>
              </div>
              <div className="space-y-2">
                <Label className="label-spec">{t('intake.recommendedIntents')}</Label>
                <div className="flex flex-col gap-1">
                  {draft.plan.recommended_intents.map((intent, i) => (
                    <div key={i} className="flex items-start gap-2 text-sm text-foreground">
                      <div className="mt-1.5 h-1.5 w-1.5 shrink-0 rounded-full bg-neutral" />
                      <span>{intent}</span>
                    </div>
                  ))}
                  {draft.plan.recommended_intents.length === 0 && <span className="text-sm text-muted-foreground">-</span>}
                </div>
              </div>
            </div>
            
            {/* 并发探索数：创建即启动时生效（经 pipeline.config 透传进 run config）。 */}
            <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
              <Label className="whitespace-nowrap text-sm font-medium text-foreground">
                {t('missions.concurrency')}
              </Label>
              <input
                type="range"
                min={1}
                max={8}
                step={1}
                value={concurrency}
                onChange={(event) => setConcurrency(Number(event.target.value))}
                className="h-1.5 w-40 cursor-pointer accent-primary"
                aria-label={t('missions.concurrency')}
              />
              <span className="w-6 text-right text-sm font-semibold tabular-nums text-foreground">{concurrency}</span>
              <span className="min-w-0 flex-1 text-xs text-muted-foreground">{t('missions.concurrencyHint')}</span>
            </div>

            <div className="flex flex-col gap-3 border-t border-border pt-4 sm:flex-row">
              <Button
                variant="outline"
                className="flex-1"
                disabled={startMutation.isPending}
                onClick={() => setDraft(null)}
              >
                {t('common.cancel')}
              </Button>
              <Button
                variant="secondary"
                className="flex-1 border-border bg-card"
                disabled={startMutation.isPending}
                onClick={() => startMutation.mutate(false)}
              >
                {startMutation.isPending ? <Loader2 className="w-4 h-4 mr-2 animate-spin" /> : <Plus className="w-4 h-4 mr-2" />}
                {t('intake.createOnly')}
              </Button>
              <Button
                className="flex-1"
                disabled={startMutation.isPending}
                onClick={() => startMutation.mutate(true)}
              >
                {startMutation.isPending ? <Loader2 className="w-4 h-4 mr-2 animate-spin" /> : <Play className="w-4 h-4 mr-2" />}
                {t('intake.createAndStart')}
              </Button>
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
