import * as React from 'react';
import { cn } from '@/lib/utils';
import { getStatusToken, evidenceQualityTone } from '@/ui/untitled/tokens';

/**
 * EvidenceQualityIndicator — a small inline indicator (dot + optional label)
 * showing the quality/confidence level of a piece of evidence.
 */
export interface EvidenceQualityIndicatorProps
  extends React.HTMLAttributes<HTMLSpanElement> {
  quality?: string | null;
  showLabel?: boolean;
  size?: 'sm' | 'md';
}

export function EvidenceQualityIndicator({
  quality,
  showLabel = true,
  size = 'sm',
  className,
  ...props
}: EvidenceQualityIndicatorProps) {
  const tone = evidenceQualityTone(quality);
  const token = getStatusToken(tone);
  const dotSize = size === 'sm' ? 'h-2 w-2' : 'h-2.5 w-2.5';

  return (
    <span
      className={cn('inline-flex items-center gap-1.5', className)}
      {...props}
    >
      <span className={cn('shrink-0 rounded-full', dotSize, token.bar)} />
      {showLabel && (
        <span className={cn('font-medium', size === 'sm' ? 'text-[11px]' : 'text-xs', token.dot)}>
          {quality ?? 'unverified'}
        </span>
      )}
    </span>
  );
}
