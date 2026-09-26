/**
 * Task-forest-live-dag view for the Mission Exploration Canvas.
 *
 * Renders a compact, left-to-right task forest using ReactFlow. Nodes are small
 * (150-190px wide, 64-86px tall) with a left status color bar, title, status
 * text, progress percentage, and a bottom progress bar. Edges are smooth bezier
 * curves with small pill labels. Supports right-click context menus, grid
 * snapping, layout persistence to localStorage, and node aggregation.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import {
  ReactFlow,
  Background,
  BackgroundVariant,
  Controls,
  Handle,
  MiniMap,
  Position,
  type Node,
  type Edge,
  type NodeTypes,
  type ReactFlowInstance,
  useNodesState,
  useEdgesState,
  type NodeMouseHandler,
  type EdgeMouseHandler,
  type OnNodeDrag,
} from '@xyflow/react';
import '@xyflow/react/dist/style.css';
import { cn } from '@/lib/utils';
import { formatStatus } from '@/lib/i18n-formatters';
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Textarea,
} from '@/ui/untitled';
import type {
  MissionGraph,
  MissionGraphNode,
  MissionGraphEdge,
  MissionGraphNodeType,
  AggregateNodeData,
} from '@/lib/missionGraphTypes';
import {
  EDGE_TYPE_STYLES,
} from '@/lib/missionGraphTypes';
import { nodeIcon, nodeColor } from './missionNodeVisuals';

/* ── Constants ── */

const GRID_SIZE = 24;
const NODE_WIDTH = 170;
const NODE_HEIGHT = 76;
const HORIZONTAL_SPACING = 260;
const VERTICAL_SPACING = 104;
const LANE_SPACING = 56;
const AGGREGATE_CHILD_THRESHOLD = 8;
const AGGREGATE_MODE_THRESHOLD = 80;
// Above this node count the DAG falls back to the mission/branch/task skeleton
// unless the user explicitly opts into the full graph via the toolbar toggle.
export const SKELETON_MODE_THRESHOLD = 200;

const SKELETON_TYPES: Set<MissionGraphNodeType> = new Set([
  'mission',
  'branch',
  'exploration_task',
]);

/* ── Status color mapping ── */

function statusBarColor(status?: string): string {
  if (!status) return '#cbd5e1';
  const s = status.toLowerCase();
  if (['running', 'active', 'testing', 'reviewing'].includes(s)) return '#3b82f6';
  if (['pending', 'waiting', 'waiting_for_decision', 'draft'].includes(s)) return '#f97316';
  if (['completed', 'confirmed', 'verified', 'succeeded'].includes(s)) return '#10b981';
  if (['blocked', 'needs_review', 'needs_decision'].includes(s)) return '#f59e0b';
  if (['failed', 'error', 'timeout', 'false_positive', 'dismissed'].includes(s)) return '#ef4444';
  return '#cbd5e1';
}

function nodeProgress(node: MissionGraphNode): number {
  const status = node.status;
  if (!status) return 0;
  const s = status.toLowerCase();
  if (['completed', 'confirmed', 'verified', 'succeeded'].includes(s)) return 100;
  if (['running', 'active', 'testing', 'reviewing'].includes(s)) return 65;
  if (['failed', 'false_positive', 'dismissed'].includes(s)) return 100;
  if (['blocked', 'needs_review'].includes(s)) return 35;
  if (['pending', 'draft', 'waiting', 'waiting_for_decision'].includes(s)) return 15;
  return 0;
}

function snapToGrid(value: number, grid: number): number {
  return Math.round(value / grid) * grid;
}

/* ── Layout persistence ── */

function getLayoutKey(missionId: string): string {
  return `lynceus.missionCanvas.layout.v2.${missionId}`;
}

function loadLayout(missionId: string): Record<string, { x: number; y: number }> {
  try {
    const raw = localStorage.getItem(getLayoutKey(missionId));
    if (!raw) return {};
    const parsed = JSON.parse(raw);
    if (parsed && typeof parsed === 'object') return parsed as Record<string, { x: number; y: number }>;
  } catch {
    // ignore malformed JSON
  }
  return {};
}

function saveLayout(missionId: string, positions: Record<string, { x: number; y: number }>): void {
  try {
    localStorage.setItem(getLayoutKey(missionId), JSON.stringify(positions));
  } catch {
    // localStorage may be full or disabled
  }
}

function clearLayout(missionId: string): void {
  try {
    localStorage.removeItem(getLayoutKey(missionId));
  } catch {
    // ignore
  }
}

const TYPE_LAYER: Partial<Record<MissionGraphNodeType, number>> = {
  mission: 0,
  directive: 0,
  branch: 1,
  exploration_task: 2,
  tool_invocation: 3,
  evidence: 4,
  asset: 4,
  finding: 5,
  decision_gate: 5,
  strategy_board: 5,
  risk: 5,
  gap: 5,
  question: 5,
  follow_up: 5,
};

function semanticLayerForNode(node: MissionGraphNode): number {
  return TYPE_LAYER[node.type] ?? 5;
}

/* ── Auto layout (task forest, left-to-right) ── */

interface LayoutResult {
  nodes: Node[];
  edges: Edge[];
}

