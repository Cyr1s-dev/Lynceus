import * as React from 'react';
import { cn } from '@/lib/utils';

/**
 * Untitled UI EntityList — a vertical scrollable list container for
 * entities. Used in split panels and sidebars.
 */
export interface EntityListProps<T> extends React.HTMLAttributes<HTMLDivElement> {
  items: T[];
  renderItem: (item: T, index: number) => React.ReactNode;
  loading?: boolean;
  empty?: React.ReactNode;
  /** Key extractor for list items. */
  getKey?: (item: T, index: number) => string;
}

export function EntityList<T>({
  items,
  renderItem,
  loading = false,
  empty,
  getKey,
  className,
  ...props
}: EntityListProps<T>) {
  if (loading) {
    return (
      <div className={cn('space-y-2 p-2', className)} {...props}>
        {Array.from({ length: 4 }).map((_, i) => (
          <div key={i} className="h-16 animate-pulse rounded-lg bg-muted/50" />
        ))}
      </div>
    );
  }

  if (items.length === 0) {
    return (
      <div className={cn('p-4', className)} {...props}>
        {empty ?? <p className="text-center text-sm text-muted-foreground">No items</p>}
      </div>
    );
  }

  return (
    <div className={cn('flex flex-col gap-1 p-2', className)} {...props}>
      {items.map((item, index) => (
        <React.Fragment key={getKey ? getKey(item, index) : index}>
          {renderItem(item, index)}
        </React.Fragment>
      ))}
    </div>
  );
}
