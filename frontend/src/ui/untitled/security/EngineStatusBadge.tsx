import * as React from 'react';
import { Badge, type BadgeProps } from '@/ui/untitled/primitives/Badge';
import { engineStatusTone } from '@/ui/untitled/tokens';

export interface EngineStatusBadgeProps
  extends Omit<BadgeProps, 'tone' | 'children'> {
  status?: string | null;
  label?: React.ReactNode;
  size?: 'sm' | 'md';
  dot?: boolean;
  /** Optional tooltip shown via title attribute. */
  tooltip?: string;
}

export function EngineStatusBadge({
  status,
  label,
  size = 'sm',
  dot = true,
  tooltip,
  className,
  ...props
}: EngineStatusBadgeProps) {
  const tone = engineStatusTone(status);
  return (
    <Badge
      tone={tone}
      variant="soft"
      size={size}
      dot={dot}
      title={tooltip}
      className={className}
      {...props}
    >
      {label ?? status ?? '—'}
    </Badge>
  );
}
