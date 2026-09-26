import { cva } from "class-variance-authority";

export type ButtonTone = "default" | "primary" | "danger" | "success" | "warning";

export const buttonVariants = cva(
  [
    "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-lg text-sm font-medium",
    "ring-offset-background transition-[color,background-color,border-color,box-shadow,opacity] duration-150 ease-out",
    "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/30 focus-visible:ring-offset-2",
    "disabled:pointer-events-none disabled:opacity-50",
    "active:scale-[0.98]",
    "[&_svg]:pointer-events-none [&_svg]:size-4 [&_svg]:shrink-0",
  ].join(" "),
  {
    variants: {
      variant: {
        default: "bg-primary text-primary-foreground shadow-xs hover:bg-primary/90",
        destructive:
          "bg-destructive text-destructive-foreground shadow-xs hover:bg-destructive/90",
        outline:
          "border border-input bg-card shadow-xs hover:bg-muted hover:text-foreground",
        secondary:
          "bg-secondary text-secondary-foreground hover:bg-secondary/80",
        ghost: "hover:bg-muted hover:text-foreground",
        link: "text-primary underline-offset-4 hover:underline active:scale-100",
      },
      tone: {
        default: "",
        primary: "bg-primary text-primary-foreground shadow-xs hover:bg-primary/90",
        danger: "bg-danger text-white shadow-xs hover:bg-danger/90 focus-visible:ring-danger/30",
        success: "bg-success text-white shadow-xs hover:bg-success/90 focus-visible:ring-success/30",
        warning: "bg-warning text-white shadow-xs hover:bg-warning/90 focus-visible:ring-warning/30",
      },
      size: {
        default: "h-9 px-4 py-2",
        sm: "h-8 rounded-lg px-3 text-xs",
        lg: "h-10 rounded-lg px-6",
        icon: "h-9 w-9",
      },
    },
    compoundVariants: [
      {
        tone: ["primary", "danger", "success", "warning"],
        variant: ["default", "destructive", "outline", "secondary"],
        className: "",
      },
    ],
    defaultVariants: {
      variant: "default",
      tone: "default",
      size: "default",
    },
  },
);
