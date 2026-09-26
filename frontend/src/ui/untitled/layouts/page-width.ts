import { useEffect, useSyncExternalStore } from 'react';

/**
 * Page width channel — lets a route widen the AppShell content column.
 *
 * The shell owns a single max-width so ordinary pages keep a readable measure.
 * Canvas-heavy routes (mission detail, whose exploration graph is the primary
 * surface) opt into the full viewport width, matching the full-bleed task
 * page. This is a tiny external store rather than context because the shell
 * sits *above* the routes that opt in and cannot consume their context.
 */
export type PageWidth = 'default' | 'full';

let currentWidth: PageWidth = 'default';
const listeners = new Set<() => void>();

function setWidth(width: PageWidth) {
  if (currentWidth === width) return;
  currentWidth = width;
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function getSnapshot(): PageWidth {
  return currentWidth;
}

/** Read the width requested by the active route. */
export function usePageWidth(): PageWidth {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

/**
 * Request a width for the lifetime of the calling component. The channel falls
 * back to `default` on unmount so navigating away restores the shell measure.
 */
export function usePageWidthMode(width: PageWidth = 'default') {
  useEffect(() => {
    setWidth(width);
    return () => setWidth('default');
  }, [width]);
}