function autoLayout(
  graph: MissionGraph,
  collapsed: boolean,
  expandedNodes: Set<string>,
  collapsedNodes: Set<string>,
  onToggleCollapse: (nodeId: string) => void,
  savedLayout: Record<string, { x: number; y: number }>,
  t: TFunction,
  skeletonDisabled: boolean = false,
): LayoutResult {
  const totalCount = graph.nodes.length;
  const skeletonMode = totalCount > SKELETON_MODE_THRESHOLD && !skeletonDisabled;
  const aggregateMode = totalCount > AGGREGATE_MODE_THRESHOLD;

  // Build children adjacency
  const childrenMap = new Map<string, string[]>();
  graph.edges.forEach((edge) => {
    if (!childrenMap.has(edge.source)) childrenMap.set(edge.source, []);
    childrenMap.get(edge.source)!.push(edge.target);
  });

  // Determine which nodes are hidden (collapsed subtrees)
  const hiddenNodeIds = new Set<string>();
  const aggregateNodes: Node[] = [];
  const aggregateEdges: Edge[] = [];
  const traversalVisited = new Set<string>();
  const traversalStack = new Set<string>();
  const missionNode = graph.nodes.find((n) => n.type === 'mission');
  const rootNodeId = missionNode?.id;

  const traverse = (nodeId: string) => {
    if (hiddenNodeIds.has(nodeId)) return;
    if (traversalVisited.has(nodeId)) return;
    if (traversalStack.has(nodeId)) return;

    traversalVisited.add(nodeId);
    traversalStack.add(nodeId);

    const children = (childrenMap.get(nodeId) || []).filter((childId) => {
      // Backend graph edges may include feedback/review links such as
      // directive -> mission. The canvas is a graph, but this layout pass is
      // tree-oriented, so never recurse into the active ancestry path.
      return childId !== nodeId && !traversalStack.has(childId);
    });

    // Filter children in skeleton/aggregate mode
    let visibleChildren = children;
    if (skeletonMode || aggregateMode) {
      visibleChildren = children.filter((childId) => {
        const childNode = graph.nodeIndex.get(childId);
        if (!childNode) return false;
        if (skeletonMode) return SKELETON_TYPES.has(childNode.type);
        return true;
      });
    }

    const isAutoCollapse =
      visibleChildren.length > AGGREGATE_CHILD_THRESHOLD && !expandedNodes.has(nodeId);
    const isManualCollapse = collapsedNodes.has(nodeId);
    const isGlobalCollapse = collapsed && !expandedNodes.has(nodeId);

    if ((isAutoCollapse || isManualCollapse || isGlobalCollapse) && visibleChildren.length > 0) {
      // Hide all children and their subtrees
      const hideSubtree = (childId: string, ancestry: Set<string>) => {
        if (childId === rootNodeId || ancestry.has(childId)) return;
        if (hiddenNodeIds.has(childId)) return;
        hiddenNodeIds.add(childId);
        const grandchildren = childrenMap.get(childId) || [];
        const nextAncestry = new Set(ancestry);
        nextAncestry.add(childId);
        grandchildren.forEach((grandchildId) => hideSubtree(grandchildId, nextAncestry));
      };
      const ancestry = new Set<string>([nodeId]);
      visibleChildren.forEach((childId) => hideSubtree(childId, ancestry));

      // Count child types for aggregate display
      const childTypeCounts: Partial<Record<MissionGraphNodeType, number>> = {};
      visibleChildren.forEach((childId) => {
        const childNode = graph.nodeIndex.get(childId);
        if (childNode) {
          childTypeCounts[childNode.type] = (childTypeCounts[childNode.type] ?? 0) + 1;
        }
      });

      const aggregateId = `${nodeId}-aggregate`;
      aggregateNodes.push({
        id: aggregateId,
        type: 'aggregateNode',
        position: { x: 0, y: 0 },
        data: {
          id: aggregateId,
          parentId: nodeId,
          type: 'aggregate',
          status: 'pending',
          childCount: visibleChildren.length,
          childTypeCounts,
          onToggle: () => onToggleCollapse(nodeId),
        } as AggregateNodeData,
      });

      aggregateEdges.push({
        id: `${nodeId}-to-aggregate`,
        source: nodeId,
        target: aggregateId,
        sourceHandle: 'source',
        targetHandle: 'target',
        type: 'smoothstep',
        style: { stroke: '#cbd5e1', strokeWidth: 1.5, strokeDasharray: '3,3' },
        animated: true,
      });
    } else {
      visibleChildren.forEach(traverse);
    }

    traversalStack.delete(nodeId);
  };

  if (missionNode) {
    traverse(missionNode.id);
  }

  // Filter visible nodes
  let visibleGraphNodes = graph.nodes.filter((node) => !hiddenNodeIds.has(node.id));

  // In skeleton mode, only show skeleton types
  if (skeletonMode) {
    visibleGraphNodes = visibleGraphNodes.filter((node) => SKELETON_TYPES.has(node.type));
  }

  const visibleIds = new Set(visibleGraphNodes.map((node) => node.id));
  const visibleEdges = graph.edges.filter(
    (edge) => visibleIds.has(edge.source) && visibleIds.has(edge.target),
  );

  const positions = new Map<string, { x: number; y: number }>();

  const graphNodeById = new Map(visibleGraphNodes.map((node) => [node.id, node]));
  const incomingMap = new Map<string, string[]>();
  visibleEdges.forEach((edge) => {
    if (!incomingMap.has(edge.target)) incomingMap.set(edge.target, []);
    incomingMap.get(edge.target)!.push(edge.source);
  });

  const resolveBranchKey = (nodeId: string, seen = new Set<string>()): string => {
    if (seen.has(nodeId)) return '__mission__';
    seen.add(nodeId);
    const node = graphNodeById.get(nodeId);
    if (!node) return '__mission__';
    if (node.type === 'branch') return node.id;
    if (typeof node.branch_id === 'string' && graphNodeById.has(node.branch_id)) {
      return node.branch_id;
    }
    if (typeof node.parent_id === 'string' && graphNodeById.has(node.parent_id)) {
      return resolveBranchKey(node.parent_id, seen);
    }
    for (const sourceId of incomingMap.get(nodeId) || []) {
      const key = resolveBranchKey(sourceId, seen);
      if (key !== '__mission__') return key;
    }
    return '__mission__';
  };

  const branchNodes = visibleGraphNodes.filter((node) => node.type === 'branch');
  const branchOrder = branchNodes.map((node) => node.id);
  const semanticGroups = new Map<string, MissionGraphNode[]>();
  for (const node of visibleGraphNodes) {
    if (node.type === 'mission') continue;
    const key = resolveBranchKey(node.id);
    if (!semanticGroups.has(key)) semanticGroups.set(key, []);
    semanticGroups.get(key)!.push(node);
  }

  const groupOrder = [
    ...branchOrder,
    ...[...semanticGroups.keys()].filter((key) => key !== '__mission__' && !branchOrder.includes(key)),
    ...(semanticGroups.has('__mission__') ? ['__mission__'] : []),
  ];

  const rowAnchorByNode = new Map<string, number>();
  const groupCenters: number[] = [];
  let cursorY = 0;

  const sortedByGraphOrder = (items: MissionGraphNode[]) =>
    [...items].sort(
      (a, b) => visibleGraphNodes.findIndex((node) => node.id === a.id) - visibleGraphNodes.findIndex((node) => node.id === b.id),
    );

  for (const groupKey of groupOrder) {
    const groupNodes = sortedByGraphOrder(semanticGroups.get(groupKey) || []);
    const branchNode = graphNodeById.get(groupKey);
    const tasks = groupNodes.filter((node) => node.type === 'exploration_task');
    const tools = groupNodes.filter((node) => node.type === 'tool_invocation');
    const evidenceAndAssets = groupNodes.filter((node) => node.type === 'evidence' || node.type === 'asset');
    const findingsAndDecisions = groupNodes.filter((node) =>
      ['finding', 'decision_gate', 'strategy_board', 'risk', 'gap', 'question', 'follow_up'].includes(node.type),
    );
    const otherNodes = groupNodes.filter((node) =>
      ![
        'branch',
        'exploration_task',
        'tool_invocation',
        'evidence',
        'asset',
        'finding',
        'decision_gate',
        'strategy_board',
        'risk',
        'gap',
        'question',
        'follow_up',
      ].includes(node.type),
    );
    const rowCount = Math.max(
      1,
      tasks.length,
      tools.filter((node) => !node.parent_id || !tasks.some((task) => task.id === node.parent_id)).length,
      evidenceAndAssets.length,
      findingsAndDecisions.length,
      otherNodes.length,
    );
    const groupHeight = Math.max(NODE_HEIGHT, (rowCount - 1) * VERTICAL_SPACING + NODE_HEIGHT);
    const groupCenterY = cursorY + groupHeight / 2 - NODE_HEIGHT / 2;
    groupCenters.push(groupCenterY);

    if (branchNode) {
      positions.set(branchNode.id, { x: semanticLayerForNode(branchNode) * HORIZONTAL_SPACING, y: groupCenterY });
      rowAnchorByNode.set(branchNode.id, groupCenterY);
    }

    tasks.forEach((node, idx) => {
      const y = cursorY + idx * VERTICAL_SPACING;
      positions.set(node.id, { x: semanticLayerForNode(node) * HORIZONTAL_SPACING, y });
      rowAnchorByNode.set(node.id, y);
    });

    const placeColumnNodes = (
      nodesForColumn: MissionGraphNode[],
      fallbackLayer: number,
      preferredParent?: (node: MissionGraphNode) => string | null | undefined,
    ) => {
      const parentCounts = new Map<string, number>();
      const parentIndex = new Map<string, number>();
      nodesForColumn.forEach((node) => {
        const parentId = preferredParent?.(node) ?? node.parent_id;
        if (parentId && rowAnchorByNode.has(parentId)) {
          parentCounts.set(parentId, (parentCounts.get(parentId) ?? 0) + 1);
        }
      });
      nodesForColumn.forEach((node, idx) => {
        const parentId = preferredParent?.(node) ?? node.parent_id;
        let y = cursorY + idx * VERTICAL_SPACING;
        if (parentId && rowAnchorByNode.has(parentId)) {
          const count = parentCounts.get(parentId) ?? 1;
          const used = parentIndex.get(parentId) ?? 0;
          parentIndex.set(parentId, used + 1);
          y = (rowAnchorByNode.get(parentId) ?? y) + (used - (count - 1) / 2) * LANE_SPACING;
        }
        const layer = TYPE_LAYER[node.type] ?? fallbackLayer;
        positions.set(node.id, { x: layer * HORIZONTAL_SPACING, y });
        rowAnchorByNode.set(node.id, y);
      });
    };

    placeColumnNodes(tools, 3);
    placeColumnNodes(evidenceAndAssets, 4);
    placeColumnNodes(findingsAndDecisions, 5);
    placeColumnNodes(otherNodes, 5);

    cursorY += groupHeight + VERTICAL_SPACING;
  }

  if (missionNode) {
    const firstCenter = groupCenters[0] ?? 0;
    const lastCenter = groupCenters[groupCenters.length - 1] ?? firstCenter;
    positions.set(missionNode.id, { x: 0, y: (firstCenter + lastCenter) / 2 });
  }

  aggregateNodes.forEach((node) => {
    const data = node.data as AggregateNodeData;
    const parentPosition = positions.get(data.parentId) ?? { x: 0, y: 0 };
    const parentNode = graphNodeById.get(data.parentId);
    const parentLayer = parentNode ? semanticLayerForNode(parentNode) : 0;
    positions.set(node.id, { x: (parentLayer + 1) * HORIZONTAL_SPACING, y: parentPosition.y });
  });

  if (positions.size > 0) {
    const yValues = [...positions.values()].map((pos) => pos.y);
    const offset = (Math.min(...yValues) + Math.max(...yValues)) / 2;
    positions.forEach((pos, id) => {
      positions.set(id, { x: pos.x, y: pos.y - offset });
    });
  }

  for (const id of positions.keys()) {
    const saved = savedLayout[id];
    if (saved) {
      positions.set(id, saved);
    }
  }

  // Build ReactFlow nodes
  const nodes: Node[] = [
    ...visibleGraphNodes.map((n) => {
      const pos = positions.get(n.id) ?? { x: 0, y: 0 };
      return {
        id: n.id,
        type: 'missionNode',
        position: pos,
        data: n,
      };
    }),
    ...aggregateNodes.map((n) => ({
      ...n,
      position: positions.get(n.id) ?? n.position,
    })),
  ];

  // Build ReactFlow edges with pill labels
  const edges: Edge[] = [
    ...visibleEdges.map((e) => {
      const style = EDGE_TYPE_STYLES[e.type];
      return {
        id: e.id,
        source: e.source,
        target: e.target,
        sourceHandle: 'source',
        targetHandle: 'target',
        type: 'default',
        data: e,
        style: {
          stroke: style.color,
          strokeWidth: e.blocking ? 2.5 : 1.8,
          strokeDasharray: style.dashed ? '5,4' : undefined,
        },
        label: t(`missions.graph.edgeLabel.${e.type}`),
        labelStyle: { fill: '#475569', fontSize: 10, fontWeight: 500 },
        labelBgStyle: { fill: '#ffffff', fillOpacity: 0.92 },
        labelBgPadding: [5, 3] as [number, number],
        labelBgBorderRadius: 4,
        animated: e.blocking,
      };
    }),
    ...aggregateEdges,
  ];

  return { nodes, edges };
}

