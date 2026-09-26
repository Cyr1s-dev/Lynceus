import * as React from 'react';
import { cn } from '@/lib/utils';

/**
 * Untitled UI SplitPanel — a two-panel layout with a left list and right
 * detail area. Useful for Mission Detail + Inspector, or list+drawer views.
 */
export interface SplitPanelProps extends React.HTMLAttributes<HTMLDivElement> {
  /** Left panel content (list, tree, etc.) */
  left: React.ReactNode;
  /** Right panel content (detail, inspector, etc.) */
  right: React.ReactNode;
  /** Left panel width in pixels. Default 400. */
  leftWidth?: number;
  /** Show a resize handle (visual only — not functional resize). */
  showDivider?: boolean;
}

export function SplitPanel({
  left,
  right,
  leftWidth = 400,
  showDivider = true,
  className,
  ...props
}: SplitPanelProps) {
  return (
    <div
      className={cn('flex h-full min-h-0 gap-0', className)}
      {...props}
    >
      <div
        className="flex shrink-0 flex-col overflow-hidden"
        style={{ width: leftWidth }}
      >
        {left}
      </div>
      {showDivider && <div className="w-px shrink-0 bg-border" />}
      <div className="flex min-w-0 flex-1 flex-col overflow-hidden">
        {right}
      </div>
    </div>
  );
}
