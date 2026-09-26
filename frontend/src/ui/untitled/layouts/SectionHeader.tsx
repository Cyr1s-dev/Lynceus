import * as React from 'react';
import { cn } from '@/lib/utils';

/**
 * Untitled UI SectionHeader — a lightweight region title for use inside
 * cards or sections. No card wrapper, just a styled header row.
 */
export interface SectionHeaderProps
  extends Omit<React.HTMLAttributes<HTMLDivElement>, 'title'> {
  title: React.ReactNode;
  description?: React.ReactNode;
  icon?: React.ReactNode;
  /** Right-aligned actions. */
  actions?: React.ReactNode;
}

export function SectionHeader({
  title,
  description,
  icon,
  actions,
  className,
  ...props
}: SectionHeaderProps) {
  return (
    <div
      className={cn('flex items-start justify-between gap-3', className)}
      {...props}
    >
      <div className="flex min-w-0 items-start gap-2.5">
        {icon && <div className="mt-0.5 text-primary">{icon}</div>}
        <div className="min-w-0 space-y-0.5">
          <h3 className="text-sm font-semibold leading-5 text-foreground">{title}</h3>
          {description && (
            <p className="text-xs leading-4 text-muted-foreground">{description}</p>
          )}
        </div>
      </div>
      {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
    </div>
  );
}
