/**
 * Right-side Inspector panel for the Mission Exploration Canvas.
 *
 * Shows type-specific content based on the selected node type:
 * - Branch: hypothesis, rationale, priority, confidence, budget + action buttons
 * - ToolInvocation: tool name, status, input/output summary, error, artifacts
 * - Evidence: evidence type, summary, location, supports, metadata
 * - Finding: title, severity, status, rule_id, CWE, confidence, evidence_ids
 * - DecisionGate: question, severity, kind, available options + open decision
 *
 * Also shows edge details when an edge is selected.
 */
import { useTranslation } from 'react-i18next';
import { X, ChevronDown, ChevronLeft, ChevronRight, PanelRight, Flag, Play, Ban, RotateCcw, ExternalLink } from 'lucide-react';
import { Badge, Button } from '@/ui/untitled';
import { formatStatus } from '@/lib/i18n-formatters';
import type { MissionGraphNode, MissionGraphEdge } from '@/lib/missionGraphTypes';
import { EDGE_TYPE_STYLES } from '@/lib/missionGraphTypes';
import { nodeIcon } from './missionNodeVisuals';

export interface MissionDetailPanelProps {
  node?: MissionGraphNode | null;
  edge?: MissionGraphEdge | null;
  collapsed?: boolean;
  onClose: () => void;
  onToggleCollapsed?: () => void;
  onPrioritizeBranch?: (branchId: string) => void;
  onAbandonBranch?: (branchId: string) => void;
  onReopenBranch?: (branchId: string) => void;
  onOpenDecisionGate?: (gateId: string) => void;
  onViewFinding?: (findingId: string) => void;
  onViewEvidence?: (evidenceId: string) => void;
  onViewToolInvocation?: (toolId: string) => void;
}

