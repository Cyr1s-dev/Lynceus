/**
 * Mission Findings Board — 发现页。
 *
 * 发现页是一张「严重度 + 漏洞名 + 摘要」的密集列表：行首 severity
 * 徽章，展开看证据/PoC，行尾是状态与时间。Lynceus 这里保持同样的版式，
 * 并补齐了原来缺的东西：
 *
 * - 状态可改：`PATCH /projects/{project_id}/findings/{finding_id}`
 *   ——原来只能渲染只读徽章，现在展开行
 *   里直接改状态/严重度；
 * - 「详情」不跳路由，改为展开行内证据与关联 id（本仓无 finding 详情页）；
 * - 时间列用 `updated_at`（`ApiFinding` 没有 `created_at`）。
 */
import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { ArrowDown, ArrowUp, Check, ChevronRight, Loader2, ShieldAlert } from 'lucide-react';

import {
  Badge,
  Button,
  Card,
  EmptyState,
  ErrorState,
  FindingStatusBadge,
  SeverityBadge,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/ui/untitled';
import { formatSeverity } from '@/lib/i18n-formatters';
import { api } from '@/lib/api';
import type { ApiFinding, FindingStatus, Severity } from '@/lib/types';

type SortDirection = 'asc' | 'desc';

/** 可写回的状态集合（后端 `FindingStatus::ALL` 的 wire 值）。 */
const WRITABLE_STATUSES: FindingStatus[] = [
  'candidate',
  'needs_review',
  'confirmed',
  'false_positive',
  'duplicate',
  'gap',
  'phenomenon',
  'fixed',
];

/** 可写回的严重度集合（后端 `Severity::ALL` 的 wire 值）。 */
const WRITABLE_SEVERITIES: Severity[] = ['info', 'low', 'medium', 'high', 'critical'];

/* ───────────────────────── Helpers ───────────────────────── */

function findingLabel(finding: ApiFinding): string {
  const title = finding.title?.trim();
  if (title) return title;
  const cwe = Array.isArray(finding.cwe) ? finding.cwe[0] : finding.cwe;
  if (cwe) return String(cwe);
  return '未分类';
}

function findingSummary(finding: ApiFinding): string {
  const parts: string[] = [];
  if (finding.description?.trim()) parts.push(finding.description.trim());
  if (finding.rule_id) parts.push(`规则 ${finding.rule_id}`);
  const cwe = Array.isArray(finding.cwe) ? finding.cwe.join(', ') : finding.cwe;
  if (cwe) parts.push(`CWE ${cwe}`);
  if (finding.source_label || finding.sink_label) {
    parts.push(`${finding.source_label ?? '?'} → ${finding.sink_label ?? '?'}`);
  }
  if (finding.confidence != null) parts.push(`置信度 ${finding.confidence}`);
  return parts.join(' · ');
}

function timestampOf(finding: ApiFinding): number {
  const parsed = Date.parse(finding.updated_at);
  return Number.isNaN(parsed) ? 0 : parsed;
}

/* ───────────────────────── Triage ───────────────────────── */

function TriageEditor({
  finding,
  projectId,
  onSaved,
}: {
  finding: ApiFinding;
  projectId: string;
  onSaved: (finding: ApiFinding) => void;
}) {
  const { t } = useTranslation();
  const [status, setStatus] = useState<FindingStatus>(finding.status);
  const [severity, setSeverity] = useState<Severity>(finding.severity);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const dirty = status !== finding.status || severity !== finding.severity;
  const statusKey = `missions.findingsBoard.status.${status}`;
  const severityKey = `missions.findingsBoard.severity.${severity}`;

  const save = async () => {
    // 只发改动过的字段——后端是指针语义，空 body 会被 422 拒掉。
    const body: { status?: FindingStatus; severity?: Severity } = {};
    if (status !== finding.status) body.status = status;
    if (severity !== finding.severity) body.severity = severity;
    if (Object.keys(body).length === 0) return;
    setSaving(true);
    setError(null);
    try {
      const updated = await api.triageFinding(projectId, finding.id, body);
      onSaved(updated);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="mt-3 rounded-md border border-border bg-background p-3">
      <div className="mb-2 text-xs font-medium text-muted-foreground">
        {t('missions.findingsBoard.triageTitle')}
      </div>
      <div className="flex flex-wrap items-end gap-3">
        <label className="flex flex-col gap-1">
          <span className="text-[11px] text-muted-foreground">
            {t('missions.findingsBoard.triageStatus')}
          </span>
          <Select value={status} onValueChange={(value) => setStatus(value as FindingStatus)}>
            <SelectTrigger className="w-44">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {WRITABLE_STATUSES.map((item) => (
                <SelectItem key={item} value={item}>
                  {t(`missions.findingsBoard.status.${item}`)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </label>
        <label className="flex flex-col gap-1">
          <span className="text-[11px] text-muted-foreground">
            {t('missions.findingsBoard.triageSeverity')}
          </span>
          <Select value={severity} onValueChange={(value) => setSeverity(value as Severity)}>
            <SelectTrigger className="w-32">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {WRITABLE_SEVERITIES.map((item) => (
                <SelectItem key={item} value={item}>
                  {t(`missions.findingsBoard.severity.${item}`)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </label>
        <Button size="sm" onClick={save} disabled={!dirty || saving}>
          {saving ? (
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
          ) : (
            <Check className="h-3.5 w-3.5" />
          )}
          {t('missions.findingsBoard.triageSave')}
        </Button>
        {dirty && !saving && (
          <span className="text-[11px] text-muted-foreground">
            {t('missions.findingsBoard.triagePending', {
              from: t(statusKey),
              to: t(severityKey),
            })}
          </span>
        )}
      </div>
      {error && (
        <div className="mt-2">
          <ErrorState
            compact
            title={t('missions.findingsBoard.triageFailed')}
            description={error}
            retryLabel={t('common.retry')}
            onRetry={() => void save()}
          />
        </div>
      )}
    </div>
  );
}

/* ───────────────────────── Row ───────────────────────── */

function FindingRow({
  finding,
  projectId,
  open,
  onToggle,
  onSaved,
}: {
  finding: ApiFinding;
  projectId: string;
  open: boolean;
  onToggle: () => void;
  onSaved: (finding: ApiFinding) => void;
}) {
  const { t } = useTranslation();
  const ts = timestampOf(finding);

  return (
    <div className="border-b border-border last:border-b-0">
      <div className="flex w-full items-center gap-3 px-4 py-3 text-sm transition-colors hover:bg-muted/30">
        <button
          type="button"
          onClick={onToggle}
          aria-expanded={open}
          className="flex min-w-0 flex-1 items-center gap-3 text-left"
        >
          <ChevronRight
            className={`h-4 w-4 shrink-0 text-muted-foreground transition-transform ${open ? 'rotate-90' : ''}`}
          />
          <SeverityBadge severity={finding.severity} label={formatSeverity(t, finding.severity)} />
          <div className="flex min-w-0 flex-1 flex-col gap-1">
            <span className="truncate font-medium text-foreground">{findingLabel(finding)}</span>
            <span className="truncate text-xs text-muted-foreground">
              {findingSummary(finding) || finding.id}
            </span>
          </div>
        </button>
        <Badge tone="neutral" variant="soft">
          {t('missions.findingsBoard.evidenceCount', { count: finding.evidence_ids?.length ?? 0 })}
        </Badge>
        {finding.produced_by_task_id && (
          <code className="hidden max-w-[10rem] shrink-0 truncate rounded bg-muted px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground lg:block">
            {finding.produced_by_task_id}
          </code>
        )}
        <FindingStatusBadge status={finding.status} />
        <span className="hidden shrink-0 text-xs tabular-nums text-muted-foreground md:block">
          {ts === 0 ? finding.updated_at : new Date(ts).toLocaleString('zh-CN')}
        </span>
      </div>

      {open && (
        <div className="bg-muted/30 px-4 pb-4 pl-11">
          <div className="mb-1 text-xs font-medium text-muted-foreground">
            {t('missions.findingsBoard.description')}
          </div>
          <pre className="overflow-auto whitespace-pre-wrap break-words rounded-md border border-border bg-background p-3 font-mono text-xs text-foreground">
            {finding.description?.trim() || t('missions.findingsBoard.noDescription')}
          </pre>
          <TriageEditor
            finding={finding}
            projectId={projectId}
            onSaved={onSaved}
          />
          <div className="mt-3 grid grid-cols-1 gap-3 sm:grid-cols-2">
            <div>
              <div className="mb-1 text-xs font-medium text-muted-foreground">
                {t('missions.findingsBoard.evidenceIds')} · {finding.evidence_ids?.length ?? 0}
              </div>
              <div className="space-y-1">
                {(finding.evidence_ids ?? []).map((id) => (
                  <code
                    key={id}
                    className="block truncate rounded bg-muted px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground"
                  >
                    {id}
                  </code>
                ))}
                {(finding.evidence_ids ?? []).length === 0 && (
                  <span className="text-xs text-muted-foreground">{'\u2014'}</span>
                )}
              </div>
            </div>
            <div>
              <div className="mb-1 text-xs font-medium text-muted-foreground">
                {t('missions.findingsBoard.relatedFacts')} · {finding.related_fact_ids?.length ?? 0}
              </div>
              <div className="space-y-1">
                {(finding.related_fact_ids ?? []).map((id) => (
                  <code
                    key={id}
                    className="block truncate rounded bg-muted px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground"
                  >
                    {id}
                  </code>
                ))}
                {(finding.related_fact_ids ?? []).length === 0 && (
                  <span className="text-xs text-muted-foreground">{'\u2014'}</span>
                )}
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/* ───────────────────────── Board ───────────────────────── */

export function MissionFindingsBoard({
  findings,
  projectId,
  onTriage,
}: {
  findings: ApiFinding[];
  /** 项目 id —— triage 端点挂在 project 作用域下。 */
  projectId: string;
  /** triage 成功后的回调（父组件用它刷新画布数据）。 */
  onTriage?: (finding: ApiFinding) => void;
}) {
  const { t } = useTranslation();
  const [openId, setOpenId] = useState<string | null>(null);
  const [direction, setDirection] = useState<SortDirection>('desc');

  const items = useMemo(() => {
    const sorted = [...findings].sort((a, b) => timestampOf(a) - timestampOf(b));
    return direction === 'desc' ? sorted.reverse() : sorted;
  }, [findings, direction]);

  if (findings.length === 0) {
    return (
      <EmptyState
        variant="card"
        icon={<ShieldAlert className="h-6 w-6" />}
        title={t('missions.findingsBoard.emptyTitle')}
        description={t('missions.findingsBoard.emptyDescription')}
      />
    );
  }

  return (
    <Card className="overflow-hidden shadow-xs">
      <div className="flex items-center border-b border-border px-4 py-2 text-xs text-muted-foreground">
        <span className="min-w-0 flex-1">{t('missions.findingsBoard.columnFinding')}</span>
        <button
          type="button"
          className="inline-flex items-center gap-1 outline-none focus-visible:underline"
          aria-label={`${t('missions.findingsBoard.columnTime')}${
            direction === 'asc' ? t('missions.findingsBoard.asc') : t('missions.findingsBoard.desc')
          }`}
          onClick={() => setDirection((current) => (current === 'asc' ? 'desc' : 'asc'))}
        >
          <span>{t('missions.findingsBoard.columnTime')}</span>
          {direction === 'asc' ? <ArrowUp className="h-3.5 w-3.5" /> : <ArrowDown className="h-3.5 w-3.5" />}
        </button>
      </div>

      <div>
        {items.map((finding) => (
          <FindingRow
            key={finding.id}
            finding={finding}
            projectId={projectId}
            open={openId === finding.id}
            onToggle={() => setOpenId((current) => (current === finding.id ? null : finding.id))}
            onSaved={(updated) => onTriage?.(updated)}
          />
        ))}
      </div>

      <div className="border-t border-border px-4 py-2 text-xs text-muted-foreground">
        {t('missions.findingsBoard.total', { count: findings.length })}
      </div>
    </Card>
  );
}
