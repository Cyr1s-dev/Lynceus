import * as React from "react";

import {
  Tooltip as ShadcnTooltip,
  TooltipContent as ShadcnTooltipContent,
  TooltipProvider,
  TooltipTrigger as ShadcnTooltipTrigger,
} from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

/* ───────────────────────── Types ───────────────────────── */

export interface TooltipProps {
  /** Tooltip body content. */
  content: React.ReactNode;
  /** Preferred side of the trigger to display the tooltip. */
  side?: "top" | "right" | "bottom" | "left";
  /** Alignment of the tooltip relative to the trigger. */
  align?: "start" | "center" | "end";
  /** Delay (ms) before the tooltip appears. */
  delayDuration?: number;
  /** The element that triggers the tooltip on hover. */
  children: React.ReactNode;
}

/* ───────────────────────── Component ───────────────────────── */

/**
 * Untitled UI Tooltip — a styled wrapper around the shadcn/Radix Tooltip.
 *
 * Provides a simplified API: pass `content` and `children`, and the tooltip
 * handles its own provider and trigger setup. Designed for contextual hints
 * on icon buttons, truncated text, and form fields.
 *
 * @example
 * ```tsx
 * <Tooltip content="Refresh data" side="bottom">
 *   <IconButton aria-label="Refresh"><RefreshCw /></IconButton>
 * </Tooltip>
 * ```
 */
function Tooltip({
  content,
  side = "top",
  align = "center",
  delayDuration = 300,
  children,
}: TooltipProps) {
  return (
    <TooltipProvider delayDuration={delayDuration}>
      <ShadcnTooltip>
        <ShadcnTooltipTrigger asChild>
          {children}
        </ShadcnTooltipTrigger>
        <ShadcnTooltipContent
          side={side}
          align={align}
          sideOffset={6}
          className={cn(
            "max-w-[240px] rounded-md border border-border bg-popover px-2.5 py-1.5",
            "text-xs font-medium text-popover-foreground shadow-card",
          )}
        >
          {content}
        </ShadcnTooltipContent>
      </ShadcnTooltip>
    </TooltipProvider>
  );
}

export { Tooltip };
