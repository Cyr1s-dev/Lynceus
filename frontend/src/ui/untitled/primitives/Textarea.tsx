import * as React from "react";

import { cn } from "@/lib/utils";
import { focusRing } from "@/ui/untitled/tokens";

/* ───────────────────────── Types ───────────────────────── */

export type TextareaProps = React.TextareaHTMLAttributes<HTMLTextAreaElement>;

/* ───────────────────────── Component ───────────────────────── */

/**
 * Untitled UI Textarea — a refined multi-line text input.
 *
 * Matches the Input styling: h-auto min-height, rounded-md, subtle border,
 * and inset focus ring. Designed for forms and comment fields in the
 * Lynceus security product UI.
 *
 * @example
 * ```tsx
 * <Textarea placeholder="Add a note…" />
 * ```
 */
const Textarea = React.forwardRef<HTMLTextAreaElement, TextareaProps>(
  ({ className, ...props }, ref) => {
    return (
      <textarea
        className={cn(
          "flex min-h-[80px] w-full rounded-md border border-input bg-background px-3 py-2 text-sm",
          "ring-offset-background placeholder:text-muted-foreground",
          focusRing.inset,
          "disabled:cursor-not-allowed disabled:opacity-50",
          className,
        )}
        ref={ref}
        {...props}
      />
    );
  },
);
Textarea.displayName = "Textarea";

export { Textarea };