/* ── Compact node component ── */

function MissionNodeComponent({ data, selected }: { data: MissionGraphNode; selected?: boolean }) {
  const { t } = useTranslation();
  const barColor = statusBarColor(data.status);
  const progress = nodeProgress(data);

  return (
    <div
      className={cn(
        'group relative overflow-hidden rounded-md border bg-white shadow-xs transition-all hover:shadow-sm',
        selected ? 'border-primary ring-1 ring-primary/30' : 'border-border',
      )}
      style={{ width: NODE_WIDTH, minHeight: NODE_HEIGHT }}
    >
      <Handle id="target" type="target" position={Position.Left} className="!h-2 !w-2 !border !border-border !bg-white" />
      <div className="absolute inset-y-0 left-0 w-[3px]" style={{ backgroundColor: barColor }} />
      <div className="flex flex-col gap-1 p-2 pl-3.5">
        <div className="flex items-center justify-between gap-1.5">
          <span className="flex items-center gap-1 text-[9px] font-medium uppercase tracking-wide text-muted-foreground">
            {nodeIcon(data.type)}
            {t(`missions.graph.nodeTypes.${data.type}`)}
          </span>
          {data.status && (
            <span className="shrink-0 text-[9px] font-medium text-muted-foreground">
              {formatStatus(t, data.status)}
            </span>
          )}
        </div>
        <div className="line-clamp-2 text-xs font-semibold leading-tight text-foreground">
          {data.title}
        </div>
        <div className="mt-auto flex items-center gap-1.5 pt-0.5">
          <div className="h-1 flex-1 overflow-hidden rounded-full bg-muted">
            <div
              className="h-full rounded-full transition-all"
              style={{ width: `${progress}%`, backgroundColor: barColor }}
            />
          </div>
          <span className="w-7 text-right text-[9px] font-medium tabular-nums text-muted-foreground">
            {progress}%
          </span>
        </div>
      </div>
      <Handle id="source" type="source" position={Position.Right} className="!h-2 !w-2 !border !border-border !bg-white" />
    </div>
  );
}

