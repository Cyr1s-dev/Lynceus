import * as React from "react";
import { ChevronRight } from "lucide-react";
import { Link } from "@tanstack/react-router";

import { cn } from "@/lib/utils";

/* ───────────────────────── Types ───────────────────────── */

export interface BreadcrumbItem {
  /** Display label for the breadcrumb link. */
  label: React.ReactNode;
  /** Navigation path. When omitted, the item is rendered as plain text (current page). */
  to?: string;
}

export interface BreadcrumbsProps {
  /** Ordered list of breadcrumb items. */
  items: BreadcrumbItem[];
  /** Additional className for the nav element. */
  className?: string;
}

/* ───────────────────────── Component ───────────────────────── */

/**
 * Untitled UI Breadcrumbs — a simple navigation trail for page hierarchies.
 *
 * Renders a list of breadcrumb items separated by chevron icons. Items with
 * a `to` property are rendered as TanStack Router `Link` components; items
 * without `to` are rendered as plain text (typically the current page).
 *
 * @example
 * ```tsx
 * <Breadcrumbs
 *   items={[
 *     { label: "Missions", to: "/missions" },
 *     { label: "Mission #42", to: "/missions/42" },
 *     { label: "Findings" },
 *   ]}
 * />
 * ```
 */
function Breadcrumbs({ items, className }: BreadcrumbsProps) {
  return (
    <nav aria-label="Breadcrumb" className={cn("flex", className)}>
      <ol className="flex flex-wrap items-center gap-1.5 text-sm text-muted-foreground">
        {items.map((item, index) => {
          const isLast = index === items.length - 1;

          return (
            <li key={index} className="flex items-center gap-1.5">
              {item.to ? (
                <Link
                  to={item.to}
                  className={cn(
                    "rounded transition-colors hover:text-foreground",
                    "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2",
                  )}
                >
                  {item.label}
                </Link>
              ) : (
                <span
                  className={cn(isLast && "font-medium text-foreground")}
                  aria-current={isLast ? "page" : undefined}
                >
                  {item.label}
                </span>
              )}
              {!isLast && (
                <ChevronRight className="h-3.5 w-3.5 shrink-0 text-muted-foreground/60" />
              )}
            </li>
          );
        })}
      </ol>
    </nav>
  );
}

export { Breadcrumbs };
