/**
 * Canvas controls for the Mission exploration workspace.
 *
 * The buttons operate on local visualization state only; they do not mutate the
 * backend Mission graph.
 */
import { Eye, EyeOff, FilterX, Maximize2, RotateCcw, Share2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/ui/untitled';

export function MissionCanvasToolbar({
  onExpandAll,
  onCollapseAll,
  onClearSelection,
  onResetLayout,
  onFitView,
  skeletonAvailable = false,
  skeletonActive = false,
  onToggleSkeleton,
}: {
  onExpandAll: () => void;
  onCollapseAll: () => void;
  onClearSelection: () => void;
  onResetLayout: () => void;
  onFitView: () => void;
  /** Whether the graph is large enough that the skeleton fallback applies. */
  skeletonAvailable?: boolean;
  /** Whether the skeleton view is currently hiding non-skeleton node types. */
  skeletonActive?: boolean;
  onToggleSkeleton?: () => void;
}) {
  const { t } = useTranslation();

  return (
    <div className="flex flex-wrap items-center justify-end gap-1.5">
      {skeletonAvailable && onToggleSkeleton && (
        <Button
          type="button"
          variant={skeletonActive ? 'secondary' : 'outline'}
          size="sm"
          className="h-8 px-2.5"
          onClick={onToggleSkeleton}
          title={t('missions.graph.skeletonModeHint')}
        >
          <Share2 className="h-3.5 w-3.5" />
          <span className="hidden sm:inline">{t('missions.graph.skeletonMode')}</span>
        </Button>
      )}
      <Button type="button" variant="outline" size="sm" className="h-8 px-2.5" onClick={onExpandAll}>
        <Eye className="h-3.5 w-3.5" />
        <span className="hidden sm:inline">{t('missions.graph.expandAll')}</span>
      </Button>
      <Button type="button" variant="outline" size="sm" className="h-8 px-2.5" onClick={onCollapseAll}>
        <EyeOff className="h-3.5 w-3.5" />
        <span className="hidden sm:inline">{t('missions.graph.collapseAll')}</span>
      </Button>
      <Button type="button" variant="outline" size="sm" className="h-8 px-2.5" onClick={onClearSelection}>
        <FilterX className="h-3.5 w-3.5" />
        <span className="hidden md:inline">{t('missions.graph.clearSelection')}</span>
      </Button>
      <Button type="button" variant="outline" size="sm" className="h-8 px-2.5" onClick={onResetLayout}>
        <RotateCcw className="h-3.5 w-3.5" />
        <span className="hidden md:inline">{t('missions.graph.resetLayout')}</span>
      </Button>
      <Button type="button" variant="outline" size="sm" className="h-8 px-2.5" onClick={onFitView}>
        <Maximize2 className="h-3.5 w-3.5" />
        <span className="hidden lg:inline">{t('missions.graph.fitView')}</span>
      </Button>
    </div>
  );
}
