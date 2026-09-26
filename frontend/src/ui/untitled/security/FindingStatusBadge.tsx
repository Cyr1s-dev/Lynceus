import * as React from 'react';
import { Badge, type BadgeProps } from '@/ui/untitled/primitives/Badge';
import { findingStatusTone } from '@/ui/untitled/tokens';

export interface FindingStatusBadgeProps
  extends Omit<BadgeProps, 'tone' | 'children'> {
  status?: string | null;
  label?: React.ReactNode;
  size?: 'sm' | 'md';
  dot?: boolean;
}

export function FindingStatusBadge({
  status,
  label,
  size = 'sm',
  dot = true,
  className,
  ...props
}: FindingStatusBadgeProps) {
  const tone = findingStatusTone(status);
  return (
    <Badge tone={tone} variant="soft" size={size} dot={dot} className={className} {...props}>
      {label ?? status ?? '—'}
    </Badge>
  );
}
