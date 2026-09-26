import * as React from 'react';
import { cn } from '@/lib/utils';
import { Card } from '@/components/ui/card';

/**
 * Untitled UI Section — a titled content region with a consistent header
 * rail and body rhythm.
 *
 * Light-first: hairline header divider, soft icon, roomier body.
 */
export interface SectionProps
  extends Omit<React.HTMLAttributes<HTMLDivElement>, 'title'> {
  title?: React.ReactNode;
  description?: React.ReactNode;
  icon?: React.ReactNode;
  /** Right-aligned slot inside the header rail (counts, links, actions). */
  actions?: React.ReactNode;
  /** Padding override for the body. Default `p-5`. */
  bodyClassName?: string;
  /** When true, body padding is removed (use for tables / lists / dividers). */
  flush?: boolean;
}

export function Section({
  title,
  description,
  icon,
  actions,
  bodyClassName,
  flush = false,
  className,
  children,
  ...props
}: SectionProps) {
  const hasHeader = title || description || icon || actions;
  return (
    <Card className={cn(className)} {...props}>
      {hasHeader && (
        <div className="flex items-start justify-between gap-3 border-b border-border px-5 py-4">
          <div className="flex min-w-0 items-start gap-2.5">
            {icon && (
              <div className="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-md bg-muted text-primary">
                {icon}
              </div>
            )}
            <div className="min-w-0 space-y-0.5">
              {title && (
                <h3 className="text-sm font-semibold leading-5 tracking-tight text-foreground">
                  {title}
                </h3>
              )}
              {description && (
                <p className="text-xs leading-4 text-muted-foreground">
                  {description}
                </p>
              )}
            </div>
          </div>
          {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
        </div>
      )}
      <div className={cn(flush ? '' : 'p-5', bodyClassName)}>{children}</div>
    </Card>
  );
}
