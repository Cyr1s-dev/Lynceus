import * as React from 'react';
import { Badge, type BadgeProps } from '@/ui/untitled/primitives/Badge';
import { missionStatusTone } from '@/ui/untitled/tokens';

/**
 * MissionStatusBadge — displays a mission lifecycle status with the
 * correct semantic tone. Running states get a live pulse on the dot.
 */
export interface MissionStatusBadgeProps
  extends Omit<BadgeProps, 'tone' | 'children' | 'pulse'> {
  status?: string | null;
  /** Override the auto-resolved label. */
  label?: React.ReactNode;
  size?: 'sm' | 'md';
  dot?: boolean;
}

const LIVE_STATUSES = new Set(['running', 'active', 'reviewing', 'reporting']);

export function MissionStatusBadge({
  status,
  label,
  size = 'sm',
  dot = true,
  className,
  ...props
}: MissionStatusBadgeProps) {
  const tone = missionStatusTone(status);
  const isLive = LIVE_STATUSES.has((status || '').toLowerCase());
  return (
    <Badge
      tone={tone}
      variant="soft"
      size={size}
      dot={dot}
      pulse={isLive}
      className={className}
      {...props}
    >
      {label ?? status ?? '—'}
    </Badge>
  );
}
