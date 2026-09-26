import * as React from 'react';
import { cn } from '@/lib/utils';

/**
 * Untitled UI LoadingState — a skeleton loading placeholder.
 * Light-first: soft muted blocks with shimmer on card surfaces.
 */
export interface LoadingStateProps extends React.HTMLAttributes<HTMLDivElement> {
  /** Number of skeleton lines. Default 3. */
  lines?: number;
  /** Show a header block above the lines. */
  showHeader?: boolean;
  /** Show a card wrapper. */
  card?: boolean;
}

export function LoadingState({
  lines = 3,
  showHeader = false,
  card = false,
  className,
  ...props
}: LoadingStateProps) {
  const content = (
    <div className="space-y-3">
      {showHeader && (
        <div className="h-6 w-48 animate-pulse rounded-lg bg-muted" />
      )}
      {Array.from({ length: lines }).map((_, i) => (
        <div
          key={i}
          className="h-4 animate-pulse rounded-md bg-muted"
          style={{ width: `${100 - i * 15}%` }}
        />
      ))}
    </div>
  );

  if (card) {
    return (
      <div
        className={cn(
          'rounded-xl border border-border bg-card p-5 shadow-card',
          className,
        )}
        {...props}
      >
        {content}
      </div>
    );
  }

  return (
    <div className={cn('p-4', className)} {...props}>
      {content}
    </div>
  );
}

/**
 * RowSkeleton — dense list/table loading placeholder.
 * Renders pulse rows matching the standard dense list row height (h-11),
 * so loading surfaces keep the same rhythm as the loaded list.
 */
export interface RowSkeletonProps extends React.HTMLAttributes<HTMLDivElement> {
  /** Number of placeholder rows. Default 5. */
  rows?: number;
  /** Row height utility class. Default matches the dense list row (h-11). */
  rowHeight?: string;
}

export function RowSkeleton({
  rows = 5,
  rowHeight = 'h-11',
  className,
  ...props
}: RowSkeletonProps) {
  return (
    <div className={cn('divide-y divide-border', className)} {...props}>
      {Array.from({ length: rows }).map((_, i) => (
        <div key={i} className={cn('flex items-center gap-3 px-4', rowHeight)}>
          <div className="h-3.5 w-1/4 animate-pulse rounded bg-muted" />
          <div className="h-3.5 w-16 animate-pulse rounded bg-muted" />
          <div className="h-3.5 flex-1 animate-pulse rounded bg-muted" />
          <div className="h-3.5 w-12 animate-pulse rounded bg-muted" />
        </div>
      ))}
    </div>
  );
}
