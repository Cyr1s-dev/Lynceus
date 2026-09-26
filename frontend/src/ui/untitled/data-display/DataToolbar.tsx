import * as React from 'react';
import { Search } from 'lucide-react';
import { cn } from '@/lib/utils';
import { Input } from '@/ui/untitled/primitives/Input';

/**
 * Untitled UI DataToolbar — a unified filter/search/toolbar bar for list
 * surfaces. Provides a consistent layout for search input, filters, and
 * right-aligned action buttons.
 */
export interface DataToolbarProps extends React.HTMLAttributes<HTMLDivElement> {
  search?: string;
  onSearchChange?: (value: string) => void;
  searchPlaceholder?: string;
  /** Left-aligned filter controls (selects, tabs, etc.) */
  filters?: React.ReactNode;
  /** Right-aligned action buttons */
  actions?: React.ReactNode;
  /** Result count displayed on the right */
  count?: number;
  countLabel?: React.ReactNode;
  /** When provided with selectionCount > 0, renders a sticky selection-action rail below the bar. */
  selectionBar?: React.ReactNode;
  selectionCount?: number;
}

export function DataToolbar({
  search,
  onSearchChange,
  searchPlaceholder = 'Search…',
  filters,
  actions,
  count,
  countLabel,
  selectionBar,
  selectionCount = 0,
  className,
  ...props
}: DataToolbarProps) {
  return (
    <div className={cn('flex flex-col gap-3', className)} {...props}>
      <div className="flex flex-wrap items-center gap-3 border-b border-border pb-3">
        {onSearchChange && (
          <Input
            value={search ?? ''}
            onChange={(e) => onSearchChange(e.target.value)}
            placeholder={searchPlaceholder}
            leftIcon={<Search className="h-4 w-4 text-muted-foreground" />}
            className="h-8 w-64 max-w-full"
          />
        )}
        {filters && <div className="flex flex-wrap items-center gap-2">{filters}</div>}
        <div className="ml-auto flex items-center gap-2">
          {count != null && (
            <span className="text-xs text-muted-foreground">
              {countLabel ?? `${count} ${count === 1 ? 'item' : 'items'}`}
            </span>
          )}
          {actions}
        </div>
      </div>
      {selectionCount > 0 && selectionBar && (
        <div className="flex flex-wrap items-center gap-3 rounded-lg border border-border bg-muted/40 px-4 py-2.5 text-sm">
          {selectionBar}
        </div>
      )}
    </div>
  );
}
