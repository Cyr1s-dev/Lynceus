import * as React from "react";

import {
  Dialog as ShadcnDialog,
  DialogClose as ShadcnDialogClose,
  DialogContent as ShadcnDialogContent,
  DialogDescription as ShadcnDialogDescription,
  DialogTitle as ShadcnDialogTitle,
  DialogTrigger as ShadcnDialogTrigger,
} from "@/components/ui/dialog";
import { cn } from "@/lib/utils";

/* ───────────────────────── Re-exports ───────────────────────── */

/**
 * Untitled UI Dialog — a cleaner-styled wrapper around the shadcn/Radix Dialog.
 *
 * Re-exports all dialog parts with the same API as shadcn, but with refined
 * Untitled UI styling: tighter padding, subtle shadow, and cleaner header
 * spacing. Designed for modal confirmations, forms, and detail views.
 *
 * @example
 * ```tsx
 * <Dialog>
 *   <DialogTrigger asChild><Button>Open</Button></DialogTrigger>
 *   <DialogContent>
 *     <DialogHeader>
 *       <DialogTitle>Confirm action</DialogTitle>
 *       <DialogDescription>This cannot be undone.</DialogDescription>
 *     </DialogHeader>
 *     <DialogFooter>
 *       <Button tone="danger">Delete</Button>
 *     </DialogFooter>
 *   </DialogContent>
 * </Dialog>
 * ```
 */

const Dialog = ShadcnDialog;
const DialogTrigger = ShadcnDialogTrigger;
const DialogClose = ShadcnDialogClose;

const DialogContent = React.forwardRef<
  React.ElementRef<typeof ShadcnDialogContent>,
  React.ComponentPropsWithoutRef<typeof ShadcnDialogContent>
>(({ className, children, ...props }, ref) => (
  <ShadcnDialogContent
    ref={ref}
    className={cn(
      "fixed left-[50%] top-[50%] z-[210] grid w-full max-w-lg translate-x-[-50%] translate-y-[-50%]",
      "gap-4 border border-border bg-background p-6 shadow-elev duration-200",
      "data-[state=open]:animate-in data-[state=closed]:animate-out",
      "data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0",
      "data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95",
      "data-[state=closed]:slide-out-to-left-1/2 data-[state=closed]:slide-out-to-top-[48%]",
      "data-[state=open]:slide-in-from-left-1/2 data-[state=open]:slide-in-from-top-[48%]",
      "sm:rounded-lg",
      className,
    )}
    {...props}
  >
    {children}
  </ShadcnDialogContent>
));
DialogContent.displayName = "DialogContent";

const DialogHeader = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, ...props }, ref) => (
  <div
    ref={ref}
    className={cn("flex flex-col space-y-1.5 text-left", className)}
    {...props}
  />
));
DialogHeader.displayName = "DialogHeader";

const DialogFooter = React.forwardRef<
  HTMLDivElement,
  React.HTMLAttributes<HTMLDivElement>
>(({ className, ...props }, ref) => (
  <div
    ref={ref}
    className={cn(
      "flex flex-col-reverse gap-2 sm:flex-row sm:justify-end sm:gap-2",
      className,
    )}
    {...props}
  />
));
DialogFooter.displayName = "DialogFooter";

const DialogTitle = React.forwardRef<
  React.ElementRef<typeof ShadcnDialogTitle>,
  React.ComponentPropsWithoutRef<typeof ShadcnDialogTitle>
>(({ className, ...props }, ref) => (
  <ShadcnDialogTitle
    ref={ref}
    className={cn("text-base font-semibold leading-tight tracking-tight", className)}
    {...props}
  />
));
DialogTitle.displayName = "DialogTitle";

const DialogDescription = React.forwardRef<
  React.ElementRef<typeof ShadcnDialogDescription>,
  React.ComponentPropsWithoutRef<typeof ShadcnDialogDescription>
>(({ className, ...props }, ref) => (
  <ShadcnDialogDescription
    ref={ref}
    className={cn("text-sm text-muted-foreground", className)}
    {...props}
  />
));
DialogDescription.displayName = "DialogDescription";

export {
  Dialog,
  DialogTrigger,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
  DialogClose,
};
