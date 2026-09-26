import * as React from 'react';
import { Badge, type BadgeProps } from '@/ui/untitled/primitives/Badge';
import { decisionGateTone } from '@/ui/untitled/tokens';

export interface DecisionGateStatusBadgeProps
  extends Omit<BadgeProps, 'tone' | 'children'> {
  status?: string | null;
  label?: React.ReactNode;
  size?: 'sm' | 'md';
  dot?: boolean;
}

export function DecisionGateStatusBadge({
  status,
  label,
  size = 'sm',
  dot = true,
  className,
  ...props
}: DecisionGateStatusBadgeProps) {
  const tone = decisionGateTone(status);
  return (
    <Badge tone={tone} variant="soft" size={size} dot={dot} className={className} {...props}>
      {label ?? status ?? '—'}
    </Badge>
  );
}
