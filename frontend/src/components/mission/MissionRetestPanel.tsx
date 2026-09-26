/**
 * Mission Retest Panel — 复测页。
 *
 * 复测由后端 `agent/retester.go` 驱动：`POST /findings/{id}/retest`
 * 拉起一个独立复测 Agent，读原证据与测试约束后给出 reproduced / fixed /
 * inconclusive 结论，并自动把「已修复」写回漏洞状态。Lynceus 现在也对齐了：
 *
 * - 「发起复测」打 `POST /missions/{id}/findings/{fid}/retests`，后端建
 *   `finding_retests` 记录 → 拉起同一个只读顾问 worker（blackboard_read /
 *   blackboard_append / knowledge_search 授权，用完即回收）→ 解析结论 →
 *   落库。**不再复用 `advise` 假装是复测，也不再存 localStorage**；
 * - 同一 Finding 同时只能有一条未收口复测：重复点击后端返回已有记录
 *   （HTTP 200），不会并发拉起第二个 worker；
 * - `verdict = fixed` 且复测正常收口时，后端会把 Finding 推到 `fixed`
 *   （`status_after_retest`）；中断/失败优先于已暂存的结论，
 *   `reproduced` / `inconclusive` 一律不改漏洞状态——重新打开漏洞是人工决定。
 *
 * 解析不出结论时后端不会猜：记录落成 `failed` 并保留原始评估文本，界面上
 * 显示原文供人工回看。
 */
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { ChevronLeft, ChevronRight, RotateCcw, ShieldAlert } from 'lucide-react';

import { api, getApiErrorMessage } from '@/lib/api';
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  EmptyState,
  ErrorState,
  FindingStatusBadge,
  LoadingState,
  SeverityBadge,
  Textarea,
} from '@/ui/untitled';
import { formatSeverity } from '@/lib/i18n-formatters';
import { useToast } from '@/hooks/use-toast';
import type { ApiFinding, FindingRetest, RetestStatus } from '@/lib/types';

const PAGE_SIZE = 20;

/* ───────────────────────── 助手 ───────────────────────── */

function findingLabel(finding: ApiFinding): string {
  const title = finding.title?.trim();
  if (title) return title;
  const cwe = Array.isArray(finding.cwe) ? finding.cwe[0] : finding.cwe;
  return cwe ? String(cwe) : finding.id;
}

/** 记录状态 → 徽章色调。 */
function statusTone(status: RetestStatus): 'info' | 'success' | 'danger' | 'warning' | 'neutral' {
  switch (status) {
    case 'pending':
    case 'running':
      return 'info';
    case 'completed':
      return 'success';
    case 'failed':
      return 'danger';
    case 'stopped':
      return 'warning';
  }
}

function statusLabelKey(status: RetestStatus, verdict: string): string {
  if (status === 'completed' && verdict) return `missions.retest.verdicts.${verdict}`;
  return `missions.retest.statuses.${status}`;
}

function verdictTone(verdict: string): 'danger' | 'success' | 'warning' {
  if (verdict === 'reproduced') return 'danger';
  if (verdict === 'fixed') return 'success';
  return 'warning';
}

/* ───────────────────────── 组件 ───────────────────────── */