export function MissionDetailPanel({
  node,
  edge,
  collapsed = false,
  onClose,
  onToggleCollapsed,
  onPrioritizeBranch,
  onAbandonBranch,
  onReopenBranch,
  onOpenDecisionGate,
  onViewFinding,
  onViewEvidence,
  onViewToolInvocation,
}: MissionDetailPanelProps) {
  const { t } = useTranslation();

  if (collapsed) {
    return (
      <div className="flex h-full flex-col items-center border-l border-border bg-background py-3 transition-opacity duration-200 ease-out">
        <Button
          variant="ghost"
          size="icon"
          className="h-8 w-8"
          onClick={onToggleCollapsed}
          aria-label={t('missions.detailPanel.title')}
        >
          <ChevronLeft className="h-4 w-4" />
        </Button>
        <div className="mt-3 flex h-8 w-8 items-center justify-center rounded-md border border-border bg-muted/40">
          <PanelRight className="h-4 w-4 text-muted-foreground" />
        </div>
      </div>
    );
  }

  if (!node && !edge) {
    return (
      <div className="flex h-full flex-col bg-background animate-in fade-in-0 slide-in-from-right-2 duration-200">
        <div className="flex items-center justify-between border-b border-border p-4">
          <h3 className="flex items-center gap-2 font-semibold text-foreground">
            <PanelRight className="h-4 w-4 text-muted-foreground" />
            {t('missions.detailPanel.title')}
          </h3>
          <Button variant="ghost" size="icon" className="h-7 w-7" onClick={onToggleCollapsed}>
            <ChevronRight className="h-4 w-4" />
          </Button>
        </div>
        <div className="flex flex-1 items-center justify-center p-8 text-center text-sm text-muted-foreground">
          {t('missions.detailPanel.empty')}
        </div>
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col bg-background animate-in fade-in-0 slide-in-from-right-2 duration-200">
      <div className="flex items-center justify-between border-b border-border p-4">
        <h3 className="flex items-center gap-2 truncate font-semibold text-foreground">
          {node && nodeIcon(node.type)}
          <span className="truncate">
            {node ? node.title : (edge?.label ?? t('missions.detailPanel.edgeDetail'))}
          </span>
        </h3>
        <div className="flex items-center gap-1">
          <Button variant="ghost" size="icon" className="h-7 w-7" onClick={onToggleCollapsed}>
            <ChevronRight className="h-4 w-4" />
          </Button>
          <Button variant="ghost" size="icon" className="h-7 w-7" onClick={onClose}>
            <X className="h-4 w-4" />
          </Button>
        </div>
      </div>
      <div className="flex-1 overflow-y-auto p-4">
        {node ? (
          <NodeDetail
            node={node}
            onPrioritizeBranch={onPrioritizeBranch}
            onAbandonBranch={onAbandonBranch}
            onReopenBranch={onReopenBranch}
            onOpenDecisionGate={onOpenDecisionGate}
            onViewFinding={onViewFinding}
            onViewEvidence={onViewEvidence}
            onViewToolInvocation={onViewToolInvocation}
          />
        ) : edge ? (
          <EdgeDetail edge={edge} />
        ) : null}
      </div>
    </div>
  );
}

/* ── Helpers to safely extract typed values from raw ── */

function asString(raw: Record<string, unknown>, key: string): string | undefined {
  const v = raw[key];
  return typeof v === 'string' ? v : v != null ? String(v) : undefined;
}

function asNumber(raw: Record<string, unknown>, key: string): number | undefined {
  const v = raw[key];
  if (typeof v === 'number') return v;
  if (typeof v === 'string' && v !== '') {
    const n = Number(v);
    return Number.isNaN(n) ? undefined : n;
  }
  return undefined;
}

function asStringArray(raw: Record<string, unknown>, key: string): string[] {
  const v = raw[key];
  if (!Array.isArray(v)) return [];
  return v.map((item) => String(item));
}

function asRecordArray(raw: Record<string, unknown>, key: string): Array<Record<string, unknown>> {
  const value = raw[key];
  return Array.isArray(value)
    ? value.filter((item): item is Record<string, unknown> => typeof item === 'object' && item !== null)
    : [];
}

function asObject(raw: Record<string, unknown>, key: string): Record<string, unknown> | undefined {
  const v = raw[key];
  if (typeof v === 'object' && v !== null && !Array.isArray(v)) {
    return v as Record<string, unknown>;
  }
  return undefined;
}

/* ── Node detail ── */

function NodeDetail({
  node,
  onPrioritizeBranch,
  onAbandonBranch,
  onReopenBranch,
  onOpenDecisionGate,
  onViewFinding,
  onViewEvidence,
  onViewToolInvocation,
}: {
  node: MissionGraphNode;
  onPrioritizeBranch?: (branchId: string) => void;
  onAbandonBranch?: (branchId: string) => void;
  onReopenBranch?: (branchId: string) => void;
  onOpenDecisionGate?: (gateId: string) => void;
  onViewFinding?: (findingId: string) => void;
  onViewEvidence?: (evidenceId: string) => void;
  onViewToolInvocation?: (toolId: string) => void;
}) {
  const { t } = useTranslation();
  const raw = node.raw as Record<string, unknown>;
  const createdAt = asString(raw, 'created_at');
  const updatedAt = asString(raw, 'updated_at');

  return (
    <div className="space-y-4 text-sm">
      {/* Base fields */}
      <DetailField label={t('missions.detailPanel.id')}>
        <code className="block break-all rounded bg-muted p-1.5 text-[10px] text-muted-foreground">
          {node.id}
        </code>
      </DetailField>
      <DetailField label={t('missions.detailPanel.type')}>
        <Badge variant="outline">{t(`missions.graph.nodeTypes.${node.type}`)}</Badge>
      </DetailField>
      {node.status && (
        <DetailField label={t('missions.detailPanel.status')}>
          <Badge tone="neutral" variant="soft">{formatStatus(t, node.status)}</Badge>
        </DetailField>
      )}
      {node.severity && (
        <DetailField label={t('missions.detailPanel.severity')}>
          <Badge variant="outline" className="border-orange-200 bg-orange-50 text-orange-700">
            {node.severity}
          </Badge>
        </DetailField>
      )}
      {node.subtitle && (
        <DetailField label={t('missions.detailPanel.summary')}>
          <p className="whitespace-pre-wrap text-foreground">{node.subtitle}</p>
        </DetailField>
      )}
      {node.branch_id && (
        <DetailField label={t('missions.detailPanel.branch')}>
          <code className="text-xs text-muted-foreground">{node.branch_id}</code>
        </DetailField>
      )}

      {/* Type-specific content */}
      {node.type === 'branch' ? (
        <BranchDetail
          node={node}
          raw={raw}
          onPrioritize={onPrioritizeBranch}
          onAbandon={onAbandonBranch}
          onReopen={onReopenBranch}
        />
      ) : null}

      {node.type === 'tool_invocation' ? (
        <ToolInvocationDetail node={node} raw={raw} onViewToolIO={onViewToolInvocation} />
      ) : null}

      {node.type === 'evidence' ? (
        <EvidenceDetail node={node} raw={raw} onViewEvidence={onViewEvidence} />
      ) : null}

      {node.type === 'finding' ? (
        <FindingDetail node={node} raw={raw} onViewFinding={onViewFinding} />
      ) : null}

      {node.type === 'decision_gate' ? (
        <DecisionGateDetail node={node} raw={raw} onOpenDecision={onOpenDecisionGate} />
      ) : null}

      {node.type === 'asset' ? (
        <AssetDetail raw={raw} />
      ) : null}

      {/* Raw user query and structured constraints from mission metadata */}
      <RawInputDetail raw={raw} />

      {/* Retrieved knowledge and legacy citation metadata */}
      <KnowledgeRetrievalMetadataDetail raw={raw} />

      {/* Related refs */}
      {node.refs && node.refs.length > 0 && (
        <DetailField label={t('missions.detailPanel.relatedFindings')}>
          <div className="space-y-1">
            {node.refs.slice(0, 5).map((ref, idx) => (
              <code
                key={idx}
                className="block break-all rounded bg-muted p-1 text-xs text-muted-foreground"
              >
                {ref}
              </code>
            ))}
            {node.refs.length > 5 && (
              <p className="text-xs text-muted-foreground/60">
                {t('missions.moreCount', { count: node.refs.length - 5 })}
              </p>
            )}
          </div>
        </DetailField>
      )}

      {/* Timestamps */}
      {(createdAt || updatedAt) && (
        <div className="grid grid-cols-2 gap-3 border-t border-border pt-3">
          {createdAt && (
            <DetailField label={t('missions.detailPanel.createdAt')}>
              <span className="text-xs text-muted-foreground">{createdAt}</span>
            </DetailField>
          )}
          {updatedAt && (
            <DetailField label={t('missions.detailPanel.updatedAt')}>
              <span className="text-xs text-muted-foreground">{updatedAt}</span>
            </DetailField>
          )}
        </div>
      )}

      {/* Raw JSON */}
      <details className="border-t border-border pt-3">
        <summary className="flex cursor-pointer items-center gap-1 text-xs font-semibold text-muted-foreground hover:text-foreground">
          <ChevronDown className="h-3 w-3" />
          {t('missions.detailPanel.rawData')}
        </summary>
        <pre className="mt-2 overflow-x-auto rounded bg-foreground p-2 text-[10px] text-background">
          {JSON.stringify(node.raw, null, 2)}
        </pre>
      </details>
    </div>
  );
}

/* ── Branch detail ── */

function BranchDetail({
  node,
  raw,
  onPrioritize,
  onAbandon,
  onReopen,
}: {
  node: MissionGraphNode;
  raw: Record<string, unknown>;
  onPrioritize?: (branchId: string) => void;
  onAbandon?: (branchId: string) => void;
  onReopen?: (branchId: string) => void;
}) {
  const { t } = useTranslation();
  const isAbandoned = node.status === 'abandoned';
  const hypothesis = asString(raw, 'hypothesis');
  const rationale = asString(raw, 'rationale');
  const priority = asString(raw, 'priority');
  const confidence = asNumber(raw, 'confidence');
  const budgetSteps = asString(raw, 'budget_steps');
  const stepsTaken = asString(raw, 'steps_taken');

  return (
    <>
      {hypothesis && (
        <DetailField label={t('missions.detailPanel.hypothesis')}>
          <p className="text-foreground">{hypothesis}</p>
        </DetailField>
      )}
      {rationale && (
        <DetailField label={t('missions.detailPanel.rationale')}>
          <p className="text-foreground">{rationale}</p>
        </DetailField>
      )}
      <div className="grid grid-cols-2 gap-3">
        {priority && (
          <DetailField label={t('missions.detailPanel.priority')}>
            <Badge variant="outline">{priority}</Badge>
          </DetailField>
        )}
        {confidence !== undefined && (
          <DetailField label={t('missions.detailPanel.confidence')}>
            <span className="text-foreground">{Math.round(confidence * 100)}%</span>
          </DetailField>
        )}
        {budgetSteps && (
          <DetailField label={t('missions.detailPanel.budget')}>
            <span className="text-foreground">{budgetSteps}</span>
          </DetailField>
        )}
        {stepsTaken && (
          <DetailField label={t('missions.detailPanel.stepsTaken')}>
            <span className="text-foreground">{stepsTaken}</span>
          </DetailField>
        )}
      </div>

      {/* Action buttons */}
      <div className="flex flex-wrap gap-2 border-t border-border pt-3">
        <Button
          size="sm"
          variant="default"
          onClick={() => onPrioritize?.(node.id)}
          disabled={!onPrioritize}
        >
          <Flag className="mr-1 h-3 w-3" />
          {t('missions.detailPanel.prioritize')}
        </Button>
        {isAbandoned ? (
          <Button
            size="sm"
            variant="outline"
            onClick={() => onReopen?.(node.id)}
            disabled={!onReopen}
          >
            <RotateCcw className="mr-1 h-3 w-3" />
            {t('missions.detailPanel.reopen')}
          </Button>
        ) : (
          <Button
            size="sm"
            variant="outline"
            onClick={() => onAbandon?.(node.id)}
            disabled={!onAbandon}
          >
            <Ban className="mr-1 h-3 w-3" />
            {t('missions.detailPanel.abandon')}
          </Button>
        )}
      </div>
    </>
  );
}

/* ── ToolInvocation detail ── */

function ToolInvocationDetail({
  node,
  raw,
  onViewToolIO,
}: {
  node: MissionGraphNode;
  raw: Record<string, unknown>;
  onViewToolIO?: (toolId: string) => void;
}) {
  const { t } = useTranslation();
  const toolName = asString(raw, 'tool_name');
  const inputSummary = asString(raw, 'input_summary');
  const outputSummary = asString(raw, 'output_summary');
  const error = asString(raw, 'error');
  const artifactPaths = asStringArray(raw, 'artifact_paths');
  const startedAt = asString(raw, 'started_at');
  const finishedAt = asString(raw, 'finished_at');

  return (
    <>
      {toolName && (
        <DetailField label={t('missions.detailPanel.toolName')}>
          <code className="text-xs font-medium text-foreground">{toolName}</code>
        </DetailField>
      )}
      {inputSummary && (
        <DetailField label={t('missions.detailPanel.inputSummary')}>
          <p className="text-foreground">{inputSummary}</p>
        </DetailField>
      )}
      {outputSummary && (
        <DetailField label={t('missions.detailPanel.outputSummary')}>
          <p className="text-foreground">{outputSummary}</p>
        </DetailField>
      )}
      {error && (
        <DetailField label={t('missions.detailPanel.error')}>
          <p className="text-destructive">{error}</p>
        </DetailField>
      )}
      {artifactPaths.length > 0 && (
        <DetailField label={t('missions.detailPanel.artifactPaths')}>
          <div className="space-y-1">
            {artifactPaths.map((path, idx) => (
              <code key={idx} className="block break-all rounded bg-muted p-1 text-xs text-muted-foreground">
                {path}
              </code>
            ))}
          </div>
        </DetailField>
      )}
      <div className="grid grid-cols-2 gap-3">
        {startedAt && (
          <DetailField label={t('missions.detailPanel.startedAt')}>
            <span className="text-xs text-muted-foreground">{startedAt}</span>
          </DetailField>
        )}
        {finishedAt && (
          <DetailField label={t('missions.detailPanel.finishedAt')}>
            <span className="text-xs text-muted-foreground">{finishedAt}</span>
          </DetailField>
        )}
      </div>
      <div className="border-t border-border pt-3">
        <Button
          size="sm"
          variant="outline"
          onClick={() => onViewToolIO?.(node.id)}
          disabled={!onViewToolIO}
        >
          <ExternalLink className="mr-1 h-3 w-3" />
          {t('missions.detailPanel.viewToolIO')}
        </Button>
      </div>
    </>
  );
}

/* ── Evidence detail ── */

function EvidenceDetail({
  node,
  raw,
  onViewEvidence,
}: {
  node: MissionGraphNode;
  raw: Record<string, unknown>;
  onViewEvidence?: (evidenceId: string) => void;
}) {
  const { t } = useTranslation();
  const evidenceType = asString(raw, 'evidence_type');
  const location = asString(raw, 'location');
  const supportsFacts = asStringArray(raw, 'supports_facts');
  const metadata = asObject(raw, 'metadata');
  const artifactPaths = asStringArray(raw, 'artifact_paths');

  return (
    <>
      {evidenceType && (
        <DetailField label={t('missions.detailPanel.evidenceType')}>
          <Badge variant="outline">{evidenceType}</Badge>
        </DetailField>
      )}
      {location && (
        <DetailField label={t('missions.detailPanel.location')}>
          <code className="break-all text-xs text-muted-foreground">{location}</code>
        </DetailField>
      )}
      {supportsFacts.length > 0 && (
        <DetailField label={t('missions.detailPanel.supportsFacts')}>
          <ul className="space-y-1">
            {supportsFacts.map((fact, idx) => (
              <li key={idx} className="text-foreground">• {fact}</li>
            ))}
          </ul>
        </DetailField>
      )}
      {metadata && (
        <DetailField label={t('missions.detailPanel.metadata')}>
          <pre className="overflow-x-auto rounded bg-muted p-2 text-[10px] text-muted-foreground">
            {JSON.stringify(metadata, null, 2)}
          </pre>
        </DetailField>
      )}
      {artifactPaths.length > 0 && (
        <DetailField label={t('missions.detailPanel.artifactPaths')}>
          <div className="space-y-1">
            {artifactPaths.map((path, idx) => (
              <code key={idx} className="block break-all rounded bg-muted p-1 text-xs text-muted-foreground">
                {path}
              </code>
            ))}
          </div>
        </DetailField>
      )}
      <div className="border-t border-border pt-3">
        <Button
          size="sm"
          variant="outline"
          onClick={() => onViewEvidence?.(node.id)}
          disabled={!onViewEvidence}
        >
          <ExternalLink className="mr-1 h-3 w-3" />
          {t('missions.detailPanel.viewEvidence')}
        </Button>
      </div>
    </>
  );
}

/* ── Finding detail ── */

function FindingDetail({
  node,
  raw,
  onViewFinding,
}: {
  node: MissionGraphNode;
  raw: Record<string, unknown>;
  onViewFinding?: (findingId: string) => void;
}) {
  const { t } = useTranslation();
  const ruleId = asString(raw, 'rule_id');
  const cwe = asString(raw, 'cwe');
  const confidence = asNumber(raw, 'confidence');
  const evidenceIds = asStringArray(raw, 'evidence_ids');

  return (
    <>
      {ruleId && (
        <DetailField label={t('missions.detailPanel.ruleId')}>
          <code className="text-xs text-foreground">{ruleId}</code>
        </DetailField>
      )}
      {cwe && (
        <DetailField label={t('missions.detailPanel.cwe')}>
          <Badge variant="outline">{cwe}</Badge>
        </DetailField>
      )}
      {confidence !== undefined && (
        <DetailField label={t('missions.detailPanel.confidence')}>
          <span className="text-foreground">{Math.round(confidence * 100)}%</span>
        </DetailField>
      )}
      {evidenceIds.length > 0 && (
        <DetailField label={t('missions.detailPanel.evidenceIds')}>
          <div className="space-y-1">
            {evidenceIds.map((eid, idx) => (
              <code key={idx} className="block break-all rounded bg-muted p-1 text-xs text-muted-foreground">
                {eid}
              </code>
            ))}
          </div>
        </DetailField>
      )}
      <div className="border-t border-border pt-3">
        <Button
          size="sm"
          variant="outline"
          onClick={() => onViewFinding?.(node.id)}
          disabled={!onViewFinding}
        >
          <ExternalLink className="mr-1 h-3 w-3" />
          {t('missions.detailPanel.viewFinding')}
        </Button>
      </div>
    </>
  );
}

/* ── DecisionGate detail ── */

function DecisionGateDetail({
  node,
  raw,
  onOpenDecision,
}: {
  node: MissionGraphNode;
  raw: Record<string, unknown>;
  onOpenDecision?: (gateId: string) => void;
}) {
  const { t } = useTranslation();
  const question = asString(raw, 'question');
  const kind = asString(raw, 'kind');
  const availableOptions = asStringArray(raw, 'available_options');

  return (
    <>
      {question && (
        <DetailField label={t('missions.detailPanel.question')}>
          <p className="font-medium text-foreground">{question}</p>
        </DetailField>
      )}
      {kind && (
        <DetailField label={t('missions.detailPanel.kind')}>
          <Badge variant="outline">{kind}</Badge>
        </DetailField>
      )}
      {availableOptions.length > 0 && (
        <DetailField label={t('missions.detailPanel.availableOptions')}>
          <div className="space-y-1">
            {availableOptions.map((opt, idx) => (
              <div key={idx} className="rounded border border-border px-2 py-1 text-xs text-foreground">
                {opt}
              </div>
            ))}
          </div>
        </DetailField>
      )}
      <div className="border-t border-border pt-3">
        <Button
          size="sm"
          variant="default"
          onClick={() => onOpenDecision?.(node.id)}
          disabled={!onOpenDecision}
        >
          <Play className="mr-1 h-3 w-3" />
          {t('missions.detailPanel.openDecision')}
        </Button>
      </div>
    </>
  );
}

/* ── Asset detail ── */

function AssetDetail({
  raw,
}: {
  raw: Record<string, unknown>;
}) {
  const { t } = useTranslation();
  const assetType = asString(raw, 'asset_type');
  const sensitivity = asString(raw, 'sensitivity');
  const source = asString(raw, 'source');
  const value = asString(raw, 'value');
  const metadata = asObject(raw, 'metadata');
  const originalFilename = metadata ? asString(metadata, 'original_filename') : undefined;
  const detectedInputType = metadata ? asString(metadata, 'detected_input_type') : undefined;
  const sizeBytes = metadata ? asNumber(metadata, 'size_bytes') : undefined;
  const sha256 = metadata ? asString(metadata, 'sha256') : asString(raw, 'sha256');
  const artifactUri = metadata ? asString(metadata, 'artifact_uri') : undefined;
  const artifactRecordId = metadata ? asString(metadata, 'artifact_record_id') : asString(raw, 'source_id');
  const evidenceIds = asStringArray(raw, 'evidence_ids');
  const findingIds = asStringArray(raw, 'finding_ids');
  const toolInvocationIds = asStringArray(raw, 'tool_invocation_ids');

  return (
    <>
      {assetType && (
        <DetailField label={t('missions.detailPanel.assetType')}>
          <Badge variant="outline">{assetType}</Badge>
        </DetailField>
      )}
      {sensitivity && (
        <DetailField label={t('missions.detailPanel.sensitivity')}>
          <Badge
            tone={sensitivity === 'secret' || sensitivity === 'credential' || sensitivity === 'account' ? 'danger' : 'neutral'}
            variant="soft"
          >
            {sensitivity}
          </Badge>
        </DetailField>
      )}
      {source && (
        <DetailField label={t('missions.detailPanel.assetSource')}>
          <span className="text-foreground">{source}</span>
        </DetailField>
      )}
      {originalFilename && (
        <DetailField label={t('missions.detailPanel.originalFilename')}>
          <p className="break-all text-foreground">{originalFilename}</p>
        </DetailField>
      )}
      {detectedInputType && (
        <DetailField label={t('missions.detailPanel.detectedInputType')}>
          <Badge variant="outline">{detectedInputType}</Badge>
        </DetailField>
      )}
      {value && !originalFilename && (
        <DetailField label={t('missions.detailPanel.assetValue')}>
          <code className="block break-all rounded bg-muted p-1.5 text-[10px] text-muted-foreground">
            {value}
          </code>
        </DetailField>
      )}
      {sizeBytes !== undefined && (
        <DetailField label={t('missions.detailPanel.fileSize')}>
          <span className="text-foreground">{formatFileSize(sizeBytes)}</span>
        </DetailField>
      )}
      {sha256 && (
        <DetailField label={t('missions.detailPanel.sha256')}>
          <code className="block break-all rounded bg-muted p-1.5 text-[10px] text-muted-foreground">
            {sha256}
          </code>
        </DetailField>
      )}
      {artifactRecordId && (
        <DetailField label={t('missions.detailPanel.artifactId')}>
          <code className="block break-all text-xs text-muted-foreground">{artifactRecordId}</code>
        </DetailField>
      )}
      {artifactUri && (
        <DetailField label={t('missions.detailPanel.artifactUri')}>
          <code className="block break-all text-xs text-muted-foreground">{artifactUri}</code>
        </DetailField>
      )}
      {(evidenceIds.length > 0 || findingIds.length > 0 || toolInvocationIds.length > 0) && (
        <div className="space-y-2 border-t border-border pt-3">
          {evidenceIds.length > 0 && (
            <DetailField label={t('missions.detailPanel.relatedEvidence')}>
              <div className="space-y-1">
                {evidenceIds.map((eid, idx) => (
                  <code key={idx} className="block break-all rounded bg-muted p-1 text-xs text-muted-foreground">
                    {eid}
                  </code>
                ))}
              </div>
            </DetailField>
          )}
          {findingIds.length > 0 && (
            <DetailField label={t('missions.detailPanel.relatedFindings')}>
              <div className="space-y-1">
                {findingIds.map((fid, idx) => (
                  <code key={idx} className="block break-all rounded bg-muted p-1 text-xs text-muted-foreground">
                    {fid}
                  </code>
                ))}
              </div>
            </DetailField>
          )}
          {toolInvocationIds.length > 0 && (
            <DetailField label={t('missions.detailPanel.relatedTools')}>
              <div className="space-y-1">
                {toolInvocationIds.map((tid, idx) => (
                  <code key={idx} className="block break-all rounded bg-muted p-1 text-xs text-muted-foreground">
                    {tid}
                  </code>
                ))}
              </div>
            </DetailField>
          )}
        </div>
      )}
    </>
  );
}

function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

/* ── Edge detail ── */

function EdgeDetail({ edge }: { edge: MissionGraphEdge }) {
  const { t } = useTranslation();
  const style = EDGE_TYPE_STYLES[edge.type];

  return (
    <div className="space-y-4 text-sm">
      <DetailField label={t('missions.detailPanel.edgeId')}>
        <code className="block break-all rounded bg-muted p-1.5 text-[10px] text-muted-foreground">
          {edge.id}
        </code>
      </DetailField>
      <DetailField label={t('missions.detailPanel.type')}>
        <div className="flex items-center gap-2">
          <div
            className="h-0.5 w-8"
            style={{ backgroundColor: style.color, borderStyle: style.dashed ? 'dashed' : 'solid' }}
          />
          <Badge variant="outline">{t(`missions.graph.edgeTypes.${edge.type}`)}</Badge>
        </div>
      </DetailField>
      <DetailField label={t('missions.detailPanel.source')}>
        <code className="block break-all text-xs text-muted-foreground">{edge.source}</code>
      </DetailField>
      <DetailField label={t('missions.detailPanel.target')}>
        <code className="block break-all text-xs text-muted-foreground">{edge.target}</code>
      </DetailField>
      {edge.label && (
        <DetailField label={t('missions.detailPanel.relationship')}>
          <p className="text-foreground">{edge.label}</p>
        </DetailField>
      )}
      {edge.blocking !== undefined && (
        <DetailField label={t('missions.detailPanel.blocking')}>
          <Badge tone={edge.blocking ? 'danger' : 'neutral'} variant="soft">
            {edge.blocking ? t('missions.detailPanel.yes') : t('missions.detailPanel.no')}
          </Badge>
        </DetailField>
      )}
      {edge.confidence !== null && edge.confidence !== undefined && (
        <DetailField label={t('missions.detailPanel.confidence')}>
          <p className="text-foreground">{Math.round(edge.confidence * 100)}%</p>
        </DetailField>
      )}
      <details className="border-t border-border pt-3">
        <summary className="flex cursor-pointer items-center gap-1 text-xs font-semibold text-muted-foreground hover:text-foreground">
          <ChevronDown className="h-3 w-3" />
          {t('missions.detailPanel.rawData')}
        </summary>
        <pre className="mt-2 overflow-x-auto rounded bg-foreground p-2 text-[10px] text-background">
          {JSON.stringify(edge.raw, null, 2)}
        </pre>
      </details>
    </div>
  );
}

/* ── Raw input detail (raw_user_query + structured_constraints) ── */

function RawInputDetail({ raw }: { raw: Record<string, unknown> }) {
  const { t } = useTranslation();

  // Try top-level first, then nested metadata
  const rawUserQuery = asString(raw, 'raw_user_query')
    ?? (raw.metadata && typeof raw.metadata === 'object'
      ? asString(raw.metadata as Record<string, unknown>, 'raw_user_query')
      : undefined)
    ?? asString(raw, 'user_goal');

  const constraintsObj = raw.metadata && typeof raw.metadata === 'object'
    ? asObject(raw.metadata as Record<string, unknown>, 'structured_constraints')
    : asObject(raw, 'structured_constraints');

  if (!rawUserQuery && !constraintsObj) return null;

  const inScope = constraintsObj ? asStringArray(constraintsObj, 'in_scope') : [];
  const forbiddenTargets = constraintsObj ? asStringArray(constraintsObj, 'forbidden_targets') : [];
  const forbiddenPorts = constraintsObj ? asStringArray(constraintsObj, 'forbidden_ports') : [];
  const forbiddenActions = constraintsObj ? asStringArray(constraintsObj, 'forbidden_actions') : [];
  const maxIntrusiveness = constraintsObj ? asString(constraintsObj, 'max_intrusiveness') : undefined;
  const notes = constraintsObj ? asStringArray(constraintsObj, 'notes') : [];

  return (
    <div className="space-y-3 border-t border-border pt-3">
      {rawUserQuery && (
        <DetailField label={t('missions.detailPanel.rawUserQuery')}>
          <div className="rounded-md border border-border bg-muted/30 p-2 text-sm text-foreground whitespace-pre-wrap break-words">
            {rawUserQuery}
          </div>
        </DetailField>
      )}
      {constraintsObj && (
        <DetailField label={t('missions.detailPanel.structuredConstraints')}>
          <div className="space-y-2">
            {inScope.length > 0 && (
              <ConstraintPill label={t('missions.detailPanel.inScope')} values={inScope} tone="success" />
            )}
            {forbiddenTargets.length > 0 && (
              <ConstraintPill label={t('missions.detailPanel.forbiddenTargets')} values={forbiddenTargets} tone="danger" />
            )}
            {forbiddenPorts.length > 0 && (
              <ConstraintPill label={t('missions.detailPanel.forbiddenPorts')} values={forbiddenPorts} tone="danger" />
            )}
            {forbiddenActions.length > 0 && (
              <ConstraintPill label={t('missions.detailPanel.forbiddenActions')} values={forbiddenActions} tone="danger" />
            )}
            {maxIntrusiveness && (
              <div className="flex items-center gap-2 text-xs">
                <span className="text-muted-foreground">{t('missions.detailPanel.maxIntrusiveness')}:</span>
                <Badge variant="outline" tone="warning">{maxIntrusiveness}</Badge>
              </div>
            )}
            {notes.length > 0 && (
              <div className="text-xs text-muted-foreground space-y-0.5">
                {notes.map((note, idx) => (
                  <div key={idx}>• {note}</div>
                ))}
              </div>
            )}
          </div>
        </DetailField>
      )}
    </div>
  );
}

function ConstraintPill({ label, values, tone }: { label: string; values: string[]; tone: 'success' | 'danger' }) {
  return (
    <div className="flex flex-wrap items-center gap-1.5 text-xs">
      <span className="text-muted-foreground">{label}:</span>
      {values.map((v, idx) => (
        <Badge key={idx} variant="soft" tone={tone}>{v}</Badge>
      ))}
    </div>
  );
}

/* ── Knowledge retrieval metadata detail ── */

function KnowledgeRetrievalMetadataDetail({ raw }: { raw: Record<string, unknown> }) {
  const { t } = useTranslation();

  const metadata = raw.metadata && typeof raw.metadata === 'object'
    ? raw.metadata as Record<string, unknown>
    : raw;

  const knowledgeItems = asRecordArray(metadata, 'knowledge_cards');
  const legacyCitations = asRecordArray(metadata, 'graphrag_citations');
  const citations = knowledgeItems.length > 0
    ? knowledgeItems
    : legacyCitations.length > 0
      ? legacyCitations
      : asRecordArray(metadata, 'citations');
  const noHitGap = asString(metadata, 'no_hit_gap')
    ?? asString(metadata, 'graphrag_no_hit_gap');

  if (citations.length === 0 && !noHitGap) return null;

  const citationsList: Array<{ source_id: string; title?: string }> = [];
  for (const citation of citations) {
    const sourceId = asString(citation, 'source_id') ?? asString(citation, 'id');
    const title = asString(citation, 'title');
    if (sourceId) citationsList.push({ source_id: sourceId, title });
  }

  return (
    <div className="space-y-2 border-t border-border pt-3">
      {citationsList.length > 0 && (
        <DetailField label={t('missions.detailPanel.knowledgeCitations')}>
          <div className="flex flex-wrap gap-1.5">
            {citationsList.map((cit, idx) => (
              <Badge key={idx} variant="soft" tone="info">
                {cit.title ?? cit.source_id}
              </Badge>
            ))}
          </div>
        </DetailField>
      )}
      {noHitGap && (
        <DetailField label={t('missions.detailPanel.noHitGap')}>
          <div className="rounded-md border border-info-border bg-info-soft p-2 text-xs text-info-foreground">
            {noHitGap}
          </div>
        </DetailField>
      )}
    </div>
  );
}

/* ── Shared ── */

function DetailField({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <div className="mb-1 text-xs text-muted-foreground">{label}</div>
      {children}
    </div>
  );
}
