/**
 * Mission Coverage Map — 资产覆盖图（力导向）。
 *
 * 用 @antv/g6 的 d3-force 力导向布局画「任务 → 根域名 → 子域名 → IP → 服务 →
 * App → 端点」的资产拓扑，节点是带 lucide 图标的实色圆（已测高亮 / 未测灰），
 * 可拖拽、滚轮缩放，孩子按类型分 group 折叠（灰色「⋯」节点，点开看隐藏列表
 * + 展示更多）。
 *
 * 数据模型：后端 `GET /missions/{id}/coverage-graph` 返回节点 + `{from→to}`
 * 父子边，不返回坐标——布局纯前端；节点字段是 key/kind/label/value/asset_type/
 * tested/confidence/*_ids/metadata，所以只区分 已测/未测，没有"范围外"态。
 * kind 多一个 `mission`（合成的树根，渲染成最顶层节点）和 `other`。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useQuery } from '@tanstack/react-query';
import type { Graph as G6Graph } from '@antv/g6';
import {
  AppWindow,
  Building2,
  type LucideIcon,
  Globe,
  Layers,
  Link2,
  RadioTower,
  RefreshCw,
  Server,
  Waypoints,
} from 'lucide-react';

import { Badge, Button, Card, Drawer, EmptyState, ErrorState, LoadingState } from '@/ui/untitled';
import { api } from '@/lib/api';
import { cn } from '@/lib/utils';
import type { CoverageEdge, CoverageNode, CoverageNodeKind } from '@/lib/types';

/* ───────────────────────── Constants ───────────────────────── */

const FOLD_LIMIT = 20;
const FOLD_STEP = 20;

type Kind = CoverageNodeKind;

interface KindMeta {
  labelKey: string;
  icon: LucideIcon;
  iconBg: string;
  hex: string;
  size: number;
}

const KIND_META: Record<Kind, KindMeta> = {
  mission: { labelKey: 'mission', icon: Building2, iconBg: 'bg-slate-500', hex: '#64748b', size: 46 },
  root_domain: { labelKey: 'rootDomain', icon: Globe, iconBg: 'bg-indigo-500', hex: '#6366f1', size: 38 },
  subdomain: { labelKey: 'subdomain', icon: Waypoints, iconBg: 'bg-blue-500', hex: '#3b82f6', size: 30 },
  ip: { labelKey: 'ip', icon: Server, iconBg: 'bg-cyan-600', hex: '#0891b2', size: 28 },
  service: { labelKey: 'service', icon: RadioTower, iconBg: 'bg-amber-500', hex: '#f59e0b', size: 26 },
  app: { labelKey: 'app', icon: AppWindow, iconBg: 'bg-fuchsia-500', hex: '#d946ef', size: 26 },
  endpoint: { labelKey: 'endpoint', icon: Link2, iconBg: 'bg-rose-500', hex: '#f43f5e', size: 20 },
  other: { labelKey: 'other', icon: Layers, iconBg: 'bg-slate-400', hex: '#94a3b8', size: 24 },
};

// G6 节点图标：把 lucide 的 SVG 路径渲染成白色描边 data-URI（白在实色/灰底都清晰）。
function svgUri(inner: string, filled = false): string {
  const attrs = filled
    ? 'fill="#fff" stroke="none"'
    : 'fill="none" stroke="#fff" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"';
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" ${attrs}>${inner}</svg>`;
  return `data:image/svg+xml,${encodeURIComponent(svg)}`;
}

