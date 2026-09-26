import * as React from "react";

import { cn } from "@/lib/utils";

/* ───────────────────────── Types ───────────────────────── */

export interface InputProps
  extends Omit<React.InputHTMLAttributes<HTMLInputElement>, "size"> {
  /** Optional icon or element rendered to the left of the input text. */
  leftIcon?: React.ReactNode;
  /** Optional icon or element rendered to the right of the input text. */
  rightIcon?: React.ReactNode;
}

/* ───────────────────────── Component ───────────────────────── */

/**
 * Untitled UI Input — refined text input with optional leading/trailing icons.
 * Light-first: soft border, quiet focus ring, rounded-lg.
 */
const Input = React.forwardRef<HTMLInputElement, InputProps>(
  ({ className, leftIcon, rightIcon, type = "text", ...props }, ref) => {
    const baseClass = cn(
      "flex h-9 w-full rounded-lg border border-input bg-card px-3 py-2 text-sm",
      "ring-offset-background placeholder:text-muted-foreground",
      "transition-colors duration-150",
      "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/15 focus-visible:border-primary/40",
      "disabled:cursor-not-allowed disabled:opacity-50",
      "file:border-0 file:bg-transparent file:text-sm file:font-medium file:text-foreground",
      leftIcon && "pl-9",
      rightIcon && "pr-9",
      className,
    );

    if (!leftIcon && !rightIcon) {
      return (
        <input
          type={type}
          className={baseClass}
          ref={ref}
          {...props}
        />
      );
    }

    return (
      <div className="relative flex items-center">
        {leftIcon && (
          <span className="pointer-events-none absolute left-3 flex items-center justify-center text-muted-foreground">
            {leftIcon}
          </span>
        )}
        <input
          type={type}
          className={baseClass}
          ref={ref}
          {...props}
        />
        {rightIcon && (
          <span className="absolute right-3 flex items-center justify-center text-muted-foreground">
            {rightIcon}
          </span>
        )}
      </div>
    );
  },
);
Input.displayName = "Input";

export { Input };
