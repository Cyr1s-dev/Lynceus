/**
 * Compact metric strip for the Mission Exploration Canvas.
 *
 * All values are derived from the normalized MissionGraph. No counts are mocked
 * or inferred from unavailable backend state.
 */
import { useMemo } from 'react';
import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { Activity, Database, GitBranch, Network, ShieldAlert, TerminalSquare, Zap } from 'lucide-react';
import { Badge } from '@/ui/untitled';
import type { MissionGraph, MissionGraphNodeType } from '@/lib/missionGraphTypes';

const STAT_DEFINITIONS: Array<{
  key: string;
  labelKey: string;
  icon: ReactNode;
  types?: MissionGraphNodeType[];
  countEdges?: boolean;
}> = [
  {
    key: 'branches',
    labelKey: 'missions.canvasStats.branches',
    icon: <GitBranch className="h-3.5 w-3.5 text-blue-500" />,
    types: ['branch'],
  },
  {
    key: 'tasks',
    labelKey: 'missions.canvasStats.tasks',
    icon: <Activity className="h-3.5 w-3.5 text-indigo-500" />,
    types: ['exploration_task'],
  },
  {
    key: 'tools',
    labelKey: 'missions.canvasStats.tools',
    icon: <TerminalSquare className="h-3.5 w-3.5 text-emerald-500" />,
    types: ['tool_invocation'],
  },
  {
    key: 'evidence',
    labelKey: 'missions.canvasStats.evidence',
    icon: <Database className="h-3.5 w-3.5 text-slate-500" />,
    types: ['evidence'],
  },
  {
    key: 'findings',
    labelKey: 'missions.canvasStats.findings',
    icon: <ShieldAlert className="h-3.5 w-3.5 text-rose-500" />,
    types: ['finding'],
  },
  {
    key: 'decisions',
    labelKey: 'missions.canvasStats.decisions',
    icon: <Zap className="h-3.5 w-3.5 text-amber-500" />,
    types: ['decision_gate'],
  },
  {
    key: 'edges',
    labelKey: 'missions.canvasStats.edges',
    icon: <Network className="h-3.5 w-3.5 text-sky-500" />,
    countEdges: true,
  },
];

export function MissionCanvasStats({ graph }: { graph: MissionGraph }) {
  const { t } = useTranslation();

  const counts = useMemo(() => {
    const byType = new Map<MissionGraphNodeType, number>();
    for (const node of graph.nodes) {
      byType.set(node.type, (byType.get(node.type) ?? 0) + 1);
    }
    return byType;
  }, [graph.nodes]);

  return (
    <div className="flex flex-wrap items-center gap-2">
      {STAT_DEFINITIONS.map((stat) => {
        const count = stat.countEdges
          ? graph.edges.length
          : (stat.types ?? []).reduce((sum, type) => sum + (counts.get(type) ?? 0), 0);
        return (
          <Badge
            key={stat.key}
            tone="neutral"
            variant="outline"
            className="h-7 gap-1.5 border-border bg-card px-2.5 text-[11px] font-medium text-muted-foreground shadow-xs"
          >
            {stat.icon}
            <span>{t(stat.labelKey)}</span>
            <span className="font-semibold tabular-nums text-foreground">{count}</span>
          </Badge>
        );
      })}
    </div>
  );
}
