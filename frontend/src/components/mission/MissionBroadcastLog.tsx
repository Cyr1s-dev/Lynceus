/**
 * Mission Broadcast Log — 日志页（播报板）。
 *
 * 播报板把探索链路节点渲染成「时间列 + 类型圆点 + 内容列」的流水
 * 时间轴：按天分组、类型筛选、关键词搜索、最新在前/最早在前、离开直播位后
 * 显示未读计数。Lynceus 的日志源是 `SwarmOperationRecord`（Agent 蜂群共用
 * 的无损脱敏 journal，`GET /missions/{id}/operation-log`），比传统探索
 * 节点更细，因此：
 *
 * - 时间轴结构与交互 1:1 复刻（含「N 条新播报 · 回到最新」）；
 * - 类型圆点由 `operation_type` 映射到六条播报泳道 + 「系统」，角色徽章
 *   （mgr/obs/adv/rev/sol）作为泳道之外的第二维度保留；
 * - 展开区直接展示脱敏后的 `payload` JSON——这就是播报里 payload
 *   区块的对应物，不摘要、不裁剪。
 */
import { useEffect, useMemo, useRef, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import {
  ArrowDown,
  ArrowUp,
  ArrowUpToLine,
  Bug,
  ChevronLeft,
  ChevronRight,
  Compass,
  Cpu,
  Flag,
  FlaskConical,
  type LucideIcon,
  Lightbulb,
  Pause,
  Play,
  Search,
  Target,
} from 'lucide-react';

import { api, getApiErrorMessage } from '@/lib/api';
import type { SwarmOperationRecord } from '@/lib/types';
import {
  Badge,
  Button,
  Card,
  EmptyState,
  ErrorState,
  Input,
  LoadingState,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/ui/untitled';
import { cn } from '@/lib/utils';

/* ───────────────────────── Constants ───────────────────────── */

const PAGE_SIZES = [20, 50, 100];
const POLL_MS = 8000;
const FETCH_LIMIT = 1000;
const DASH = '\u2014';

type LaneKey = 'begin' | 'goal' | 'intent' | 'fact' | 'hint' | 'finding' | 'system';

interface LaneMeta {
  key: LaneKey;
  labelKey: string;
  icon: LucideIcon;
  /** 时间轴圆点配色。 */
  dot: string;
  /** 类型 chip 配色。 */
  chip: string;
  /** `operation_type` 前缀/精确匹配表。 */
  matches: (operationType: string) => boolean;
}

const exact = (...values: string[]) => (type: string) => values.includes(type);
const prefix = (...values: string[]) => (type: string) => values.some((value) => type.startsWith(value));

const LANES: LaneMeta[] = [
  {
    key: 'begin',
    labelKey: 'begin',
    icon: Flag,
    dot: 'bg-slate-500',
    chip: 'bg-slate-500/15 text-slate-600 dark:text-slate-300',
    matches: exact('run_started', 'run_resumed'),
  },
  {
    key: 'goal',
    labelKey: 'goal',
    icon: Target,
    dot: 'bg-emerald-500',
    chip: 'bg-emerald-500/15 text-emerald-600 dark:text-emerald-400',
    matches: prefix('goal', 'objective'),
  },
  {
    key: 'intent',
    labelKey: 'intent',
    icon: Compass,
    dot: 'bg-blue-500',
    chip: 'bg-blue-500/15 text-blue-600 dark:text-blue-400',
    matches: prefix('task_', 'intent', 'solver_'),
  },
  {
    key: 'fact',
    labelKey: 'fact',
    icon: FlaskConical,
    dot: 'bg-amber-500',
    chip: 'bg-amber-500/15 text-amber-600 dark:text-amber-400',
    matches: prefix('observation.', 'evidence_', 'asset_', 'tool_invocation_completed', 'tool_invocation_failed'),
  },
  {
    key: 'hint',
    labelKey: 'hint',
    icon: Lightbulb,
    dot: 'bg-violet-500',
    chip: 'bg-violet-500/15 text-violet-600 dark:text-violet-400',
    matches: prefix('hint', 'advisor', 'user_note'),
  },
  {
    key: 'finding',
    labelKey: 'finding',
    icon: Bug,
    dot: 'bg-rose-500',
    chip: 'bg-rose-500/15 text-rose-600 dark:text-rose-400',
    matches: exact('finding_added'),
  },
  {
    key: 'system',
    labelKey: 'system',
    icon: Cpu,
    dot: 'bg-teal-500',
    chip: 'bg-teal-500/15 text-teal-600 dark:text-teal-400',
    matches: () => true,
  },
];

const LANE_BY_KEY = new Map(LANES.map((lane) => [lane.key, lane]));

/** 「新播报」空集合：同一个常量引用，避免 useMemo 每次返回新 Set 触发下游重渲染。 */
const EMPTY_IDS: ReadonlySet<string> = new Set();

/** `operation_type` → 播报泳道（顺序匹配，`system` 兜底）。 */
function laneOf(operationType: string): LaneMeta {
  const normalized = operationType.toLowerCase();
  for (const lane of LANES) {
    if (lane.key === 'system') continue;
    if (lane.matches(normalized)) return lane;
  }
  return LANE_BY_KEY.get('system') as LaneMeta;
}

const dayFormatter = new Intl.DateTimeFormat('zh-CN', {
  month: 'long',
  day: 'numeric',
  weekday: 'short',
});
const clockFormatter = new Intl.DateTimeFormat('zh-CN', {
  hour: '2-digit',
  minute: '2-digit',
  second: '2-digit',
  hour12: false,
});

function relativeTime(ts: number, now: number): string {
  if (!now || !ts) return '';
  const seconds = Math.max(0, (now - ts) / 1000);
  if (seconds < 60) return '刚刚';
  if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟前`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)} 小时前`;
  return `${Math.floor(seconds / 86400)} 天前`;
}

function prettyPayload(payload: Record<string, unknown>): string {
  if (Object.keys(payload).length === 0) return '';
  try {
    return JSON.stringify(payload, null, 2);
  } catch {
    return '';
  }
}

/* ───────────────────────── Row ───────────────────────── */

function BroadcastRow({
  record,
  now,
  fresh,
  open,
  onToggle,
}: {
  record: SwarmOperationRecord;
  now: number;
  fresh: boolean;
  open: boolean;
  onToggle: () => void;
}) {
  const { t } = useTranslation();
  const lane = laneOf(record.operation_type);
  const Icon = lane.icon;
  const ts = Date.parse(record.created_at);
  const validTs = !Number.isNaN(ts);
  const payload = prettyPayload(record.payload);

  return (
    <div className={cn('relative grid grid-cols-[4.5rem_1.75rem_1fr] gap-x-2', fresh && 'bg-primary/5')}>
      {/* 时间列 */}
      <div className="py-3 text-right text-xs tabular-nums text-muted-foreground">
        <div>{validTs ? clockFormatter.format(ts) : '--:--:--'}</div>
        <div className="text-[11px] opacity-70">{relativeTime(ts, now)}</div>
      </div>

      {/* 时间轴：竖线 + 泳道圆点 */}
      <div className="relative flex justify-center">
        <span className="absolute inset-y-0 w-px bg-border" />
        <span
          className={cn(
            'relative mt-3.5 flex h-6 w-6 items-center justify-center rounded-full text-white ring-4 ring-background',
            lane.dot,
          )}
        >
          <Icon className="h-3.5 w-3.5" />
        </span>
      </div>

      {/* 内容列 */}
      <div className="min-w-0 border-b border-border py-3 pr-1 last:border-b-0">
        <button
          type="button"
          onClick={onToggle}
          aria-expanded={open}
          className="flex w-full min-w-0 items-start gap-2 text-left"
        >
          <ChevronRight
            className={cn(
              'mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform',
              open && 'rotate-90',
            )}
          />
          <div className="min-w-0 flex-1">
            <div className="flex min-w-0 flex-wrap items-center gap-1.5">
              <span className={cn('rounded px-1.5 py-0.5 text-xs font-medium whitespace-nowrap', lane.chip)}>
                {t(`missions.broadcast.lanes.${lane.labelKey}`)}
              </span>
              <code className="rounded bg-muted px-1.5 py-0.5 font-mono text-xs text-muted-foreground">
                {record.operation_type}
              </code>
              {record.role && (
                <Badge tone="neutral" variant="soft">
                  {t(`missions.broadcast.roles.${record.role}`)}
                </Badge>
              )}
              {fresh && (
                <span className="rounded bg-primary px-1.5 py-0.5 text-[10px] font-semibold text-primary-foreground">
                  {t('missions.broadcast.fresh')}
                </span>
              )}
              <span className="ml-auto shrink-0 text-xs text-muted-foreground">
                {record.actor_label || DASH}
              </span>
            </div>
            <p className={cn('mt-1 text-sm text-foreground', !open && 'line-clamp-2')}>
              {record.entry || `#${record.sequence}`}
            </p>
          </div>
        </button>

        {open && (
          <div className="mt-2 ml-5 flex flex-col gap-3 rounded-md border border-border bg-muted/30 p-3">
            <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
              <span>
                {t('missions.broadcast.sequence')} <code className="font-mono">#{record.sequence}</code>
              </span>
              {record.worker_id && (
                <span>
                  {t('missions.broadcast.worker')} <code className="font-mono">{record.worker_id}</code>
                </span>
              )}
              {record.model && (
                <span>
                  {t('missions.broadcast.model')} <code className="font-mono">{record.model}</code>
                </span>
              )}
              {record.branch_id && (
                <span>
                  {t('missions.broadcast.branch')} <code className="font-mono">{record.branch_id}</code>
                </span>
              )}
              {record.task_id && (
                <span>
                  {t('missions.broadcast.task')} <code className="font-mono">{record.task_id}</code>
                </span>
              )}
              {record.source_ids.length > 0 && (
                <span>
                  {t('missions.broadcast.sources')} <code className="font-mono">{record.source_ids.join(', ')}</code>
                </span>
              )}
            </div>
            {payload !== '' && (
              <div>
                <div className="mb-1.5 text-xs font-medium text-muted-foreground">
                  {t('missions.broadcast.payload')}
                </div>
                <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-all rounded-md border border-border bg-background p-3 font-mono text-xs text-foreground">
                  {payload}
                </pre>
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

/* ───────────────────────── Board ───────────────────────── */

export function MissionBroadcastLog({ missionId }: { missionId: string }) {
  const { t } = useTranslation();
  const [lanes, setLanes] = useState<LaneKey[]>([]);
  const [queryInput, setQueryInput] = useState('');
  const [query, setQuery] = useState('');
  const [order, setOrder] = useState<'asc' | 'desc'>('desc');
  const [page, setPage] = useState(1);
  const [size, setSize] = useState(20);
  const [live, setLive] = useState(true);
  const [openId, setOpenId] = useState<string | null>(null);
  const [now, setNow] = useState(0);

  // 「新播报」基线用 state 而非 ref：React Compiler 禁止在渲染期读 ref.current。
  const [seenIds, setSeenIds] = useState<Set<string>>(() => new Set());
  const [baseline, setBaseline] = useState(0);
  const streamRef = useRef('');

  const atLive = page === 1 && order === 'desc';

  const logQuery = useQuery({
    queryKey: ['mission-operation-log', missionId],
    queryFn: () => api.getMissionOperationLog(missionId, { limit: FETCH_LIMIT }),
    refetchInterval: live ? POLL_MS : false,
  });

  const records = useMemo(() => logQuery.data?.records ?? [], [logQuery.data]);
  const total = logQuery.data?.total ?? 0;

  // 输入防抖：停 300ms 才真正查询，并回到第一页。
  useEffect(() => {
    const timer = setTimeout(() => {
      setQuery(queryInput);
      setPage(1);
    }, 300);
    return () => clearTimeout(timer);
  }, [queryInput]);

  useEffect(() => {
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(timer);
  }, []);

  // 换筛选/排序 = 换了一条播报流：清掉「新」标记与未读基线。翻页不算换流。
  const streamKey = `${lanes.join(',')}|${query}|${order}`;
  useEffect(() => {
    if (streamRef.current === streamKey) return;
    streamRef.current = streamKey;
    setSeenIds(new Set());
    setBaseline(0);
  }, [streamKey]);

  const visibleRecords = useMemo(() => {
    const needle = query.trim().toLowerCase();
    const filtered = records.filter((record) => {
      if (lanes.length > 0 && !lanes.includes(laneOf(record.operation_type).key)) return false;
      if (needle === '') return true;
      return [
        record.entry,
        record.operation_type,
        record.actor_label,
        record.worker_id,
        record.model,
        record.branch_id,
        record.task_id,
        JSON.stringify(record.payload),
      ].some((value) => value?.toLowerCase().includes(needle));
    });
    const sorted = [...filtered].sort((a, b) => {
      if (a.sequence !== b.sequence) return a.sequence - b.sequence;
      return Date.parse(a.created_at) - Date.parse(b.created_at);
    });
    return order === 'desc' ? sorted.reverse() : sorted;
  }, [records, lanes, query, order]);

  const freshIds = useMemo(() => {
    if (!atLive || seenIds.size === 0) return EMPTY_IDS;
    return new Set(visibleRecords.map((record) => record.id).filter((id) => !seenIds.has(id)));
  }, [atLive, visibleRecords, seenIds]);

  useEffect(() => {
    if (!atLive) return;
    setSeenIds(new Set(visibleRecords.map((record) => record.id)));
    setBaseline(total);
  }, [atLive, visibleRecords, total]);

  const pending = atLive ? 0 : Math.max(0, total - baseline);

  const pageCount = Math.max(1, Math.ceil(visibleRecords.length / size));
  const safePage = Math.min(page, pageCount);
  const start = visibleRecords.length === 0 ? 0 : (safePage - 1) * size + 1;
  const end = (safePage - 1) * size + Math.min(size, visibleRecords.length - (safePage - 1) * size);
  const pageRows = visibleRecords.slice((safePage - 1) * size, safePage * size);

  // 按天分组：播报流按日期断行，长任务翻页时还能认出「这是哪天的事」。
  const groups = useMemo(() => {
    const result: Array<{ day: string; rows: SwarmOperationRecord[] }> = [];
    for (const record of pageRows) {
      const ts = Date.parse(record.created_at);
      const day = Number.isNaN(ts) ? t('missions.broadcast.unknownDay') : dayFormatter.format(ts);
      const last = result[result.length - 1];
      if (last && last.day === day) last.rows.push(record);
      else result.push({ day, rows: [record] });
    }
    return result;
  }, [pageRows, t]);

  if (logQuery.isError) {
    return (
      <ErrorState
        title={t('missions.operationLog.loadFailed')}
        description={getApiErrorMessage(logQuery.error)}
        retryLabel={t('common.refresh')}
        onRetry={() => {
          void logQuery.refetch();
        }}
      />
    );
  }

  if (logQuery.isLoading) {
    return <LoadingState card showHeader lines={6} />;
  }

  return (
    <Card className="overflow-hidden shadow-xs">
      {/* 工具条 */}
      <div className="flex flex-wrap items-center gap-2 border-b border-border px-4 py-2.5">
        <div className="relative w-full sm:w-64">
          <Search className="absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={queryInput}
            onChange={(event) => setQueryInput(event.target.value)}
            placeholder={t('missions.broadcast.searchPlaceholder')}
            className="h-8 pl-8"
            aria-label={t('missions.broadcast.searchPlaceholder')}
          />
        </div>
        <div className="flex flex-wrap items-center gap-1">
          {LANES.map((lane) => {
            const active = lanes.includes(lane.key);
            const Icon = lane.icon;
            return (
              <button
                key={lane.key}
                type="button"
                aria-pressed={active}
                onClick={() => {
                  setLanes((current) =>
                    current.includes(lane.key) ? current.filter((key) => key !== lane.key) : [...current, lane.key],
                  );
                  setPage(1);
                }}
                className={cn(
                  'inline-flex items-center gap-1 rounded-md border px-2 py-0.5 text-xs font-medium transition-colors',
                  active ? lane.chip : 'border-transparent text-muted-foreground hover:bg-muted',
                )}
              >
                <Icon className="h-3 w-3" />
                {t(`missions.broadcast.lanes.${lane.labelKey}`)}
              </button>
            );
          })}
          {lanes.length > 0 && (
            <Button
              variant="ghost"
              size="sm"
              className="h-7 px-2 text-xs"
              onClick={() => {
                setLanes([]);
                setPage(1);
              }}
            >
              {t('missions.broadcast.clear')}
            </Button>
          )}
        </div>
        <div className="ml-auto flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            className="h-8"
            onClick={() => {
              setOrder((current) => (current === 'desc' ? 'asc' : 'desc'));
              setPage(1);
            }}
            aria-label={order === 'desc' ? t('missions.broadcast.newestFirst') : t('missions.broadcast.oldestFirst')}
          >
            {order === 'desc' ? <ArrowDown className="h-3.5 w-3.5" /> : <ArrowUp className="h-3.5 w-3.5" />}
            {order === 'desc' ? t('missions.broadcast.newestFirst') : t('missions.broadcast.oldestFirst')}
          </Button>
          <Button
            variant={live ? 'outline' : 'secondary'}
            size="sm"
            className="h-8"
            onClick={() => setLive((value) => !value)}
            aria-label={live ? t('missions.broadcast.pause') : t('missions.broadcast.resume')}
          >
            {live ? <Pause className="h-3.5 w-3.5" /> : <Play className="h-3.5 w-3.5" />}
            {live ? t('missions.broadcast.live') : t('missions.broadcast.paused')}
          </Button>
        </div>
      </div>

      {/* 离开直播位时的未读提示 */}
      {!atLive && pending > 0 && (
        <button
          type="button"
          onClick={() => {
            setPage(1);
            setOrder('desc');
          }}
          className="flex w-full items-center justify-center gap-1.5 border-b border-border bg-primary/10 py-1.5 text-xs font-medium text-primary transition-colors hover:bg-primary/15"
        >
          <ArrowUpToLine className="h-3.5 w-3.5" />
          {pending > 99 ? '99+' : pending} {t('missions.broadcast.pending')} · {t('missions.broadcast.backToLive')}
        </button>
      )}

      <div className="px-4 py-0">
        {pageRows.length === 0 ? (
          <EmptyState
            variant="bare"
            compact
            className="py-12"
            title={
              records.length === 0
                ? t('missions.operationLog.empty')
                : t('missions.broadcast.noMatch')
            }
            description={
              records.length === 0 ? t('missions.operationLog.emptyDescription') : undefined
            }
          />
        ) : (
          groups.map((group) => (
            <div key={group.day}>
              <div className="py-2 pl-[6.25rem] text-xs font-medium text-muted-foreground">{group.day}</div>
              {group.rows.map((record) => (
                <BroadcastRow
                  key={record.id}
                  record={record}
                  now={now}
                  fresh={freshIds.has(record.id)}
                  open={openId === record.id}
                  onToggle={() => setOpenId((current) => (current === record.id ? null : record.id))}
                />
              ))}
            </div>
          ))
        )}
      </div>

      <div className="flex flex-wrap items-center gap-2 border-t border-border px-4 py-2.5 text-xs text-muted-foreground">
        <Select
          value={String(size)}
          onValueChange={(value) => {
            setSize(Number(value));
            setPage(1);
          }}
        >
          <SelectTrigger className="h-7 w-24">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {PAGE_SIZES.map((value) => (
              <SelectItem key={value} value={String(value)}>
                {value} / {t('missions.broadcast.perPage')}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <span className="tabular-nums">
          {start}–{end} / {visibleRecords.length}
        </span>
        <div className="ml-auto flex items-center gap-2">
          <Button
            variant="outline"
            size="icon"
            className="h-7 w-7"
            disabled={safePage <= 1}
            onClick={() => setPage((current) => Math.max(1, current - 1))}
            aria-label={t('missions.broadcast.prev')}
          >
            <ChevronLeft className="h-3.5 w-3.5" />
          </Button>
          <span className="tabular-nums">
            {safePage} / {pageCount}
          </span>
          <Button
            variant="outline"
            size="icon"
            className="h-7 w-7"
            disabled={safePage >= pageCount}
            onClick={() => setPage((current) => Math.min(pageCount, current + 1))}
            aria-label={t('missions.broadcast.next')}
          >
            <ChevronRight className="h-3.5 w-3.5" />
          </Button>
        </div>
      </div>
    </Card>
  );
}
