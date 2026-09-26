import * as React from "react";
import { X } from "lucide-react";

import {
  Sheet,
  SheetClose,
  SheetContent as ShadcnSheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { ScrollArea } from "@/components/ui/scroll-area";
import { cn } from "@/lib/utils";

/* ───────────────────────── Types ───────────────────────── */

export type DrawerWidth = "sm" | "md" | "lg";

export interface DrawerProps {
  /** Whether the drawer is open. */
  open: boolean;
  /** Called when the open state changes. */
  onOpenChange: (open: boolean) => void;
  /** Drawer title (rendered in the header). */
  title: React.ReactNode;
  /** Optional description below the title. */
  description?: React.ReactNode;
  /** Optional icon rendered before the title. */
  icon?: React.ReactNode;
  /** Right-aligned header actions (status badge, external link). */
  headerActions?: React.ReactNode;
  /** Sticky footer (save / cancel actions). */
  footer?: React.ReactNode;
  /** Drawer width preset. */
  width?: DrawerWidth;
  /** Drawer body content. */
  children: React.ReactNode;
  /** Additional className for the sheet content. */
  className?: string;
}

/* ───────────────────────── Width map ───────────────────────── */

const WIDTH_MAP: Record<DrawerWidth, string> = {
  sm: "sm:max-w-md",
  md: "sm:max-w-xl",
  lg: "sm:max-w-3xl",
};

/* ───────────────────────── Component ───────────────────────── */

/**
 * Untitled UI Drawer — a right-side slide-over panel for inspecting details.
 *
 * Wraps the shadcn Sheet with a consistent layout: header (icon + title +
 * description + header actions), scrollable body, and optional sticky
 * footer action bar. This replaces the older `components/design/DetailDrawer.tsx`
 * as the canonical drawer for the Lynceus design system.
 *
 * @example
 * ```tsx
 * <Drawer
 *   open={open}
 *   onOpenChange={setOpen}
 *   title="Finding Details"
 *   description="SQL Injection in /api/users"
 *   width="md"
 *   footer={<Button tone="primary">Save</Button>}
 * >
 *   <p>Detail content…</p>
 * </Drawer>
 * ```
 */
function Drawer({
  open,
  onOpenChange,
  title,
  description,
  icon,
  headerActions,
  footer,
  width = "md",
  children,
  className,
}: DrawerProps) {
  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <ShadcnSheetContent
        side="right"
        className={cn(
          "flex w-full flex-col gap-0 p-0",
          WIDTH_MAP[width],
          className,
        )}
      >
        {/* Header */}
        <SheetHeader className="flex-row items-start justify-between border-b border-border px-6 py-4 text-left">
          <div className="flex min-w-0 items-start gap-3 pr-8">
            {icon && <div className="mt-0.5 shrink-0 text-primary">{icon}</div>}
            <div className="min-w-0 space-y-1">
              <SheetTitle className="truncate text-base font-semibold text-foreground">
                {title}
              </SheetTitle>
              {description && (
                <SheetDescription className="text-xs text-muted-foreground">
                  {description}
                </SheetDescription>
              )}
            </div>
          </div>
          {headerActions && (
            <div className="flex shrink-0 items-center gap-2">{headerActions}</div>
          )}
        </SheetHeader>

        {/* Body */}
        <ScrollArea className="flex-1">
          <div className="px-6 py-5">{children}</div>
        </ScrollArea>

        {/* Footer */}
        {footer && (
          <div className="flex items-center justify-end gap-2 border-t border-border bg-muted/30 px-6 py-3">
            {footer}
          </div>
        )}

        {/* Close button */}
        <SheetClose className="absolute right-4 top-4 rounded-sm opacity-60 ring-offset-background transition-opacity hover:opacity-100 focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2 disabled:pointer-events-none">
          <X className="h-4 w-4" />
          <span className="sr-only">Close</span>
        </SheetClose>
      </ShadcnSheetContent>
    </Sheet>
  );
}

export { Drawer };
