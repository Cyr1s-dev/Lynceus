/**
 * Frontend graph model for the Mission Exploration Graph.
 *
 * The backend ships a {@link MissionCanvas} (flat record lists plus
 * `{src, dst, relation}` edges). UI components must not reason about that raw
 * shape directly; instead the {@link missionCanvasMapper} normalizes it into the
 * node/edge model declared here, and views only render this model.
 *
 * This model is intentionally aligned with `@xyflow/react` so the DAG view can
 * adopt it without another transformation pass.
 */

export type MissionGraphNodeType =
  | 'mission'
  | 'phase'
  | 'branch'
  | 'exploration_task'
  | 'tool_invocation'
  | 'evidence'
  | 'finding'
  | 'directive'
  | 'decision_gate'
  | 'strategy_board'
  | 'risk'
  | 'gap'
  | 'question'
  | 'follow_up'
  | 'aggregate'
  | 'asset';

export type MissionGraphEdgeType =
  | 'child_of'
  | 'decomposes'
  | 'depends_on'
  | 'blocked_by'
  | 'supports'
  | 'derived_from'
  | 'clarifies'
  | 'supersedes'
  | 'duplicates'
  | 'produced'
  | 'invoked'
  | 'reviewed'
  | 'mission_to_asset'
  | 'branch_to_asset'
  | 'tool_invocation_to_asset'
  | 'evidence_to_asset'
  | 'asset_to_evidence'
  | 'asset_to_finding'
  | 'finding_to_asset';

/** Normalized graph node. `raw` retains the source record for the detail panel. */
export interface MissionGraphNode {
  id: string;
  type: MissionGraphNodeType;
  title: string;
  subtitle?: string;
  status?: string;
  severity?: string;
  parent_id?: string | null;
  branch_id?: string | null;
  run_id?: string | null;
  /** Cross-references into other graph entities (evidence/finding/tool ids, etc.). */
  refs?: string[];
  /** Backend source record, kept opaque. Narrow before reading fields. */
  raw?: unknown;
  /** Index signature for @xyflow/react compatibility. */
  [key: string]: unknown;
}

/** Normalized graph edge. `raw` retains the source edge for the detail panel. */
export interface MissionGraphEdge {
  id: string;
  source: string;
  target: string;
  type: MissionGraphEdgeType;
  label?: string;
  blocking?: boolean;
  confidence?: number | null;
  /** Backend source edge (`{src, dst, relation}`), kept opaque. */
  raw?: unknown;
  /** Index signature for @xyflow/react compatibility. */
  [key: string]: unknown;
}

/**
 * Triage queues derived from the canvas. Each queue holds graph nodes so the UI
 * can link a queue entry straight to its node in the canvas.
 *
 * Queues without a backend signal yet (e.g. observer review) resolve to empty
 * arrays — the UI shows a real empty state rather than fabricated counts.
 */
export interface StatusQueues {
  active_branches: MissionGraphNode[];
  blocked: MissionGraphNode[];
  needs_user_decision: MissionGraphNode[];
  needs_evidence: MissionGraphNode[];
  needs_observer_review: MissionGraphNode[];
  ready_to_verify: MissionGraphNode[];
  ready_to_patch: MissionGraphNode[];
  high_value_next_actions: MissionGraphNode[];
}

/** Assembled view model consumed by the Mission Workspace. */
export interface MissionGraph {
  nodes: MissionGraphNode[];
  edges: MissionGraphEdge[];
  statusQueues: StatusQueues;
  /** Stable index for O(1) node lookups by id (e.g. resolving an edge endpoint). */
  nodeIndex: Map<string, MissionGraphNode>;
}

export const STATUS_QUEUE_KEYS: Array<keyof StatusQueues> = [
  'active_branches',
  'blocked',
  'needs_user_decision',
  'needs_evidence',
  'needs_observer_review',
  'ready_to_verify',
  'ready_to_patch',
  'high_value_next_actions',
];