/* ── Aggregate node component ── */

function AggregateNodeComponent({ data, selected }: { data: AggregateNodeData; selected?: boolean }) {
  const { t } = useTranslation();
  const typeSummary = Object.entries(data.childTypeCounts)
    .filter(([type]) => type !== 'aggregate')
    .slice(0, 2)
    .map(([type, count]) => {
      const labelKey = type === 'tool_invocation'
        ? 'missions.canvas.aggregate.toolCalls'
        : type === 'evidence'
          ? 'missions.canvas.aggregate.evidence'
          : type === 'finding'
            ? 'missions.canvas.aggregate.findings'
            : type === 'asset'
              ? 'missions.canvas.aggregate.assets'
              : 'missions.canvas.aggregate.items';
      return t(labelKey, { count: count ?? 0 });
    })
    .join(' · ');

  return (
    <div
      onDoubleClick={(e) => {
        e.stopPropagation();
        data.onToggle();
      }}
      onClick={(e) => {
        e.stopPropagation();
        data.onToggle();
      }}
      className={cn(
        'flex cursor-pointer flex-col items-center justify-center gap-1.5 rounded-md border border-dashed border-border bg-muted/30 p-3 text-center transition-all hover:border-foreground/30 hover:bg-muted/50',
        selected && 'border-primary ring-1 ring-primary/20',
      )}
      style={{ width: NODE_WIDTH, minHeight: NODE_HEIGHT }}
    >
      <Handle id="target" type="target" position={Position.Left} className="!h-2 !w-2 !border !border-border !bg-white" />
      <div className="flex h-6 w-6 items-center justify-center rounded-full border border-border bg-background text-xs font-semibold text-muted-foreground">
        {data.childCount}
      </div>
      <div className="text-[10px] font-medium text-muted-foreground">{typeSummary}</div>
      <div className="text-[9px] text-muted-foreground/70">
        {t('missions.canvas.aggregate.doubleClickHint')}
      </div>
      <Handle id="source" type="source" position={Position.Right} className="!h-2 !w-2 !border !border-border !bg-white" />
    </div>
  );
}

