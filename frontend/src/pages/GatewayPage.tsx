import { useMemo, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import {
  Activity,
  CircleDollarSign,
  DatabaseZap,
  Gauge,
  Layers,
  Loader2,
  Play,
  RefreshCw,
  Square,
  Waypoints,
  XCircle,
} from 'lucide-react';
import { api, getApiErrorMessage } from '@/lib/api';
import type {
  GatewayStatus,
  WorkerUsageBreakdown,
  WorkerUsageDimension,
  WorkerUsageGroupedDailyPoint,
  WorkerUsageModelSlice,
} from '@/lib/types';
import { cn } from '@/lib/utils';
import { Badge, Button, ErrorState, LoadingState, PageHeader, Tabs, TabsList, TabsTrigger } from '@/ui/untitled';
import { useToast } from '@/hooks/use-toast';

/**
 * LiteLLM Gateway 页 —— 启停受管网关 + token 用量统计仪表盘。
 *
 * 数据源：`/gateway/status`（5s 轮询）与 `/gateway/usage`（按窗口 + 分组维度取数）。
 * 成本口径：仅上游/CLI 真实报出时聚合，缺失显示 `—`，绝不伪造。
 * 记账口径：worker_usage 中 cached_input_tokens 与 input_tokens 互斥
 * （anthropic cache_read 单列），因此「输入（未命中）」= input、
 * 「缓存命中」= cached、命中率 = cached /（input + cached）。
 */

type WindowDays = 7 | 30 | 90 | 365;

const WINDOW_OPTIONS: WindowDays[] = [7, 30, 90, 365];

type GroupDimension = 'total' | WorkerUsageDimension;

const GROUP_TABS: GroupDimension[] = ['total', 'runtime', 'model'];

/** 单一系列的完整配色（柱 fill / 图例点 / 进度条 / 数值文字）。 */
interface SeriesColor {
  bar: string;
  dot: string;
  track: string;
  text: string;
}

/** 总计三系列的配色：输入蓝 / 缓存绿 / 输出紫。 */
const SERIES_COLORS: Record<'input' | 'cached' | 'output', SeriesColor> = {
  input: { bar: 'fill-blue-500', dot: 'bg-blue-500', track: 'bg-blue-500', text: 'text-blue-600' },
  cached: { bar: 'fill-emerald-500', dot: 'bg-emerald-500', track: 'bg-emerald-500', text: 'text-emerald-600' },
  output: { bar: 'fill-violet-500', dot: 'bg-violet-500', track: 'bg-violet-500', text: 'text-violet-600' },
};

/** 分组堆叠系列的调色板（前三色与总计三系列一致）。 */
const GROUPED_PALETTE: SeriesColor[] = [
  SERIES_COLORS.input,
  SERIES_COLORS.cached,
  SERIES_COLORS.output,
  { bar: 'fill-amber-500', dot: 'bg-amber-500', track: 'bg-amber-500', text: 'text-amber-600' },
  { bar: 'fill-rose-500', dot: 'bg-rose-500', track: 'bg-rose-500', text: 'text-rose-600' },
  { bar: 'fill-cyan-600', dot: 'bg-cyan-600', track: 'bg-cyan-600', text: 'text-cyan-600' },
];
const OTHER_SERIES_COLOR: SeriesColor = {
  bar: 'fill-slate-400',
  dot: 'bg-slate-400',
  track: 'bg-slate-400',
  text: 'text-slate-500',
};

/** 分组堆叠图最多单独着色的键数，其余合并为「其他」。 */
const MAX_LEGEND_KEYS = 5;

function formatCompact(value: number): string {
  return new Intl.NumberFormat(undefined, {
    notation: 'compact',
    maximumFractionDigits: 1,
  }).format(value);
}

function formatCost(value: number | null): string {
  if (value === null) return '—';
  return `$${value.toFixed(value < 1 ? 4 : 2)}`;
}

function formatTime(rfc3339: string | null): string {
  if (!rfc3339) return '—';
  const date = new Date(rfc3339);
  if (Number.isNaN(date.getTime())) return rfc3339;
  return date.toLocaleString(undefined, {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
}

/** 图表单日的一个堆叠段。 */
interface ChartSegment {
  key: string;
  /** 展示名（已本地化）。 */
  label: string;
  value: number;
  color: SeriesColor;
}

interface ChartPoint {
  day: string;
  runs: number;
  cost: number | null;
  segments: ChartSegment[];
}

interface LegendEntry {
  key: string;
  label: string;
  color: SeriesColor;
}

/** 总计口径：单系列三段（输入未命中 / 缓存命中 / 输出，自下而上）。 */
function buildTotalPoints(
  daily: WorkerUsageBreakdown['daily'],
  labelOf: (key: 'input' | 'cached' | 'output') => string,
): { points: ChartPoint[]; legend: LegendEntry[] } {
  const order: Array<'input' | 'cached' | 'output'> = ['input', 'cached', 'output'];
  const points = daily.map((point) => ({
    day: point.day,
    runs: point.runs,
    cost: point.cost_usd,
    segments: order
      .map((key) => ({
        key,
        label: labelOf(key),
        value:
          key === 'input'
            ? point.input_tokens
            : key === 'cached'
              ? point.cached_input_tokens
              : point.output_tokens,
        color: SERIES_COLORS[key],
      }))
      .filter((segment) => segment.value > 0),
  }));
  const legend = order.map((key) => ({
    key,
    label: labelOf(key),
    color: SERIES_COLORS[key],
  }));
  return { points, legend };
}

/** 分组口径：每日按维度键堆叠，超出 MAX_LEGEND_KEYS 的键合并为「其他」。 */
function buildGroupedPoints(
  grouped: WorkerUsageGroupedDailyPoint[],
  labelOf: (key: string) => string,
  unattributedLabel: string,
  otherLabel: string,
): { points: ChartPoint[]; legend: LegendEntry[] } {
  const keyLabel = (key: string) => (key === 'unattributed' ? unattributedLabel : labelOf(key));
  const pointTotal = (row: WorkerUsageGroupedDailyPoint) =>
    row.input_tokens + row.cached_input_tokens + row.output_tokens;
  const totals = new Map<string, number>();
  for (const point of grouped) {
    const key = point.key ?? 'unattributed';
    totals.set(key, (totals.get(key) ?? 0) + pointTotal(point));
  }
  const orderedKeys = [...totals.entries()]
    .sort((a, b) => b[1] - a[1])
    .map(([key]) => key);
  const topKeys = orderedKeys.slice(0, MAX_LEGEND_KEYS);
  const hasOther = orderedKeys.length > MAX_LEGEND_KEYS;

  const dayKeys = [...new Set(grouped.map((point) => point.day))].sort();
  const points: ChartPoint[] = dayKeys.map((day) => {
    const rows = grouped.filter((point) => point.day === day);
    const runs = rows.reduce((sum, row) => sum + row.runs, 0);
    const costs = rows.map((row) => row.cost_usd);
    const allCosted = rows.length > 0 && costs.every((cost) => cost !== null);
    const segments: ChartSegment[] = topKeys
      .map((key, index) => {
        const row = rows.find((candidate) => (candidate.key ?? 'unattributed') === key);
        return {
          key,
          label: keyLabel(key),
          value: row ? pointTotal(row) : 0,
          color: GROUPED_PALETTE[index % GROUPED_PALETTE.length],
        };
      })
      .filter((segment) => segment.value > 0);
    if (hasOther) {
      const otherRows = rows.filter((row) => !topKeys.includes(row.key ?? 'unattributed'));
      const otherValue = otherRows.reduce((sum, row) => sum + pointTotal(row), 0);
      if (otherValue > 0) {
        segments.push({ key: '__other__', label: otherLabel, value: otherValue, color: OTHER_SERIES_COLOR });
      }
    }
    return {
      day,
      runs,
      cost: allCosted ? costs.reduce((sum, cost) => sum + (cost ?? 0), 0) : null,
      segments,
    };
  });

  const legend: LegendEntry[] = topKeys.map((key, index) => ({
    key,
    label: keyLabel(key),
    color: GROUPED_PALETTE[index % GROUPED_PALETTE.length],
  }));
  if (hasOther) {
    legend.push({ key: '__other__', label: otherLabel, color: OTHER_SERIES_COLOR });
  }
  return { points, legend };
}

/**
 * Y 轴刻度：步进取 1/2/2.5/5 阶梯，顶刻度 = ceil(max·1.06 / step)·step ——
 * 最高柱子与顶格线之间留出可见 headroom，不顶满绘图区。
 */
function niceTicks(max: number, targetCount = 4): number[] {
  if (max <= 0) return [0, 1];
  const rawStep = max / targetCount;
  const magnitude = Math.pow(10, Math.floor(Math.log10(rawStep)));
  const normalized = rawStep / magnitude;
  const step =
    (normalized <= 1 ? 1 : normalized <= 2 ? 2 : normalized <= 2.5 ? 2.5 : normalized <= 5 ? 5 : 10) *
    magnitude;
  const tickCount = Math.max(2, Math.ceil((max * 1.06) / step));
  const ticks: number[] = [];
  for (let i = 0; i <= tickCount; i += 1) {
    ticks.push(i * step);
  }
  return ticks;
}

/** 每日活动堆叠柱状图（SVG 手绘 + Y 轴网格 + 悬浮 tooltip，无第三方图表依赖）。 */
function DailyActivityChart({
  points,
  legend,
}: {
  points: ChartPoint[];
  legend: LegendEntry[];
}) {
  const { t } = useTranslation();
  const [hoverIndex, setHoverIndex] = useState<number | null>(null);
  if (points.length === 0) {
    return (
      <p className="py-10 text-center text-xs text-muted-foreground">
        {t('litellm.activity.empty')}
      </p>
    );
  }
  const width = 720;
  const height = 224;
  const padding = { top: 12, right: 8, bottom: 22, left: 52 };
  const innerHeight = height - padding.top - padding.bottom;
  const plotWidth = width - padding.left - padding.right;
  const maxTotal = Math.max(
    ...points.map((point) => point.segments.reduce((sum, segment) => sum + segment.value, 0)),
    1,
  );
  const ticks = niceTicks(maxTotal);
  const yMax = ticks[ticks.length - 1];
  const slot = plotWidth / points.length;
  const barWidth = Math.min(Math.max(slot * 0.5, 6), 48);
  const yOf = (value: number) => height - padding.bottom - (value / yMax) * innerHeight;
  // tooltip 锚点：柱心在 SVG 宽度中的百分比（外层 min-w 容器与 SVG 同宽）。
  const hoverAnchor = (index: number) =>
    Math.min(90, Math.max(10, ((padding.left + (index + 0.5) * slot) / width) * 100));

  return (
    <div className="w-full overflow-x-auto">
      <div className="relative min-w-[560px]">
      {/* 图例：图表上方色点 + 名称 */}
      <div className="mb-1 flex flex-wrap items-center gap-x-4 gap-y-1 text-[11px] text-muted-foreground">
        {legend.map((entry) => (
          <span key={entry.key} className="flex items-center gap-1.5">
            <span className={cn('size-2 rounded-full', entry.color.dot)} />
            {entry.label}
          </span>
        ))}
      </div>
      <svg
        viewBox={`0 0 ${width} ${height}`}
        className="h-56 w-full"
        role="img"
        aria-label={t('litellm.activity.title')}
        onMouseLeave={() => setHoverIndex(null)}
      >
        {/* Y 轴网格 + 刻度 */}
        {ticks.map((tick) => (
          <g key={tick}>
            <line
              x1={padding.left}
              x2={width - padding.right}
              y1={yOf(tick)}
              y2={yOf(tick)}
              className="stroke-border/70"
              strokeWidth={1}
            />
            <text
              x={padding.left - 8}
              y={yOf(tick) + 3}
              textAnchor="end"
              className="fill-muted-foreground text-[10px] tabular-nums"
            >
              {formatCompact(tick)}
            </text>
          </g>
        ))}
        {points.map((point, index) => {
          const x = padding.left + index * slot + (slot - barWidth) / 2;
          const total = point.segments.reduce((sum, segment) => sum + segment.value, 0);
          let cursor = height - padding.bottom;
          return (
            <g key={point.day}>
              {/* 整列命中区：悬浮即出 tooltip */}
              <rect
                x={padding.left + index * slot}
                y={padding.top}
                width={slot}
                height={innerHeight}
                fill="transparent"
                onMouseEnter={() => setHoverIndex(index)}
              />
              {point.segments.map((segment, segmentIndex) => {
                const segmentHeight = (segment.value / yMax) * innerHeight;
                const y = cursor - segmentHeight;
                cursor = y;
                const isTop = segmentIndex === point.segments.length - 1;
                return (
                  <rect
                    key={segment.key}
                    x={x}
                    y={y}
                    width={barWidth}
                    height={Math.max(segmentHeight, total > 0 ? 1.5 : 0)}
                    rx={isTop ? 3 : 0}
                    className={segment.color.bar}
                    onMouseEnter={() => setHoverIndex(index)}
                  />
                );
              })}
              {(points.length <= 16 || index % Math.ceil(points.length / 12) === 0) && (
                <text
                  x={x + barWidth / 2}
                  y={height - 6}
                  textAnchor="middle"
                  className="fill-muted-foreground text-[10px]"
                >
                  {point.day.slice(5)}
                </text>
              )}
            </g>
          );
        })}
      </svg>
      {/* 悬浮 tooltip（绝对定位，跟随柱心） */}
      {hoverIndex !== null && points[hoverIndex] && (
        <div
          className="pointer-events-none absolute top-8 z-10 min-w-[172px] -translate-x-1/2 rounded-xl border border-border bg-popover px-3.5 py-2.5 text-xs shadow-lg"
          style={{ left: `${hoverAnchor(hoverIndex)}%` }}
        >
          <p className="mb-1.5 font-semibold text-foreground">{points[hoverIndex].day}</p>
          <div className="space-y-1">
            {points[hoverIndex].segments.map((segment) => (
              <div key={segment.key} className="flex items-center justify-between gap-4">
                <span className="flex items-center gap-1.5 text-muted-foreground">
                  <span className={cn('size-2 rounded-full', segment.color.dot)} />
                  {segment.label}
                </span>
                <span className="font-mono font-semibold tabular-nums text-foreground">
                  {formatCompact(segment.value)}
                </span>
              </div>
            ))}
            <div className="flex items-center justify-between gap-4 border-t border-border pt-1 text-muted-foreground">
              <span>{t('litellm.breakdown.runs')}</span>
              <span className="font-mono tabular-nums">{points[hoverIndex].runs}</span>
            </div>
            {points[hoverIndex].cost !== null && (
              <div className="flex items-center justify-between gap-4 text-muted-foreground">
                <span>{t('litellm.metrics.cost')}</span>
                <span className="font-mono tabular-nums">${points[hoverIndex].cost?.toFixed(4)}</span>
              </div>
            )}
          </div>
        </div>
      )}
      </div>
    </div>
  );
}

/** 分析切片表（runtime × model / runtime 聚合），横条按总 token 占比。 */
function BreakdownTable({
  title,
  slices,
  showModel,
}: {
  title: string;
  slices: WorkerUsageModelSlice[];
  showModel: boolean;
}) {
  const { t } = useTranslation();
  const top = slices.slice(0, 6);
  const maxTokens = Math.max(
    ...top.map((slice) => slice.input_tokens + slice.output_tokens),
    1,
  );
  return (
    <section className="rounded-xl border border-border bg-card shadow-xs">
      <header className="border-b border-border px-4 py-3">
        <h3 className="text-sm font-bold text-foreground">{title}</h3>
      </header>
      {top.length === 0 ? (
        <p className="px-4 py-8 text-center text-xs text-muted-foreground">
          {t('litellm.breakdown.empty')}
        </p>
      ) : (
        <div className="divide-y divide-border">
          {top.map((slice, index) => {
            const total = slice.input_tokens + slice.output_tokens;
            const runtimeLabel = t(`litellm.agent.${slice.runtime}`, {
              defaultValue: slice.runtime,
            });
            const color = GROUPED_PALETTE[index % GROUPED_PALETTE.length];
            return (
              <div key={`${slice.runtime}-${slice.model ?? ''}-${slice.requested_model ?? ''}`} className="px-4 py-2.5">
                <div className="flex items-center gap-2 text-xs">
                  <span className={cn('size-2 shrink-0 rounded-sm', color.dot)} />
                  <span className="shrink-0 font-semibold text-foreground">
                    {runtimeLabel}
                  </span>
                  {showModel && (
                    <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-muted-foreground">
                      {slice.model ?? t('litellm.breakdown.unattributed')}
                      {slice.requested_model && slice.requested_model !== slice.model
                        ? ` → ${slice.requested_model}`
                        : ''}
                    </span>
                  )}
                  <span className="ml-auto shrink-0 font-mono tabular-nums text-muted-foreground">
                    {formatCompact(total)} tok · {slice.runs}{' '}
                    {t('litellm.breakdown.runs')} · {formatCost(slice.cost_usd)}
                  </span>
                </div>
                <div className="mt-1.5 h-1.5 overflow-hidden rounded-full bg-muted/60">
                  <div
                    className={cn('h-full rounded-full', color.track)}
                    style={{ width: `${Math.max((total / maxTokens) * 100, 1.5)}%` }}
                  />
                </div>
              </div>
            );
          })}
        </div>
      )}
    </section>
  );
}

/** 指标面板行：标签 + 着色数值 + 占比横条。 */
function MetricBarRow({
  label,
  value,
  fraction,
  color,
}: {
  label: string;
  value: string;
  /** 占合计的比例（0..1），驱动横条宽度。 */
  fraction: number;
  color: SeriesColor;
}) {
  return (
    <div className="py-2">
      <div className="flex items-center justify-between gap-3 text-xs">
        <span className="text-muted-foreground">{label}</span>
        <span className={cn('font-mono font-semibold tabular-nums', color.text)}>{value}</span>
      </div>
      <div className="mt-1.5 h-1.5 overflow-hidden rounded-full bg-muted/70">
        <div
          className={cn('h-full rounded-full transition-all', color.track)}
          style={{ width: `${Math.min(100, Math.max(fraction * 100, fraction > 0 ? 2 : 0))}%` }}
        />
      </div>
    </div>
  );
}

export function GatewayPage() {
  const { t } = useTranslation();
  const { toast } = useToast();
  const queryClient = useQueryClient();
  const [days, setDays] = useState<WindowDays>(30);
  const [dimension, setDimension] = useState<GroupDimension>('total');

  const gatewayQuery = useQuery({
    queryKey: ['gateway-status'],
    queryFn: api.getGatewayStatus,
    refetchInterval: 5_000,
  });
  const usageQuery = useQuery({
    queryKey: ['gateway-usage', days, dimension],
    queryFn: () =>
      api.getGatewayUsage(days, dimension === 'total' ? undefined : dimension),
    refetchInterval: 15_000,
  });

  const gatewayStart = useMutation({
    mutationFn: api.startGateway,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['gateway-status'] });
    },
    onError: (error) => {
      toast({ title: getApiErrorMessage(error), variant: 'destructive' });
    },
  });
  const gatewayStop = useMutation({
    mutationFn: api.stopGateway,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['gateway-status'] });
      void queryClient.invalidateQueries({ queryKey: ['gateway-usage'] });
    },
  });

  const gateway: GatewayStatus | undefined = gatewayQuery.data;
  const usage: WorkerUsageBreakdown | undefined = usageQuery.data;

  const summary = usage?.summary;
  const daily = useMemo(() => usage?.daily ?? [], [usage]);
  const grouped = useMemo(() => usage?.grouped_daily ?? [], [usage]);

  // 口径：cached 与 input 互斥记账 → 未命中 = input，可观测输入 = input + cached。
  const totalInput = summary?.input_tokens ?? 0;
  const totalOutput = summary?.output_tokens ?? 0;
  const cached = summary?.cached_input_tokens ?? 0;
  const totalTokens = totalInput + totalOutput + cached;
  const runs = summary?.runs ?? 0;
  const observableInput = totalInput + cached;
  const cacheRate = observableInput > 0 ? cached / observableInput : null;

  const { points, legend } = useMemo(() => {
    if (dimension === 'total') {
      return buildTotalPoints(daily, (key) => t(`litellm.activity.${key}`));
    }
    return buildGroupedPoints(
      grouped,
      (key) =>
        dimension === 'runtime'
          ? t(`litellm.agent.${key}`, { defaultValue: key })
          : key,
      t('litellm.breakdown.unattributed'),
      t('litellm.tabs.other'),
    );
  }, [dimension, daily, grouped, t]);

  return (
    <div className="page-stack">
      <PageHeader
        icon={<Waypoints className="h-5 w-5" />}
        title={t('litellm.title')}
        description={t('litellm.description')}
        actions={
          <div className="flex items-center gap-2">
            <Tabs
              value={String(days)}
              onValueChange={(value) => setDays(Number(value) as WindowDays)}
            >
              <TabsList
                animated
                indicatorClassName="bg-card"
                aria-label={t('litellm.window.label')}
                className="h-8 bg-muted/60 p-0.5"
              >
                {WINDOW_OPTIONS.map((option) => (
                  <TabsTrigger key={option} value={String(option)} className="px-2.5 text-xs">
                    {t(`litellm.window.days${option}`)}
                  </TabsTrigger>
                ))}
              </TabsList>
            </Tabs>
            {gateway?.running ? (
              <Button
                variant="outline"
                size="sm"
                onClick={() => gatewayStop.mutate()}
                disabled={gatewayStop.isPending}
              >
                {gatewayStop.isPending ? (
                  <Loader2 className="mr-2 size-3.5 animate-spin" />
                ) : (
                  <Square className="mr-2 size-3.5" />
                )}
                {gatewayStop.isPending
                  ? t('litellm.status.stopping')
                  : t('litellm.control.stop')}
              </Button>
            ) : (
              <Button size="sm" onClick={() => gatewayStart.mutate()} disabled={gatewayStart.isPending}>
                {gatewayStart.isPending ? (
                  <Loader2 className="mr-2 size-3.5 animate-spin" />
                ) : (
                  <Play className="mr-2 size-3.5" />
                )}
                {gatewayStart.isPending
                  ? t('litellm.status.starting')
                  : t('litellm.control.start')}
              </Button>
            )}
          </div>
        }
      />

      {/* 网关控制卡 */}
      <section className="rounded-xl border border-border bg-muted/30 p-4 sm:p-5">
        <header className="flex flex-wrap items-center justify-between gap-3">
          <div className="min-w-0">
            <h2 className="flex items-center gap-2 text-sm font-bold text-foreground">
              {t('litellm.control.cardTitle')}
              <Badge tone={gateway?.running ? 'success' : 'neutral'}>
                {gateway?.running
                  ? t('litellm.status.running')
                  : t('litellm.status.stopped')}
              </Badge>
            </h2>
            <p className="mt-0.5 text-xs text-muted-foreground">
              {t('litellm.control.cardDescription')}
            </p>
          </div>
          <Button
            variant="ghost"
            size="icon"
            aria-label={t('litellm.refresh')}
            onClick={() => {
              void gatewayQuery.refetch();
              void usageQuery.refetch();
            }}
          >
            <RefreshCw className="size-4" />
          </Button>
        </header>

        {gateway && (
          <div className="mt-3 grid grid-cols-2 gap-3 text-xs sm:grid-cols-4">
            <div>
              <p className="flex items-center gap-1 text-muted-foreground/70">
                <Gauge className="size-3" /> {t('litellm.control.port')}
              </p>
              <p className="font-mono tabular-nums">{gateway.port ?? '—'}</p>
            </div>
            <div>
              <p className="flex items-center gap-1 text-muted-foreground/70">
                <Activity className="size-3" /> {t('litellm.control.startedAt')}
              </p>
              <p className="font-mono tabular-nums">{formatTime(gateway.started_at)}</p>
            </div>
            <div className="min-w-0">
              <p className="flex items-center gap-1 text-muted-foreground/70">
                <Layers className="size-3" /> {t('litellm.control.models')}
              </p>
              <p className="truncate font-mono">
                {gateway.models.length > 0 ? gateway.models.join(', ') : '—'}
              </p>
            </div>
            <div className="min-w-0">
              <p className="flex items-center gap-1 text-muted-foreground/70">
                <Waypoints className="size-3" /> {t('litellm.control.bindings')}
              </p>
              <p className="truncate font-mono">
                {Object.entries(gateway.agents)
                  .map(
                    ([agent, binding]) =>
                      `${t(`litellm.agent.${agent}`, { defaultValue: agent })}→${binding.alias}`,
                  )
                  .join(' · ') || t('litellm.control.noBindings')}
              </p>
            </div>
          </div>
        )}
        {gateway?.last_error && (
          <p className="mt-2 flex items-center gap-1.5 text-xs text-destructive">
            <XCircle className="size-3 shrink-0" />
            {gateway.last_error}
          </p>
        )}
        {gateway && !gateway.enabled && (
          <p className="mt-2 text-xs text-muted-foreground">
            {t('litellm.control.notConfigured')}
          </p>
        )}
      </section>

      {usageQuery.isLoading ? (
        <LoadingState lines={2} card />
      ) : usageQuery.isError ? (
        <ErrorState
          title={t('litellm.breakdown.title')}
          description={getApiErrorMessage(usageQuery.error)}
          onRetry={() => usageQuery.refetch()}
          retryLabel={t('litellm.refresh')}
        />
      ) : (
        <>
          {/* 指标面板 + 每日活动：左指标 / 右图表 */}
          <section className="rounded-xl border border-border bg-card p-4 shadow-xs sm:p-5">
            <div className="grid grid-cols-1 gap-6 xl:grid-cols-12">
              <div className="min-w-0 xl:col-span-4">
                <p className="flex items-center gap-1.5 text-[11px] font-medium uppercase tracking-wide text-muted-foreground/80">
                  <DatabaseZap className="size-3.5" />
                  {t('litellm.metrics.total')}
                </p>
                <p className="mt-1 truncate text-3xl font-bold tabular-nums tracking-tight text-foreground">
                  {formatCompact(totalTokens)}
                </p>
                <p className="mt-0.5 text-xs text-muted-foreground">
                  {t('litellm.metrics.runsUnit', { count: runs })}
                </p>

                <div className="mt-3 divide-y divide-border/70 border-t border-border/70">
                  <MetricBarRow
                    label={t('litellm.metrics.inputUncached')}
                    value={formatCompact(totalInput)}
                    fraction={totalTokens > 0 ? totalInput / totalTokens : 0}
                    color={SERIES_COLORS.input}
                  />
                  <MetricBarRow
                    label={t('litellm.metrics.cachedHit')}
                    value={formatCompact(cached)}
                    fraction={totalTokens > 0 ? cached / totalTokens : 0}
                    color={SERIES_COLORS.cached}
                  />
                  <MetricBarRow
                    label={t('litellm.metrics.output')}
                    value={formatCompact(totalOutput)}
                    fraction={totalTokens > 0 ? totalOutput / totalTokens : 0}
                    color={SERIES_COLORS.output}
                  />
                </div>

                {/* 缓存命中率高亮卡（分母剔除无 usage 的 run：无可观测输入即 —） */}
                <div className="mt-4 flex items-center justify-between gap-3 rounded-xl border border-border bg-muted/40 px-3.5 py-2.5">
                  <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
                    <DatabaseZap className="size-3.5" />
                    {t('litellm.metrics.cacheRate')}
                  </span>
                  <span
                    className={cn(
                      'font-mono text-lg font-bold tabular-nums',
                      cacheRate === null
                        ? 'text-muted-foreground'
                        : cacheRate >= 0.5
                          ? 'text-emerald-600'
                          : cacheRate >= 0.2
                            ? 'text-blue-600'
                            : 'text-amber-600',
                    )}
                  >
                    {cacheRate === null ? '—' : `${Math.round(cacheRate * 100)}%`}
                  </span>
                </div>
                <p className="mt-1.5 text-[11px] text-muted-foreground/80">
                  {t('litellm.metrics.observableOf', { total: formatCompact(observableInput) })}
                </p>

                <div className="mt-2 flex items-center justify-between border-t border-border/70 pt-2 text-xs">
                  <span className="flex items-center gap-1.5 text-muted-foreground">
                    <CircleDollarSign className="size-3.5" />
                    {t('litellm.metrics.cost')}
                  </span>
                  <span className="font-mono font-semibold tabular-nums text-foreground">
                    {formatCost(summary?.cost_usd ?? null)}
                  </span>
                </div>
              </div>

              <div className="min-w-0 xl:col-span-8">
                <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
                  <h3 className="text-sm font-bold text-foreground">
                    {t('litellm.activity.title')}
                  </h3>
                  {/* 分组维度 tabs：切换只改 daily 聚合的 GROUP BY 维度 */}
                  <Tabs
                    value={dimension}
                    onValueChange={(value) => setDimension(value as GroupDimension)}
                  >
                    <TabsList
                      animated
                      indicatorClassName="bg-card"
                      aria-label={t('litellm.tabs.label')}
                      className="h-8 bg-muted/60 p-0.5"
                    >
                      {GROUP_TABS.map((tab) => (
                        <TabsTrigger key={tab} value={tab} className="px-2.5 text-xs">
                          {t(`litellm.tabs.${tab}`)}
                        </TabsTrigger>
                      ))}
                    </TabsList>
                  </Tabs>
                </div>
                <DailyActivityChart points={points} legend={legend} />
              </div>
            </div>
          </section>

          {/* 分析 */}
          <div className="grid grid-cols-1 gap-4 xl:grid-cols-2">
            <BreakdownTable
              title={`${t('litellm.breakdown.title')} · ${t('litellm.breakdown.byModel')}`}
              slices={usage?.by_model ?? []}
              showModel
            />
            <BreakdownTable
              title={`${t('litellm.breakdown.title')} · ${t('litellm.breakdown.byRuntime')}`}
              slices={usage?.by_runtime ?? []}
              showModel={false}
            />
          </div>
        </>
      )}
    </div>
  );
}
