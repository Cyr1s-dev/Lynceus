/**
 * Maps the backend {@link MissionCanvas} into the normalized
 * {@link MissionGraph} consumed by the Mission Workspace.
 *
 * Rules:
 * - Never fabricate nodes, edges, statuses, severities, or counts. Everything
 *   here is derived from real canvas records.
 * - The backend emits duplicate edges for the same pair (a generic and a
 *   specific relation, e.g. `has_branch` + `mission_to_branch`). We collapse
 *   those to a single canonical typed edge and dedupe by (source, target, type).
 * - Missing fields degrade to empty/undefined so the UI can show real empty
 *   states instead of guessing.
 */
import type {
  MissionCanvas,
  Branch,
  ApiToolInvocation,
  ApiFinding,
  UserDirective,
  DecisionGate,
  Observation,
  MissionAsset,
} from './types';
import type {
  MissionGraph,
  MissionGraphNode,
  MissionGraphEdge,
  MissionGraphEdgeType,
  MissionGraphNodeType,
  StatusQueues,
} from './missionGraphTypes';

/** Read a string property from an opaque record (evidence/task come as loose objects). */
function readString(record: unknown, key: string): string | undefined {
  if (record && typeof record === 'object' && key in record) {
    const value = (record as Record<string, unknown>)[key];
    if (typeof value === 'string') return value;
    if (typeof value === 'number') return String(value);
  }
  return undefined;
}

/** Read a string array property from an opaque record. */
function readStringArray(record: unknown, key: string): string[] {
  if (record && typeof record === 'object' && key in record) {
    const value = (record as Record<string, unknown>)[key];
    if (Array.isArray(value)) return value.filter((item): item is string => typeof item === 'string');
  }
  return [];
}

/**
 * Canonical backend-relation → frontend-edge-type mapping.
 *
 * The backend deliberately emits several relation strings per logical edge; all
 * synonyms map to the same {@link MissionGraphEdgeType} so the deduped result is
 * stable regardless of which synonym arrives.
 */
const RELATION_TO_EDGE_TYPE: Record<string, MissionGraphEdgeType> = {
  // hierarchy
  has_run: 'child_of',
  mission_to_run: 'child_of',
  has_branch: 'child_of',
  mission_to_branch: 'child_of',
  contains_branch: 'child_of',
  run_to_branch: 'child_of',
  branch_task: 'child_of',
  branch_to_task: 'child_of',
  has_task: 'child_of',
  run_to_task: 'child_of',
  run_to_intent: 'child_of',
  // decomposition
  mission_to_intent: 'decomposes',
  branch_to_intent: 'decomposes',
  intent_to_task: 'decomposes',
  // derivation
  derived_branch: 'derived_from',
  // tool invocation
  used_tool: 'invoked',
  task_to_tool_invocation: 'invoked',
  tool_for_task: 'invoked',
  // production of artifacts
  produced_evidence: 'produced',
  branch_to_evidence: 'produced',
  task_to_evidence: 'produced',
  evidence_for_branch: 'produced',
  tool_invocation_to_evidence: 'produced',
  produced_finding: 'produced',
  branch_to_finding: 'produced',
  task_to_finding: 'produced',
  finding_for_branch: 'produced',
  // evidence supports finding
  evidences: 'supports',
  evidence_to_finding: 'supports',
  // directives clarify scope
  user_directive: 'clarifies',
  directive_to_mission: 'clarifies',
  directive_targets_branch: 'clarifies',
  directive_to_branch: 'clarifies',
  // decision gates block progress
  requires_decision: 'blocked_by',
  decision_gate_to_run: 'blocked_by',
  decision_gate_to_mission: 'blocked_by',
  // observer / strategy board review
  observed: 'reviewed',
  strategy_board_to_mission: 'reviewed',
  strategy_board_to_run: 'reviewed',
  // asset relationships
  mission_to_asset: 'mission_to_asset',
  branch_to_asset: 'branch_to_asset',
  tool_invocation_to_asset: 'tool_invocation_to_asset',
  evidence_to_asset: 'evidence_to_asset',
  asset_to_evidence: 'asset_to_evidence',
  asset_to_finding: 'asset_to_finding',
  finding_to_asset: 'finding_to_asset',
};

const BLOCKING_EDGE_TYPES = new Set<MissionGraphEdgeType>(['blocked_by', 'depends_on']);

function branchToNode(branch: Branch, observations: Observation[]): MissionGraphNode {
  const branchObservations = observations.filter((o) => o.branch_id === branch.id);
  const hasFailureBoundary = branchObservations.some(
    (o) => o.observation_type === 'failure_boundary' || o.observation_type === 'blockage',
  );
  const refs = [
    ...branch.related_tool_invocation_ids,
    ...branch.related_evidence_ids,
    ...branch.related_finding_ids,
  ];
  return {
    id: branch.id,
    type: 'branch',
    title: branch.title,
    subtitle: branch.hypothesis,
    status: branch.status,
    parent_id: branch.parent_branch_id ?? branch.mission_id,
    branch_id: branch.id,
    run_id: branch.run_id ?? null,
    refs,
    raw: { ...branch, _hasFailureBoundary: hasFailureBoundary },
  };
}

