import * as React from 'react';
import { Badge, type BadgeProps } from '@/ui/untitled/primitives/Badge';
import { severityTone } from '@/ui/untitled/tokens';

export interface SeverityBadgeProps
  extends Omit<BadgeProps, 'tone' | 'children'> {
  severity?: string | null;
  label?: React.ReactNode;
  size?: 'sm' | 'md';
  dot?: boolean;
}

export function SeverityBadge({
  severity,
  label,
  size = 'sm',
  dot = true,
  className,
  ...props
}: SeverityBadgeProps) {
  const tone = severityTone(severity);
  return (
    <Badge tone={tone} variant="soft" size={size} dot={dot} className={className} {...props}>
      {label ?? severity ?? '—'}
    </Badge>
  );
}