export function MissionRetestPanel({
  missionId,
  findings,
  onRetestFinished,
}: {
  missionId: string;
  findings: ApiFinding[];
  /** 复测收口后回调（父组件用它刷新画布：fixed 结论可能已改写漏洞状态）。 */
  onRetestFinished?: () => void;
}) {
  const { t } = useTranslation();
  const { toast } = useToast();
  const queryClient = useQueryClient();
  const [page, setPage] = useState(1);
  const [selectedId, setSelectedId] = useState('');
  const [dialogOpen, setDialogOpen] = useState(false);
  const [notes, setNotes] = useState('');
  const [starting, setStarting] = useState(false);

  const activeQuery = useQuery({
    queryKey: ['mission-retests-active', missionId],
    queryFn: () => api.listActiveMissionRetests(missionId),
    refetchInterval: 15_000,
  });

  useEffect(() => {
    if (!starting) return;
    // 复测是同步收口的，正常情况下不会停在这里；兜底解锁避免按钮卡死。
    const timer = setTimeout(() => setStarting(false), 120_000);
    return () => clearTimeout(timer);
  }, [starting]);

  const pageCount = Math.max(1, Math.ceil(findings.length / PAGE_SIZE));
  const safePage = Math.min(page, pageCount);
  const pageFindings = findings.slice((safePage - 1) * PAGE_SIZE, safePage * PAGE_SIZE);
  const selected =
    pageFindings.find((finding) => finding.id === selectedId) ?? pageFindings[0] ?? null;
  const selectedFindingId = selected?.id ?? '';

  const recordsQuery = useQuery({
    queryKey: ['finding-retests', missionId, selectedFindingId],
    queryFn: () => api.listFindingRetests(missionId, selectedFindingId),
    enabled: selectedFindingId !== '',
  });

  const activeByFinding = new Map<string, FindingRetest>();
  for (const record of activeQuery.data ?? []) {
    activeByFinding.set(record.finding_id, record);
  }

  if (findings.length === 0) {
    return (
      <EmptyState
        variant="card"
        icon={<ShieldAlert className="h-6 w-6" />}
        title={t('missions.retest.emptyTitle')}
        description={t('missions.retest.emptyDescription')}
      />
    );
  }

  const runRetest = async () => {
    if (!selected) return;
    setStarting(true);
    try {
      const record = await api.startFindingRetest(missionId, selected.id, notes.trim());
      await queryClient.invalidateQueries({
        queryKey: ['finding-retests', missionId, selected.id],
      });
      await queryClient.invalidateQueries({ queryKey: ['mission-retests-active', missionId] });
      if (record.status === 'running' || record.status === 'pending') {
        // 后端只在校验通过后才可能返回未收口记录（幂等重放）。
        toast({ title: t('missions.retest.statusRunning') });
      } else if (record.status === 'completed') {
        toast({ title: t('missions.retest.completed') });
      } else {
        toast({
          title: t('missions.retest.failed'),
          description: record.error || getApiErrorMessage(new Error('retest failed')),
          variant: 'destructive',
        });
      }
      setDialogOpen(false);
      setNotes('');
      // 结论可能已经把漏洞推到 fixed，画布必须重新拉。
      onRetestFinished?.();
    } catch (error) {
      toast({
        title: t('missions.retest.failed'),
        description: getApiErrorMessage(error),
        variant: 'destructive',
      });
    } finally {
      setStarting(false);
    }
  };

  const records = recordsQuery.data ?? [];

  return (
    <div className="flex flex-col gap-4">
      <div className="grid items-start gap-4 lg:grid-cols-[minmax(16rem,22rem)_minmax(0,1fr)]">
        <Card className="min-w-0 shadow-xs">
          <CardHeader>
            <CardTitle>
              {t('missions.retest.selectTitle')}
              {findings.length > 0 ? ` · ${findings.length}` : ''}
            </CardTitle>
            <CardDescription>{t('missions.retest.selectDescription')}</CardDescription>
          </CardHeader>
          <CardContent className="flex max-h-[32rem] flex-col gap-1 overflow-y-auto">
            {pageFindings.map((finding) => (
              <button
                key={finding.id}
                type="button"
                aria-pressed={selected?.id === finding.id}
                onClick={() => setSelectedId(finding.id)}
                className={`flex w-full shrink-0 flex-col items-start gap-2 whitespace-normal rounded-md px-3 py-2.5 text-left transition-colors ${
                  selected?.id === finding.id ? 'bg-muted' : 'hover:bg-muted/50'
                }`}
              >
                <span className="line-clamp-2 break-words text-sm text-foreground">{findingLabel(finding)}</span>
                <span className="flex flex-wrap items-center gap-2">
                  <SeverityBadge severity={finding.severity} label={formatSeverity(t, finding.severity)} />
                  <FindingStatusBadge status={finding.status} />
                  {activeByFinding.has(finding.id) && (
                    <Badge tone="info" variant="soft" pulse dot>
                      {t('missions.retest.statusRunning')}
                    </Badge>
                  )}
                </span>
              </button>
            ))}
          </CardContent>
          {findings.length > PAGE_SIZE && (
            <CardFooter className="justify-between gap-2">
              <Button
                variant="outline"
                size="icon"
                className="h-7 w-7"
                disabled={safePage <= 1}
                onClick={() => setPage(safePage - 1)}
                aria-label={t('missions.retest.prev')}
              >
                <ChevronLeft className="h-3.5 w-3.5" />
              </Button>
              <span className="text-xs text-muted-foreground">
                {safePage} / {pageCount}
              </span>
              <Button
                variant="outline"
                size="icon"
                className="h-7 w-7"
                disabled={safePage >= pageCount}
                onClick={() => setPage(safePage + 1)}
                aria-label={t('missions.retest.next')}
              >
                <ChevronRight className="h-3.5 w-3.5" />
              </Button>
            </CardFooter>
          )}
        </Card>

        {selected && (
          <div className="flex min-w-0 flex-col gap-4">
            <div className="flex flex-col gap-2">
              <h2 className="min-w-0 break-words text-sm font-medium text-foreground">
                {findingLabel(selected)}
              </h2>
              <p className="line-clamp-3 break-words text-xs text-muted-foreground">
                {selected.description?.trim() || selected.id}
              </p>
            </div>

            <Card className="shadow-xs">
              <CardHeader className="flex-row flex-wrap items-start justify-between gap-3">
                <div className="flex flex-col gap-1.5">
                  <CardTitle>{t('missions.retest.panelTitle')}</CardTitle>
                  <CardDescription>{t('missions.retest.panelDescription')}</CardDescription>
                </div>
                <Button
                  size="sm"
                  onClick={() => setDialogOpen(true)}
                  disabled={starting || activeByFinding.has(selected.id)}
                >
                  <RotateCcw className="h-3.5 w-3.5" />
                  {t('missions.retest.start')}
                </Button>
              </CardHeader>
              <CardContent className="flex flex-col gap-3">
                <p className="rounded-md border border-dashed border-border px-3 py-2 text-xs leading-relaxed text-muted-foreground">
                  {t('missions.retest.scopeNote')}
                </p>

                {recordsQuery.isPending ? (
                  <LoadingState card={false} lines={3} />
                ) : recordsQuery.isError ? (
                  <ErrorState
                    compact
                    title={t('missions.retest.loadFailed')}
                    description={getApiErrorMessage(recordsQuery.error)}
                    retryLabel={t('common.retry')}
                    onRetry={() => void recordsQuery.refetch()}
                  />
                ) : records.length === 0 ? (
                  <EmptyState
                    variant="bare"
                    compact
                    className="py-8"
                    title={t('missions.retest.noRecords')}
                    description={t('missions.retest.noRecordsHint')}
                  />
                ) : (
                  records.map((record) => (
                    <div
                      key={record.id}
                      className="flex min-w-0 flex-col gap-2 rounded-lg border border-border p-3"
                    >
                      <div className="flex flex-wrap items-center gap-2">
                        <Badge
                          tone={
                            record.status === 'completed' && record.verdict
                              ? verdictTone(record.verdict)
                              : statusTone(record.status)
                          }
                          variant="soft"
                          dot
                          pulse={record.status === 'running' || record.status === 'pending'}
                        >
                          {t(statusLabelKey(record.status, record.verdict))}
                        </Badge>
                        <span className="text-xs text-muted-foreground">
                          {new Date(record.created_at).toLocaleString('zh-CN')}
                        </span>
                        {record.model && (
                          <code className="rounded bg-muted px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground">
                            {record.model}
                          </code>
                        )}
                        {record.status === 'completed' && record.verdict === 'fixed' && (
                          <Badge tone="success" variant="outline">
                            {t('missions.retest.statusAutoFixed')}
                          </Badge>
                        )}
                        {/* 上下文来源必须显式：只拿到内联快照的结论置信度不同。 */}
                        {record.context_source === 'inline_snapshot_only' && (
                          <Badge tone="warning" variant="outline">
                            {t('missions.retest.contextInlineOnly')}
                          </Badge>
                        )}
                        {record.context_source === 'readonly_mcp_grant+inline_snapshot' && (
                          <Badge tone="neutral" variant="outline">
                            {t('missions.retest.contextWithGrant')}
                          </Badge>
                        )}
                      </div>

                      {(record.status === 'running' || record.status === 'pending') && (
                        <LoadingState card={false} lines={2} />
                      )}

                      {record.error && (
                        <p className="whitespace-pre-wrap break-words text-xs text-danger">{record.error}</p>
                      )}

                      {record.summary && (
                        <p className="whitespace-pre-wrap break-words text-xs leading-relaxed text-foreground">
                          {record.summary}
                        </p>
                      )}

                      {record.evidence && (
                        <p className="whitespace-pre-wrap break-words text-[11px] leading-relaxed text-muted-foreground">
                          {t('missions.retest.evidenceLabel')}：{record.evidence}
                        </p>
                      )}

                      {/* 解析不出结论时保留原文，供人工回看——不猜结论。 */}
                      {!record.summary && record.assessment && (
                        <p className="whitespace-pre-wrap break-words text-[11px] leading-relaxed text-muted-foreground">
                          {record.assessment}
                        </p>
                      )}

                      {record.notes && (
                        <p className="whitespace-pre-wrap break-words text-[11px] text-muted-foreground">
                          {t('missions.retest.notes')}：{record.notes}
                        </p>
                      )}
                    </div>
                  ))
                )}
              </CardContent>
            </Card>
          </div>
        )}
      </div>

      <Dialog open={dialogOpen} onOpenChange={setDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {t('missions.retest.dialogTitle')} #{selected?.id ?? ''}
            </DialogTitle>
            <DialogDescription className="break-words">
              {selected ? <span className="mb-2 block">{findingLabel(selected)}</span> : null}
              {t('missions.retest.dialogDescription')}
            </DialogDescription>
          </DialogHeader>
          <div className="grid gap-1.5">
            <span className="text-xs font-medium text-muted-foreground">{t('missions.retest.notesLabel')}</span>
            <Textarea
              value={notes}
              maxLength={4000}
              rows={4}
              onChange={(event) => setNotes(event.target.value)}
              placeholder={t('missions.retest.notesPlaceholder')}
            />
            <span className="text-[11px] text-muted-foreground">{t('missions.retest.notesHint')}</span>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDialogOpen(false)}>
              {t('common.cancel')}
            </Button>
            <Button onClick={() => void runRetest()} disabled={starting}>
              <RotateCcw className="h-3.5 w-3.5" />
              {t('missions.retest.start')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
