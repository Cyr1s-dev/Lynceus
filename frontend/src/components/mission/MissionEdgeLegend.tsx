/**
 * Edge legend shared with the DAG view. It renders the same stable edge colors
 * declared in missionGraphTypes so users can read the canvas without guessing.
 */
import { useTranslation } from 'react-i18next';
import { Badge } from '@/ui/untitled';
import { EDGE_TYPE_STYLES, type MissionGraph, type MissionGraphEdgeType } from '@/lib/missionGraphTypes';

const EDGE_TYPES = Object.keys(EDGE_TYPE_STYLES) as MissionGraphEdgeType[];

export function MissionEdgeLegend({ graph }: { graph: MissionGraph }) {
  const { t } = useTranslation();
  const counts = new Map<MissionGraphEdgeType, number>();

  for (const edge of graph.edges) {
    counts.set(edge.type, (counts.get(edge.type) ?? 0) + 1);
  }

  return (
    <div className="overflow-hidden rounded-lg border border-border bg-card shadow-xs">
      <div className="border-b border-border px-4 py-3">
        <h3 className="text-sm font-semibold text-foreground">{t('missions.graph.edgeLegend')}</h3>
      </div>
      <div className="space-y-1.5 p-3">
        {EDGE_TYPES.map((type) => {
          const style = EDGE_TYPE_STYLES[type];
          const count = counts.get(type) ?? 0;
          return (
            <div key={type} className="flex items-center gap-2 text-xs text-muted-foreground">
              <span
                className="h-0 w-7 shrink-0 border-t-2"
                style={{
                  borderColor: style.color,
                  borderStyle: style.dashed ? 'dashed' : 'solid',
                }}
              />
              <span className="min-w-0 flex-1 truncate text-foreground/80" title={t(`missions.graph.edgeTypes.${type}`)}>
                {t(`missions.graph.edgeTypes.${type}`)}
              </span>
              <Badge tone="neutral" variant="outline" className="h-5 min-w-7 shrink-0 justify-center px-1.5 tabular-nums text-[10px]">
                {count}
              </Badge>
            </div>
          );
        })}
      </div>
    </div>
  );
}
