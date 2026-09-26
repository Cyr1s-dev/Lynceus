/**
 * Mission Tool Calls Board — 工具调用页（工具执行页）。
 *
 * 工具执行页是一张「时间 / 任务 / Worker / 工具 / 输入 / 状态」的
 * 密集表 + 右侧输入输出详情抽屉 + 工具调用统计弹窗。Lynceus 后端没有
 * `GET /commands` 聚合端点，改由 `MissionCanvas.tool_invocations` 直接驱动：
 *
 * - 数据同源、分页/搜索/统计全部在前端完成，无需新端点；
 * - 「Worker」列取 `worker_id`（后端 `ToolInvocation.worker_id` 是一等字段，
 *   由 `ToolBroker` 从 `ExecutionScope.worker_id` 透传）。历史记录没有这个
 *   字段时回落 `metadata.worker_id`，再取不到显示「—」，不编造；
 *   `module_id` 是「哪个模块提供工具」，不是「谁点的按钮」，已移到详情抽屉；
 * - 统计弹窗按工具聚合调用次数与失败数 + 占比条。
 */
import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  ChartColumn,
  ChevronLeft,
  ChevronRight,
  Search,
  Terminal,
} from 'lucide-react';

import {
  Badge,
  Button,
  Card,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  Drawer,
  EmptyState,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  ToolInvocationStatusBadge,
} from '@/ui/untitled';
import { cn } from '@/lib/utils';
import type { ApiToolInvocation, ToolStatus } from '@/lib/types';

/* ───────────────────────── Constants ───────────────────────── */

const PAGE_SIZES = [25, 50, 100];
const INPUT_MAX_LEN = 80;

const ERROR_STATUSES: ToolStatus[] = ['error', 'timeout', 'denied'];

/* ───────────────────────── Helpers ───────────────────────── */

/**
 * Worker 列的显示值。
 *
 * 优先 `worker_id`（后端一等字段）；老记录回落到
 * `metadata.worker_id`（`ToolBroker` 早期只写 metadata）；都没有才显示「—」。
 * `module_id` 故意不用在这里——它是"哪个模块提供工具"，不是"谁点的按钮"。
 */
function workerLabel(invocation: ApiToolInvocation): string {
  const direct = invocation.worker_id?.trim();
  if (direct) return direct;
  const meta = invocation.metadata?.worker_id;
  if (typeof meta === 'string' && meta.trim()) return meta.trim();
  return '\u2014';
}

