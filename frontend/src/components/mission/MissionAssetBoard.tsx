/**
 * Mission Asset Board — 资产页。
 *
 * 资产页按「根域名 / IP / 子域名 / 应用 / 服务 / 接口」六类分桶，
 * 每类一张专用列的表格，顶部类型 tab 带计数，底部统一分页。Lynceus 的
 * `MissionAsset` 只有扁平的 `asset_type`（19 种 wire 值），所以：
 *
 * - 桶映射见 `asset-kinds.ts`（六类 + 「其他」兜底，未覆盖类型不丢）；
 * - 每类的专用列从 `metadata` 兜底读取（后端 `runtime/assets.rs` 只保证
 *   写入 `host` / `port` / `service` / `method` / `fact_kind` 等键），
 *   取不到就显示「—」，不编造字段；
 * - 没有后端「新增/移出任务资产」端点，因此新增 Sheet 与移出
 *   确认框未移植，改为只读检视 + 详情抽屉。
 */
import { useMemo, useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import {
  ChevronLeft,
  ChevronRight,
  Database,
  FileSearch,
  KeyRound,
  Search,
  ShieldAlert,
  TerminalSquare,
} from 'lucide-react';

import {
  ASSET_GROUPS,
  assetGroup,
  assetGroupOf,
  assetPrimaryValue,
  assetSecondaryValue,
  metaNumber,
  metaString,
  metaStringList,
  rootDomainOf,
  type AssetGroupKey,
} from './asset-kinds';
import {
  Badge,
  Button,
  Card,
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
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
  type StatusTone,
} from '@/ui/untitled';
import type { MissionAsset } from '@/lib/types';

/* ───────────────────────── Constants ───────────────────────── */

const PAGE_SIZES = [25, 50, 100, 200];
const DASH = '\u2014';

/** HTTP 方法徽章配色。 */
const METHOD_COLOR: Record<string, string> = {
  DELETE: 'bg-red-100 text-red-700 dark:bg-red-500/15 dark:text-red-300',
  GET: 'bg-emerald-100 text-emerald-700 dark:bg-emerald-500/15 dark:text-emerald-300',
  HEAD: 'bg-purple-100 text-purple-700 dark:bg-purple-500/15 dark:text-purple-300',
  OPTIONS: 'bg-slate-100 text-slate-600 dark:bg-slate-500/15 dark:text-slate-300',
  PATCH: 'bg-orange-100 text-orange-700 dark:bg-orange-500/15 dark:text-orange-300',
  POST: 'bg-blue-100 text-blue-700 dark:bg-blue-500/15 dark:text-blue-300',
  PUT: 'bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-300',
};

function sensitivityTone(sensitivity: string): StatusTone {
  switch (sensitivity.toLowerCase()) {
    case 'secret':
    case 'credential':
    case 'account':
      return 'danger';
    case 'confidential':
      return 'warning';
    default:
      return 'neutral';
  }
}

function statusTone(code: number): string {
  if (code >= 500) return 'text-red-500';
  if (code >= 400) return 'text-amber-500';
  if (code >= 300) return 'text-blue-500';
  return 'text-emerald-500';
}

function formatBytes(value: number | undefined): string {
  if (value == null || !Number.isFinite(value) || value <= 0) return DASH;
  const units = ['B', 'KB', 'MB', 'GB'];
  const index = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1);
  return `${index === 0 ? value : (value / 1024 ** index).toFixed(1)} ${units[index]}`;
}

/** 从 metadata 取 HTTP 方法（`method` 键，兼容大小写）。 */
function assetMethod(asset: MissionAsset): string {
  const raw = metaString(asset.metadata, 'method') ?? metaString(asset.metadata, 'http_method');
  return raw ? raw.toUpperCase() : '';
}

function MethodBadge({ method }: { method: string }) {
  if (method === '') return <span className="text-xs text-muted-foreground">{DASH}</span>;
  return (
    <span
      className={`inline-block rounded px-1.5 py-0.5 font-mono text-[10px] font-semibold leading-none ${
        METHOD_COLOR[method] ?? 'bg-muted text-muted-foreground'
      }`}
    >
      {method}
    </span>
  );
}

