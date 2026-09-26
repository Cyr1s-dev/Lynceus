import * as React from 'react';
import { Link } from '@tanstack/react-router';
import { ChevronRight } from 'lucide-react';
import { cn } from '@/lib/utils';
import { typeScale } from '@/ui/untitled/tokens';

/**
 * Untitled UI PageHeader — the canonical page title block.
 *
 * Light-first: quiet icon chip, tight tracking title, generous description.
 * Title sizes come from the token type scale (pageTitle / pageTitleDense).
 */
export interface PageHeaderBreadcrumb {
  label: React.ReactNode;
  /** Optional route target. When omitted the crumb is plain text. */
  to?: string;
}

export interface PageHeaderProps
  extends Omit<React.HTMLAttributes<HTMLDivElement>, 'title'> {
  title: React.ReactNode;
  description?: React.ReactNode;
  icon?: React.ReactNode;
  /** Right-aligned primary/secondary actions. */
  actions?: React.ReactNode;
  /** Breadcrumb trail rendered above the title. */
  breadcrumbs?: PageHeaderBreadcrumb[];
  /** Dense variant for sub-pages (smaller title). */
  dense?: boolean;
  /** Optional result/entity count rendered next to the title. */
  count?: number;
}

export function PageHeader({
  title,
  description,
  icon,
  actions,
  breadcrumbs,
  dense = false,
  count,
  className,
  ...props
}: PageHeaderProps) {
  return (
    <div
      className={cn(
        'flex flex-col gap-3 md:flex-row md:items-start md:justify-between',
        dense ? 'mb-4' : 'mb-5',
        className,
      )}
      {...props}
    >
      <div className="flex min-w-0 items-start gap-3.5">
        {icon && (
          <div className="mt-0.5 flex h-10 w-10 shrink-0 items-center justify-center rounded-xl border border-border bg-card text-primary shadow-xs">
            {icon}
          </div>
        )}
        <div className="min-w-0 space-y-1.5">
          {breadcrumbs && breadcrumbs.length > 0 && (
            <nav
              aria-label="Breadcrumb"
              className="flex items-center gap-1 text-xs text-muted-foreground"
            >
              {breadcrumbs.map((crumb, idx) => {
                const isLast = idx === breadcrumbs.length - 1;
                return (
                  <span key={idx} className="flex items-center gap-1">
                    {crumb.to && !isLast ? (
                      <Link
                        to={crumb.to}
                        className="transition-colors hover:text-foreground"
                      >
                        {crumb.label}
                      </Link>
                    ) : (
                      <span className={isLast ? 'text-foreground' : undefined}>
                        {crumb.label}
                      </span>
                    )}
                    {!isLast && (
                      <ChevronRight className="h-3 w-3 text-muted-foreground/60" />
                    )}
                  </span>
                );
              })}
            </nav>
          )}
          <h1
            className={cn(
              'text-foreground',
              dense ? typeScale.pageTitleDense : typeScale.pageTitle,
            )}
          >
            {title}
            {typeof count === 'number' && (
              <span className="ml-2 inline-flex items-center rounded-md bg-muted px-1.5 py-0.5 align-middle text-xs font-medium tabular-nums leading-4 text-muted-foreground">
                {count}
              </span>
            )}
          </h1>
          {description && (
            <p className="text-sm leading-relaxed text-muted-foreground md:max-w-3xl">
              {description}
            </p>
          )}
        </div>
      </div>
      {actions && (
        <div className="flex shrink-0 flex-wrap items-center gap-2">{actions}</div>
      )}
    </div>
  );
}