const KIND_ICON: Record<Kind, string> = {
  mission: svgUri(
    '<path d="M10 12h4"/><path d="M10 8h4"/><path d="M14 21v-3a2 2 0 0 0-4 0v3"/><path d="M6 10H4a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V9a2 2 0 0 0-2-2h-2"/><path d="M6 21V5a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v16"/>',
  ),
  root_domain: svgUri(
    '<circle cx="12" cy="12" r="10"/><path d="M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20"/><path d="M2 12h20"/>',
  ),
  subdomain: svgUri(
    '<path d="m10.586 5.414-5.172 5.172"/><path d="m18.586 13.414-5.172 5.172"/><path d="M6 12h12"/><circle cx="12" cy="20" r="2"/><circle cx="12" cy="4" r="2"/><circle cx="20" cy="12" r="2"/><circle cx="4" cy="12" r="2"/>',
  ),
  ip: svgUri(
    '<rect width="20" height="8" x="2" y="2" rx="2" ry="2"/><rect width="20" height="8" x="2" y="14" rx="2" ry="2"/><line x1="6" x2="6.01" y1="6" y2="6"/><line x1="6" x2="6.01" y1="18" y2="18"/>',
  ),
  service: svgUri(
    '<path d="M16.247 7.761a6 6 0 0 1 0 8.478"/><path d="M19.075 4.933a10 10 0 0 1 0 14.134"/><path d="M4.925 19.067a10 10 0 0 1 0-14.134"/><path d="M7.753 16.239a6 6 0 0 1 0-8.478"/><circle cx="12" cy="12" r="2"/>',
  ),
  app: svgUri(
    '<rect x="2" y="4" width="20" height="16" rx="2"/><path d="M10 4v4"/><path d="M2 8h20"/><path d="M6 4v4"/>',
  ),
  endpoint: svgUri(
    '<path d="M9 17H7A5 5 0 0 1 7 7h2"/><path d="M15 7h2a5 5 0 1 1 0 10h-2"/><line x1="8" x2="16" y1="12" y2="12"/>',
  ),
  other: svgUri(
    '<path d="m12.83 2.18a2 2 0 0 0-1.66 0L2.6 6.08a1 1 0 0 0 0 1.83l8.58 3.91a2 2 0 0 0 1.66 0l8.58-3.9a1 1 0 0 0 0-1.83Z"/><path d="m22 17.65-9.17 4.16a2 2 0 0 1-1.66 0L2 17.65"/><path d="m22 12.65-9.17 4.16a2 2 0 0 1-1.66 0L2 12.65"/>',
  ),
};

const FOLD_ICON = svgUri(
  '<circle cx="5" cy="12" r="1.6"/><circle cx="12" cy="12" r="1.6"/><circle cx="19" cy="12" r="1.6"/>',
  true,
);

/* ───────────────────────── 折叠：全图 → 可见集 ───────────────────────── */

interface FoldNode {
  fold: true;
  key: string;
  groupId: string;
  parentKey: string;
  kind: Kind;
  hidden: CoverageNode[];
}
interface AssetNode {
  fold: false;
  key: string;
  kind: Kind;
  node: CoverageNode;
}
type RenderNode = AssetNode | FoldNode;

function sortChildren(a: CoverageNode, b: CoverageNode): number {
  if (a.tested !== b.tested) return a.tested ? -1 : 1;
  return (a.label || '').localeCompare(b.label || '');
}

