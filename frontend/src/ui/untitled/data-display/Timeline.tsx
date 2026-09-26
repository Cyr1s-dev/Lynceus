import * as React from 'react';
import { cn } from '@/lib/utils';

/**
 * Untitled UI Timeline — a vertical event timeline with status markers.
 * Used for mission activity feeds, audit run events, and branch history.
 */
export interface TimelineEvent {
  id: string;
  title: React.ReactNode;
  description?: React.ReactNode;
  timestamp?: React.ReactNode;
  status?: React.ReactNode;
  icon?: React.ReactNode;
}

export interface TimelineProps extends React.HTMLAttributes<HTMLDivElement> {
  events: TimelineEvent[];
  /** Direction of the timeline. */
  direction?: 'asc' | 'desc';
}

export function Timeline({
  events,
  direction = 'desc',
  className,
  ...props
}: TimelineProps) {
  const ordered = direction === 'desc' ? events : [...events].reverse();

  return (
    <div className={cn('flex flex-col', className)} {...props}>
      {ordered.map((event, index) => {
        const isLast = index === ordered.length - 1;
        return (
          <div key={event.id} className="flex gap-3">
            {/* Rail */}
            <div className="flex flex-col items-center">
              <div className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full border border-border bg-card text-muted-foreground">
                {event.icon ?? <span className="h-2 w-2 rounded-full bg-current" />}
              </div>
              {!isLast && <div className="w-px flex-1 bg-border" />}
            </div>
            {/* Content */}
            <div className={cn('flex-1', isLast ? 'pb-0' : 'pb-4')}>
              <div className="flex items-start justify-between gap-2">
                <div className="min-w-0 space-y-0.5">
                  <p className="text-sm font-medium text-foreground">{event.title}</p>
                  {event.description && (
                    <p className="text-xs leading-5 text-muted-foreground">
                      {event.description}
                    </p>
                  )}
                </div>
                <div className="flex shrink-0 flex-col items-end gap-1">
                  {event.timestamp && (
                    <span className="text-xs text-muted-foreground">
                      {event.timestamp}
                    </span>
                  )}
                  {event.status}
                </div>
              </div>
            </div>
          </div>
        );
      })}
    </div>
  );
}
