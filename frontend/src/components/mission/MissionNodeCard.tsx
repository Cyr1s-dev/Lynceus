/**
 * Compact card for a single Mission graph node. Shows only a short title,
 * status, and type — full detail lives in the {@link MissionDetailPanel}.
 * Reused by the Tree view and the status queues.
 */
import { useTranslation } from 'react-i18next';
import { Badge } from '@/ui/untitled';
import { cn } from '@/lib/utils';
import { formatStatus } from '@/lib/i18n-formatters';
import type { MissionGraphNode } from '@/lib/missionGraphTypes';
import { nodeIcon, nodeAccentClass } from './missionNodeVisuals';

function isFailedStatus(status?: string): boolean {
  return status === 'failed' || status === 'error' || status === 'timeout';
}

function isBlockedStatus(status?: string): boolean {
  return status === 'blocked';
}

export function MissionNodeCard({
  node,
  selected,
  onClick,
  compact,
}: {
  node: MissionGraphNode;
  selected?: boolean;
  onClick?: () => void;
  compact?: boolean;
}) {
  const { t } = useTranslation();
  const failed = isFailedStatus(node.status);
  const blocked = isBlockedStatus(node.status);

  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        'w-full rounded-lg border p-3 text-left transition-all hover:border-slate-300 hover:shadow-sm',
        nodeAccentClass(node.type),
        failed && 'border-red-200 bg-red-50/40',
        blocked && 'border-amber-200 bg-amber-50/40',
        selected && 'border-primary ring-1 ring-primary/30',
      )}
    >
      <div className="flex items-start gap-2">
        <div className="mt-0.5 shrink-0">{nodeIcon(node.type)}</div>
        <div className="min-w-0 flex-1">
          <div className="flex items-center justify-between gap-1 mb-1">
            <span className="text-[10px] font-medium uppercase tracking-wider text-slate-500">
              {t(`missions.graph.nodeTypes.${node.type}`)}
            </span>
            {node.status && (
              <span
                className={cn(
                  'text-[10px] px-1.5 py-0.5 rounded-sm shrink-0',
                  failed ? 'bg-red-100 text-red-700' : blocked ? 'bg-amber-100 text-amber-700' : 'bg-slate-100 text-slate-600',
                )}
              >
                {formatStatus(t, node.status)}
              </span>
            )}
          </div>
          <div className="text-sm font-semibold text-slate-800 line-clamp-2 leading-tight">{node.title}</div>
          {!compact && node.subtitle && (
            <div className="mt-1 text-xs text-slate-600 line-clamp-2">{node.subtitle}</div>
          )}
          {node.severity && (
            <Badge variant="outline" className="mt-2 text-[10px] bg-white/70 border-current">
              {node.severity}
            </Badge>
          )}
        </div>
      </div>
    </button>
  );
}
