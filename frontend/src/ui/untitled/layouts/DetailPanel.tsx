import * as React from 'react';
import { X } from 'lucide-react';
import { cn } from '@/lib/utils';
import { Button } from '@/ui/untitled/primitives/Button';

/**
 * Untitled UI DetailPanel — a right-side detail panel for Inspector-style
 * views. Slides in from the right when open. Used by Exploration Canvas
 * Inspector, Mission Detail side panels, etc.
 */
export interface DetailPanelProps {
  open: boolean;
  onClose: () => void;
  title?: React.ReactNode;
  description?: React.ReactNode;
  actions?: React.ReactNode;
  children?: React.ReactNode;
  footer?: React.ReactNode;
  /** Panel width in pixels. Default 420. */
  width?: number;
  className?: string;
}

export function DetailPanel({
  open,
  onClose,
  title,
  description,
  actions,
  children,
  footer,
  width = 420,
  className,
}: DetailPanelProps) {
  if (!open) return null;

  return (
    <div className="absolute inset-0 z-30 flex justify-end">
      {/* Backdrop */}
      <div
        className="absolute inset-0 bg-black/20 transition-opacity"
        onClick={onClose}
      />
      {/* Panel */}
      <div
        className={cn(
          'relative flex h-full flex-col border-l border-border bg-card shadow-elev',
          className,
        )}
        style={{ width }}
      >
        {/* Header */}
        {(title || description || actions) && (
          <div className="flex items-start justify-between gap-3 border-b border-border px-5 py-4">
            <div className="min-w-0 space-y-1">
              {title && (
                <h3 className="text-sm font-semibold leading-5 text-foreground">
                  {title}
                </h3>
              )}
              {description && (
                <p className="text-xs leading-4 text-muted-foreground">
                  {description}
                </p>
              )}
            </div>
            <div className="flex shrink-0 items-center gap-1">
              {actions}
              <Button
                variant="ghost"
                size="icon"
                className="h-7 w-7"
                onClick={onClose}
                aria-label="Close panel"
              >
                <X className="h-4 w-4" />
              </Button>
            </div>
          </div>
        )}
        {/* Body */}
        <div className="flex-1 overflow-y-auto p-5">{children}</div>
        {/* Footer */}
        {footer && (
          <div className="border-t border-border px-5 py-3">{footer}</div>
        )}
      </div>
    </div>
  );
}
