import * as React from 'react';
import { Badge, type BadgeProps } from '@/ui/untitled/primitives/Badge';
import { branchStatusTone } from '@/ui/untitled/tokens';

export interface BranchStatusBadgeProps
  extends Omit<BadgeProps, 'tone' | 'children'> {
  status?: string | null;
  label?: React.ReactNode;
  size?: 'sm' | 'md';
  dot?: boolean;
}

export function BranchStatusBadge({
  status,
  label,
  size = 'sm',
  dot = true,
  className,
  ...props
}: BranchStatusBadgeProps) {
  const tone = branchStatusTone(status);
  return (
    <Badge tone={tone} variant="soft" size={size} dot={dot} className={className} {...props}>
      {label ?? status ?? '—'}
    </Badge>
  );
}
