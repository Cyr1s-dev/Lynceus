import * as React from 'react';
import { cn } from '@/lib/utils';
import { Card } from '@/components/ui/card';
import { RowSkeleton } from '@/ui/untitled/feedback/LoadingState';

/**
 * Untitled UI TableShell — the unified table surface.
 * Wraps tabular data in a single clean card with soft borders,
 * provides toolbar slot (filters / actions) and standardized loading / empty states.
 */
export interface TableShellProps extends React.HTMLAttributes<HTMLDivElement> {
  /** Toolbar row rendered above the table (filters left, actions right). */
  toolbar?: React.ReactNode;
  /** Loading state replaces the table body. */
  loading?: boolean;
  /** Render the empty-state panel instead of rows. */
  empty?: React.ReactNode;
  /** Optional className applied to the inner scroll container. */
  bodyClassName?: string;
  /** Make the table full-height of its container (for scrollable panes). */
  fill?: boolean;
}

export function TableShell({
  toolbar,
  loading,
  empty,
  bodyClassName,
  fill = false,
  className,
  children,
  ...props
}: TableShellProps) {
  return (
    <Card
      className={cn(
        'overflow-hidden rounded-xl border border-border/80 bg-card p-0 shadow-card',
        fill && 'flex h-full flex-col',
        className,
      )}
      {...props}
    >
      {toolbar && (
        <div className="flex flex-col gap-3 border-b border-border bg-muted/20 px-4 py-3 md:flex-row md:items-center md:justify-between">
          {toolbar}
        </div>
      )}
      <div className={cn('relative w-full overflow-auto', fill && 'flex-1', bodyClassName)}>
        {loading ? (
          <RowSkeleton rows={6} />
        ) : empty ? (
          <div className="py-6">{empty}</div>
        ) : (
          children
        )}
      </div>
    </Card>
  );
}