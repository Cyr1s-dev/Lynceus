import * as React from "react";

import {
  Popover,
  PopoverContent as ShadcnPopoverContent,
  PopoverTrigger as ShadcnPopoverTrigger,
} from "@/components/ui/popover";
import { cn } from "@/lib/utils";
import { focusRing } from "@/ui/untitled/tokens";

/* ───────────────────────── Types ───────────────────────── */

export interface DropdownMenuProps {
  /** The trigger element (usually a button). */
  children: React.ReactNode;
  /** Dropdown menu content — typically a list of `DropdownMenuItem`s. */
  items: React.ReactNode;
  /** Alignment of the popover relative to the trigger. */
  align?: "start" | "center" | "end";
  /** Side of the trigger to display the menu. */
  side?: "top" | "right" | "bottom" | "left";
  /** Additional className for the menu content. */
  className?: string;
}

/* ───────────────────────── Item ───────────────────────── */

export interface DropdownMenuItemProps
  extends Omit<React.ButtonHTMLAttributes<HTMLButtonElement>, "onSelect"> {
  /** Called when the item is clicked. */
  onSelect?: () => void;
  /** Render a leading icon. */
  icon?: React.ReactNode;
  /** Show a trailing shortcut hint. */
  shortcut?: React.ReactNode;
  /** Visual variant for destructive actions. */
  destructive?: boolean;
}

/**
 * A single menu item in the dropdown. Renders as a full-width button row
 * with optional icon and shortcut hint.
 */
function DropdownMenuItem({
  onSelect,
  icon,
  shortcut,
  destructive = false,
  className,
  children,
  ...props
}: DropdownMenuItemProps) {
  return (
    <button
      type="button"
      onClick={onSelect}
      className={cn(
        "flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-left text-sm",
        "transition-colors",
        focusRing.inset,
        destructive
          ? "text-danger hover:bg-danger-soft"
          : "text-foreground hover:bg-accent",
        "disabled:pointer-events-none disabled:opacity-50",
        className,
      )}
      {...props}
    >
      {icon && <span className="shrink-0 [&_svg]:size-4">{icon}</span>}
      <span className="flex-1 truncate">{children}</span>
      {shortcut && (
        <span className="shrink-0 text-xs text-muted-foreground">{shortcut}</span>
      )}
    </button>
  );
}
DropdownMenuItem.displayName = "DropdownMenuItem";

/* ───────────────────────── Separator ───────────────────────── */

/** A thin separator between groups of menu items. */
function DropdownMenuSeparator({
  className,
}: React.HTMLAttributes<HTMLDivElement>) {
  return <div className={cn("-mx-1 my-1 h-px bg-border", className)} />;
}
DropdownMenuSeparator.displayName = "DropdownMenuSeparator";

/* ───────────────────────── Label ───────────────────────── */

/** A non-interactive section label inside the menu. */
function DropdownMenuLabel({
  className,
  ...props
}: React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={cn("px-2.5 py-1.5 text-xs font-semibold text-muted-foreground", className)}
      {...props}
    />
  );
}
DropdownMenuLabel.displayName = "DropdownMenuLabel";

/* ───────────────────────── Component ───────────────────────── */

/**
 * Untitled UI DropdownMenu — a lightweight menu built on the Radix Popover.
 *
 * Since `@radix-ui/react-dropdown-menu` is not installed in this project,
 * this component uses the existing Popover primitive to deliver a simple
 * dropdown menu with trigger + items. It supports keyboard focus and
 * click-outside dismissal via Popover.
 *
 * @example
 * ```tsx
 * <DropdownMenu
 *   align="end"
 *   items={
 *     <>
 *       <DropdownMenuLabel>Actions</DropdownMenuLabel>
 *       <DropdownMenuItem icon={<Edit />} onSelect={handleEdit}>Edit</DropdownMenuItem>
 *       <DropdownMenuSeparator />
 *       <DropdownMenuItem destructive icon={<Trash />} onSelect={handleDelete}>Delete</DropdownMenuItem>
 *     </>
 *   }
 * >
 *   <Button variant="ghost" size="icon"><MoreHorizontal /></Button>
 * </DropdownMenu>
 * ```
 */
function DropdownMenu({
  children,
  items,
  align = "start",
  side = "bottom",
  className,
}: DropdownMenuProps) {
  return (
    <Popover>
      <ShadcnPopoverTrigger asChild>{children}</ShadcnPopoverTrigger>
      <ShadcnPopoverContent
        align={align}
        side={side}
        sideOffset={4}
        className={cn(
          "min-w-[12rem] rounded-md border border-border bg-popover p-1.5",
          "shadow-elev outline-none",
          "data-[state=open]:animate-in data-[state=closed]:animate-out",
          "data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0",
          "data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95",
          className,
        )}
      >
        <div className="flex flex-col">{items}</div>
      </ShadcnPopoverContent>
    </Popover>
  );
}

export {
  DropdownMenu,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuLabel,
};