/** 标签 chips（mono 用于域名/端口/参数这类机读值）。 */
function Chips({ items, mono = false }: { items: string[]; mono?: boolean }) {
  const clean = items.filter((item) => item !== '');
  if (clean.length === 0) return <span className="text-xs text-muted-foreground">{DASH}</span>;
  return (
    <div className="flex flex-wrap gap-1">
      {clean.map((item) => (
        <Badge key={item} tone="neutral" variant="soft" size="sm">
          <span className={mono ? 'font-mono' : undefined}>{item}</span>
        </Badge>
      ))}
    </div>
  );
}

function MonoCell({ value, title }: { value: string; title?: string }) {
  if (value === '') return <span className="text-xs text-muted-foreground">{DASH}</span>;
  return (
    <span className="block max-w-[22rem] truncate font-mono text-xs" title={title ?? value}>
      {value}
    </span>
  );
}

/* ───────────────────────── 列定义 ───────────────────────── */

interface Column {
  key: string;
  header: string;
  width?: string;
  render: (asset: MissionAsset) => ReactNode;
}

function columnsFor(group: AssetGroupKey, t: (key: string) => string): Column[] {
  const c = (key: string) => t(`missions.assetBoard.columns.${key}`);
  const sourceCell = (asset: MissionAsset) => (
    <span className="text-xs text-muted-foreground">{asset.source || DASH}</span>
  );
  const countCell = (ids: string[]) => (
    <span className="text-xs tabular-nums text-muted-foreground">{ids.length}</span>
  );

  switch (group) {
    case 'root_domain':
      return [
        {
          key: 'domain',
          header: c('domain'),
          render: (a) => <MonoCell value={a.value} />,
        },
        { key: 'rootDomain', header: c('rootDomain'), render: (a) => <MonoCell value={rootDomainOf(a.value)} /> },
        { key: 'icp', header: c('icp'), render: (a) => <span className="text-xs">{metaString(a.metadata, 'icp') ?? DASH}</span> },
        { key: 'source', header: c('source'), render: sourceCell },
        { key: 'findings', header: c('findings'), render: (a) => countCell(a.finding_ids) },
      ];
    case 'ip':
      return [
        { key: 'ip', header: c('ip'), render: (a) => <MonoCell value={a.value} /> },
        {
          key: 'cSegment',
          header: c('cSegment'),
          render: (a) => {
            const octets = a.value.split('.');
            return <MonoCell value={octets.length === 4 ? `${octets.slice(0, 3).join('.')}.0/24` : ''} />;
          },
        },
        { key: 'boundDomains', header: c('boundDomains'), render: (a) => <Chips items={metaStringList(a.metadata, 'bound_domains')} mono /> },
        {
          key: 'openPorts',
          header: c('openPorts'),
          render: (a) => (
            <Chips
              mono
              items={(Array.isArray(a.metadata?.open_ports) ? (a.metadata.open_ports as unknown[]) : []).map((item) => {
                if (typeof item === 'number' || typeof item === 'string') return String(item);
                if (item != null && typeof item === 'object') {
                  const record = item as Record<string, unknown>;
                  const port = record.port;
                  const service = typeof record.service === 'string' ? record.service : '';
                  if (port != null) return service ? `${port}/${service}` : String(port);
                }
                return '';
              })}
            />
          ),
        },
        { key: 'source', header: c('source'), render: sourceCell },
      ];
    case 'subdomain':
      return [
        { key: 'host', header: c('host'), render: (a) => <MonoCell value={a.value} /> },
        { key: 'rootDomain', header: c('rootDomain'), render: (a) => <MonoCell value={rootDomainOf(a.value)} /> },
        { key: 'recordType', header: c('recordType'), render: (a) => <span className="text-xs">{metaString(a.metadata, 'record_type') ?? DASH}</span> },
        {
          key: 'recordValue',
          header: c('recordValue'),
          render: (a) => <Chips mono items={metaStringList(a.metadata, 'record_value')} />,
        },
        { key: 'source', header: c('source'), render: sourceCell },
      ];
    case 'app':
      return [
        {
          key: 'app',
          header: c('app'),
          render: (a) => (
            <span className="block max-w-[14rem] truncate text-xs font-medium" title={a.label ?? a.value}>
              {a.label || DASH}
            </span>
          ),
        },
        { key: 'address', header: c('address'), render: (a) => <MonoCell value={a.value} /> },
        { key: 'category', header: c('category'), render: (a) => <span className="text-xs">{metaString(a.metadata, 'category') ?? DASH}</span> },
        { key: 'title', header: c('title'), render: (a) => <span className="block max-w-[14rem] truncate text-xs">{metaString(a.metadata, 'page_title') ?? DASH}</span> },
        { key: 'technologies', header: c('technologies'), render: (a) => <Chips items={metaStringList(a.metadata, 'technologies')} /> },
        { key: 'source', header: c('source'), render: sourceCell },
      ];
    case 'service':
      return [
        {
          key: 'address',
          header: c('address'),
          render: (a) => {
            const host = metaString(a.metadata, 'host') ?? metaString(a.metadata, 'ip') ?? '';
            const port = metaNumber(a.metadata, 'port');
            const service = metaString(a.metadata, 'service');
            const composed = [host, port != null ? String(port) : ''].filter((part) => part !== '').join(':');
            return <MonoCell value={composed || a.value} title={service ? `${service} · ${a.value}` : a.value} />;
          },
        },
        {
          key: 'statusCode',
          header: c('statusCode'),
          render: (a) => {
            const code = metaNumber(a.metadata, 'status_code');
            if (code == null) return <span className="text-xs text-muted-foreground">{DASH}</span>;
            return <span className={`font-mono text-xs font-semibold tabular-nums ${statusTone(code)}`}>{code}</span>;
          },
        },
        { key: 'title', header: c('title'), render: (a) => <span className="block max-w-[14rem] truncate text-xs">{metaString(a.metadata, 'page_title') ?? DASH}</span> },
        {
          key: 'contentLength',
          header: c('contentLength'),
          render: (a) => <span className="text-xs tabular-nums text-muted-foreground">{formatBytes(metaNumber(a.metadata, 'content_length'))}</span>,
        },
        { key: 'technologies', header: c('technologies'), render: (a) => <Chips items={metaStringList(a.metadata, 'technologies')} /> },
        {
          key: 'auth',
          header: c('auth'),
          render: (a) => {
            const items = Array.isArray(a.metadata?.auth) ? (a.metadata.auth as unknown[]) : [];
            if (items.length === 0) return <span className="text-xs text-muted-foreground">{DASH}</span>;
            return (
              <div className="flex flex-col gap-0.5">
                {items.map((item, index) => {
                  const record = (item != null && typeof item === 'object' ? item : {}) as Record<string, unknown>;
                  const type = typeof record.type === 'string' ? record.type : '';
                  const username = typeof record.username === 'string' ? record.username : '';
                  return (
                    <span key={`${a.id}-auth-${index}`} className="inline-flex items-center gap-1 text-[11px]">
                      <KeyRound className="h-3 w-3 shrink-0 text-muted-foreground" />
                      <span className="font-mono">{type || username || 'auth'}</span>
                    </span>
                  );
                })}
              </div>
            );
          },
        },
        { key: 'source', header: c('source'), render: sourceCell },
      ];
    case 'endpoint':
      return [
        { key: 'method', header: c('method'), width: 'w-20', render: (a) => <MethodBadge method={assetMethod(a)} /> },
        { key: 'fullUrl', header: c('fullUrl'), render: (a) => <MonoCell value={a.value} /> },
        {
          key: 'params',
          header: c('params'),
          render: (a) => (
            <Chips
              mono
              items={(Array.isArray(a.metadata?.params) ? (a.metadata.params as unknown[]) : []).map((item) => {
                if (typeof item === 'string') return item;
                if (item != null && typeof item === 'object') {
                  const record = item as Record<string, unknown>;
                  const name = typeof record.name === 'string' ? record.name : '';
                  const location = typeof record.location === 'string' ? record.location : typeof record.in === 'string' ? record.in : '';
                  if (name === '') return '';
                  return location ? `${name}(${location})` : name;
                }
                return '';
              })}
            />
          ),
        },
        { key: 'source', header: c('source'), render: sourceCell },
      ];
    default:
      return [
        {
          key: 'asset',
          header: c('asset'),
          render: (a) => (
            <span className="block max-w-[24rem] truncate text-xs" title={a.value}>
              {assetPrimaryValue(a)}
            </span>
          ),
        },
        {
          key: 'type',
          header: c('type'),
          render: (a) => (
            <Badge tone="neutral" variant="soft" size="sm">
              {a.asset_type}
            </Badge>
          ),
        },
        {
          key: 'sensitivity',
          header: c('sensitivity'),
          render: (a) => (
            <Badge tone={sensitivityTone(a.sensitivity)} variant="soft" size="sm" dot>
              {a.sensitivity}
            </Badge>
          ),
        },
        { key: 'source', header: c('source'), render: sourceCell },
      ];
  }
}

