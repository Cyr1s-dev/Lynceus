import { useTranslation } from 'react-i18next';
import { Badge, type StatusTone } from '@/ui/untitled';
import type { DecisionGateKind, DecisionSeverity } from '@/lib/types';
import { cn } from '@/lib/utils';

interface DecisionGateBadgeProps {
  kind?: DecisionGateKind;
  severity?: DecisionSeverity;
  className?: string;
}

const SEVERITY_TONE: Record<DecisionSeverity, StatusTone> = {
  critical: 'danger',
  high: 'danger',
  medium: 'warning',
  low: 'info',
};

export function DecisionGateBadge({ kind, severity, className }: DecisionGateBadgeProps) {
  const { t } = useTranslation();

  if (kind) {
    const tone: StatusTone = kind === 'blocking' ? 'danger' : kind === 'review' ? 'warning' : 'neutral';
    return (
      <Badge tone={tone} className={cn(className)}>
        {t(`decisionGate.kind.${kind}`)}
      </Badge>
    );
  }

  if (severity) {
    return (
      <Badge tone={SEVERITY_TONE[severity]} className={cn(className)}>
        {t(`decisionGate.severity.${severity}`)}
      </Badge>
    );
  }

  return null;
}
