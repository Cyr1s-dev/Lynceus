import * as React from 'react';
import { cn } from '@/lib/utils';

/**
 * Untitled UI FilterTabs — a status/category tab strip with counts and
 * right-aligned actions. Used on list pages for status filtering.
 */
export interface FilterTabItem {
  key: string;
  label: React.ReactNode;
  count?: number;
}

export interface FilterTabsProps
  extends Omit<React.HTMLAttributes<HTMLDivElement>, 'onChange'> {
  tabs: FilterTabItem[];
  value: string;
  onChange: (key: string) => void;
  /** Right-aligned actions */
  actions?: React.ReactNode;
}

export function FilterTabs({
  tabs,
  value,
  onChange,
  actions,
  className,
  ...props
}: FilterTabsProps) {
  return (
    <div
      className={cn(
        'flex items-center justify-between gap-3 border-b border-border',
        className,
      )}
      {...props}
    >
      <div className="flex items-center gap-1 overflow-x-auto">
        {tabs.map((tab) => {
          const isActive = tab.key === value;
          return (
            <button
              key={tab.key}
              type="button"
              onClick={() => onChange(tab.key)}
              className={cn(
                'flex items-center gap-1.5 whitespace-nowrap border-b-2 px-3 py-2.5 text-sm font-medium transition-colors',
                isActive
                  ? 'border-primary text-primary'
                  : 'border-transparent text-muted-foreground hover:text-foreground',
              )}
            >
              {tab.label}
              {tab.count != null && (
                <span
                  className={cn(
                    'rounded-full px-1.5 py-0.5 text-[10px] font-semibold tabular-nums',
                    isActive
                      ? 'bg-primary/10 text-primary'
                      : 'bg-muted text-muted-foreground',
                  )}
                >
                  {tab.count}
                </span>
              )}
            </button>
          );
        })}
      </div>
      {actions && <div className="flex shrink-0 items-center gap-2 pb-2">{actions}</div>}
    </div>
  );
}