function computeVisible(
  nodes: CoverageNode[],
  edges: CoverageEdge[],
  expanded: Map<string, number>,
): { renderNodes: RenderNode[]; renderEdges: { from: string; to: string }[] } {
  const byKey = new Map(nodes.map((n) => [n.key, n]));
  const parentOf = new Map<string, string>();
  const childrenOf = new Map<string, string[]>();
  for (const e of edges) {
    if (!byKey.has(e.from) || !byKey.has(e.to)) continue;
    if (!parentOf.has(e.to)) parentOf.set(e.to, e.from);
    const arr = childrenOf.get(e.from);
    if (arr) arr.push(e.to);
    else childrenOf.set(e.from, [e.to]);
  }

  const renderNodes: RenderNode[] = [];
  const visible = new Set<string>();
  const foldNodes: FoldNode[] = [];

  const queue: string[] = [];
  for (const n of nodes) {
    if (!parentOf.has(n.key)) {
      queue.push(n.key);
      visible.add(n.key);
    }
  }

  for (let qi = 0; qi < queue.length; qi++) {
    const parent = queue[qi];
    const kids = childrenOf.get(parent) ?? [];
    if (kids.length === 0) continue;
    const groups = new Map<Kind, CoverageNode[]>();
    for (const ck of kids) {
      const child = byKey.get(ck);
      if (!child) continue;
      const g = groups.get(child.kind);
      if (g) g.push(child);
      else groups.set(child.kind, [child]);
    }
    for (const [kind, arr] of groups) {
      arr.sort(sortChildren);
      const groupId = `${parent}::${kind}`;
      const shown = expanded.get(groupId) ?? FOLD_LIMIT;
      const visibleChildren = arr.slice(0, shown);
      const hidden = arr.slice(shown);
      for (const c of visibleChildren) {
        if (!visible.has(c.key)) {
          visible.add(c.key);
          queue.push(c.key);
        }
      }
      if (hidden.length > 0) {
        foldNodes.push({ fold: true, key: `fold:${groupId}`, groupId, parentKey: parent, kind, hidden });
      }
    }
  }

  for (const n of nodes) {
    if (visible.has(n.key)) renderNodes.push({ fold: false, key: n.key, kind: n.kind, node: n });
  }
  const renderEdges = edges
    .filter((e) => visible.has(e.from) && visible.has(e.to))
    .map((e) => ({ from: e.from, to: e.to }));
  for (const f of foldNodes) {
    renderNodes.push(f);
    renderEdges.push({ from: f.parentKey, to: f.key });
  }
  return { renderNodes, renderEdges };
}

/* ───────────────────────── G6 数据映射 ───────────────────────── */

interface G6NodeDatum {
  id: string;
  kind: Kind;
  fold: boolean;
  tested: boolean;
  lbl: string;
  size: number;
  // G6 的 NodeData 带字符串索引签名；这里补一个 unknown 索引以满足其契约，
  // 让本结构可直接作为 graph data 传入（具体字段仍受上面强类型约束）。
  [key: string]: unknown;
}

function trunc(s: string, n = 26): string {
  return s.length > n ? `${s.slice(0, n - 1)}…` : s;
}

function graphLabel(n: CoverageNode): string {
  if (n.kind === 'endpoint') {
    try {
      return new URL(n.value || n.label).pathname || '/';
    } catch {
      /* 非法 URL：回退完整 label */
    }
  }
  return n.label;
}

function toG6Nodes(renderNodes: RenderNode[]): G6NodeDatum[] {
  return renderNodes.map((rn) => {
    if (rn.fold) {
      return {
        id: rn.key,
        kind: rn.kind,
        fold: true,
        tested: false,
        lbl: `还有 ${rn.hidden.length} 个${t0(KIND_META[rn.kind].labelKey)}`,
        size: 24,
      };
    }
    return {
      id: rn.key,
      kind: rn.kind,
      fold: false,
      tested: rn.node.tested,
      lbl: trunc(graphLabel(rn.node) || t0(KIND_META[rn.kind].labelKey)),
      size: KIND_META[rn.kind].size,
    };
  });
}

// 折叠节点标签里的类型名要本地化；这里用一个极简查找，避免把 t 传进纯函数。
function t0(labelKey: string): string {
  const map: Record<string, string> = {
    mission: '任务',
    rootDomain: '根域名',
    subdomain: '子域名',
    ip: 'IP',
    service: '服务',
    app: 'App',
    endpoint: '端点',
    other: '其它',
  };
  return map[labelKey] ?? labelKey;
}

// G6 把 datum 当 unknown 传回回调；这里做一次窄化，省得每个回调都写 as。
function datum(d: unknown): G6NodeDatum {
  return d as G6NodeDatum;
}

const FILL_FOLD = '#f1f5f9';
const FILL_UNTESTED = '#94a3b8';
const STROKE_TESTED = '#0f766e';
const STROKE_FOLD = '#94a3b8';
const STROKE_UNTESTED = '#64748b';

function nodeFill(d: G6NodeDatum): string {
  if (d.fold) return FILL_FOLD;
  if (d.tested) return KIND_META[d.kind].hex;
  return FILL_UNTESTED;
}
function nodeStroke(d: G6NodeDatum): string {
  if (d.fold) return STROKE_FOLD;
  if (d.tested) return STROKE_TESTED;
  return STROKE_UNTESTED;
}

