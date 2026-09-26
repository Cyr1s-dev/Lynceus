import * as React from "react";

import { cn } from "@/lib/utils";
import { Button } from "./Button";
import type { ButtonTone } from "./Button.styles";

/* ───────────────────────── Types ───────────────────────── */

export type IconButtonSize = "sm" | "md" | "lg";

export interface IconButtonProps
  extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  /** Square button size. */
  size?: IconButtonSize;
  /** Semantic tone for the icon button. */
  tone?: ButtonTone;
  /** Render as the child element (Radix Slot pattern). */
  asChild?: boolean;
  /** Accessible label — required for icon-only buttons. */
  "aria-label": string;
}

/* ───────────────────────── Size map ───────────────────────── */

const SIZE_MAP: Record<IconButtonSize, string> = {
  sm: "h-8 w-8",
  md: "h-9 w-9",
  lg: "h-10 w-10",
};

/* ───────────────────────── Component ───────────────────────── */

/**
 * Untitled UI IconButton — a square, icon-only button for compact actions.
 *
 * Wraps the Untitled UI `Button` with `size="icon"` semantics. Always
 * requires an `aria-label` for accessibility since it has no visible text.
 *
 * @example
 * ```tsx
 * <IconButton aria-label="Refresh" tone="primary" size="sm">
 *   <RefreshCw className="h-4 w-4" />
 * </IconButton>
 * ```
 */
const IconButton = React.forwardRef<HTMLButtonElement, IconButtonProps>(
  ({ className, size = "md", tone = "default", ...props }, ref) => {
    return (
      <Button
        ref={ref}
        variant={tone === "default" ? "ghost" : "default"}
        tone={tone}
        className={cn(SIZE_MAP[size], className)}
        {...props}
      />
    );
  },
);
IconButton.displayName = "IconButton";

export { IconButton };