const nodeTypes: NodeTypes = {
  missionNode: MissionNodeComponent,
  aggregateNode: AggregateNodeComponent,
};

/* ── Context menu ── */

interface ContextMenuItem {
  key: string;
  labelKey: string;
  icon?: React.ReactNode;
  onClick: () => void;
  disabled?: boolean;
  disabledHintKey?: string;
  separator?: boolean;
}

interface ContextMenuState {
  x: number;
  y: number;
  items: ContextMenuItem[];
}

function ContextMenu({
  state,
  onClose,
}: {
  state: ContextMenuState;
  onClose: () => void;
}) {
  const { t } = useTranslation();

  useEffect(() => {
    const handleClick = () => onClose();
    const handleEscape = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('click', handleClick);
    window.addEventListener('keydown', handleEscape);
    return () => {
      window.removeEventListener('click', handleClick);
      window.removeEventListener('keydown', handleEscape);
    };
  }, [onClose]);

  return (
    <div
      className="fixed z-50 min-w-[180px] rounded-md border border-border bg-popover py-1 shadow-md"
      style={{ left: state.x, top: state.y }}
      onClick={(e) => e.stopPropagation()}
    >
      {state.items.map((item, idx) => (
        <div key={item.key}>
          {item.separator && idx > 0 && <div className="my-1 h-px bg-border" />}
          <button
            type="button"
            disabled={item.disabled}
            onClick={() => {
              if (item.disabled) return;
              item.onClick();
              onClose();
            }}
            className={cn(
              'flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs transition-colors',
              item.disabled
                ? 'cursor-not-allowed text-muted-foreground/50'
                : 'text-foreground hover:bg-accent',
            )}
            title={item.disabled && item.disabledHintKey ? t(item.disabledHintKey) : undefined}
          >
            {item.icon && <span className="shrink-0">{item.icon}</span>}
            <span className="flex-1">{t(item.labelKey)}</span>
            {item.disabled && (
              <span className="text-[9px] text-muted-foreground/50">
                {t('missions.canvas.contextMenu.backendPending')}
              </span>
            )}
          </button>
        </div>
      ))}
    </div>
  );
}

/* ── Main component ── */

export interface MissionDagViewProps {
  graph: MissionGraph;
  missionId: string;
  selectedNodeId?: string | null;
  selectedEdgeId?: string | null;
  collapsed?: boolean;
  actionSignal?: number;
  /** Increments on each toolbar/context-menu "expand all" to clear manual collapses. */
  expandAllSignal?: number;
  /** Force the full graph even past SKELETON_MODE_THRESHOLD. */
  skeletonDisabled?: boolean;
  onNodeClick?: (node: MissionGraphNode) => void;
  onEdgeClick?: (edge: MissionGraphEdge) => void;
  onNodeDoubleClick?: (node: MissionGraphNode) => void;
  onAutoLayout?: () => void;
  onFitView?: () => void;
  onExpandAll?: () => void;
  onCollapseAll?: () => void;
  onResetLayout?: () => void;
  onInjectHint?: (content: string) => void;
  onPrioritizeBranch?: (branchId: string) => void;
  onAbandonBranch?: (branchId: string) => void;
  onReopenBranch?: (branchId: string) => void;
  onOpenDecisionGate?: (gateId: string) => void;
  onViewFinding?: (findingId: string) => void;
  onViewEvidence?: (evidenceId: string) => void;
  onViewToolInvocation?: (toolId: string) => void;
}

