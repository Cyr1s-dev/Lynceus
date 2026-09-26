import * as React from "react";

import {
  Switch as ShadcnSwitch,
} from "@/components/ui/switch";
import { cn } from "@/lib/utils";

/* ───────────────────────── Types ───────────────────────── */

export interface SwitchProps
  extends React.ComponentPropsWithoutRef<typeof ShadcnSwitch> {
  /** Optional label rendered next to the switch. */
  label?: React.ReactNode;
  /** Optional description text rendered below the label. */
  description?: React.ReactNode;
}

/* ───────────────────────── Component ───────────────────────── */

/**
 * Untitled UI Switch — a refined toggle switch wrapping the shadcn/Radix Switch.
 *
 * Re-exports with the same API as shadcn Switch, but allows an optional
 * `label` and `description` for inline form usage. The switch itself
 * uses Radix for full keyboard accessibility.
 *
 * @example
 * ```tsx
 * <Switch label="Enable notifications" description="Receive alerts for new findings." />
 * <Switch checked={enabled} onCheckedChange={setEnabled} />
 * ```
 */
const Switch = React.forwardRef<
  React.ElementRef<typeof ShadcnSwitch>,
  SwitchProps
>(({ className, label, description, ...props }, ref) => {
  if (!label && !description) {
    return <ShadcnSwitch ref={ref} className={cn(className)} {...props} />;
  }

  return (
    <label className="flex cursor-pointer items-start gap-2.5">
      <ShadcnSwitch ref={ref} className={cn("mt-0.5", className)} {...props} />
      <span className="flex flex-col gap-0.5">
        {label && (
          <span className="text-sm font-medium leading-tight text-foreground">
            {label}
          </span>
        )}
        {description && (
          <span className="text-xs text-muted-foreground">{description}</span>
        )}
      </span>
    </label>
  );
});
Switch.displayName = "Switch";

export { Switch };
