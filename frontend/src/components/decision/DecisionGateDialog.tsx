import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import {
  Dialog,
  DialogContent,
} from '@/components/ui/dialog';
import { api } from '@/lib/api';
import { DecisionGatePanel } from './DecisionGatePanel';
import type { DecisionAnswer } from '@/lib/types';
import { toast } from '@/hooks/use-toast';

interface DecisionGateDialogProps {
  runId: string;
  projectId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function DecisionGateDialog({ runId, projectId, open, onOpenChange }: DecisionGateDialogProps) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();

  const { data: gates = [], isLoading } = useQuery({
    queryKey: ['decision-gates', projectId, runId],
    queryFn: () => api.getRunDecisionGates(projectId, runId, { status: 'pending' }),
    enabled: open,
  });

  const gate = gates.find(g => g.status === 'pending');

  const answerMutation = useMutation({
    mutationFn: async (answer: DecisionAnswer) => {
      if (!gate) throw new Error('No gate');
      await api.answerDecisionGate(gate.id, answer);
      if (gate.kind === 'blocking') {
        await api.resumeAuditRun(projectId, runId);
      }
    },
    onSuccess: () => {
      toast({ title: t('decisionGate.answeredSuccessfully') });
      queryClient.invalidateQueries({ queryKey: ['decision-gates'] });
      queryClient.invalidateQueries({ queryKey: ['audit-runs'] });
      queryClient.invalidateQueries({ queryKey: ['dashboard'] });
      onOpenChange(false);
    },
    onError: () => {
      toast({ title: t('decisionGate.errorSubmitting'), variant: 'destructive' });
    }
  });

  const cancelMutation = useMutation({
    mutationFn: async () => {
      if (!gate) throw new Error('No gate');
      await api.cancelDecisionGate(gate.id);
    },
    onSuccess: () => {
      toast({ title: t('decisionGate.cancelledSuccessfully') });
      queryClient.invalidateQueries({ queryKey: ['decision-gates'] });
      onOpenChange(false);
    },
    onError: () => {
      toast({ title: t('decisionGate.errorCancelling'), variant: 'destructive' });
    }
  });

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-3xl overflow-hidden p-0">
        {isLoading ? (
          <div className="rounded-lg bg-card p-8 text-center text-sm text-muted-foreground">
            {t('common.loading')}
          </div>
        ) : gate ? (
          <div className="bg-card p-6">
            <DecisionGatePanel
              gate={gate}
              onSubmit={(answer) => answerMutation.mutateAsync(answer)}
              onCancel={async () => { await cancelMutation.mutateAsync(); }}
              isSubmitting={answerMutation.isPending || cancelMutation.isPending}
            />
          </div>
        ) : (
          <div className="rounded-lg bg-card p-8 text-center text-sm text-muted-foreground">
            {t('decisionGate.noPendingDecisions')}
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
