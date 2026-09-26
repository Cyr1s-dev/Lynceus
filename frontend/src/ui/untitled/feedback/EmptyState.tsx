import * as React from 'react';
import { cn } from '@/lib/utils';
import { Card } from '@/components/ui/card';
import { Button } from '@/ui/untitled/primitives/Button';

/**
 * Untitled UI EmptyState — a consistent "nothing here yet" panel.
 * Light-first: dashed hairline card, soft icon circle, clear hierarchy.
 */
export interface EmptyStateProps
  extends Omit<React.HTMLAttributes<HTMLDivElement>, 'title'> {
  icon?: React.ReactNode;
  title: React.ReactNode;
  description?: React.ReactNode;
  action?: React.ReactNode;
  secondaryAction?: React.ReactNode;
  variant?: 'card' | 'bare';
  compact?: boolean;
}

export function EmptyState({
  icon,
  title,
  description,
  action,
  secondaryAction,
  variant = 'card',
  compact = false,
  className,
  ...props
}: EmptyStateProps) {
  const body = (
    <div
      className={cn(
        'flex flex-col items-center justify-center text-center',
        compact ? 'py-6' : 'py-12',
      )}
    >
      {icon && (
        <div
          className={cn(
            'flex items-center justify-center rounded-full border border-border bg-muted/60 text-muted-foreground',
            compact ? 'mb-2.5 h-10 w-10' : 'mb-3.5 h-12 w-12',
          )}
        >
          {icon}
        </div>
      )}
      <p
        className={cn(
          'font-semibold tracking-tight text-foreground',
          compact ? 'text-sm' : 'text-base',
        )}
      >
        {title}
      </p>
      {description && (
        <p
          className={cn(
            'mt-1.5 max-w-sm leading-relaxed text-muted-foreground',
            compact ? 'text-xs' : 'text-sm',
          )}
        >
          {description}
        </p>
      )}
      {(action || secondaryAction) && (
        <div className="mt-4 flex flex-wrap items-center justify-center gap-2">
          {action}
          {secondaryAction}
        </div>
      )}
    </div>
  );

  if (variant === 'bare') {
    return (
      <div className={cn(className)} {...props}>
        {body}
      </div>
    );
  }

  return (
    <Card
      className={cn(
        'border-dashed border-border bg-card/60 shadow-none',
        className,
      )}
      {...props}
    >
      {body}
    </Card>
  );
}

/** Convenience CTA button for empty states. */
export function EmptyStateAction({
  children,
  ...props
}: React.ComponentProps<typeof Button>) {
  return (
    <Button size="sm" {...props}>
      {children}
    </Button>
  );
}