/* ───────────────────────── Component ───────────────────────── */

export function MissionCoverageMap({ missionId }: { missionId: string }) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState<Map<string, number>>(new Map());
  const [selected, setSelected] = useState<CoverageNode | null>(null);

  const graphQuery = useQuery({
    queryKey: ['mission-coverage-graph', missionId],
    queryFn: () => api.missionCoverageGraph(missionId),
  });

  const graph = graphQuery.data;
  const { renderNodes, renderEdges } = useMemo(
    () => (graph ? computeVisible(graph.nodes, graph.edges, expanded) : { renderNodes: [], renderEdges: [] }),
    [graph, expanded],
  );

  const containerRef = useRef<HTMLDivElement>(null);
  const graphRef = useRef<G6Graph | null>(null);
  const renderMapRef = useRef<Map<string, RenderNode>>(new Map());
  const dataRef = useRef<{ nodes: G6NodeDatum[]; edges: { source: string; target: string }[] }>({
    nodes: [],
    edges: [],
  });

  // G6 数据 + key→RenderNode 映射：纯派生（useMemo），不在 render 期间写 ref。
  const g6data = useMemo(() => {
    const rmap = new Map<string, RenderNode>();
    for (const rn of renderNodes) rmap.set(rn.key, rn);
    return {
      rmap,
      data: {
        nodes: toG6Nodes(renderNodes),
        edges: renderEdges.map((e) => ({ source: e.from, target: e.to })),
      },
    };
  }, [renderNodes, renderEdges]);

  const applyData = useCallback(() => {
    const g = graphRef.current;
    if (!g || g.destroyed) return;
    g.setData(dataRef.current);
    void g.render().catch((err: unknown) => {
      if (!g.destroyed) console.error('[coverage-graph] render:', err);
    });
  }, []);

  // 可见集变化 → 更新 ref + 重灌数据 + 重跑布局。
  useEffect(() => {
    renderMapRef.current = g6data.rmap;
    dataRef.current = g6data.data;
    applyData();
  }, [g6data, applyData]);

  // 建图（一次）。动态 import 避开 SSR 期 window 依赖。
  useEffect(() => {
    let destroyed = false;
    let g: G6Graph | null = null;
    void (async () => {
      const { Graph } = await import('@antv/g6');
      if (destroyed || !containerRef.current) return;
      g = new Graph({
        container: containerRef.current,
        autoResize: true,
        autoFit: 'view',
        background: '#f0f2f7',
        node: {
          style: {
            size: (d: unknown) => datum(d).size,
      fill: (d: unknown) => nodeFill(datum(d)),
      stroke: (d: unknown) => nodeStroke(datum(d)),
            lineWidth: (d: unknown) => (datum(d).fold ? 1 : 1.5),
            lineDash: (d: unknown) => (datum(d).fold ? [3, 3] : [0]),
            iconSrc: (d: unknown) => (datum(d).fold ? FOLD_ICON : KIND_ICON[datum(d).kind]),
            iconWidth: (d: unknown) => Math.max(12, datum(d).size * 0.55),
            iconHeight: (d: unknown) => Math.max(12, datum(d).size * 0.55),
            labelText: (d: unknown) => datum(d).lbl,
            labelFontSize: 10,
            labelPlacement: 'bottom',
            labelFill: '#475569',
            labelBackground: true,
            labelBackgroundFill: 'rgba(255,255,255,0.75)',
            labelBackgroundRadius: 3,
            labelPadding: [1, 3],
          },
        },
        edge: {
          style: { stroke: '#cbd5e1', lineWidth: 1, endArrow: false },
        },
        layout: {
          type: 'd3-force',
          collide: { radius: (d: unknown) => (datum(d).size ? datum(d).size : 20) + 8 },
          link: {
            distance: (edge: unknown) => {
              const s = (edge as { source: string | { id?: string } }).source;
              const srcId = typeof s === 'string' ? s : (s?.id ?? '');
              const src = renderMapRef.current.get(srcId);
              const k = src && !src.fold ? src.kind : 'endpoint';
              return k === 'mission' || k === 'root_domain' ? 120 : 60;
            },
          },
          manyBody: {
            strength: (d: unknown) => (datum(d).kind === 'endpoint' || datum(d).fold ? -60 : -200),
          },
        },
        behaviors: ['drag-element-force', 'drag-canvas', 'zoom-canvas'],
      });
      g.on('node:click', (evt: unknown) => {
        const id = (evt as { target?: { id?: string } }).target?.id;
        if (!id) return;
        const rn = renderMapRef.current.get(id);
        if (!rn) return;
        if (rn.fold) {
          setExpanded((current) => {
            const next = new Map(current);
            next.set(rn.groupId, (current.get(rn.groupId) ?? FOLD_LIMIT) + FOLD_STEP);
            return next;
          });
        } else {
          setSelected(rn.node);
        }
      });
      graphRef.current = g;
      applyData();
    })();
    return () => {
      destroyed = true;
      try {
        g?.stopLayout();
      } catch {
        /* 图可能尚未建成 */
      }
      g?.destroy();
      graphRef.current = null;
    };
  }, [applyData]);

  if (graphQuery.isPending) {
    return <LoadingState card lines={6} />;
  }
  if (graphQuery.isError) {
    return (
      <ErrorState
        title={t('missions.coverage.errorTitle')}
        description={
          graphQuery.error instanceof Error
            ? graphQuery.error.message
            : t('missions.coverage.errorDescription')
        }
        retryLabel={t('common.retry')}
        onRetry={() => void graphQuery.refetch()}
      />
    );
  }
  if (!graph || graph.nodes.length === 0) {
    return (
      <EmptyState
        variant="card"
        icon={<Globe className="h-6 w-6" />}
        title={t('missions.coverage.emptyTitle')}
        description={t('missions.coverage.emptyDescription')}
      />
    );
  }

  const total = graph.stats?.total ?? graph.nodes.length;
  const tested = graph.stats?.tested ?? graph.nodes.filter((n) => n.tested).length;
  const nodeCount = graph.stats?.by_kind
    ? Object.values(graph.stats.by_kind).reduce((sum, value) => sum + value, 0)
    : graph.nodes.length;

  return (
    <div className="flex flex-col gap-4">
      <Card className="overflow-hidden shadow-xs">
        <div className="relative h-[70vh] min-h-[420px] w-full overflow-hidden rounded-xl">
          <div ref={containerRef} className="h-full w-full" />

          {/* 图例 + 统计 + 刷新（叠加层） */}
          <div className="pointer-events-auto absolute left-3 top-3 flex max-w-[320px] flex-col gap-2.5 rounded-lg border border-border bg-card/95 p-3 text-xs shadow-sm backdrop-blur">
            <div className="flex items-center justify-between gap-3">
              <span className="text-muted-foreground">
                {t('missions.coverage.inScope')}{' '}
                <span className="font-semibold tabular-nums text-foreground">{total}</span>
                {' · '}
                {t('missions.coverage.tested')}{' '}
                <span className="font-semibold tabular-nums text-emerald-600 dark:text-emerald-400">{tested}</span>
                {' · '}
                {t('missions.coverage.nodes')}{' '}
                <span className="font-semibold tabular-nums text-foreground">{nodeCount}</span>
              </span>
              <Button
                variant="ghost"
                size="icon"
                className="pointer-events-auto h-6 w-6"
                onClick={() => void graphQuery.refetch()}
                title={t('missions.coverage.reset')}
              >
                <RefreshCw className={cn('h-3.5 w-3.5', graphQuery.isFetching && 'animate-spin')} />
              </Button>
            </div>
            <div className="flex flex-wrap gap-x-3 gap-y-1.5">
              {(Object.keys(KIND_META) as Kind[]).map((kind) => {
                const meta = KIND_META[kind];
                const Icon = meta.icon;
                return (
                  <span key={kind} className="inline-flex items-center gap-1.5 text-foreground">
                    <span className={cn('flex h-4 w-4 items-center justify-center rounded', meta.iconBg)}>
                      <Icon className="h-2.5 w-2.5 text-white" />
                    </span>
                    {t(`missions.assetBoard.kinds.${meta.labelKey}`)}
                  </span>
                );
              })}
            </div>
            <div className="flex flex-wrap gap-x-3 gap-y-1.5 border-t border-border/60 pt-2 text-muted-foreground">
              <span className="inline-flex items-center gap-1.5">
                <span className="h-3 w-3 rounded-full bg-emerald-500" /> {t('missions.coverage.legendTested')}
              </span>
              <span className="inline-flex items-center gap-1.5">
                <span className="h-3 w-3 rounded-full bg-slate-400" /> {t('missions.coverage.legendUntested')}
              </span>
              <span className="inline-flex items-center gap-1.5">
                <span className="h-3 w-3 rounded border border-dashed border-slate-400 bg-slate-200" />{' '}
                {t('missions.coverage.legendFold')}
              </span>
            </div>
            <p className="border-t border-border/60 pt-2 leading-relaxed text-muted-foreground/80">
              {t('missions.coverage.hint')}
            </p>
          </div>
        </div>
      </Card>

      <Drawer
        open={selected !== null}
        onOpenChange={(open) => {
          if (!open) setSelected(null);
        }}
        title={selected?.label ?? ''}
        description={selected ? t(`missions.assetBoard.kinds.${KIND_META[selected.kind].labelKey}`) : ''}
        icon={<Globe className="h-4 w-4" />}
        headerActions={
          selected ? (
            <Badge tone={selected.tested ? 'success' : 'neutral'} variant="soft" dot>
              {selected.tested ? t('missions.coverage.tested') : t('missions.coverage.untested')}
            </Badge>
          ) : undefined
        }
        width="md"
      >
        {selected && (
          <div className="space-y-3 text-sm">
            <Row label={t('missions.assetBoard.columns.type')}>
              <Badge tone="neutral" variant="soft">
                {selected.asset_type ?? selected.kind}
              </Badge>
            </Row>
            <Row label={t('missions.assetBoard.detail.label')}>
              <span className="break-all text-foreground">{selected.value || selected.label}</span>
            </Row>
            {selected.confidence != null && (
              <Row label={t('missions.assetBoard.detail.confidence')}>
                <span className="tabular-nums text-foreground">{selected.confidence}</span>
              </Row>
            )}
            <Row label={t('missions.coverage.associatedFindings')}>
              <span className="tabular-nums text-foreground">{selected.finding_ids.length}</span>
            </Row>
            <Row label={t('missions.coverage.associatedEvidence')}>
              <span className="tabular-nums text-foreground">{selected.evidence_ids.length}</span>
            </Row>
            <Row label={t('missions.coverage.associatedTools')}>
              <span className="tabular-nums text-foreground">{selected.tool_invocation_ids.length}</span>
            </Row>
            {selected.finding_ids.length > 0 && (
              <div>
                <div className="mb-1 text-xs font-medium text-muted-foreground">
                  {t('missions.coverage.findingIds')}
                </div>
                <div className="space-y-1">
                  {selected.finding_ids.map((id) => (
                    <code
                      key={id}
                      className="block truncate rounded bg-muted px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground"
                    >
                      {id}
                    </code>
                  ))}
                </div>
              </div>
            )}
            <div className="border-t border-border pt-3">
              <div className="mb-1.5 text-xs font-medium text-muted-foreground">
                {t('missions.assetBoard.detail.metadata')}
              </div>
              <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-all rounded-md border border-border bg-background p-3 font-mono text-[11px] text-foreground">
                {JSON.stringify(selected.metadata ?? {}, null, 2)}
              </pre>
            </div>
          </div>
        )}
      </Drawer>
    </div>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-start gap-3">
      <span className="w-24 shrink-0 text-xs text-muted-foreground">{label}</span>
      <span className="min-w-0 flex-1">{children}</span>
    </div>
  );
}
