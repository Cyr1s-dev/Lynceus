import { Sheet, SheetContent, SheetHeader, SheetTitle, SheetDescription } from '@/components/ui/sheet';
import { Badge } from '@/components/ui/badge';
import { ScrollArea } from '@/components/ui/scroll-area';
import { formatDistanceToNow } from 'date-fns';
import type { FindingView } from '@/lib/types';
import { ShieldAlert, Clock, Fingerprint, Code, Server, SearchCode, Hash } from 'lucide-react';

import { useTranslation } from 'react-i18next';
import { formatFindingStatus, formatSeverity } from '@/lib/i18n-formatters';

interface FindingDetailDrawerProps {
  finding: FindingView | null;
  isOpen: boolean;
  onClose: () => void;
}

export function FindingDetailDrawer({ finding, isOpen, onClose }: FindingDetailDrawerProps) {
  const { t } = useTranslation();

  if (!finding) return null;

  return (
    <Sheet open={isOpen} onOpenChange={(open) => !open && onClose()}>
      <SheetContent className="w-[400px] sm:w-[540px] border-l border-slate-200 shadow-xl overflow-hidden flex flex-col p-0 bg-white" closeLabel={t('common.close')}>
        <SheetHeader className="p-6 border-b border-slate-100 bg-slate-50/50 shrink-0">
          <div className="flex items-center justify-between mb-2">
            <div className="flex items-center gap-2">
              <Badge variant="outline" className={`${getSeverityColor(finding.severity)} uppercase text-[10px]`}>
                {formatSeverity(t, finding.severity)}
              </Badge>
            </div>
            <Badge variant="secondary" className="uppercase text-[10px] font-semibold tracking-wider">
              {formatFindingStatus(t, finding.status)}
            </Badge>
          </div>
          <SheetTitle className="text-xl font-semibold text-slate-900 flex items-start gap-2">
            <ShieldAlert className="w-5 h-5 text-primary mt-0.5 shrink-0" />
            <span className="leading-tight">{finding.title}</span>
          </SheetTitle>
          <SheetDescription className="text-xs text-slate-500 flex flex-wrap items-center gap-x-4 gap-y-2 mt-3">
            <span className="flex items-center gap-1.5"><Clock className="w-3.5 h-3.5" /> {t('project.updatedRelative', { time: formatDistanceToNow(new Date(finding.updated_at), { addSuffix: true }) })}</span>
            <span className="flex items-center gap-1.5"><SearchCode className="w-3.5 h-3.5" /> {t('common.engine')}: {finding.engine}</span>
            <span className="flex items-center gap-1.5"><Hash className="w-3.5 h-3.5" /> {t('common.confidence')}: {Math.round(finding.confidence * 100)}%</span>
          </SheetDescription>
        </SheetHeader>

        <ScrollArea className="flex-1 p-6">
          <div className="space-y-6">

            {finding.rule_id && (
              <div>
                <h4 className="text-sm font-medium text-slate-900 mb-2 flex items-center gap-2">
                  <Code className="w-4 h-4 text-slate-400" />
                  {t('common.ruleId')}
                </h4>
                <div className="bg-slate-50 border border-slate-100 rounded-md p-2.5 text-xs font-mono text-slate-700 break-all">
                  {finding.rule_id}
                </div>
              </div>
            )}

            {finding.cwe && (
              <div>
                <h4 className="text-sm font-medium text-slate-900 mb-2 flex items-center gap-2">
                  <ShieldAlert className="w-4 h-4 text-slate-400" />
                  {t('common.cwe')}
                </h4>
                <div className="flex flex-wrap gap-2">
                  {(Array.isArray(finding.cwe) ? finding.cwe : [finding.cwe]).map((c, i) => (
                    <Badge key={i} variant="outline" className="bg-slate-50 font-mono text-slate-600 border-slate-200 text-xs">
                      {c}
                    </Badge>
                  ))}
                </div>
              </div>
            )}

            {finding.description && (
              <div>
                <h4 className="text-sm font-medium text-slate-900 mb-2">{t('common.description')}</h4>
                <div className="text-sm text-slate-600 leading-relaxed whitespace-pre-wrap">
                  {finding.description}
                </div>
              </div>
            )}

            {(finding.source_label || finding.sink_label) && (
              <div className="grid grid-cols-2 gap-4">
                {finding.source_label && (
                  <div>
                    <h4 className="text-sm font-medium text-slate-900 mb-2">{t('common.source')}</h4>
                    <div className="bg-slate-50 border border-slate-100 rounded-md p-2 text-xs font-mono text-slate-600 truncate">
                      {finding.source_label}
                    </div>
                  </div>
                )}
                {finding.sink_label && (
                  <div>
                    <h4 className="text-sm font-medium text-slate-900 mb-2">{t('common.sink')}</h4>
                    <div className="bg-slate-50 border border-slate-100 rounded-md p-2 text-xs font-mono text-slate-600 truncate">
                      {finding.sink_label}
                    </div>
                  </div>
                )}
              </div>
            )}

            {finding.fingerprint && (
              <div>
                <h4 className="text-sm font-medium text-slate-900 mb-2 flex items-center gap-2">
                  <Fingerprint className="w-4 h-4 text-slate-400" />
                  {t('common.fingerprint')}
                </h4>
                <div className="bg-slate-50 border border-slate-100 rounded-md p-2 text-[11px] font-mono text-slate-500 break-all">
                  {finding.fingerprint}
                </div>
              </div>
            )}

            {finding.evidence_ids && finding.evidence_ids.length > 0 && (
              <div>
                <h4 className="text-sm font-medium text-slate-900 mb-2 flex items-center gap-2">
                  <Server className="w-4 h-4 text-slate-400" />
                  {t('findings.evidenceChain', { count: finding.evidence_ids.length })}
                </h4>
                <div className="space-y-2">
                  {finding.evidence_ids.map((eid, idx) => (
                    <div key={idx} className="flex items-center gap-2 text-xs text-slate-600 bg-white border border-slate-200 rounded-md p-2">
                      <span className="font-mono truncate">{eid}</span>
                    </div>
                  ))}
                </div>
              </div>
            )}

            {finding.related_fact_ids && finding.related_fact_ids.length > 0 && (
              <div>
                <h4 className="text-sm font-medium text-slate-900 mb-2 flex items-center gap-2">
                  <Hash className="w-4 h-4 text-slate-400" />
                  {t('findings.relatedFacts', { count: finding.related_fact_ids.length })}
                </h4>
                <div className="space-y-2">
                  {finding.related_fact_ids.map((fid, idx) => (
                    <div key={idx} className="flex items-center gap-2 text-xs text-slate-600 bg-white border border-slate-200 rounded-md p-2">
                      <span className="font-mono truncate">{fid}</span>
                    </div>
                  ))}
                </div>
              </div>
            )}

            <div className="pt-4 border-t border-slate-100 grid grid-cols-2 gap-4 text-xs text-slate-500">
              <div>
                <span className="block font-medium text-slate-700 mb-1">{t('findings.findingId')}</span>
                <span className="font-mono truncate block">{finding.id}</span>
              </div>
              {finding.produced_by_task_id && (
                <div>
                  <span className="block font-medium text-slate-700 mb-1">{t('common.taskId')}</span>
                  <span className="font-mono truncate block">{finding.produced_by_task_id}</span>
                </div>
              )}
            </div>

          </div>
        </ScrollArea>
      </SheetContent>
    </Sheet>
  );
}

function getSeverityColor(severity: string) {
  switch (severity.toLowerCase()) {
    case 'critical': return 'border-red-200 text-red-700 bg-red-50 text-red-700';
    case 'high': return 'border-orange-200 text-orange-700 bg-orange-50 text-orange-700';
    case 'medium': return 'border-amber-200 text-amber-700 bg-amber-50 text-amber-700';
    case 'low': return 'border-blue-200 text-blue-700 bg-blue-50 text-blue-700';
    default: return 'border-slate-200 text-slate-700 bg-slate-50 text-slate-700';
  }
}
