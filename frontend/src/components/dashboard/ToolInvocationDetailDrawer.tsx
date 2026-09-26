import { Sheet, SheetContent, SheetHeader, SheetTitle, SheetDescription } from '@/components/ui/sheet';
import { Badge, toolStatusTone } from '@/ui/untitled';
import { ScrollArea } from '@/components/ui/scroll-area';
import type { ApiToolInvocation } from '@/lib/types';
import { TerminalSquare, Clock, FileJson, Hash, Calendar } from 'lucide-react';
import { cn } from '@/lib/utils';
import { useTranslation } from 'react-i18next';
import { formatStatus } from '@/lib/i18n-formatters';

interface ToolInvocationDetailDrawerProps {
  invocation: ApiToolInvocation | null;
  isOpen: boolean;
  onClose: () => void;
}

export function ToolInvocationDetailDrawer({ invocation, isOpen, onClose }: ToolInvocationDetailDrawerProps) {
  const { t } = useTranslation();
  if (!invocation) return null;

  const renderField = (label: string, value?: string | number | null, isMono: boolean = false) => (
    <div className="space-y-1">
      <span className="label-spec flex items-center gap-1">
        {label}
      </span>
      <span className={cn(
        "block truncate rounded border border-border bg-muted/40 p-1.5 text-xs",
        isMono ? "font-mono text-muted-foreground" : "font-medium text-foreground"
      )}>
        {value !== undefined && value !== null ? String(value) : '-'}
      </span>
    </div>
  );

  return (
    <Sheet open={isOpen} onOpenChange={(open) => !open && onClose()}>
      <SheetContent className="flex w-[400px] flex-col overflow-hidden border-l border-border bg-card p-0 shadow-xl sm:w-[540px]" closeLabel={t('common.close')}>
        {/* Header: tool badge + status + title */}
        <SheetHeader className="shrink-0 border-b border-border bg-muted/30 p-6">
          <div className="mb-2 flex items-center justify-between">
            <Badge tone="neutral" variant="outline" className="font-mono">
              {invocation.tool_name}
            </Badge>
            <Badge tone={toolStatusTone(invocation.status)} dot>
              {formatStatus(t, invocation.status)}
            </Badge>
          </div>
          <SheetTitle className="flex items-center gap-2 text-lg font-semibold leading-7 tracking-tight text-foreground">
            <TerminalSquare className="h-5 w-5 text-primary" />
            {t('toolInvocations.detailsTitle')}
          </SheetTitle>
          {/* Metadata strip */}
          <SheetDescription className="mt-3 flex flex-wrap items-center gap-x-4 gap-y-2 text-xs text-muted-foreground">
             <span className="flex items-center gap-1.5">
               <Hash className="h-3.5 w-3.5" /> {t('common.id')}: <span className="font-mono">{invocation.id}</span>
             </span>
             {invocation.exit_code !== undefined && (
               <span className="flex items-center gap-1.5 font-medium">
                 {t('common.exitCode')}: <span className={invocation.exit_code === 0 ? "text-success" : "text-danger"}>{invocation.exit_code}</span>
               </span>
             )}
             {invocation.module_id && (
               <span className="flex items-center gap-1.5">
                 {t('common.moduleId')}: <span className="font-mono">{invocation.module_id}</span>
               </span>
             )}
          </SheetDescription>
        </SheetHeader>

        {/* Primary content */}
        <ScrollArea className="flex-1 p-6">
          <div className="space-y-6">
            <div className="grid grid-cols-3 gap-3">
              {renderField(t('toolInvocations.projectId'), invocation.project_id, true)}
              {renderField(t('toolInvocations.runId'), invocation.run_id, true)}
              {renderField(t('toolInvocations.taskId'), invocation.task_id, true)}
            </div>

            <div className="grid grid-cols-2 gap-4 border-y border-border py-4">
               <div className="space-y-1">
                 <span className="label-spec flex items-center gap-1">
                   <Calendar className="h-3 w-3" /> {t('common.startedAt')}
                 </span>
                 <span className="text-xs font-medium text-muted-foreground">
                   {new Date(invocation.started_at).toLocaleString()}
                 </span>
               </div>
               <div className="space-y-1">
                 <span className="label-spec flex items-center gap-1">
                   <Clock className="h-3 w-3" /> {t('common.finishedAt')}
                 </span>
                 <span className="text-xs font-medium text-muted-foreground">
                   {invocation.finished_at ? new Date(invocation.finished_at).toLocaleString() : '-'}
                 </span>
               </div>
               <div className="col-span-2 mt-1 flex items-center gap-2">
                  <span className="label-spec">{t('common.duration')}:</span>
                  <span className="font-mono text-xs text-muted-foreground">{invocation.duration_ms ? `${invocation.duration_ms}ms` : '-'}</span>
               </div>
            </div>

            <div>
              <h4 className="label-spec mb-2">{t('common.inputSummary')}</h4>
              <div className="whitespace-pre-wrap break-words rounded-md border border-border bg-muted/40 p-3 font-mono text-xs leading-5 text-foreground">
                {invocation.input_summary || t('common.noInputProvided')}
              </div>
            </div>

            <div>
              <h4 className="label-spec mb-2">{t('common.outputSummary')}</h4>
              <div className="whitespace-pre-wrap break-words rounded-md border border-border bg-muted/40 p-3 font-mono text-xs leading-5 text-foreground">
                {invocation.output_summary || t('common.noOutputRecorded')}
              </div>
            </div>

            {invocation.error && (
              <div>
                <h4 className="label-spec mb-2 text-danger">{t('common.errorDetails')}</h4>
                <div className="whitespace-pre-wrap break-words rounded-md border border-danger-border bg-danger-soft p-3 font-mono text-xs leading-5 text-danger-soft-foreground">
                  {invocation.error}
                </div>
              </div>
            )}

            {invocation.artifact_paths && invocation.artifact_paths.length > 0 && (
              <div>
                <h4 className="label-spec mb-2">{t('common.artifacts')}</h4>
                <div className="space-y-2">
                  {invocation.artifact_paths.map((path, idx) => (
                    <div key={idx} className="flex items-center gap-2 rounded-md border border-border bg-card p-2 text-muted-foreground">
                      <FileJson className="h-4 w-4 text-muted-foreground/60" />
                      <span className="truncate font-mono text-xs">{path}</span>
                    </div>
                  ))}
                </div>
              </div>
            )}
          </div>
        </ScrollArea>
      </SheetContent>
    </Sheet>
  );
}