function toolToNode(tool: ApiToolInvocation): MissionGraphNode {
  return {
    id: tool.id,
    type: 'tool_invocation',
    title: tool.tool_name,
    subtitle: tool.output_summary || tool.input_summary || undefined,
    status: tool.status,
    parent_id: tool.task_id ?? tool.branch_id ?? null,
    branch_id: tool.branch_id ?? null,
    run_id: tool.run_id ?? null,
    raw: tool,
  };
}

function findingToNode(finding: ApiFinding): MissionGraphNode {
  return {
    id: finding.id,
    type: 'finding',
    title: finding.title,
    subtitle: finding.description || finding.rule_id || undefined,
    status: finding.status,
    severity: finding.severity,
    parent_id: finding.produced_by_task_id ?? null,
    refs: finding.evidence_ids ?? [],
    raw: finding,
  };
}

function directiveToNode(directive: UserDirective): MissionGraphNode {
  return {
    id: directive.id,
    type: 'directive',
    title: directive.directive_type,
    subtitle: directive.content,
    status: directive.status,
    parent_id: directive.branch_id ?? directive.mission_id,
    branch_id: directive.branch_id ?? null,
    run_id: directive.run_id ?? null,
    raw: directive,
  };
}

function decisionGateToNode(gate: DecisionGate): MissionGraphNode {
  return {
    id: gate.id,
    type: 'decision_gate',
    title: gate.question,
    subtitle: gate.context_summary || gate.kind,
    status: gate.status,
    severity: gate.severity,
    parent_id: gate.audit_run_id,
    run_id: gate.audit_run_id,
    raw: gate,
  };
}

function evidenceToNode(evidence: unknown): MissionGraphNode | null {
  const id = readString(evidence, 'id');
  if (!id) return null;
  return {
    id,
    type: 'evidence',
    title: readString(evidence, 'summary') || id,
    subtitle: readString(evidence, 'kind'),
    branch_id: readString(evidence, 'branch_id') ?? null,
    parent_id: readString(evidence, 'produced_by_task_id') ?? null,
    raw: evidence,
  };
}

function taskToNode(task: unknown): MissionGraphNode | null {
  const id = readString(task, 'id');
  if (!id) return null;
  return {
    id,
    type: 'exploration_task',
    title: readString(task, 'title') || readString(task, 'goal') || id,
    subtitle: readString(task, 'description') || readString(task, 'status'),
    status: readString(task, 'status'),
    parent_id: readString(task, 'branch_id') ?? readString(task, 'run_id') ?? null,
    branch_id: readString(task, 'branch_id') ?? null,
    run_id: readString(task, 'run_id') ?? null,
    refs: [...readStringArray(task, 'tool_invocation_ids'), ...readStringArray(task, 'produced_evidence_ids')],
    raw: task,
  };
}

function assetToNode(asset: MissionAsset): MissionGraphNode {
  const meta = asset.metadata || {};
  const originalFilename = typeof meta.original_filename === 'string' ? meta.original_filename : undefined;
  const detectedInputType = typeof meta.detected_input_type === 'string' ? meta.detected_input_type : undefined;
  const sizeBytes = typeof meta.size_bytes === 'number' ? meta.size_bytes : asset.value ? undefined : undefined;

  // Build title: prefer label, then original_filename, then value (truncated)
  const title = asset.label || originalFilename || (asset.value.length > 40 ? asset.value.slice(0, 37) + '...' : asset.value);

  // Build subtitle: asset_type + detected_input_type + size
  const parts: string[] = [asset.asset_type];
  if (detectedInputType) parts.push(detectedInputType);
  if (sizeBytes !== undefined) parts.push(`${sizeBytes} bytes`);
  const subtitle = parts.join(' · ');

  // Determine status from sensitivity
  const status =
    asset.sensitivity === 'secret' || asset.sensitivity === 'credential' || asset.sensitivity === 'account'
      ? 'blocked'
      : 'completed';

  // Build refs from associated entities
  const refs: string[] = [
    ...(asset.evidence_ids || []),
    ...(asset.finding_ids || []),
    ...(asset.tool_invocation_ids || []),
  ];

  return {
    id: asset.id,
    type: 'asset' as MissionGraphNodeType,
    title,
    subtitle,
    status,
    severity: asset.sensitivity,
    branch_id: null,
    run_id: asset.run_id || null,
    refs,
    raw: asset,
  };
}

