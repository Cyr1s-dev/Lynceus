import * as React from 'react';
import { cn } from '@/lib/utils';
import { Card } from '@/components/ui/card';
import { getStatusToken, type StatusTone } from '@/ui/untitled/tokens';

/**
 * Untitled UI EntityCard — unified card for list surfaces.
 *
 * Light-first refinements:
 *   - Soft icon chip
 *   - Quiet hover lift (no loud primary border)
 *   - Clear title / meta / status hierarchy
 */
export interface EntityCardMetric {
  label: React.ReactNode;
  value: React.ReactNode;
}

export interface EntityCardProps
  extends Omit<React.HTMLAttributes<HTMLDivElement>, 'title'> {
  icon?: React.ReactNode;
  title: React.ReactNode;
  description?: React.ReactNode;
  meta?: React.ReactNode;
  status?: React.ReactNode;
  metrics?: EntityCardMetric[];
  actions?: React.ReactNode;
  /** Render as a clickable surface with hover affordance. */
  interactive?: boolean;
  /** Accent tone for the icon chip. Default `neutral`. */
  tone?: StatusTone;
  /** Extra classes for the description block (e.g. min-h to reserve lines). */
  descriptionClassName?: string;
}

export function EntityCard({
  icon,
  title,
  description,
  descriptionClassName,
  meta,
  status,
  metrics,
  actions,
  interactive = false,
  tone = 'neutral',
  className,
  ...props
}: EntityCardProps) {
  const token = getStatusToken(tone);
  return (
    <Card
      className={cn(
        'transition-all duration-150',
        interactive &&
          'lift cursor-pointer hover:border-border-strong hover:bg-card',
        className,
      )}
      {...props}
    >
      <div className="flex items-start gap-3 p-4">
        {icon && (
          <div
            className={cn(
              'flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border border-border/80',
              token.tint,
              token.dot,
            )}
          >
            {icon}
          </div>
        )}
        <div className="min-w-0 flex-1 space-y-1">
          <div className="flex items-start justify-between gap-2">
            <div className="min-w-0 space-y-0.5">
              <p className="truncate text-sm font-semibold leading-5 tracking-tight text-foreground">
                {title}
              </p>
              {meta && <div className="text-xs text-muted-foreground">{meta}</div>}
            </div>
            {status && <div className="flex shrink-0 items-center gap-2">{status}</div>}
          </div>
          {description && (
            <p
              className={cn(
                'line-clamp-2 text-xs leading-5 text-muted-foreground',
                descriptionClassName,
              )}
            >
              {description}
            </p>
          )}
        </div>
      </div>
      {(metrics || actions) && (
        <div className="flex items-center justify-between gap-3 border-t border-border px-4 py-3">
          {metrics ? (
            <div className="flex flex-wrap items-center gap-x-6 gap-y-1.5">
              {metrics.map((m, i) => (
                <div key={i} className="flex items-baseline gap-1.5">
                  <span className="text-sm font-semibold tabular-nums text-foreground">
                    {m.value}
                  </span>
                  <span className="text-xs text-muted-foreground">{m.label}</span>
                </div>
              ))}
            </div>
          ) : (
            <span />
          )}
          {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
        </div>
      )}
    </Card>
  );
}
