import * as React from "react";
import { Check } from "lucide-react";

import { cn } from "@/lib/utils";
import { focusRing } from "@/ui/untitled/tokens";

/* ───────────────────────── Types ───────────────────────── */

export interface CheckboxProps
  extends Omit<React.InputHTMLAttributes<HTMLInputElement>, "type" | "size"> {
  /** Optional label rendered next to the checkbox. */
  label?: React.ReactNode;
  /** Optional description text rendered below the label. */
  description?: React.ReactNode;
}

/* ───────────────────────── Visual box ───────────────────────── */

/**
 * The visual checkbox box: a styled container with a check icon that
 * appears when the input is in the `:checked` state. Uses the Tailwind
 * `peer` pattern to toggle visibility based on the sibling input state.
 */
function CheckboxVisual({ className }: { className?: string }) {
  return (
    <span
      className={cn(
        "pointer-events-none absolute left-0 top-0 flex h-full w-full items-center justify-center text-primary-foreground",
        className,
      )}
    >
      <Check className="h-3 w-3 opacity-0 peer-checked:opacity-100 transition-opacity" strokeWidth={3} />
    </span>
  );
}

/* ───────────────────────── Component ───────────────────────── */

/**
 * Untitled UI Checkbox — a styled checkbox with optional label and description.
 *
 * Uses a native `<input type="checkbox">` with `appearance-none` and a visual
 * overlay for full keyboard accessibility and form integration. The check
 * icon appears via the Tailwind `peer` pattern when the input is checked.
 *
 * @example
 * ```tsx
 * <Checkbox label="Notify on completion" description="Send an email when the mission finishes." />
 * ```
 */
const Checkbox = React.forwardRef<HTMLInputElement, CheckboxProps>(
  ({ className, label, description, checked, disabled, ...props }, ref) => {
    const inputId = React.useId();

    const boxClass = cn(
      "peer relative h-4 w-4 shrink-0 cursor-pointer appearance-none rounded border border-input bg-background transition-colors",
      focusRing.inset,
      "checked:border-primary checked:bg-primary",
      "disabled:cursor-not-allowed disabled:opacity-50",
      className,
    );

    if (!label && !description) {
      return (
        <span className="relative inline-flex h-4 w-4">
          <input
            ref={ref}
            type="checkbox"
            id={inputId}
            checked={checked}
            disabled={disabled}
            className={boxClass}
            {...props}
          />
          <CheckboxVisual />
        </span>
      );
    }

    return (
      <label htmlFor={inputId} className="flex cursor-pointer items-start gap-2">
        <span className="relative mt-0.5 inline-flex h-4 w-4">
          <input
            ref={ref}
            type="checkbox"
            id={inputId}
            checked={checked}
            disabled={disabled}
            className={boxClass}
            {...props}
          />
          <CheckboxVisual />
        </span>
        <span className="flex flex-col gap-0.5">
          {label && (
            <span
              className={cn(
                "text-sm font-medium leading-tight text-foreground",
                disabled && "opacity-50",
              )}
            >
              {label}
            </span>
          )}
          {description && (
            <span className="text-xs text-muted-foreground">{description}</span>
          )}
        </span>
      </label>
    );
  },
);
Checkbox.displayName = "Checkbox";

export { Checkbox };
