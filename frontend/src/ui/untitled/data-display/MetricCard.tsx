import * as React from 'react';
import { cn } from '@/lib/utils';
import { Card } from '@/components/ui/card';
import { getStatusToken, type StatusTone } from '@/ui/untitled/tokens';

/**
 * Untitled UI MetricCard — a compact KPI tile.
 *
 * Light-first refinements:
 *   - Soft icon chip with tone tint
 *   - Large tabular value (text-3xl)
 *   - Quiet lift on hover
 *   - Optional trend + hint
 */
export interface MetricCardTrend {
  value: React.ReactNode;
  tone?: StatusTone;
}

export interface MetricCardProps extends React.HTMLAttributes<HTMLDivElement> {
  label: React.ReactNode;
  value: React.ReactNode;
  hint?: React.ReactNode;
  icon?: React.ReactNode;
  tone?: StatusTone;
  trend?: MetricCardTrend;
}

function trendToneOf(trend: MetricCardTrend): StatusTone {
  if (trend.tone) return trend.tone;
  const raw = typeof trend.value === 'string' ? trend.value.trim() : '';
  if (raw.startsWith('+')) return 'success';
  if (raw.startsWith('-')) return 'danger';
  return 'neutral';
}

export function MetricCard({
  label,
  value,
  hint,
  icon,
  tone = 'neutral',
  trend,
  className,
  ...props
}: MetricCardProps) {
  const token = getStatusToken(tone);
  return (
    <Card
      className={cn(
        'lift px-5 py-4',
        className,
      )}
      {...props}
    >
      <div className="flex items-start justify-between gap-3">
        <span className="text-xs font-medium text-muted-foreground">{label}</span>
        {icon && (
          <span
            className={cn(
              'flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border',
              token.tint,
              token.dot,
              'border-border/80',
            )}
          >
            {icon}
          </span>
        )}
      </div>
      <div className="mt-3 flex items-baseline gap-2">
        <span className="text-3xl font-semibold tabular-nums leading-none tracking-tight text-foreground">
          {value}
        </span>
        {trend && (
          <span
            className={cn(
              'text-xs font-medium tabular-nums',
              getStatusToken(trendToneOf(trend)).dot,
            )}
          >
            {trend.value}
          </span>
        )}
      </div>
      {hint && (
        <p className="mt-2 text-xs text-muted-foreground">{hint}</p>
      )}
    </Card>
  );
}
