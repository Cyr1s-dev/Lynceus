/**
 * Left sidebar showing triage queues. Each queue holds nodes requiring a specific
 * action (blocked, needs decision, needs evidence, etc.). Clicking a queue item
 * selects that node in the main canvas.
 *
 * Queues without backend support yet (observer review, ready-to-verify) render
 * as real empty states — the UI never fabricates queue contents.
 */
import { useTranslation } from 'react-i18next';
import { GitBranch, AlertCircle, Zap, Database, Eye, CheckCircle2, Wrench, Star } from 'lucide-react';
import { Badge } from '@/ui/untitled';
import type { StatusQueues, MissionGraphNode } from '@/lib/missionGraphTypes';
import { STATUS_QUEUE_KEYS } from '@/lib/missionGraphTypes';
import { MissionNodeCard } from './MissionNodeCard';

const QUEUE_LABEL_KEYS: Record<keyof StatusQueues, string> = {
  active_branches: 'activeBranches',
  blocked: 'blocked',
  needs_user_decision: 'needsUserDecision',
  needs_evidence: 'needsEvidence',
  needs_observer_review: 'needsObserverReview',
  ready_to_verify: 'readyToVerify',
  ready_to_patch: 'readyToPatch',
  high_value_next_actions: 'highValueNextActions',
};

const QUEUE_META: Record<
  keyof StatusQueues,
  {
    icon: React.ReactNode;
    iconClass?: string;
  }
> = {
  active_branches: { icon: <GitBranch className="w-3.5 h-3.5 text-blue-500" /> },
  blocked: { icon: <AlertCircle className="w-3.5 h-3.5 text-amber-500" /> },
  needs_user_decision: { icon: <Zap className="w-3.5 h-3.5 text-amber-500" /> },
  needs_evidence: { icon: <Database className="w-3.5 h-3.5 text-slate-500" /> },
  needs_observer_review: { icon: <Eye className="w-3.5 h-3.5 text-purple-500" /> },
  ready_to_verify: { icon: <CheckCircle2 className="w-3.5 h-3.5 text-green-500" /> },
  ready_to_patch: { icon: <Wrench className="w-3.5 h-3.5 text-teal-500" /> },
  high_value_next_actions: { icon: <Star className="w-3.5 h-3.5 text-amber-500" /> },
};

export function MissionStatusQueues({
  queues,
  onNodeClick,
}: {
  queues: StatusQueues;
  onNodeClick?: (node: MissionGraphNode) => void;
}) {
  const { t } = useTranslation();

  return (
    <div className="space-y-4">
      <div className="overflow-hidden rounded-lg border border-border bg-card shadow-xs">
        <div className="border-b border-border px-4 py-3">
          <h3 className="text-sm font-semibold text-foreground">{t('missions.statusQueues.title')}</h3>
        </div>
        <div className="divide-y divide-border">
          {STATUS_QUEUE_KEYS.map((key) => {
            const items = queues[key];
            const meta = QUEUE_META[key];
            return (
              <div key={key} className="p-3.5">
                <div className="mb-2 flex items-center gap-2 text-xs font-semibold text-foreground">
                  <span className="shrink-0">{meta.icon}</span>
                  <span className="min-w-0 flex-1 truncate">{t(`missions.statusQueues.${QUEUE_LABEL_KEYS[key]}`)}</span>
                  <Badge tone="neutral" variant="outline" className="ml-auto shrink-0 tabular-nums text-[10px]">
                    {items.length}
                  </Badge>
                </div>
                {items.length === 0 ? (
                  <p className="text-xs text-muted-foreground">{t('missions.statusQueues.empty')}</p>
                ) : (
                  <div className="max-h-56 space-y-1.5 overflow-y-auto pr-1">
                    {items.map((node) => (
                      <MissionNodeCard
                        key={node.id}
                        node={node}
                        onClick={() => onNodeClick?.(node)}
                        compact
                      />
                    ))}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