/* ───────────────────────── Props ───────────────────────── */

export interface MissionAssetBoardProps {
  assets: MissionAsset[];
  onViewEvidence?: (evidenceId: string) => void;
  onViewFinding?: (findingId: string) => void;
  onViewToolInvocation?: (toolId: string) => void;
}

/* ───────────────────────── Component ───────────────────────── */

export function MissionAssetBoard({
  assets,
  onViewEvidence,
  onViewFinding,
  onViewToolInvocation,
}: MissionAssetBoardProps) {
  const { t } = useTranslation();
  const [group, setGroup] = useState<AssetGroupKey>('root_domain');
  const [query, setQuery] = useState('');
  const [page, setPage] = useState(0);
  const [size, setSize] = useState(50);
  const [selected, setSelected] = useState<MissionAsset | null>(null);

  const counts = useMemo(() => {
    const map = new Map<AssetGroupKey, number>();
    for (const asset of assets) {
      const key = assetGroupOf(asset.asset_type);
      map.set(key, (map.get(key) ?? 0) + 1);
    }
    return map;
  }, [assets]);

  const rows = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return assets.filter((asset) => {
      if (assetGroupOf(asset.asset_type) !== group) return false;
      if (needle === '') return true;
      return [
        asset.value,
        asset.label,
        asset.asset_type,
        asset.source,
        assetPrimaryValue(asset),
      ].some((value) => value?.toLowerCase().includes(needle));
    });
  }, [assets, group, query]);

  const columns = useMemo(() => columnsFor(group, t), [group, t]);
  const total = rows.length;
  const pageCount = Math.max(1, Math.ceil(total / size));
  const safePage = Math.min(page, pageCount - 1);
  const visible = rows.slice(safePage * size, safePage * size + size);
  const rangeStart = total === 0 ? 0 : safePage * size + 1;
  const rangeEnd = safePage * size + visible.length;

  if (assets.length === 0) {
    return (
      <EmptyState
        variant="card"
        icon={<FileSearch className="h-6 w-6" />}
        title={t('missions.assetBoard.emptyAll')}
        description={t('missions.assetBoard.emptyAllDescription')}
      />
    );
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="min-w-0">
          <h2 className="text-sm font-medium text-foreground">{t('missions.assetBoard.title')}</h2>
          <p className="text-xs text-muted-foreground">
            {t('missions.assetBoard.summary', { count: assets.length })}
          </p>
        </div>
      </div>

      <Tabs
        value={group}
        onValueChange={(value) => {
          setGroup(value as AssetGroupKey);
          setPage(0);
        }}
        className="flex flex-col gap-3"
      >
        <div className="min-w-0 overflow-x-auto">
          <TabsList className="h-9 w-max bg-muted/60 p-1">
            {[...ASSET_GROUPS, assetGroup('other')].map((item) => {
              const Icon = item.icon;
              return (
                <TabsTrigger key={item.key} value={item.key} className="gap-1.5">
                  <Icon className="h-3.5 w-3.5" />
                  {t(`missions.assetBoard.kinds.${item.labelKey}`)}
                  <span className="tabular-nums text-muted-foreground">{counts.get(item.key) ?? 0}</span>
                </TabsTrigger>
              );
            })}
          </TabsList>
        </div>

        <div className="relative w-full sm:w-72">
          <Search className="absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={query}
            onChange={(event) => {
              setQuery(event.target.value);
              setPage(0);
            }}
            placeholder={t('missions.assetBoard.searchPlaceholder')}
            className="h-8 pl-8"
            aria-label={t('missions.assetBoard.searchPlaceholder')}
          />
        </div>

        <TabsContent value={group} className="mt-0 outline-none">
          <Card className="overflow-hidden shadow-xs">
            {total === 0 ? (
              <EmptyState
                variant="bare"
                compact
                icon={<FileSearch className="h-5 w-5" />}
                title={t('missions.assetBoard.empty')}
                className="py-16"
              />
            ) : (
              <Table>
                <TableHeader>
                  <TableRow className="bg-muted/40 hover:bg-muted/40">
                    {columns.map((column) => (
                      <TableHead key={column.key} className={`label-spec ${column.width ?? ''}`}>
                        {column.header}
                      </TableHead>
                    ))}
                    <TableHead className="label-spec w-16">{t('missions.assetBoard.columns.actions')}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {visible.map((asset) => (
                    <TableRow
                      key={asset.id}
                      className="cursor-pointer hover:bg-muted/30"
                      onClick={() => setSelected(asset)}
                    >
                      {columns.map((column) => (
                        <TableCell key={column.key} className={column.key === 'method' ? 'w-20' : undefined}>
                          {column.render(asset)}
                        </TableCell>
                      ))}
                      <TableCell>
                        <Button
                          variant="ghost"
                          size="sm"
                          onClick={(event) => {
                            event.stopPropagation();
                            setSelected(asset);
                          }}
                        >
                          {t('missions.assetBoard.columns.actions')}
                        </Button>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            )}
          </Card>
        </TabsContent>
      </Tabs>

      {total > 0 && (
        <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
          <span className="tabular-nums">
            {t('missions.assetBoard.range', { start: rangeStart, end: rangeEnd, total })}
          </span>
          <Select
            value={String(size)}
            onValueChange={(value) => {
              setSize(Number(value));
              setPage(0);
            }}
          >
            <SelectTrigger className="h-7 w-24" aria-label={t('missions.assetBoard.perPage')}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {PAGE_SIZES.map((value) => (
                <SelectItem key={value} value={String(value)}>
                  {value} / {t('missions.assetBoard.perPage')}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          {pageCount > 1 && (
            <div className="ml-auto flex items-center gap-2">
              <Button
                variant="outline"
                size="icon"
                className="h-7 w-7"
                disabled={safePage <= 0}
                onClick={() => setPage(Math.max(0, safePage - 1))}
                aria-label={t('missions.assetBoard.prev')}
              >
                <ChevronLeft className="h-3.5 w-3.5" />
              </Button>
              <span className="tabular-nums">
                {safePage + 1} / {pageCount}
              </span>
              <Button
                variant="outline"
                size="icon"
                className="h-7 w-7"
                disabled={safePage + 1 >= pageCount}
                onClick={() => setPage(Math.min(pageCount - 1, safePage + 1))}
                aria-label={t('missions.assetBoard.next')}
              >
                <ChevronRight className="h-3.5 w-3.5" />
              </Button>
            </div>
          )}
        </div>
      )}

      <Drawer
        open={selected !== null}
        onOpenChange={(open) => {
          if (!open) setSelected(null);
        }}
        title={selected ? assetPrimaryValue(selected) : ''}
        description={selected ? t(`missions.assetBoard.kinds.${assetGroup(assetGroupOf(selected.asset_type)).labelKey}`) : ''}
        icon={<FileSearch className="h-4 w-4" />}
        headerActions={
          selected ? (
            <Badge tone={sensitivityTone(selected.sensitivity)} variant="soft" dot>
              {selected.sensitivity}
            </Badge>
          ) : undefined
        }
        width="lg"
      >
        {selected && (
          <AssetDetail
            asset={selected}
            onViewEvidence={onViewEvidence}
            onViewFinding={onViewFinding}
            onViewToolInvocation={onViewToolInvocation}
          />
        )}
      </Drawer>
    </div>
  );
}

/* ───────────────────────── Drawer ───────────────────────── */

function AssetDetail({
  asset,
  onViewEvidence,
  onViewFinding,
  onViewToolInvocation,
}: {
  asset: MissionAsset;
  onViewEvidence?: (evidenceId: string) => void;
  onViewFinding?: (findingId: string) => void;
  onViewToolInvocation?: (toolId: string) => void;
}) {
  const { t } = useTranslation();
  const metadataEntries = Object.entries(asset.metadata ?? {});
  const secondary = assetSecondaryValue(asset);

  return (
    <div className="space-y-5 text-sm">
      <div className="space-y-3">
        <DetailField label={t('missions.assetBoard.detail.value')}>
          <code className="block break-all rounded bg-muted p-2 font-mono text-[11px] text-foreground">
            {assetPrimaryValue(asset)}
          </code>
        </DetailField>
        {secondary && (
          <DetailField label={t('missions.assetBoard.detail.label')}>
            <span className="text-foreground">{secondary}</span>
          </DetailField>
        )}
        <div className="grid grid-cols-2 gap-4">
          <DetailField label={t('missions.assetBoard.columns.type')}>
            <Badge tone="neutral" variant="soft">
              {asset.asset_type}
            </Badge>
          </DetailField>
          <DetailField label={t('missions.assetBoard.detail.confidence')}>
            <span className="tabular-nums text-foreground">{asset.confidence}</span>
          </DetailField>
          <DetailField label={t('missions.assetBoard.columns.source')}>
            <span className="text-foreground">{asset.source || '\u2014'}</span>
          </DetailField>
          <DetailField label={t('missions.assetBoard.detail.sourceKind')}>
            <span className="font-mono text-[11px] text-muted-foreground">{asset.source_id ?? '\u2014'}</span>
          </DetailField>
          <DetailField label={t('missions.assetBoard.detail.discoveredAt')}>
            <span className="text-xs text-muted-foreground">{formatTimestamp(asset.created_at)}</span>
          </DetailField>
          <DetailField label={t('missions.assetBoard.detail.updatedAt')}>
            <span className="text-xs text-muted-foreground">{formatTimestamp(asset.updated_at)}</span>
          </DetailField>
        </div>
      </div>

      <div className="space-y-4 border-t border-border pt-4">
        <AssociationList
          label={t('missions.assetBoard.detail.evidence')}
          ids={asset.evidence_ids}
          onView={onViewEvidence}
          icon={<Database className="h-3.5 w-3.5 text-muted-foreground" />}
        />
        <AssociationList
          label={t('missions.assetBoard.detail.findings')}
          ids={asset.finding_ids}
          onView={onViewFinding}
          icon={<ShieldAlert className="h-3.5 w-3.5 text-muted-foreground" />}
        />
        <AssociationList
          label={t('missions.assetBoard.detail.toolCalls')}
          ids={asset.tool_invocation_ids}
          onView={onViewToolInvocation}
          icon={<TerminalSquare className="h-3.5 w-3.5 text-muted-foreground" />}
        />
      </div>

      <div className="border-t border-border pt-4">
        <div className="mb-2 text-xs font-medium text-muted-foreground">
          {t('missions.assetBoard.detail.metadata')}
          {metadataEntries.length > 0 && ` · ${metadataEntries.length}`}
        </div>
        {metadataEntries.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t('missions.assetBoard.detail.noMetadata')}</p>
        ) : (
          <div className="space-y-1.5">
            {metadataEntries.map(([key, value]) => (
              <div key={key} className="flex flex-wrap items-baseline gap-2 text-xs">
                <code className="shrink-0 rounded bg-muted px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground">
                  {key}
                </code>
                <code className="min-w-0 flex-1 break-all font-mono text-[11px] text-foreground">
                  {typeof value === 'string' ? value : JSON.stringify(value)}
                </code>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function DetailField({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div>
      <div className="mb-1 text-xs font-medium text-muted-foreground">{label}</div>
      {children}
    </div>
  );
}

function AssociationList({
  label,
  ids,
  onView,
  icon,
}: {
  label: string;
  ids: string[];
  onView?: (id: string) => void;
  icon: ReactNode;
}) {
  return (
    <div>
      <div className="mb-1.5 flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
        {icon}
        {label}
        <span className="rounded-full bg-muted px-1.5 py-0.5 text-[10px] font-semibold tabular-nums">
          {ids.length}
        </span>
      </div>
      {ids.length === 0 ? (
        <p className="text-xs text-muted-foreground">{'\u2014'}</p>
      ) : (
        <div className="space-y-1">
          {ids.map((id) => (
            <button
              key={id}
              type="button"
              disabled={!onView}
              onClick={() => onView?.(id)}
              className="flex w-full items-center gap-2 rounded-md border border-border bg-card px-2 py-1.5 text-left transition-colors hover:bg-muted/50 disabled:cursor-default disabled:opacity-60 disabled:hover:bg-transparent"
            >
              <code className="truncate text-[11px] text-muted-foreground">{id}</code>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function formatTimestamp(value: string): string {
  const parsed = Date.parse(value);
  if (Number.isNaN(parsed)) return value;
  return new Date(parsed).toLocaleString('zh-CN');
}
