import * as React from 'react';
import { Outlet } from '@tanstack/react-router';
import { cn } from '@/lib/utils';
import { Toaster } from '@/components/ui/toaster';
import { Sidebar } from './Sidebar';
import { Topbar } from './Topbar';
import { usePageWidth } from './page-width';

/**
 * Untitled UI AppShell — the root layout for the Lynceus Mission Control
 * operating system surface.
 *
 * Structure: fixed sidebar + flexible content area (topbar + scrollable main).
 * Light-first canvas: cold near-white background, pure white chrome.
 *
 * The shell is chrome only — it does not impose a content measure. Ordinary
 * pages get theirs from PageContainer (default 1920); canvas-heavy routes such
 * as mission detail opt into full viewport width through the page-width channel.
 */
export interface AppShellProps {
  children?: React.ReactNode;
}

export function AppShell({ children }: AppShellProps) {
  const pageWidth = usePageWidth();
  return (
    <div className="h-screen w-full bg-card p-2 font-sans text-foreground">
      <div className="flex h-full w-full gap-2">
        <Sidebar />
        <div className="flex min-w-0 flex-1 flex-col overflow-hidden">
          <Topbar />
          <main className="flex-1 overflow-auto p-6 md:p-8">
            <div className={cn('mx-auto h-full', pageWidth === 'full' ? 'max-w-full' : 'max-w-[1920px]')}>
              {children ?? <Outlet />}
            </div>
          </main>
        </div>
      </div>
      <Toaster />
    </div>
  );
}
