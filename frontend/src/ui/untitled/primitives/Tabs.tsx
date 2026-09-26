import * as React from "react";

import {
  Tabs as ShadcnTabs,
  TabsContent as ShadcnTabsContent,
  TabsList as ShadcnTabsList,
  TabsTrigger as ShadcnTabsTrigger,
} from "@/components/ui/tabs";
import { cn } from "@/lib/utils";

/* ───────────────────────── Re-exports ───────────────────────── */

/**
 * Untitled UI Tabs — a cleaner-styled wrapper around the shadcn/Radix Tabs.
 *
 * Re-exports `Tabs`, `TabsList`, `TabsTrigger`, and `TabsContent` with the
 * same API as shadcn, but with refined Untitled UI styling: flatter list
 * background, tighter trigger padding, and subtler active-state shadow.
 *
 * @example
 * ```tsx
 * <Tabs defaultValue="overview">
 *   <TabsList>
 *     <TabsTrigger value="overview">Overview</TabsTrigger>
 *     <TabsTrigger value="findings">Findings</TabsTrigger>
 *   </TabsList>
 *   <TabsContent value="overview">…</TabsContent>
 *   <TabsContent value="findings">…</TabsContent>
 * </Tabs>
 * ```
 */

const Tabs = ShadcnTabs;

interface TabsListProps extends React.ComponentPropsWithoutRef<typeof ShadcnTabsList> {
  animated?: boolean;
  indicatorClassName?: string;
}

interface IndicatorRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

const TabsList = React.forwardRef<
  React.ElementRef<typeof ShadcnTabsList>,
  TabsListProps
>(({ className, animated = false, indicatorClassName, children, ...props }, forwardedRef) => {
  const listRef = React.useRef<React.ElementRef<typeof ShadcnTabsList>>(null);
  const [indicator, setIndicator] = React.useState<IndicatorRect | null>(null);
  const [motionReady, setMotionReady] = React.useState(false);

  React.useImperativeHandle(
    forwardedRef,
    () => listRef.current as React.ElementRef<typeof ShadcnTabsList>,
  );

  const measureActiveTab = React.useCallback((withMotion: boolean) => {
    const list = listRef.current;
    const activeTab = list?.querySelector<HTMLElement>(
      ':scope > [role="tab"][data-state="active"]',
    );
    if (!list || !activeTab) return;

    setIndicator({
      x: activeTab.offsetLeft,
      y: activeTab.offsetTop,
      width: activeTab.offsetWidth,
      height: activeTab.offsetHeight,
    });
    if (withMotion) setMotionReady(true);
  }, []);

  React.useLayoutEffect(() => {
    if (!animated) return undefined;
    measureActiveTab(false);
    const list = listRef.current;
    if (!list) return undefined;

    const scheduleAnimatedMeasure = () => {
      window.requestAnimationFrame(() => measureActiveTab(true));
    };
    const mutationObserver = new MutationObserver(scheduleAnimatedMeasure);
    mutationObserver.observe(list, {
      subtree: true,
      attributes: true,
      attributeFilter: ['data-state'],
    });

    const resizeObserver = new ResizeObserver(() => measureActiveTab(false));
    resizeObserver.observe(list);
    list.querySelectorAll<HTMLElement>(':scope > [role="tab"]').forEach((tab) => {
      resizeObserver.observe(tab);
    });

    return () => {
      mutationObserver.disconnect();
      resizeObserver.disconnect();
    };
  }, [animated, measureActiveTab]);

  const hasIndicator = animated && indicator !== null;

  return (
    <ShadcnTabsList
      ref={listRef}
      className={cn(
        "inline-flex h-9 items-center justify-center rounded-lg bg-muted p-1 text-muted-foreground",
        animated && "relative isolate",
        hasIndicator && "[&>[role=tab]]:relative [&>[role=tab]]:z-20 [&>[role=tab][data-state=active]]:bg-transparent [&>[role=tab][data-state=active]]:shadow-none",
        className,
      )}
      {...props}
    >
      {hasIndicator && (
        <span
          aria-hidden="true"
          className={cn(
            "pointer-events-none absolute left-0 top-0 z-10 rounded-md bg-background shadow-xs",
            motionReady && "transition-[transform,width,height] duration-300 ease-out",
            indicatorClassName,
          )}
          style={{
            width: indicator.width,
            height: indicator.height,
            transform: `translate3d(${indicator.x}px, ${indicator.y}px, 0)`,
          }}
        />
      )}
      {children}
    </ShadcnTabsList>
  );
});
TabsList.displayName = "TabsList";

const TabsTrigger = React.forwardRef<
  React.ElementRef<typeof ShadcnTabsTrigger>,
  React.ComponentPropsWithoutRef<typeof ShadcnTabsTrigger>
>(({ className, ...props }, ref) => (
  <ShadcnTabsTrigger
    ref={ref}
    className={cn(
      "inline-flex items-center justify-center whitespace-nowrap rounded-md px-3 py-1 text-sm font-medium",
      "ring-offset-background transition-[color,background-color,box-shadow,opacity] duration-150 ease-out",
      "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2",
      "disabled:pointer-events-none disabled:opacity-50",
      "data-[state=active]:bg-background data-[state=active]:text-foreground data-[state=active]:shadow-xs",
      className,
    )}
    {...props}
  />
));
TabsTrigger.displayName = "TabsTrigger";

const TabsContent = React.forwardRef<
  React.ElementRef<typeof ShadcnTabsContent>,
  React.ComponentPropsWithoutRef<typeof ShadcnTabsContent>
>(({ className, ...props }, ref) => (
  <ShadcnTabsContent
    ref={ref}
    className={cn(
      "mt-2 ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2",
      className,
    )}
    {...props}
  />
));
TabsContent.displayName = "TabsContent";

export { Tabs, TabsList, TabsTrigger, TabsContent };
