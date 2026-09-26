import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Textarea } from '@/components/ui/textarea';
import { DecisionGateBadge } from './DecisionGateBadge';
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group';
import { Badge, Label } from '@/ui/untitled';
import type { DecisionGate, DecisionAnswer } from '@/lib/types';
import { CheckCircle2 } from 'lucide-react';
import { cn } from '@/lib/utils';

interface DecisionGatePanelProps {
  gate: DecisionGate;
  onSubmit: (answer: DecisionAnswer) => Promise<void>;
  onCancel?: () => Promise<void>;
  isSubmitting?: boolean;
}

export function DecisionGatePanel({ gate, onSubmit, onCancel, isSubmitting }: DecisionGatePanelProps) {
  const { t } = useTranslation();
  const actionPreview = asRecord(gate.metadata?.action_preview);

  const [selectedOptionId, setSelectedOptionId] = useState<string>(
    gate.recommended_option_id || gate.options[0]?.id || '',
  );
  const [rationale, setRationale] = useState('');

  const handleSubmit = async () => {
    if (!selectedOptionId) return;
    await onSubmit({
      option_id: selectedOptionId,
      rationale: rationale.trim() || undefined,
    });
  };

  const isResolved = gate.status !== 'pending';

  return (
    <div className="section-stack">
      <div className="border-b border-border pb-4">
        <h2 className="mb-2 text-xl font-semibold text-foreground">{gate.question}</h2>
        <div className="flex items-center gap-2">
          <DecisionGateBadge kind={gate.kind} />
          <DecisionGateBadge severity={gate.severity} />
          <span className="ml-2 text-xs text-muted-foreground">ID: {gate.id}</span>
        </div>
      </div>

      {actionPreview ? (
        <div className="space-y-3 rounded-md border border-warning-border bg-warning-soft p-4">
          <div>
            <p className="text-sm font-semibold text-foreground">
              {t('decisionGate.actionPreview.title')}
            </p>
            <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
              {t('decisionGate.actionPreview.description')}
            </p>
          </div>
          <dl className="grid gap-3 text-sm sm:grid-cols-2">
            <PreviewField label={t('decisionGate.actionPreview.tool')} value={actionPreview.tool_name} />
            <PreviewField label={t('decisionGate.actionPreview.actionClass')} value={actionPreview.action_class} />
            <PreviewField label={t('decisionGate.actionPreview.target')} value={actionPreview.target} />
            <PreviewField label={t('decisionGate.actionPreview.port')} value={actionPreview.port} />
            <PreviewField label={t('decisionGate.actionPreview.policy')} value={actionPreview.execution_policy} />
            <PreviewField
              label={t('decisionGate.actionPreview.autoRetry')}
              value={actionPreview.safe_to_retry === true ? t('common.yes') : t('common.no')}
            />
          </dl>
          <div>
            <p className="mb-1 text-xs font-medium text-foreground">
              {t('decisionGate.actionPreview.arguments')}
            </p>
            <pre className="max-h-48 overflow-auto whitespace-pre-wrap break-all rounded-md border border-border bg-background p-3 text-xs text-foreground">
              {JSON.stringify(actionPreview.arguments ?? {}, null, 2)}
            </pre>
          </div>
        </div>
      ) : gate.context_summary && (
        <div className="rounded-md border border-border bg-muted/40 p-4 text-sm leading-relaxed text-foreground">
          {gate.context_summary}
        </div>
      )}

      {isResolved && (
        <div className="flex items-center gap-2 rounded-md border border-success-border bg-success-soft p-3 text-sm text-success-foreground">
          <CheckCircle2 className="h-4 w-4" />
          <span className="font-medium">{t('decisionGate.alreadyResolved')}</span>
          <span className="ml-auto text-xs opacity-80">{gate.status}</span>
        </div>
      )}

      <div className="space-y-4">
        <h3 className="font-medium text-foreground">{t('common.actions')}</h3>
        <RadioGroup
          value={selectedOptionId}
          onValueChange={setSelectedOptionId}
          disabled={isResolved || isSubmitting}
          className="flex flex-col gap-3"
        >
          {gate.options.map((option) => {
            const isRecommended = option.id === gate.recommended_option_id;
            return (
              <div
                key={option.id}
                className={cn(
                  'flex items-start space-x-3 rounded-md border p-4 transition-colors',
                  selectedOptionId === option.id
                    ? 'border-primary/40 bg-primary/5'
                    : 'border-border bg-card',
                  isResolved && 'opacity-70',
                )}
              >
                <RadioGroupItem value={option.id} id={`option-${option.id}`} className="mt-1" />
                <Label htmlFor={`option-${option.id}`} className="flex-1 cursor-pointer">
                  <div className="mb-1 flex items-center gap-2">
                    <span className="font-medium text-foreground">{option.label}</span>
                    {isRecommended && (
                      <Badge tone="info">{t('decisionGate.recommended')}</Badge>
                    )}
                  </div>
                  {option.description && (
                    <p className="mb-2 text-sm leading-relaxed text-muted-foreground">
                      {option.description}
                    </p>
                  )}
                  {(option.impact || option.risk) && (
                    <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-xs">
                      {option.impact && (
                        <div className="flex gap-1">
                          <span className="font-medium text-foreground">{t('decisionGate.impact')}:</span>
                          <span className="text-muted-foreground">{option.impact}</span>
                        </div>
                      )}
                      {option.risk && (
                        <div className="flex gap-1">
                          <span className="font-medium text-foreground">{t('decisionGate.risk')}:</span>
                          <span className="text-muted-foreground">{option.risk}</span>
                        </div>
                      )}
                    </div>
                  )}
                </Label>
              </div>
            );
          })}
        </RadioGroup>
      </div>

      {!isResolved && (
        <div className="space-y-3 border-t border-border pt-4">
          <Label htmlFor="rationale" className="text-foreground">
            {t('decisionGate.rationalePlaceholder')}
          </Label>
          <Textarea
            id="rationale"
            placeholder={t('decisionGate.rationalePlaceholder')}
            value={rationale}
            onChange={(e) => setRationale(e.target.value)}
            disabled={isSubmitting}
            className="min-h-[80px]"
          />

          <div className="flex justify-end gap-3 pt-2">
            {onCancel && (
              <Button variant="outline" onClick={onCancel} disabled={isSubmitting}>
                {t('decisionGate.cancelDecision')}
              </Button>
            )}
            <Button onClick={handleSubmit} disabled={!selectedOptionId || isSubmitting}>
              {isSubmitting ? t('common.saving') : t('decisionGate.submitAnswer')}
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function PreviewField({ label, value }: { label: string; value: unknown }) {
  return (
    <div className="min-w-0">
      <dt className="text-xs font-medium text-muted-foreground">{label}</dt>
      <dd className="mt-0.5 break-all font-mono text-xs text-foreground">
        {value === null || value === undefined || value === '' ? '-' : String(value)}
      </dd>
    </div>
  );
}
