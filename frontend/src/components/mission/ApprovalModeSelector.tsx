import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  AlertTriangle,
  Check,
  ChevronDown,
  MousePointerClick,
  RefreshCw,
} from 'lucide-react';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { cn } from '@/lib/utils';
import { focusRing } from '@/ui/untitled/tokens';
import type { ApprovalMode } from '@/lib/types';

interface ModeMeta {
  icon: typeof MousePointerClick;
  titleKey: string;
  descriptionKey: string;
  /** Icon tint on the compact trigger. */
  triggerIconClass: string;
  /** Icon-box soft background + deep foreground inside the card panel. */
  iconBoxClass: string;
  /** Warning modes render their title/icon in the warning color. */
  warning?: boolean;
}

const MODES: ApprovalMode[] = ['ask_for_approval', 'approve_for_me', 'full_access'];

const MODE_META: Record<ApprovalMode, ModeMeta> = {
  ask_for_approval: {
    icon: MousePointerClick,
    titleKey: 'approvalMode.askForApproval',
    descriptionKey: 'approvalMode.description.ask_for_approval',
    triggerIconClass: 'text-muted-foreground',
    iconBoxClass: 'bg-muted text-foreground',
  },
  approve_for_me: {
    icon: RefreshCw,
    titleKey: 'approvalMode.approveForMe',
    descriptionKey: 'approvalMode.description.approve_for_me',
    triggerIconClass: 'text-success',
    iconBoxClass: 'bg-success-soft text-success-soft-foreground',
  },
  full_access: {
    icon: AlertTriangle,
    titleKey: 'approvalMode.fullAccess',
    descriptionKey: 'approvalMode.description.full_access',
    triggerIconClass: 'text-warning',
    iconBoxClass: 'bg-warning-soft text-warning-soft-foreground',
    warning: true,
  },
};

export interface ApprovalModeSelectorProps {
  value: ApprovalMode;
  onChange: (mode: ApprovalMode) => void;
  disabled?: boolean;
  /** Where the panel opens relative to the trigger. */
  side?: 'top' | 'bottom';
  align?: 'start' | 'center' | 'end';
  /** Extra classes for the compact trigger button. */
  className?: string;
}

/**
 * ApprovalModeSelector — card-style approval-mode picker.
 *
 * Compact trigger shows the active mode; the panel lists the three approval
 * modes as option cards (icon + title + description) with a checkmark on the
 * active one. The full-access card renders in the warning color so the riskier
 * mode stays visually distinct. Pure UI: selection is delegated to `onChange`.
 */
export function ApprovalModeSelector({
  value,
  onChange,
  disabled = false,
  side = 'bottom',
  align = 'start',
  className,
}: ApprovalModeSelectorProps) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const active = MODE_META[value];
  const ActiveIcon = active.icon;

  const select = (mode: ApprovalMode) => {
    setOpen(false);
    if (mode !== value) {
      onChange(mode);
    }
  };

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <button
          type="button"
          disabled={disabled}
          aria-label={t('approvalMode.label')}
          className={cn(
            'inline-flex h-8 items-center gap-1.5 rounded-md bg-transparent px-2.5',
            'text-xs font-medium text-foreground transition-colors',
            'hover:bg-muted/60 disabled:pointer-events-none disabled:opacity-50',
            focusRing.inset,
            className,
          )}
        >
          <ActiveIcon className={cn('h-3.5 w-3.5', active.triggerIconClass)} />
          {t(active.titleKey)}
          <ChevronDown className="h-3 w-3 text-muted-foreground" />
        </button>
      </PopoverTrigger>
      <PopoverContent
        side={side}
        align={align}
        sideOffset={6}
        className={cn(
          'w-[320px] rounded-lg border border-border bg-popover p-2',
          'shadow-elev outline-none',
          'data-[state=open]:animate-in data-[state=closed]:animate-out',
          'data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0',
          'data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95',
        )}
      >
        <div className="px-1.5 pb-2 pt-1">
          <p className="text-xs font-semibold text-foreground">
            {t('approvalMode.panelTitle')}
          </p>
          <p className="mt-0.5 text-xs leading-4 text-muted-foreground">
            {t('approvalMode.panelSubtitle')}
          </p>
        </div>
        <div className="flex flex-col gap-1" role="radiogroup" aria-label={t('approvalMode.panelTitle')}>
          {MODES.map((mode) => {
            const meta = MODE_META[mode];
            const Icon = meta.icon;
            const selected = mode === value;
            return (
              <button
                key={mode}
                type="button"
                role="radio"
                aria-checked={selected}
                disabled={disabled}
                onClick={() => select(mode)}
                className={cn(
                  'flex w-full items-start gap-3 rounded-lg border p-2.5 text-left',
                  'transition-colors disabled:pointer-events-none disabled:opacity-50',
                  selected
                    ? 'border-primary/30 bg-accent/60'
                    : 'border-transparent hover:border-border hover:bg-accent/40',
                  focusRing.inset,
                )}
              >
                <span
                  className={cn(
                    'mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg',
                    '[&_svg]:h-4 [&_svg]:w-4',
                    meta.iconBoxClass,
                  )}
                >
                  <Icon />
                </span>
                <span className="min-w-0 flex-1">
                  <span
                    className={cn(
                      'block text-sm font-medium',
                      meta.warning ? 'text-warning' : 'text-foreground',
                    )}
                  >
                    {t(meta.titleKey)}
                  </span>
                  <span className="mt-0.5 block text-xs leading-4 text-muted-foreground">
                    {t(meta.descriptionKey)}
                  </span>
                </span>
                {selected && (
                  <Check className="mt-1 h-4 w-4 shrink-0 text-primary" aria-hidden />
                )}
              </button>
            );
          })}
        </div>
      </PopoverContent>
    </Popover>
  );
}
