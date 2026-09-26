import * as React from "react";

import { cn } from "@/lib/utils";
import { getStatusToken, type StatusTone } from "@/ui/untitled/tokens";

/* ───────────────────────── Types ───────────────────────── */

export type BadgeVariant = "solid" | "soft" | "outline";
export type BadgeSize = "sm" | "md";

export interface BadgeProps extends React.HTMLAttributes<HTMLSpanElement> {
  /** Semantic status tone used for colour mapping. */
  tone?: StatusTone;
  /** Visual style of the badge. */
  variant?: BadgeVariant;
  /** Badge height / padding. `sm` is the compact pill for tables. */
  size?: BadgeSize;
  /** Show a leading coloured dot. */
  dot?: boolean;
  /** Animate the dot (for live / running states). */
  pulse?: boolean;
}

/* ───────────────────────── Variant classes ───────────────────────── */

function resolveToneClasses(tone: StatusTone, variant: BadgeVariant): string {
  const token = getStatusToken(tone);

  switch (variant) {
    case "solid":
      return cn(token.bar, "text-white border-transparent");
    case "outline":
      return cn(
        "bg-transparent",
        token.dot,
        "border-current/30",
      );
    case "soft":
    default:
      return token.badge;
  }
}

/* ───────────────────────── Component ───────────────────────── */

/**
 * Untitled UI Badge — general-purpose status pill / tag.
 * Soft tint by default for light-first surfaces.
 */
function Badge({
  tone = "neutral",
  variant = "soft",
  size = "sm",
  dot = false,
  pulse = false,
  className,
  children,
  ...props
}: BadgeProps) {
  const toneClasses = resolveToneClasses(tone, variant);
  const token = getStatusToken(tone);

  return (
    <span
      className={cn(
        "inline-flex items-center gap-1.5 whitespace-nowrap rounded-md border font-medium leading-none transition-colors",
        size === "sm" ? "px-2 py-0.5 text-[11px]" : "px-2.5 py-1 text-xs",
        toneClasses,
        className,
      )}
      {...props}
    >
      {dot && (
        <span className="relative flex h-1.5 w-1.5 shrink-0">
          {pulse && (
            <span
              className={cn(
                "absolute inline-flex h-full w-full animate-ping rounded-full opacity-40",
                token.bar,
              )}
            />
          )}
          <span
            className={cn(
              "relative inline-block h-1.5 w-1.5 rounded-full",
              token.bar,
            )}
          />
        </span>
      )}
      {children}
    </span>
  );
}

export { Badge };
