import * as React from 'react';
import { cn } from '@/lib/utils';

/**
 * Untitled UI CodeBlock — a code/log display block with monospace font
 * and dark background. No syntax highlighting — just clean monospace.
 */
export interface CodeBlockProps extends React.HTMLAttributes<HTMLDivElement> {
  /** The code content. If not a string, it will be rendered as-is. */
  children: React.ReactNode;
  /** Optional language label shown in the top-right corner. */
  language?: string;
  /** Max height with scroll. E.g. '300px', '50vh'. */
  maxHeight?: string;
  /** Show a top bar with the language label. */
  showHeader?: boolean;
}

export function CodeBlock({
  children,
  language,
  maxHeight = '400px',
  showHeader = true,
  className,
  ...props
}: CodeBlockProps) {
  return (
    <div
      className={cn(
        'overflow-hidden rounded-md border border-border bg-slate-950',
        className,
      )}
      {...props}
    >
      {showHeader && (
        <div className="flex items-center justify-between border-b border-slate-800 px-3 py-1.5">
          <span className="text-[11px] font-medium uppercase tracking-wide text-slate-400">
            {language ?? 'output'}
          </span>
        </div>
      )}
      <pre
        className="overflow-auto p-3 text-xs leading-5 text-slate-200"
        style={{ maxHeight }}
      >
        <code className="font-mono">{children}</code>
      </pre>
    </div>
  );
}