/** Build the normalized node list. The mission is always the single root. */
function buildNodes(canvas: MissionCanvas): MissionGraphNode[] {
  const nodes: MissionGraphNode[] = [];

  nodes.push({
    id: canvas.mission.id,
    type: 'mission',
    title: canvas.mission.user_goal,
    subtitle: Object.entries(canvas.mission.target || {})
      .map(([k, v]) => `${k}: ${v}`)
      .join(', '),
    status: canvas.mission.status,
    parent_id: null,
    raw: canvas.mission,
  });

  const observations = canvas.observations ?? [];
  canvas.branches.forEach((branch) => nodes.push(branchToNode(branch, observations)));
  (canvas.tasks ?? []).forEach((task) => {
    const node = taskToNode(task);
    if (node) nodes.push(node);
  });
  canvas.tool_invocations.forEach((tool) => nodes.push(toolToNode(tool)));
  canvas.evidence.forEach((item) => {
    const node = evidenceToNode(item);
    if (node) nodes.push(node);
  });
  canvas.findings.forEach((finding) => nodes.push(findingToNode(finding)));
  for (const asset of canvas.assets ?? []) {
    nodes.push(assetToNode(asset));
  }
  canvas.directives.forEach((directive) => nodes.push(directiveToNode(directive)));
  canvas.decision_gates.forEach((gate) => nodes.push(decisionGateToNode(gate)));

  return nodes;
}

/**
 * Normalize and dedupe edges. Only edges whose endpoints both resolve to a known
 * node are kept, so the DAG never references a missing node.
 */
function buildEdges(canvas: MissionCanvas, nodeIndex: Map<string, MissionGraphNode>): MissionGraphEdge[] {
  const edges: MissionGraphEdge[] = [];
  const seen = new Set<string>();
  const taskToolEdges = new Set<string>();

  for (const raw of canvas.edges ?? []) {
    const type = RELATION_TO_EDGE_TYPE[raw.relation];
    if (type !== 'invoked') continue;
    const sourceNode = nodeIndex.get(raw.src);
    const targetNode = nodeIndex.get(raw.dst);
    if (sourceNode?.type === 'exploration_task' && targetNode?.type === 'tool_invocation') {
      taskToolEdges.add(`${raw.src}->${raw.dst}`);
    }
  }

  for (const raw of canvas.edges ?? []) {
    const type = RELATION_TO_EDGE_TYPE[raw.relation];
    if (!type) continue; // unknown relation: skip rather than guess
    if (!nodeIndex.has(raw.src) || !nodeIndex.has(raw.dst)) continue;
    const sourceNode = nodeIndex.get(raw.src);
    const targetNode = nodeIndex.get(raw.dst);
    if (
      type === 'invoked'
      && sourceNode?.type === 'branch'
      && targetNode?.type === 'tool_invocation'
      && typeof targetNode.parent_id === 'string'
      && taskToolEdges.has(`${targetNode.parent_id}->${raw.dst}`)
    ) {
      continue;
    }

    const key = `${raw.src}->${raw.dst}:${type}`;
    if (seen.has(key)) continue;
    seen.add(key);

    edges.push({
      id: key,
      source: raw.src,
      target: raw.dst,
      type,
      label: type,
      blocking: BLOCKING_EDGE_TYPES.has(type),
      confidence: null, // backend does not yet provide edge confidence
      raw,
    });
  }

  return edges;
}

/**
 * Derive triage queues from real records only. Queues without a backend signal
 * yet stay empty so the UI renders an honest empty state.
 */
function buildStatusQueues(
  canvas: MissionCanvas,
  nodeIndex: Map<string, MissionGraphNode>,
): StatusQueues {
  const branchNode = (id: string) => nodeIndex.get(id);

  const active_branches = canvas.branches
    .filter((b) => b.status === 'active')
    .map((b) => branchNode(b.id))
    .filter((n): n is MissionGraphNode => Boolean(n));

  const blocked = canvas.branches
    .filter((b) => b.status === 'blocked')
    .map((b) => branchNode(b.id))
    .filter((n): n is MissionGraphNode => Boolean(n));

  const needs_user_decision = canvas.decision_gates
    .filter((gate) => gate.status === 'pending')
    .map((gate) => nodeIndex.get(gate.id))
    .filter((n): n is MissionGraphNode => Boolean(n));

  return {
    active_branches,
    blocked,
    needs_user_decision,
    // The signals below require backend support that does not exist yet
    // (observer review state, verify/patch readiness, ranked next actions).
    // Keep them empty rather than fabricate queue contents.
    needs_evidence: [],
    needs_observer_review: [],
    ready_to_verify: [],
    ready_to_patch: [],
    high_value_next_actions: [],
  };
}

/** Map a backend canvas into the normalized Mission graph view model. */
export function mapMissionCanvas(canvas: MissionCanvas | undefined): MissionGraph | null {
  if (!canvas) return null;

  const nodes = buildNodes(canvas);
  const nodeIndex = new Map<string, MissionGraphNode>();
  for (const node of nodes) nodeIndex.set(node.id, node);

  const edges = buildEdges(canvas, nodeIndex);
  const statusQueues = buildStatusQueues(canvas, nodeIndex);

  return { nodes, edges, statusQueues, nodeIndex };
}
