/**
 * Tree view of the Mission Exploration Graph. Shows the stable hierarchy:
 * Mission -> Branches -> Tasks / Tools / Evidence / Findings. Only shows direct
 * parent-child structure — cross-references and non-hierarchical edges are hidden
 * (those belong to the DAG view).
 *
 * Renders as vertical swim-lanes with nodes in columns. The Mission always
 * appears alone in the first column; Branches in the second; everything else fans
 * out in the remaining columns by type.
 */
import { useTranslation } from 'react-i18next';
import { FileSearch } from 'lucide-react';
import { ScrollArea } from '@/ui/untitled';
import type { MissionGraph, MissionGraphNode } from '@/lib/missionGraphTypes';
import { MissionNodeCard } from './MissionNodeCard';

interface TreeColumn {
  key: string;
  label: string;
  types: MissionGraphNode['type'][];
}

export function MissionTreeView({
  graph,
  selectedNodeId,
  onNodeClick,
}: {
  graph: MissionGraph;
  selectedNodeId?: string | null;
  onNodeClick?: (node: MissionGraphNode) => void;
}) {
  const { t } = useTranslation();

  const columns: TreeColumn[] = [
    { key: 'mission', label: t('missions.columnMission'), types: ['mission'] },
    { key: 'branches', label: t('missions.columnBranches'), types: ['branch'] },
    {
      key: 'tasks',
      label: t('missions.graph.nodeTypes.exploration_task'),
      types: ['exploration_task', 'tool_invocation', 'directive'],
    },
    {
      key: 'results',
      label: t('missions.columnResults'),
      types: ['evidence', 'finding', 'decision_gate', 'strategy_board'],
    },
  ];

  if (graph.nodes.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center py-16 text-center">
        <FileSearch className="w-8 h-8 text-slate-300 mb-3" />
        <p className="text-sm text-slate-500">{t('missions.graph.empty')}</p>
      </div>
    );
  }

  return (
    <ScrollArea className="flex-1 w-full h-full">
      <div className="flex gap-6 min-w-[1000px] p-4 pb-10">
        {columns.map((col) => {
          const colNodes = graph.nodes.filter((n) => col.types.includes(n.type));
          return (
            <div key={col.key} className="flex-1 flex flex-col min-w-[240px]">
              <h4 className="text-xs font-bold text-slate-500 uppercase tracking-wider mb-4 px-1">
                {col.label}
              </h4>
              <div className="space-y-3">
                {colNodes.length === 0 ? (
                  <div className="text-xs text-slate-400 p-3 border border-dashed rounded-md text-center">
                    {t('missions.statusQueues.empty')}
                  </div>
                ) : (
                  colNodes.map((node) => (
                    <MissionNodeCard
                      key={node.id}
                      node={node}
                      selected={selectedNodeId === node.id}
                      onClick={() => onNodeClick?.(node)}
                    />
                  ))
                )}
              </div>
            </div>
          );
        })}
      </div>
    </ScrollArea>
  );
}
