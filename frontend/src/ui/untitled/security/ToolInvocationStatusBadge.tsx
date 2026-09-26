import * as React from 'react';
import { Badge, type BadgeProps } from '@/ui/untitled/primitives/Badge';
import { toolStatusTone } from '@/ui/untitled/tokens';

export interface ToolInvocationStatusBadgeProps
  extends Omit<BadgeProps, 'tone' | 'children'> {
  status?: string | null;
  label?: React.ReactNode;
  size?: 'sm' | 'md';
  dot?: boolean;
}

export function ToolInvocationStatusBadge({
  status,
  label,
  size = 'sm',
  dot = true,
  className,
  ...props
}: ToolInvocationStatusBadgeProps) {
  const tone = toolStatusTone(status);
  return (
    <Badge tone={tone} variant="soft" size={size} dot={dot} className={className} {...props}>
      {label ?? status ?? '—'}
    </Badge>
  );
}
