import * as React from 'react';
import { Badge, type BadgeProps } from '@/ui/untitled/primitives/Badge';
import { providerReadinessTone } from '@/ui/untitled/tokens';

export interface ProviderReadinessBadgeProps
  extends Omit<BadgeProps, 'tone' | 'children'> {
  usable: boolean;
  configComplete: boolean;
  tested?: boolean | null;
  label?: React.ReactNode;
  size?: 'sm' | 'md';
  dot?: boolean;
}

export function ProviderReadinessBadge({
  usable,
  configComplete,
  tested = null,
  label,
  size = 'sm',
  dot = true,
  className,
  ...props
}: ProviderReadinessBadgeProps) {
  const tone = providerReadinessTone(usable, configComplete, tested);
  const autoLabel = usable
    ? 'Ready'
    : configComplete
      ? tested === false
        ? 'Test failed'
        : 'Configured'
      : 'Not configured';
  return (
    <Badge tone={tone} variant="soft" size={size} dot={dot} className={className} {...props}>
      {label ?? autoLabel}
    </Badge>
  );
}
