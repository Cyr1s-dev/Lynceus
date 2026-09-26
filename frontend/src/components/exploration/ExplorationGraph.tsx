/**
 * 探索链路图 —— 基于 @xyflow/react 画布的有向链路可视化。
 *
 *
 * 节点六类：起点(begin)/目标(goal)/意图(intent)/事实(fact)/提示(hint)/漏洞(finding)；
 * 边四类：派生(spawns 绿)/意图链(derived_from 靛)/产出(yields 琥珀)/证明(proves 红)。
 * 布局为 同款算法：最长路径分层（周期安全 DFS 断环）+ 重心排序扫掠 4 轮，
 * 推导链从左向右阅读；节点卡为 Dify 风格（图标签 + 标签 + 优先级 + 摘要 + 状态）。
 * 画布带 fitView / 平移缩放 / 点阵背景 / 控件 / 小地图 / 左上角图例 Panel，
 * 点击节点弹出右侧详情抽屉（属性 + 原始 JSON 复制）。
 *
 * 适配点（与既有 model 的差异，非样式差异）：
 * - 我们的节点模型无 digest/task 类型、无 payload JSON（summary 为纯文本），
 *   故未移植 digest 折叠 collapseDigestGraph 与 nodeSummary 字段提取；
 * - 详情抽屉用本仓库设计系统 Drawer（ 用裸 Sheet+ScrollArea）；
 * - 文案走 react-i18next（ 为硬编码中文）；
 * - 数据获取/20s 轮询/加载/错误态由本文件的 ExplorationGraphCanvas 外壳负责
 *   （由页面层传 nodes/edges），视图位置与视口由 React Flow 自行保留。
 */
