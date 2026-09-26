import * as React from 'react';
import { AlertTriangle, RotateCcw } from 'lucide-react';
import { cn } from '@/lib/utils';
import { Button } from '@/ui/untitled/primitives/Button';

/**
 * Untitled UI ErrorState — an error display panel with optional retry.
 * Uses the danger tone from the design token system.
 */
export interface ErrorStateProps
  extends Omit<React.HTMLAttributes<HTMLDivElement>, 'title'> {
  title?: React.ReactNode;
  description?: React.ReactNode;
  /** Retry button — rendered if provided. */
  retry?: React.ReactNode;
  /** Callback when retry is clicked (if retry node is not provided). */
  onRetry?: () => void;
  /** Localized label for the default retry button. */
  retryLabel?: string;
  compact?: boolean;
}

export function ErrorState({
  title = 'Something went wrong',
  description,
  retry,
  onRetry,
  retryLabel = 'Retry',
  compact = false,
  className,
  ...props
}: ErrorStateProps) {
  return (
    <div
      className={cn(
        'flex flex-col items-center justify-center text-center',
        compact ? 'py-6' : 'py-12',
        className,
      )}
      {...props}
    >
      <div
        className={cn(
          'flex items-center justify-center rounded-full bg-danger-soft text-danger',
          compact ? 'mb-2 h-9 w-9' : 'mb-3 h-12 w-12',
        )}
      >
        <AlertTriangle className={compact ? 'h-4 w-4' : 'h-6 w-6'} />
      </div>
      <p
        className={cn(
          'font-medium text-foreground',
          compact ? 'text-sm' : 'text-base',
        )}
      >
        {title}
      </p>
      {description && (
        <p
          className={cn(
            'mt-1 max-w-sm text-muted-foreground',
            compact ? 'text-xs' : 'text-sm',
          )}
        >
          {description}
        </p>
      )}
      {(retry || onRetry) && (
        <div className="mt-4">
          {retry ?? (
            <Button variant="outline" size="sm" onClick={onRetry}>
              <RotateCcw className="h-4 w-4" />
              {retryLabel}
            </Button>
          )}
        </div>
      )}
    </div>
  );
}