/**
 * Stable visual identity per edge type, shared by the DAG view and the edge
 * legend so colors never drift between the two.
 *
 * Color mapping follows the task-forest-live-dag convention:
 *   child_of / decomposes → green (sub-task)
 *   depends_on            → blue (dependency)
 *   blocked_by            → red (blocking)
 *   supports              → teal (evidence)
 *   derived_from / produced → purple (contribution / output)
 *   clarifies             → amber (clarification / human decision)
 *   other                 → slate (generic)
 */
export const EDGE_TYPE_STYLES: Record<MissionGraphEdgeType, { color: string; dashed?: boolean }> = {
  child_of: { color: '#10b981' }, // emerald-500 — sub-task
  decomposes: { color: '#10b981' }, // emerald-500 — sub-task
  depends_on: { color: '#3b82f6', dashed: true }, // blue-500 — dependency
  blocked_by: { color: '#ef4444', dashed: true }, // red-500 — blocking
  supports: { color: '#14b8a6' }, // teal-500 — evidence
  derived_from: { color: '#8b5cf6', dashed: true }, // violet-500 — contribution
  clarifies: { color: '#f59e0b', dashed: true }, // amber-500 — clarification
  supersedes: { color: '#64748b', dashed: true }, // slate-500 — generic
  duplicates: { color: '#64748b', dashed: true }, // slate-500 — generic
  produced: { color: '#8b5cf6' }, // violet-500 — output
  invoked: { color: '#64748b' }, // slate-500 — generic
  reviewed: { color: '#64748b', dashed: true }, // slate-500 — generic
  mission_to_asset: { color: '#6366f1' }, // indigo-500 — asset link from mission
  branch_to_asset: { color: '#6366f1' }, // indigo-500 — asset link from branch
  tool_invocation_to_asset: { color: '#6366f1', dashed: true }, // indigo-500 — tool produced asset
  evidence_to_asset: { color: '#14b8a6' }, // teal-500 — evidence linked to asset
  asset_to_evidence: { color: '#14b8a6' }, // teal-500 — asset linked to evidence
  asset_to_finding: { color: '#8b5cf6' }, // violet-500 — asset contributed to finding
  finding_to_asset: { color: '#8b5cf6', dashed: true }, // violet-500 — finding references asset
};

/**
 * Short pill labels for edge types, used on the canvas. These are shorter
 * than the full edge type names and fit inside small pill labels.
 */
export const EDGE_TYPE_PILL_LABELS: Record<MissionGraphEdgeType, string> = {
  child_of: 'edgeLabel.child_of',
  decomposes: 'edgeLabel.decomposes',
  depends_on: 'edgeLabel.depends_on',
  blocked_by: 'edgeLabel.blocked_by',
  supports: 'edgeLabel.supports',
  derived_from: 'edgeLabel.derived_from',
  clarifies: 'edgeLabel.clarifies',
  supersedes: 'edgeLabel.supersedes',
  duplicates: 'edgeLabel.duplicates',
  produced: 'edgeLabel.produced',
  invoked: 'edgeLabel.invoked',
  reviewed: 'edgeLabel.reviewed',
  mission_to_asset: 'edgeLabel.mission_to_asset',
  branch_to_asset: 'edgeLabel.branch_to_asset',
  tool_invocation_to_asset: 'edgeLabel.tool_invocation_to_asset',
  evidence_to_asset: 'edgeLabel.evidence_to_asset',
  asset_to_evidence: 'edgeLabel.asset_to_evidence',
  asset_to_finding: 'edgeLabel.asset_to_finding',
  finding_to_asset: 'edgeLabel.finding_to_asset',
};

/** Data carried by an aggregate (collapsed) node on the canvas. */
export interface AggregateNodeData {
  id: string;
  parentId: string;
  type: 'aggregate';
  status: string;
  childCount: number;
  childTypeCounts: Partial<Record<MissionGraphNodeType, number>>;
  onToggle: () => void;
  [key: string]: unknown;
}