import { useEffect, useMemo, useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import {
  Background,
  BackgroundVariant,
  Controls,
  type Edge as RFEdge,
  Handle,
  MarkerType,
  MiniMap,
  type Node as RFNode,
  type NodeProps,
  Panel,
  Position,
  ReactFlow,
  ReactFlowProvider,
  useEdgesState,
  useNodesState,
} from '@xyflow/react';
import '@xyflow/react/dist/style.css';
import {
  Bug,
  Compass,
  Copy,
  Flag,
  FlaskConical,
  Lightbulb,
  type LucideIcon,
  Loader2,
  Target,
} from 'lucide-react';

import { api, getApiErrorMessage } from '@/lib/api';
import type { ExploreKind, ExplorationEdge, ExplorationNode } from '@/lib/types';
import { cn } from '@/lib/utils';
import { useToast } from '@/hooks/use-toast';
import { Drawer, ErrorState } from '@/ui/untitled';

/** 边类型中文标签。 */
const REL_LABEL: Record<string, string> = {
  spawns: '派生',
  derived_from: '意图链',
  yields: '产出',
  proves: '证明',
};

/** 边类型颜色。 */
const REL_COLOR: Record<string, string> = {
  spawns: '#10b981',
  derived_from: '#6366f1',
  yields: '#f59e0b',
  proves: '#ef4444',
};

type TypeMeta = {
  label: string;
  Icon: LucideIcon;
  iconBg: string;
  hex: string;
};

/** 六类节点元数据：图标 + 图标底色 + 小地图取色。 */
const TYPE_META: Record<Exclude<ExploreKind, 'task'>, TypeMeta> = {
  begin: { label: '起点', Icon: Flag, iconBg: 'bg-slate-500', hex: '#64748b' },
  goal: { label: '目标', Icon: Target, iconBg: 'bg-emerald-500', hex: '#10b981' },
  intent: { label: '意图', Icon: Compass, iconBg: 'bg-blue-500', hex: '#3b82f6' },
  fact: { label: '事实', Icon: FlaskConical, iconBg: 'bg-amber-500', hex: '#f59e0b' },
  hint: { label: '提示', Icon: Lightbulb, iconBg: 'bg-violet-500', hex: '#8b5cf6' },
  finding: { label: '漏洞', Icon: Bug, iconBg: 'bg-rose-500', hex: '#f43f5e' },
};

const NODE_KINDS: Array<Exclude<ExploreKind, 'task'>> = [
  'begin',
  'goal',
  'intent',
  'fact',
  'hint',
  'finding',
];

const COL_W = 360;
const ROW_H = 140;

function pushMap(m: Map<string, string[]>, k: string, v: string) {
  const a = m.get(k);
  if (a) a.push(v);
  else m.set(k, [v]);
}

/**
 * 自动分层布局：沿边最长路径定列（周期安全 DFS 断环），重心排序扫掠 4 轮定行，
 * origin → … → finding 从左向右阅读。与 布局计算 同算法同参数。
 */
function computeLayout(
  nodes: ExplorationNode[],
  edges: ExplorationEdge[],
): Map<string, { x: number; y: number }> {
  const order = nodes.map((n) => n.id);
  const idset = new Set(order);
  const valid = edges.filter((e) => idset.has(e.src) && idset.has(e.dst));
  const key = (s: string, d: string) => `${s}->${d}`;

  const out = new Map<string, string[]>();
  for (const e of valid) pushMap(out, e.src, e.dst);

  const seen = new Map<string, 0 | 1 | 2>();
  const back = new Set<string>();
  for (const root of order) {
    if ((seen.get(root) ?? 0) !== 0) continue;
    const stack: Array<{ u: string; i: number }> = [{ u: root, i: 0 }];
    seen.set(root, 1);
    while (stack.length) {
      const top = stack[stack.length - 1];
      const nbrs = out.get(top.u) ?? [];
      if (top.i >= nbrs.length) {
        seen.set(top.u, 2);
        stack.pop();
        continue;
      }
      const v = nbrs[top.i++];
      const s = seen.get(v) ?? 0;
      if (s === 1) back.add(key(top.u, v));
      else if (s === 0) {
        seen.set(v, 1);
        stack.push({ u: v, i: 0 });
      }
    }
  }

  const dagSucc = new Map<string, string[]>();
  const indeg = new Map<string, number>(order.map((id) => [id, 0]));
  for (const e of valid) {
    if (back.has(key(e.src, e.dst))) continue;
    pushMap(dagSucc, e.src, e.dst);
    indeg.set(e.dst, (indeg.get(e.dst) ?? 0) + 1);
  }
  const layer = new Map<string, number>(order.map((id) => [id, 0]));
  const queue = order.filter((id) => (indeg.get(id) ?? 0) === 0);
  for (let qi = 0; qi < queue.length; qi++) {
    const u = queue[qi];
    for (const v of dagSucc.get(u) ?? []) {
      layer.set(v, Math.max(layer.get(v) ?? 0, (layer.get(u) ?? 0) + 1));
      const d = (indeg.get(v) ?? 0) - 1;
      indeg.set(v, d);
      if (d === 0) queue.push(v);
    }
  }

  const maxLayer = Math.max(0, ...layer.values());
  const columns: string[][] = Array.from({ length: maxLayer + 1 }, () => []);
  for (const id of order) columns[layer.get(id) ?? 0].push(id);

  const colIndexOf = new Map<string, number>();
  columns.forEach((ids, c) => ids.forEach((id) => colIndexOf.set(id, c)));
  const neighbors = new Map<string, string[]>();
  for (const e of valid) {
    const cs = colIndexOf.get(e.src);
    const cd = colIndexOf.get(e.dst);
    if (cs === undefined || cd === undefined || Math.abs(cs - cd) !== 1) continue;
    pushMap(neighbors, e.src, e.dst);
    pushMap(neighbors, e.dst, e.src);
  }
  const sweep = (dir: 1 | -1) => {
    const start = dir === 1 ? 1 : columns.length - 2;
    const end = dir === 1 ? columns.length : -1;
    for (let c = start; c !== end; c += dir) {
      const ref = c - dir;
      const refIndex = new Map(columns[ref].map((id, i) => [id, i]));
      const scored = columns[c].map((id, i) => {
        const ns = (neighbors.get(id) ?? [])
          .map((nid) => refIndex.get(nid))
          .filter((v): v is number => v !== undefined);
        const bary = ns.length ? ns.reduce((a, b) => a + b, 0) / ns.length : i;
        return { id, bary, i };
      });
      scored.sort((a, b) => a.bary - b.bary || a.i - b.i);
      columns[c] = scored.map((s) => s.id);
    }
  };
  for (let iter = 0; iter < 4; iter++) {
    sweep(1);
    sweep(-1);
  }

  const maxCount = Math.max(1, ...columns.map((c) => c.length));
  const midline = ((maxCount - 1) * ROW_H) / 2;
  const pos = new Map<string, { x: number; y: number }>();
  columns.forEach((ids, c) => {
    const startY = midline - ((ids.length - 1) * ROW_H) / 2;
    ids.forEach((id, i) => pos.set(id, { x: c * COL_W, y: startY + i * ROW_H }));
  });
  return pos;
}

type ExploreNodeData = { node: ExplorationNode };
type ExploreRFNode = RFNode<ExploreNodeData, 'explore'>;

/** Dify 风格节点卡（节点卡 同款；选中蓝框、运行中意图脉冲点 + P 优先级）。 */
function ExploreNode({ data, selected }: NodeProps<ExploreRFNode>) {
  const n = data.node;
  const meta = TYPE_META[n.kind] ?? TYPE_META.intent;
  const Icon = meta.Icon;
  const isLive = n.kind === 'intent' && n.state === 'running';
  const showPriority = (n.kind === 'goal' || n.kind === 'intent') && n.priority > 0;

  return (
    <div
      className={cn(
        'w-[240px] cursor-pointer rounded-2xl border-2 transition-colors',
        selected ? 'border-blue-500' : isLive ? 'border-blue-400/70' : 'border-transparent',
      )}
    >
      <div className="group relative rounded-[14px] border border-black/[0.06] bg-white shadow-sm transition-shadow hover:shadow-lg">
        <Handle
          type="target"
          position={Position.Left}
          className="!size-2 !border-2 !border-neutral-300 !bg-white opacity-0 transition-opacity group-hover:opacity-100"
        />
        <div className="flex items-center gap-2 px-3 pt-3 pb-1.5">
          <span
            className={cn(
              'flex size-6 shrink-0 items-center justify-center rounded-lg shadow-sm',
              meta.iconBg,
            )}
          >
            <Icon className="size-3.5 text-white" />
          </span>
          <span className="grow truncate text-[13px] font-semibold text-neutral-700">
            {meta.label}
          </span>
          {isLive && (
            <span className="relative flex size-2 shrink-0">
              <span className="absolute inline-flex size-full animate-ping rounded-full bg-blue-400 opacity-70" />
              <span className="relative inline-flex size-2 rounded-full bg-blue-500" />
            </span>
          )}
          {showPriority && (
            <span
              className={cn(
                'shrink-0 rounded-md px-1.5 py-0.5 text-[10px] font-semibold tabular-nums',
                n.priority >= 8
                  ? 'bg-red-500/15 text-red-600'
                  : n.priority >= 5
                    ? 'bg-amber-500/15 text-amber-600'
                    : 'bg-slate-500/15 text-slate-600',
              )}
            >
              P{n.priority}
            </span>
          )}
        </div>
        <div className="line-clamp-3 px-3 pb-2 text-[13px] leading-snug text-neutral-500">
          {n.summary || n.title || meta.label}
        </div>
        <div className="flex items-center justify-between gap-2 px-3 pb-2.5">
          {n.state ? (
            <span className="truncate rounded-md bg-muted/60 px-1.5 py-0 text-[10px] text-muted-foreground">
              {n.state}
            </span>
          ) : (
            <span className="text-[10px] text-neutral-400">{meta.label}</span>
          )}
          <span className="shrink-0 font-mono text-[10px] text-neutral-400">#{n.id}</span>
        </div>
        <Handle
          type="source"
          position={Position.Right}
          className="!size-2 !border-2 !border-neutral-300 !bg-white opacity-0 transition-opacity group-hover:opacity-100"
        />
      </div>
    </div>
  );
}

const nodeTypes = { explore: ExploreNode };

function DetailRow({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-start gap-3 py-1.5 text-sm">
      <span className="text-muted-foreground w-14 shrink-0">{label}</span>
      <span className="text-foreground min-w-0 flex-1 break-words">{children}</span>
    </div>
  );
}

/** 节点详情抽屉：属性表 + 原始 JSON 复制（节点详情抽屉 同结构）。 */
function NodeDetailSheet({
  node,
  onOpenChange,
}: {
  node: ExplorationNode | null;
  onOpenChange: (open: boolean) => void;
}) {
  const { t } = useTranslation();
  const { toast } = useToast();
  const [copied, setCopied] = useState(false);
  const raw = useMemo(() => (node ? JSON.stringify(node, null, 2) : ''), [node]);
  const meta = node ? TYPE_META[node.kind] : null;

  const copy = () => {
    void navigator.clipboard?.writeText(raw).then(() => {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
      toast({ title: t('exploration.copied') });
    });
  };

  return (
    <Drawer
      open={node !== null}
      onOpenChange={onOpenChange}
      width="sm"
      icon={
        meta ? (
          <span
            className={cn(
              'flex size-8 shrink-0 items-center justify-center rounded-lg shadow-sm',
              meta.iconBg,
            )}
          >
            <meta.Icon className="size-4 text-white" />
          </span>
        ) : undefined
      }
      title={meta?.label ?? ''}
      description={node ? <span className="font-mono text-xs">#{node.id}</span> : undefined}
    >
      {node && meta && (
        <div className="flex w-full min-w-0 flex-col gap-4">
          <section>
            <h4 className="text-muted-foreground mb-1 text-xs font-medium">
              {t('exploration.properties')}
            </h4>
            <DetailRow label={t('exploration.fieldKind')}>{meta.label}</DetailRow>
            {node.title && <DetailRow label={t('exploration.fieldTitle')}>{node.title}</DetailRow>}
            <DetailRow label={t('exploration.fieldState')}>
              {node.state ? (
                <span className="font-mono text-xs">{node.state}</span>
              ) : (
                <span className="text-muted-foreground">—</span>
              )}
            </DetailRow>
            <DetailRow label={t('exploration.fieldPriority')}>
              <span className="tabular-nums">{node.priority}</span>
            </DetailRow>
            <DetailRow label={t('exploration.fieldOrigin')}>
              <span className="font-mono text-xs">{node.origin}</span>
            </DetailRow>
            <DetailRow label={t('exploration.fieldTime')}>
              {node.ts ? new Date(node.ts).toLocaleString() : '—'}
            </DetailRow>
          </section>

          <section className="border-t border-border pt-3">
            <div className="mb-1.5 flex items-center justify-between">
              <h4 className="text-muted-foreground text-xs font-medium">
                {t('exploration.rawJson')}
              </h4>
              <button
                type="button"
                onClick={copy}
                className="text-muted-foreground hover:text-foreground inline-flex items-center gap-1 text-xs transition-colors"
              >
                <Copy className="h-3 w-3" />
                {copied ? t('exploration.copied') : t('common.copy')}
              </button>
            </div>
            <pre className="bg-muted/50 text-foreground max-w-full overflow-hidden rounded-md border p-3 font-mono text-xs leading-relaxed break-all whitespace-pre-wrap">
              {raw}
            </pre>
          </section>
        </div>
      )}
    </Drawer>
  );
}

/**
 * React Flow 画布：自动布局 + fitView，节点位置在轮询刷新间保留
 * （prevPos 记忆，用户手动拖动不丢）；左上角图例 Panel，点击节点开详情抽屉。
 */
function ExplorationGraphInner({
  nodes,
  edges,
  emptyHint,
}: {
  nodes: ExplorationNode[];
  edges: ExplorationEdge[];
  emptyHint?: string;
}) {
  const [rfNodes, setRfNodes, onNodesChange] = useNodesState<ExploreRFNode>([]);
  const [rfEdges, setRfEdges, onEdgesChange] = useEdgesState<RFEdge>([]);
  const [selected, setSelected] = useState<ExplorationNode | null>(null);
  const nodesById = useMemo(() => new Map(nodes.map((n) => [n.id, n])), [nodes]);

  useEffect(() => {
    setSelected((cur) => (cur ? nodesById.get(cur.id) ?? cur : cur));
    const liveTargets = new Set(
      nodes.filter((n) => n.kind === 'intent' && n.state === 'running').map((n) => n.id),
    );
    const layout = computeLayout(nodes, edges);
    setRfNodes((prev) => {
      const prevPos = new Map(prev.map((p) => [p.id, p.position]));
      return nodes.map((n) => ({
        id: n.id,
        type: 'explore' as const,
        position: prevPos.get(n.id) ?? layout.get(n.id) ?? { x: 0, y: 0 },
        data: { node: n },
        sourcePosition: Position.Right,
        targetPosition: Position.Left,
      }));
    });
    setRfEdges(
      edges.map((e, i) => {
        const color = REL_COLOR[e.rel] ?? '#94a3b8';
        const animated = liveTargets.has(e.dst);
        return {
          id: `e${i}-${e.src}-${e.dst}`,
          source: e.src,
          target: e.dst,
          label: REL_LABEL[e.rel] ?? e.rel,
          type: 'default',
          animated,
          labelShowBg: false,
          labelStyle: { fontSize: 10, fontWeight: 600, fill: color },
          style: { stroke: color, strokeWidth: 2, strokeOpacity: animated ? 0.9 : 0.55 },
          markerEnd: { type: MarkerType.ArrowClosed, color, width: 14, height: 14 },
        } satisfies RFEdge;
      }),
    );
  }, [nodes, edges, nodesById, setRfNodes, setRfEdges]);

  const isEmpty = nodes.length === 0;

  return (
    <>
      <ReactFlow
        nodes={rfNodes}
        edges={rfEdges}
        onNodesChange={onNodesChange}
        onEdgesChange={onEdgesChange}
        onNodeClick={(_, node) => setSelected(node.data.node)}
        nodeTypes={nodeTypes}
        fitView
        fitViewOptions={{ padding: 0.2 }}
        proOptions={{ hideAttribution: true }}
        onlyRenderVisibleElements
        minZoom={0.1}
        className="!bg-[#f0f2f7]"
      >
        <Background
          variant={BackgroundVariant.Dots}
          gap={16}
          size={1}
          className="text-neutral-400/50"
        />
        <Controls
          showInteractive={false}
          className="!rounded-lg !border !shadow-sm [&>button]:!border-border [&>button]:!bg-card [&>button:hover]:!bg-accent [&_svg]:!fill-foreground"
        />
        <MiniMap
          pannable
          zoomable
          className="!bg-card !rounded-lg !border !shadow-sm"
          maskColor="rgb(148 163 184 / 0.18)"
          nodeColor={(node) => {
            const data = node.data as ExploreNodeData | undefined;
            const kind = data?.node ? data.node.kind : 'intent';
            return TYPE_META[kind]?.hex ?? '#94a3b8';
          }}
          nodeStrokeWidth={0}
          nodeBorderRadius={4}
        />
        <Panel position="top-left">
          <div className="bg-card/95 flex flex-col gap-2.5 rounded-lg border p-3 text-xs shadow-sm backdrop-blur">
            {isEmpty && <span className="text-muted-foreground">{emptyHint}</span>}
            <div className="flex flex-wrap gap-x-3 gap-y-1.5">
              {NODE_KINDS.map((k) => {
                const m = TYPE_META[k];
                const Icon = m.Icon;
                return (
                  <span key={k} className="text-foreground inline-flex items-center gap-1.5">
                    <span
                      className={cn('flex size-4 items-center justify-center rounded', m.iconBg)}
                    >
                      <Icon className="size-2.5 text-white" />
                    </span>
                    {m.label}
                  </span>
                );
              })}
            </div>
            <div className="border-border/60 text-muted-foreground flex flex-wrap gap-x-3 gap-y-1.5 border-t pt-2">
              {Object.entries(REL_LABEL).map(([k, v]) => (
                <span key={k} className="inline-flex items-center gap-1.5">
                  <span className="h-0.5 w-4 rounded-full" style={{ backgroundColor: REL_COLOR[k] }} />
                  {v}
                </span>
              ))}
            </div>
          </div>
        </Panel>
      </ReactFlow>
      <NodeDetailSheet node={selected} onOpenChange={(o) => !o && setSelected(null)} />
    </>
  );
}

/** 展示型画布：接收 nodes/edges，外包 ReactFlowProvider。 */
export function ExplorationGraph({
  nodes,
  edges,
  className,
  emptyHint,
}: {
  nodes: ExplorationNode[];
  edges: ExplorationEdge[];
  className?: string;
  emptyHint?: string;
}) {
  return (
    <div className={cn('w-full overflow-hidden rounded-xl', className)}>
      <ReactFlowProvider>
        <ExplorationGraphInner nodes={nodes} edges={edges} emptyHint={emptyHint} />
      </ReactFlowProvider>
    </div>
  );
}

/** 任务详情画布 tab：拉取探索链投影（20s 轮询），加载/错误/空态外壳。 */
export function ExplorationGraphCanvas({
  missionId,
  className,
}: {
  missionId: string;
  className?: string;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();

  const graphQuery = useQuery({
    queryKey: ['exploration-graph', missionId],
    queryFn: () => api.getExplorationGraph(missionId),
    refetchInterval: 20_000,
  });

  if (graphQuery.isLoading) {
    return (
      <div
        className={cn(
          'flex items-center justify-center rounded-xl border border-border bg-muted/20',
          className,
        )}
      >
        <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
      </div>
    );
  }
  if (graphQuery.isError) {
    return (
      <ErrorState
        title={t('exploration.failedToLoad')}
        description={getApiErrorMessage(graphQuery.error)}
        onRetry={() => queryClient.refetchQueries({ queryKey: ['exploration-graph', missionId] })}
        retryLabel={t('common.retry')}
        className={className}
      />
    );
  }

  return (
    <ExplorationGraph
      nodes={graphQuery.data?.nodes ?? []}
      edges={graphQuery.data?.edges ?? []}
      emptyHint={t('exploration.empty')}
      className={className}
    />
  );
}