export function MissionDagView({
  graph,
  missionId,
  selectedNodeId,
  selectedEdgeId,
  collapsed = false,
  actionSignal = 0,
  expandAllSignal = 0,
  skeletonDisabled = false,
  onNodeClick,
  onEdgeClick,
  onNodeDoubleClick,
  onAutoLayout,
  onFitView,
  onExpandAll,
  onCollapseAll,
  onResetLayout,
  onInjectHint,
  onPrioritizeBranch,
  onAbandonBranch,
  onReopenBranch,
  onOpenDecisionGate,
  onViewFinding,
  onViewEvidence,
  onViewToolInvocation,
}: MissionDagViewProps) {
  const { t } = useTranslation();
  const reactFlowRef = useRef<ReactFlowInstance | null>(null);
  const [contextMenu, setContextMenu] = useState<ContextMenuState | null>(null);

  const [expandedNodes, setExpandedNodes] = useState<Set<string>>(new Set());
  const [collapsedNodes, setCollapsedNodes] = useState<Set<string>>(new Set());
  const [savedLayout, setSavedLayout] = useState<Record<string, { x: number; y: number }>>({});
  const [injectHintOpen, setInjectHintOpen] = useState(false);
  const [injectHintText, setInjectHintText] = useState('');

  // Load saved layout on mount and when missionId changes
  useEffect(() => {
    setSavedLayout(loadLayout(missionId));
  }, [missionId]);

  const handleToggleCollapse = useCallback((nodeId: string) => {
    setExpandedNodes((prev) => {
      const next = new Set(prev);
      if (next.has(nodeId)) {
        next.delete(nodeId);
      } else {
        next.add(nodeId);
      }
      return next;
    });
  }, []);

  // "Expand all" must also clear the manual (double-click) collapses, otherwise
  // subtrees stay hidden and the button appears to do nothing.
  useEffect(() => {
    if (expandAllSignal <= 0) return;
    setCollapsedNodes(new Set());
  }, [expandAllSignal]);

  const layout = useMemo(
    () =>
      autoLayout(
        graph,
        collapsed,
        expandedNodes,
        collapsedNodes,
        handleToggleCollapse,
        savedLayout,
        t,
        skeletonDisabled,
      ),
    [graph, collapsed, expandedNodes, collapsedNodes, handleToggleCollapse, savedLayout, t, skeletonDisabled],
  );

  const [nodes, setNodes, onNodesChange] = useNodesState(layout.nodes);
  const [edges, setEdges, onEdgesChange] = useEdgesState(layout.edges);

  // Sync layout when graph or selection changes
  useEffect(() => {
    setNodes(
      layout.nodes.map((node) => ({
        ...node,
        selected: selectedNodeId === node.id,
      })),
    );
    setEdges(
      layout.edges.map((edge) => {
        const edgeData = edge.data as MissionGraphEdge | undefined;
        const labelText = edgeData ? t(`missions.graph.edgeLabel.${edgeData.type}`) : null;
        return {
          ...edge,
          label: labelText ?? edge.label,
          selected: selectedEdgeId === edge.id,
          style: {
            ...edge.style,
            strokeWidth: selectedEdgeId === edge.id ? 3 : edge.style?.strokeWidth,
          },
        };
      }),
    );
  }, [layout, selectedNodeId, selectedEdgeId, actionSignal, setNodes, setEdges, t]);

  // Fit view on action signal
  useEffect(() => {
    if (!reactFlowRef.current) return;
    window.requestAnimationFrame(() => {
      reactFlowRef.current?.fitView({ padding: 0.16, duration: 220 });
    });
  }, [actionSignal, collapsed, expandedNodes, collapsedNodes]);

  const handleNodeClick: NodeMouseHandler = useCallback(
    (_event, node) => {
      if (node.type === 'aggregateNode') {
        const data = node.data as AggregateNodeData;
        data.onToggle?.();
      } else if (node.data) {
        onNodeClick?.(node.data as MissionGraphNode);
      }
    },
    [onNodeClick],
  );

  const handleNodeDoubleClick: NodeMouseHandler = useCallback(
    (_event, node) => {
      if (node.type === 'aggregateNode') {
        const data = node.data as AggregateNodeData;
        data.onToggle?.();
        return;
      }

      const graphNode = node.data as MissionGraphNode;
      if (!graphNode) return;

      const nodeType = graphNode.type;

      // Branch / Intent / Task: toggle collapse
      if (nodeType === 'branch' || nodeType === 'exploration_task') {
        setCollapsedNodes((prev) => {
          const next = new Set(prev);
          if (next.has(graphNode.id)) {
            next.delete(graphNode.id);
          } else {
            next.add(graphNode.id);
          }
          return next;
        });
        return;
      }

      // DecisionGate: open decision drawer
      if (nodeType === 'decision_gate') {
        onOpenDecisionGate?.(graphNode.id);
        return;
      }

      // Evidence / Finding / ToolInvocation: open Inspector (same as click)
      if (nodeType === 'evidence' || nodeType === 'finding' || nodeType === 'tool_invocation') {
        onNodeDoubleClick?.(graphNode);
        onNodeClick?.(graphNode);
        return;
      }

      // Default: pass to parent
      onNodeDoubleClick?.(graphNode);
    },
    [onNodeClick, onNodeDoubleClick, onOpenDecisionGate],
  );

  const handleEdgeClick: EdgeMouseHandler = useCallback(
    (_event, edge) => {
      if (edge.data) onEdgeClick?.(edge.data as MissionGraphEdge);
    },
    [onEdgeClick],
  );

  // Node drag end: snap to grid and save layout
  const handleNodeDragStop: OnNodeDrag = useCallback(() => {
    setNodes((currentNodes) => {
      const newPositions: Record<string, { x: number; y: number }> = { ...savedLayout };
      currentNodes.forEach((node) => {
        const snappedX = snapToGrid(node.position.x, GRID_SIZE);
        const snappedY = snapToGrid(node.position.y, GRID_SIZE);
        node.position.x = snappedX;
        node.position.y = snappedY;
        newPositions[node.id] = { x: snappedX, y: snappedY };
      });
      saveLayout(missionId, newPositions);
      setSavedLayout(newPositions);
      return [...currentNodes];
    });
  }, [missionId, savedLayout, setNodes]);

  // Node right-click context menu
  const handleNodeContextMenu: NodeMouseHandler = useCallback(
    (event, node) => {
      event.preventDefault();

      if (node.type === 'aggregateNode') {
        const data = node.data as AggregateNodeData;
        setContextMenu({
          x: event.clientX,
          y: event.clientY,
          items: [
            {
              key: 'expand',
              labelKey: 'missions.canvas.contextMenu.expandSubgraph',
              onClick: () => data.onToggle?.(),
            },
          ],
        });
        return;
      }

      const graphNode = node.data as MissionGraphNode;
      if (!graphNode) return;

      const items: ContextMenuItem[] = [];
      const nodeType = graphNode.type;
      const nodeStatus = graphNode.status?.toLowerCase() ?? '';

      // Common: view details
      items.push({
        key: 'viewDetails',
        labelKey: 'missions.canvas.contextMenu.viewDetails',
        onClick: () => onNodeClick?.(graphNode),
      });

      // View evidence chain (for findings and evidence)
      if (nodeType === 'finding' || nodeType === 'evidence') {
        items.push({
          key: 'viewEvidenceChain',
          labelKey: 'missions.canvas.contextMenu.viewEvidenceChain',
          onClick: () => {
            if (nodeType === 'finding') onViewFinding?.(graphNode.id);
            else onViewEvidence?.(graphNode.id);
          },
        });
      }

      // Copy node ID
      items.push({
        key: 'copyNodeId',
        labelKey: 'missions.canvas.contextMenu.copyNodeId',
        onClick: () => {
          navigator.clipboard?.writeText(graphNode.id).catch(() => {});
        },
      });

      items.push({ key: 'sep1', labelKey: '', separator: true, onClick: () => {} });

      // Branch-specific actions
      if (nodeType === 'branch') {
        const branchId = graphNode.id;
        items.push({
          key: 'prioritize',
          labelKey: 'missions.canvas.contextMenu.prioritize',
          onClick: () => onPrioritizeBranch?.(branchId),
          disabled: !onPrioritizeBranch,
          disabledHintKey: 'missions.canvas.contextMenu.backendPending',
        });
        if (graphNode.status === 'abandoned') {
          items.push({
            key: 'reopen',
            labelKey: 'missions.canvas.contextMenu.reopen',
            onClick: () => onReopenBranch?.(branchId),
            disabled: !onReopenBranch,
            disabledHintKey: 'missions.canvas.contextMenu.backendPending',
          });
        } else {
          items.push({
            key: 'abandon',
            labelKey: 'missions.canvas.contextMenu.abandon',
            onClick: () => onAbandonBranch?.(branchId),
            disabled: !onAbandonBranch,
            disabledHintKey: 'missions.canvas.contextMenu.backendPending',
          });
        }
      }

      // Failed / error / blocked: rollback and retry
      if (['failed', 'error', 'blocked'].includes(nodeStatus)) {
        items.push({
          key: 'rollbackRetry',
          labelKey: 'missions.canvas.contextMenu.rollbackRetry',
          onClick: () => {},
          disabled: true,
          disabledHintKey: 'missions.canvas.contextMenu.backendPending',
        });
      }

      // DecisionGate: open decision
      if (nodeType === 'decision_gate') {
        items.push({
          key: 'openDecision',
          labelKey: 'missions.canvas.contextMenu.openDecision',
          onClick: () => onOpenDecisionGate?.(graphNode.id),
          disabled: !onOpenDecisionGate,
          disabledHintKey: 'missions.canvas.contextMenu.backendPending',
        });
      }

      // Finding: view finding detail
      if (nodeType === 'finding') {
        items.push({
          key: 'viewFinding',
          labelKey: 'missions.canvas.contextMenu.viewFinding',
          onClick: () => onViewFinding?.(graphNode.id),
          disabled: !onViewFinding,
          disabledHintKey: 'missions.canvas.contextMenu.backendPending',
        });
      }

      // Evidence: view evidence detail
      if (nodeType === 'evidence') {
        items.push({
          key: 'viewEvidence',
          labelKey: 'missions.canvas.contextMenu.viewEvidence',
          onClick: () => onViewEvidence?.(graphNode.id),
          disabled: !onViewEvidence,
          disabledHintKey: 'missions.canvas.contextMenu.backendPending',
        });
      }

      // ToolInvocation: view tool I/O
      if (nodeType === 'tool_invocation') {
        items.push({
          key: 'viewToolIO',
          labelKey: 'missions.canvas.contextMenu.viewToolIO',
          onClick: () => onViewToolInvocation?.(graphNode.id),
          disabled: !onViewToolInvocation,
          disabledHintKey: 'missions.canvas.contextMenu.backendPending',
        });
      }

      setContextMenu({ x: event.clientX, y: event.clientY, items });
    },
    [
      onNodeClick,
      onPrioritizeBranch,
      onAbandonBranch,
      onReopenBranch,
      onOpenDecisionGate,
      onViewFinding,
      onViewEvidence,
      onViewToolInvocation,
    ],
  );

  // Canvas (pane) right-click context menu
  const handlePaneContextMenu = useCallback(
    (event: MouseEvent | React.MouseEvent) => {
      event.preventDefault();

      const items: ContextMenuItem[] = [
        {
          key: 'injectHint',
          labelKey: 'missions.canvas.contextMenu.injectHint',
          onClick: () => setInjectHintOpen(true),
          disabled: !onInjectHint,
          disabledHintKey: 'missions.canvas.contextMenu.backendPending',
        },
        { key: 'sep1', labelKey: '', separator: true, onClick: () => {} },
        {
          key: 'autoLayout',
          labelKey: 'missions.canvas.contextMenu.autoLayout',
          onClick: () => {
            clearLayout(missionId);
            setSavedLayout({});
            onAutoLayout?.();
          },
        },
        {
          key: 'fitView',
          labelKey: 'missions.canvas.contextMenu.fitView',
          onClick: () => onFitView?.(),
        },
        { key: 'sep2', labelKey: '', separator: true, onClick: () => {} },
        {
          key: 'expandAll',
          labelKey: 'missions.canvas.contextMenu.expandAll',
          onClick: () => onExpandAll?.(),
        },
        {
          key: 'collapseAll',
          labelKey: 'missions.canvas.contextMenu.collapseAll',
          onClick: () => onCollapseAll?.(),
        },
        {
          key: 'resetLayout',
          labelKey: 'missions.canvas.contextMenu.resetLayout',
          onClick: () => {
            clearLayout(missionId);
            setSavedLayout({});
            setExpandedNodes(new Set());
            setCollapsedNodes(new Set());
            onResetLayout?.();
          },
        },
      ];

      setContextMenu({ x: event.clientX, y: event.clientY, items });
    },
    [missionId, onInjectHint, onAutoLayout, onFitView, onExpandAll, onCollapseAll, onResetLayout],
  );

  if (graph.nodes.length === 0) {
    return (
      <div className="flex flex-1 items-center justify-center text-sm text-muted-foreground">
        {t('missions.graph.empty')}
      </div>
    );
  }

  return (
    <>
      <ReactFlow
        nodes={nodes}
        edges={edges}
        onNodesChange={onNodesChange}
        onEdgesChange={onEdgesChange}
        onNodeClick={handleNodeClick}
        onNodeDoubleClick={handleNodeDoubleClick}
        onNodeContextMenu={handleNodeContextMenu}
        onPaneContextMenu={handlePaneContextMenu}
        onNodeDragStop={handleNodeDragStop}
        onEdgeClick={handleEdgeClick}
        nodeTypes={nodeTypes}
        fitView
        minZoom={0.1}
        maxZoom={2}
        snapToGrid
        snapGrid={[GRID_SIZE, GRID_SIZE]}
        defaultEdgeOptions={{ type: 'default' }}
        onInit={(instance) => {
          reactFlowRef.current = instance;
          instance.fitView({ padding: 0.16 });
        }}
        className="bg-background"
        proOptions={{ hideAttribution: true }}
      >
        <Background color="#e2e8f0" gap={GRID_SIZE} size={1.5} variant={BackgroundVariant.Dots} />
        <Controls className="!border-border !bg-background !shadow-sm" />
        <MiniMap
          className="!rounded-md !border !border-border !bg-background !shadow-sm"
          nodeBorderRadius={4}
          nodeColor={(node) => {
            const data = node.data as MissionGraphNode | AggregateNodeData;
            if (data.type === 'aggregate') return '#cbd5e1';
            return nodeColor((data as MissionGraphNode).type);
          }}
          zoomable
          pannable
        />
      </ReactFlow>
      {contextMenu && <ContextMenu state={contextMenu} onClose={() => setContextMenu(null)} />}
      <Dialog open={injectHintOpen} onOpenChange={setInjectHintOpen}>
        <DialogContent closeLabel={t('common.close')}>
          <DialogHeader>
            <DialogTitle>{t('missions.canvas.contextMenu.injectHint')}</DialogTitle>
            <DialogDescription>{t('missions.canvas.contextMenu.injectHintPrompt')}</DialogDescription>
          </DialogHeader>
          <Textarea
            value={injectHintText}
            onChange={(e) => setInjectHintText(e.target.value)}
            placeholder={t('missions.canvas.contextMenu.injectHintPlaceholder')}
            rows={4}
            className="min-h-[100px]"
          />
          <DialogFooter>
            <Button variant="ghost" onClick={() => { setInjectHintOpen(false); setInjectHintText(''); }}>
              {t('common.cancel')}
            </Button>
            <Button
              variant="default"
              disabled={!injectHintText.trim()}
              onClick={() => {
                onInjectHint?.(injectHintText.trim());
                setInjectHintOpen(false);
                setInjectHintText('');
              }}
            >
              {t('missions.canvas.contextMenu.injectHint')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
