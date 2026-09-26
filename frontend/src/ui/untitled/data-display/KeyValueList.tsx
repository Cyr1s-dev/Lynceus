import * as React from 'react';
import { cn } from '@/lib/utils';

/**
 * Untitled UI KeyValueList — a simple key-value display for metadata,
 * used in detail panels, inspectors, and sidebar info sections.
 */
export interface KeyValueItem {
  key: React.ReactNode;
  value: React.ReactNode;
  /** Render the value in monospace font. */
  mono?: boolean;
}

export interface KeyValueListProps
  extends React.HTMLAttributes<HTMLDListElement> {
  items: KeyValueItem[];
  /** Number of columns. Default 2. */
  columns?: 1 | 2 | 3;
}

export function KeyValueList({
  items,
  columns = 2,
  className,
  ...props
}: KeyValueListProps) {
  return (
    <dl
      className={cn(
        'grid gap-x-6 gap-y-3',
        columns === 1 && 'grid-cols-1',
        columns === 2 && 'grid-cols-2',
        columns === 3 && 'grid-cols-3',
        className,
      )}
      {...props}
    >
      {items.map((item, i) => (
        <div key={i} className="space-y-0.5">
          <dt className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground">
            {item.key}
          </dt>
          <dd
            className={cn(
              'text-sm text-foreground',
              item.mono && 'font-mono text-xs',
            )}
          >
            {item.value}
          </dd>
        </div>
      ))}
    </dl>
  );
}
