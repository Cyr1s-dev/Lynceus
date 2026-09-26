import * as React from 'react';
import { cn } from '@/lib/utils';
import { riskScoreTone } from '@/ui/untitled/tokens';

/**
 * RiskScoreIndicator — displays a numeric risk score (0-10) with a coloured
 * background. Used in finding cards and mission overviews.
 */
export interface RiskScoreIndicatorProps
  extends React.HTMLAttributes<HTMLSpanElement> {
  score?: number | null;
  showLabel?: boolean;
  size?: 'sm' | 'md';
}

const TONE_BG: Record<string, string> = {
  danger: 'bg-danger-soft text-danger-soft-foreground border-danger-border',
  warning: 'bg-warning-soft text-warning-soft-foreground border-warning-border',
  info: 'bg-info-soft text-info-soft-foreground border-info-border',
  neutral: 'bg-neutral-soft text-neutral-soft-foreground border-neutral-border',
  success: 'bg-success-soft text-success-soft-foreground border-success-border',
};

export function RiskScoreIndicator({
  score,
  showLabel = false,
  size = 'sm',
  className,
  ...props
}: RiskScoreIndicatorProps) {
  const tone = riskScoreTone(score);
  const bgClass = TONE_BG[tone] ?? TONE_BG.neutral;
  const sizeClass =
    size === 'sm' ? 'h-5 min-w-5 px-1 text-[11px]' : 'h-6 min-w-6 px-1.5 text-xs';

  return (
    <span
      className={cn(
        'inline-flex items-center justify-center rounded-md border font-semibold tabular-nums',
        sizeClass,
        bgClass,
        className,
      )}
      title={showLabel ? undefined : `Risk: ${score ?? 'N/A'}/10`}
      {...props}
    >
      {score != null ? score : '—'}
    </span>
  );
}
