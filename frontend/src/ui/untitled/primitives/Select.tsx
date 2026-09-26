import * as React from "react";

import {
  Select as ShadcnSelect,
  SelectContent as ShadcnSelectContent,
  SelectGroup as ShadcnSelectGroup,
  SelectItem as ShadcnSelectItem,
  SelectLabel as ShadcnSelectLabel,
  SelectScrollDownButton as ShadcnSelectScrollDownButton,
  SelectScrollUpButton as ShadcnSelectScrollUpButton,
  SelectSeparator as ShadcnSelectSeparator,
  SelectTrigger as ShadcnSelectTrigger,
  SelectValue as ShadcnSelectValue,
} from "@/components/ui/select";
import { cn } from "@/lib/utils";
import { focusRing } from "@/ui/untitled/tokens";

/* ───────────────────────── Re-exports ───────────────────────── */

/**
 * Untitled UI Select — a cleaner-styled wrapper around the shadcn/Radix Select.
 *
 * Re-exports all select parts with the same API as shadcn, but with refined
 * Untitled UI styling: h-9 trigger height, tighter padding, subtle border,
 * and inset focus ring. Designed for dropdowns in forms and filter bars.
 *
 * @example
 * ```tsx
 * <Select defaultValue="active">
 *   <SelectTrigger><SelectValue placeholder="Status" /></SelectTrigger>
 *   <SelectContent>
 *     <SelectItem value="active">Active</SelectItem>
 *     <SelectItem value="paused">Paused</SelectItem>
 *   </SelectContent>
 * </Select>
 * ```
 */

const Select = ShadcnSelect;
const SelectGroup = ShadcnSelectGroup;
const SelectValue = ShadcnSelectValue;

/**
 * Trigger button for the select. Wraps the shadcn trigger with Untitled UI
 * height (h-9) and inset focus ring. The chevron icon is included internally.
 */
const SelectTrigger = React.forwardRef<
  React.ElementRef<typeof ShadcnSelectTrigger>,
  React.ComponentPropsWithoutRef<typeof ShadcnSelectTrigger>
>(({ className, ...props }, ref) => (
  <ShadcnSelectTrigger
    ref={ref}
    className={cn(
      "h-9 rounded-md",
      focusRing.inset,
      className,
    )}
    {...props}
  />
));
SelectTrigger.displayName = "SelectTrigger";

/**
 * Popover content container for the select. Wraps the shadcn content with
 * Untitled UI shadow and border styling. The viewport and scroll buttons
 * are included internally.
 */
const SelectContent = React.forwardRef<
  React.ElementRef<typeof ShadcnSelectContent>,
  React.ComponentPropsWithoutRef<typeof ShadcnSelectContent>
>(({ className, ...props }, ref) => (
  <ShadcnSelectContent
    ref={ref}
    className={cn(
      "rounded-md border-border shadow-elev",
      className,
    )}
    {...props}
  />
));
SelectContent.displayName = "SelectContent";

const SelectLabel = React.forwardRef<
  React.ElementRef<typeof ShadcnSelectLabel>,
  React.ComponentPropsWithoutRef<typeof ShadcnSelectLabel>
>(({ className, ...props }, ref) => (
  <ShadcnSelectLabel
    ref={ref}
    className={cn("text-xs font-semibold text-muted-foreground", className)}
    {...props}
  />
));
SelectLabel.displayName = "SelectLabel";

/**
 * Individual option in the select dropdown. Wraps the shadcn item with
 * Untitled UI hover styling. The check indicator is included internally.
 */
const SelectItem = React.forwardRef<
  React.ElementRef<typeof ShadcnSelectItem>,
  React.ComponentPropsWithoutRef<typeof ShadcnSelectItem>
>(({ className, ...props }, ref) => (
  <ShadcnSelectItem
    ref={ref}
    className={cn(
      "rounded-sm transition-colors",
      className,
    )}
    {...props}
  />
));
SelectItem.displayName = "SelectItem";

const SelectSeparator = React.forwardRef<
  React.ElementRef<typeof ShadcnSelectSeparator>,
  React.ComponentPropsWithoutRef<typeof ShadcnSelectSeparator>
>(({ className, ...props }, ref) => (
  <ShadcnSelectSeparator
    ref={ref}
    className={cn("bg-border", className)}
    {...props}
  />
));
SelectSeparator.displayName = "SelectSeparator";

const SelectScrollUpButton = ShadcnSelectScrollUpButton;
const SelectScrollDownButton = ShadcnSelectScrollDownButton;

export {
  Select,
  SelectGroup,
  SelectValue,
  SelectTrigger,
  SelectContent,
  SelectLabel,
  SelectItem,
  SelectSeparator,
  SelectScrollUpButton,
  SelectScrollDownButton,
};