function formatTime(value: string): string {
  const parsed = Date.parse(value);
  if (Number.isNaN(parsed)) return value;
  return new Date(parsed).toLocaleString('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
}

/** 输入摘要单行预览。 */
function truncateFirstLine(value: string, maxLen: number): string {
  const first = value.split('\n')[0] ?? '';
  return first.length <= maxLen ? first : `${first.slice(0, maxLen)}…`;
}

function isError(status: ToolStatus): boolean {
  return ERROR_STATUSES.includes(status);
}

interface ToolStat {
  tool: string;
  total: number;
  errors: number;
}

/* ───────────────────────── Component ───────────────────────── */

export function MissionToolCallsBoard({ invocations }: { invocations: ApiToolInvocation[] }) {
  const { t } = useTranslation();
  const [page, setPage] = useState(0);
  const [size, setSize] = useState(50);
  const [query, setQuery] = useState('');
  const [selected, setSelected] = useState<ApiToolInvocation | null>(null);
  const [statsOpen, setStatsOpen] = useState(false);

  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (needle === '') return invocations;
    return invocations.filter((invocation) =>
      [
        invocation.tool_name,
        invocation.input_summary,
        invocation.output_summary,
        invocation.task_id,
        invocation.module_id,
        workerLabel(invocation),
      ].some((value) => value?.toLowerCase().includes(needle)),
    );
  }, [invocations, query]);

  const ordered = useMemo(
    () =>
      [...filtered].sort((a, b) => {
        const left = Date.parse(a.started_at);
        const right = Date.parse(b.started_at);
        if (Number.isNaN(left) || Number.isNaN(right)) return 0;
        return right - left;
      }),
    [filtered],
  );

  const stats = useMemo<ToolStat[]>(() => {
    const map = new Map<string, ToolStat>();
    for (const invocation of filtered) {
      const entry = map.get(invocation.tool_name) ?? { tool: invocation.tool_name, total: 0, errors: 0 };
      entry.total += 1;
      if (isError(invocation.status)) entry.errors += 1;
      map.set(invocation.tool_name, entry);
    }
    return [...map.values()].sort((a, b) => b.total - a.total || a.tool.localeCompare(b.tool));
  }, [filtered]);

  const statsTotal = stats.reduce((sum, item) => sum + item.total, 0);
  const statsErrors = stats.reduce((sum, item) => sum + item.errors, 0);
  const statsMax = stats.reduce((max, item) => Math.max(max, item.total), 0);

  const totalPages = Math.max(1, Math.ceil(ordered.length / size));
  const safePage = Math.min(page, totalPages - 1);
  const visible = ordered.slice(safePage * size, safePage * size + size);
  const rangeStart = ordered.length === 0 ? 0 : safePage * size + 1;
  const rangeEnd = safePage * size + visible.length;

  if (invocations.length === 0) {
    return (
      <EmptyState
        variant="card"
        icon={<Terminal className="h-6 w-6" />}
        title={t('missions.toolCallsBoard.emptyTitle')}
        description={t('missions.toolCallsBoard.emptyDescription')}
      />
    );
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center gap-2">
        <div className="relative w-full sm:w-72">
          <Search className="absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={query}
            onChange={(event) => {
              setQuery(event.target.value);
              setPage(0);
            }}
            placeholder={t('missions.toolCallsBoard.searchPlaceholder')}
            className="h-8 pl-8"
            aria-label={t('missions.toolCallsBoard.searchPlaceholder')}
          />
        </div>
        <Select
          value={String(size)}
          onValueChange={(value) => {
            setSize(Number(value));
            setPage(0);
          }}
        >
          <SelectTrigger className="h-8 w-28">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {PAGE_SIZES.map((value) => (
              <SelectItem key={value} value={String(value)}>
                {value} / {t('missions.toolCallsBoard.perPage')}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Button variant="outline" size="sm" className="h-8" onClick={() => setStatsOpen(true)}>
          <ChartColumn className="h-3.5 w-3.5" />
          {t('missions.toolCallsBoard.stats')}
        </Button>
        <div className="ml-auto flex items-center gap-2 text-xs text-muted-foreground">
          <span className="tabular-nums">
            {rangeStart}–{rangeEnd} / {ordered.length}
          </span>
          <Button
            variant="outline"
            size="icon"
            className="h-7 w-7"
            disabled={safePage <= 0}
            onClick={() => setPage(Math.max(0, safePage - 1))}
            aria-label={t('missions.toolCallsBoard.prev')}
          >
            <ChevronLeft className="h-3.5 w-3.5" />
          </Button>
          <span className="tabular-nums">
            {safePage + 1} / {totalPages}
          </span>
          <Button
            variant="outline"
            size="icon"
            className="h-7 w-7"
            disabled={safePage + 1 >= totalPages}
            onClick={() => setPage(Math.min(totalPages - 1, safePage + 1))}
            aria-label={t('missions.toolCallsBoard.next')}
          >
            <ChevronRight className="h-3.5 w-3.5" />
          </Button>
        </div>
      </div>

      <Card className="overflow-hidden shadow-xs">
        <div className="max-h-[68vh] min-h-0 overflow-auto">
          <Table>
            <TableHeader className="sticky top-0 z-10 bg-card">
              <TableRow>
                <TableHead className="label-spec w-[140px]">{t('missions.toolCallsBoard.columnTime')}</TableHead>
                <TableHead className="label-spec w-[110px]">{t('missions.toolCallsBoard.columnTask')}</TableHead>
                <TableHead className="label-spec w-[130px]">{t('missions.toolCallsBoard.columnWorker')}</TableHead>
                <TableHead className="label-spec w-[130px]">{t('missions.toolCallsBoard.columnTool')}</TableHead>
                <TableHead className="label-spec">{t('missions.toolCallsBoard.columnInput')}</TableHead>
                <TableHead className="label-spec w-[110px]">{t('missions.toolCallsBoard.columnStatus')}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {ordered.length === 0 ? (
                <TableRow>
                  <TableCell colSpan={6} className="py-12 text-center text-sm text-muted-foreground">
                    {t('missions.toolCallsBoard.noMatch')}
                  </TableCell>
                </TableRow>
              ) : (
                visible.map((invocation) => (
                  <TableRow
                    key={invocation.id}
                    className={cn('cursor-pointer', selected?.id === invocation.id && 'bg-muted/50')}
                    onClick={() => setSelected(invocation)}
                  >
                    <TableCell className="text-xs tabular-nums text-muted-foreground">
                      {formatTime(invocation.started_at)}
                    </TableCell>
                    <TableCell className="font-mono text-[11px] text-muted-foreground">
                      {invocation.task_id ?? invocation.run_id ?? '\u2014'}
                    </TableCell>
                    <TableCell>
                      <Badge tone="neutral" variant="soft">
                        <span className="font-mono">{workerLabel(invocation)}</span>
                      </Badge>
                    </TableCell>
                    <TableCell>
                      <Badge tone="neutral" variant="soft">
                        <span className="font-mono">{invocation.tool_name}</span>
                      </Badge>
                    </TableCell>
                    <TableCell className="max-w-0">
                      <code className="block truncate font-mono text-xs text-foreground">
                        {truncateFirstLine(invocation.input_summary ?? '', INPUT_MAX_LEN)}
                      </code>
                    </TableCell>
                    <TableCell>
                      <ToolInvocationStatusBadge status={invocation.status} />
                    </TableCell>
                  </TableRow>
                ))
              )}
            </TableBody>
          </Table>
        </div>
      </Card>

      {/* 工具调用统计：与表格同一批记录（同筛选、不分页） */}
      <Dialog open={statsOpen} onOpenChange={setStatsOpen}>
        <DialogContent className="max-w-lg">
          <DialogHeader>
            <DialogTitle>{t('missions.toolCallsBoard.statsTitle')}</DialogTitle>
            <DialogDescription>
              {query.trim() !== ''
                ? t('missions.toolCallsBoard.statsScoped')
                : t('missions.toolCallsBoard.statsAll')}
              {stats.length > 0 && (
                <>
                  {' · '}
                  <span className="tabular-nums">{stats.length}</span>{' '}
                  {t('missions.toolCallsBoard.statsTools')} ·{' '}
                  <span className="tabular-nums">{statsTotal}</span>{' '}
                  {t('missions.toolCallsBoard.statsCalls')}
                  {statsErrors > 0 && (
                    <>
                      {' · '}
                      {t('missions.toolCallsBoard.statsErrors')}{' '}
                      <span className="tabular-nums text-danger">{statsErrors}</span>
                    </>
                  )}
                </>
              )}
            </DialogDescription>
          </DialogHeader>

          {stats.length === 0 ? (
            <div className="py-10 text-center text-sm text-muted-foreground">
              {t('missions.toolCallsBoard.statsEmpty')}
            </div>
          ) : (
            <div className="-mr-2 max-h-[55vh] space-y-1 overflow-auto pr-2">
              {stats.map((item) => (
                <div key={item.tool} className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-3 rounded-md p-2">
                  <div className="min-w-0">
                    <div className="flex items-center gap-2">
                      <span className="truncate font-mono text-xs font-medium text-foreground">{item.tool}</span>
                      {item.errors > 0 && (
                        <span className="text-[11px] tabular-nums text-danger">
                          {t('missions.toolCallsBoard.statsErrors')} {item.errors}
                        </span>
                      )}
                    </div>
                    <div className="mt-1.5 h-1.5 overflow-hidden rounded-full bg-muted">
                      <div
                        className="h-full rounded-full bg-primary"
                        style={{ width: `${statsMax > 0 ? (item.total / statsMax) * 100 : 0}%` }}
                      />
                    </div>
                  </div>
                  <div className="text-right">
                    <div className="text-xs font-semibold tabular-nums text-foreground">{item.total}</div>
                    <div className="text-[11px] tabular-nums text-muted-foreground">
                      {statsTotal > 0 ? ((item.total / statsTotal) * 100).toFixed(1) : '0.0'}%
                    </div>
                  </div>
                </div>
              ))}
            </div>
          )}
        </DialogContent>
      </Dialog>

      {/* 单次调用详情：输入 / 输出 */}
      <Drawer
        open={selected !== null}
        onOpenChange={(open) => {
          if (!open) setSelected(null);
        }}
        title={selected ? selected.tool_name : ''}
        description={selected ? formatTime(selected.started_at) : ''}
        icon={<Terminal className="h-4 w-4" />}
        headerActions={
          selected ? <ToolInvocationStatusBadge status={selected.status} /> : undefined
        }
        width="lg"
      >
        {selected && (
          <div className="flex flex-col gap-4 text-sm">
            <div className="flex flex-wrap items-center gap-2">
              <Badge tone="neutral" variant="soft">
                <span className="font-mono">{selected.task_id ?? selected.run_id ?? '\u2014'}</span>
              </Badge>
              <Badge tone="neutral" variant="soft">
                <span className="font-mono">{selected.module_id ?? '\u2014'}</span>
              </Badge>
              {selected.branch_id && (
                <Badge tone="neutral" variant="soft">
                  <span className="font-mono">{selected.branch_id}</span>
                </Badge>
              )}
              {selected.duration_ms != null && (
                <Badge tone="neutral" variant="soft">
                  {selected.duration_ms} ms
                </Badge>
              )}
              {selected.exit_code != null && (
                <Badge tone="neutral" variant="soft">
                  exit {selected.exit_code}
                </Badge>
              )}
            </div>

            <DetailBlock title={t('missions.toolCallsBoard.input')}>
              <pre className="whitespace-pre-wrap break-all rounded-md border border-border bg-background p-3 font-mono text-xs text-foreground">
                {selected.input_summary || t('missions.toolCallsBoard.emptyValue')}
              </pre>
            </DetailBlock>

            <DetailBlock title={t('missions.toolCallsBoard.output')}>
              <pre
                className={cn(
                  'whitespace-pre-wrap break-all rounded-md border border-border bg-background p-3 font-mono text-xs',
                  isError(selected.status) ? 'text-danger' : 'text-foreground',
                )}
              >
                {selected.output_summary || selected.error || t('missions.toolCallsBoard.emptyValue')}
              </pre>
            </DetailBlock>

            {(selected.artifact_paths ?? []).length > 0 && (
              <DetailBlock title={t('missions.toolCallsBoard.artifacts')}>
                <div className="space-y-1">
                  {(selected.artifact_paths ?? []).map((path) => (
                    <code
                      key={path}
                      className="block truncate rounded bg-muted px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground"
                    >
                      {path}
                    </code>
                  ))}
                </div>
              </DetailBlock>
            )}
          </div>
        )}
      </Drawer>
    </div>
  );
}

function DetailBlock({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div>
      <div className="mb-1.5 text-xs font-medium text-muted-foreground">{title}</div>
      {children}
    </div>
  );
}
