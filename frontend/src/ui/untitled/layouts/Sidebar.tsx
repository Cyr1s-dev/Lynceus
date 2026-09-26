import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from 'react'
import { Link, useRouterState } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import {
  Crosshair,
  Archive,
  Radar,
  TerminalSquare,
  Cable,
  BookOpen,
  Bot,
  Puzzle,
  Sparkles,
  Waypoints,
  Settings,
  Shield,
  PanelLeftClose,
  PanelLeftOpen,
  type LucideIcon,
} from 'lucide-react'
import { cn } from '@/lib/utils'
import { APP_NAME, formatAppVersion } from '@/lib/app-meta'
import { Tooltip } from '@/ui/untitled/primitives/Tooltip'

/**
 * Untitled UI Sidebar — Mission-first navigation for the Lynceus platform.
 *
 * Light-first aesthetic:
 *   - Pure white surface on cold-white canvas
 *   - Active = sliding pill + left accent bar (shared indicator, not remounted)
 *   - Collapsible rail: 260px expanded / 60px icon-only (square icon hit targets)
 *   - Compact collapse control sits next to platform meta in the footer
 *   - Preference persisted to localStorage
 */

const SIDEBAR_COLLAPSED_KEY = 'lynceus_sidebar_collapsed'
/** Collapsed rail width: 12px padding × 2 + 36px square icon button. */
const SIDEBAR_COLLAPSED_WIDTH = 60
const SIDEBAR_EXPANDED_WIDTH = 260
/** Keep in sync with `transition-[width] duration-300`. */
const SIDEBAR_WIDTH_TRANSITION_MS = 300

interface NavItem {
  nameKey: string
  href: string
  icon: LucideIcon
}

interface NavSection {
  labelKey: string
  items: NavItem[]
}

interface IndicatorBox {
  top: number
  left: number
  width: number
  height: number
}

const NAV_SECTIONS: NavSection[] = [
  {
    labelKey: 'nav.section.features',
    items: [
      { nameKey: 'nav.missionControl', href: '/', icon: Crosshair },
      { nameKey: 'nav.missionArchive', href: '/missions', icon: Archive },
      { nameKey: 'nav.findingsInbox', href: '/inbox', icon: Shield },
      { nameKey: 'nav.toolAudit', href: '/tools', icon: TerminalSquare },
      { nameKey: 'nav.intelligenceHub', href: '/intelligence', icon: Radar },
      { nameKey: 'nav.knowledgeBase', href: '/knowledge', icon: BookOpen },
    ],
  },
  {
    labelKey: 'nav.section.system',
    items: [
      { nameKey: 'nav.agents', href: '/agents', icon: Sparkles },
      { nameKey: 'nav.skills', href: '/skills', icon: Puzzle },
      { nameKey: 'nav.workerRuntimes', href: '/worker-runtimes', icon: Bot },
      { nameKey: 'nav.litellm', href: '/gateway', icon: Waypoints },
      { nameKey: 'nav.engineManager', href: '/modules', icon: Cable },
      { nameKey: 'nav.settings', href: '/settings', icon: Settings },
    ],
  },
]

function readCollapsedPreference(): boolean {
  try {
    if (typeof window === 'undefined' || !window.localStorage) return false
    return window.localStorage.getItem(SIDEBAR_COLLAPSED_KEY) === '1'
  } catch {
    return false
  }
}

function persistCollapsedPreference(collapsed: boolean): void {
  try {
    if (typeof window === 'undefined' || !window.localStorage) return
    window.localStorage.setItem(SIDEBAR_COLLAPSED_KEY, collapsed ? '1' : '0')
  } catch {
    // Storage may be unavailable in private mode / tests.
  }
}

function isItemActive(pathname: string, href: string): boolean {
  return pathname === href || (href !== '/' && pathname.startsWith(href))
}

export interface SidebarProps {
  collapsed?: boolean
  onCollapsedChange?: (collapsed: boolean) => void
}

export function Sidebar({
  collapsed: controlledCollapsed,
  onCollapsedChange,
}: SidebarProps = {}) {
  const router = useRouterState()
  const { t } = useTranslation()
  const [uncontrolledCollapsed, setUncontrolledCollapsed] = useState(false)
  const appVersion = formatAppVersion()
  const pathname = router.location.pathname

  const navRef = useRef<HTMLElement | null>(null)
  const itemRefs = useRef(new Map<string, HTMLElement>())
  const [indicator, setIndicator] = useState<IndicatorBox | null>(null)
  const [indicatorReady, setIndicatorReady] = useState(false)
  // Disable transform transitions while the rail width is animating so the pill
  // stays glued to the active item instead of lagging across the gap.
  const [layoutSyncing, setLayoutSyncing] = useState(false)
  const prevCollapsedRef = useRef<boolean | null>(null)

  // Hydrate from localStorage after mount to avoid SSR/local mismatch.
  useEffect(() => {
    if (controlledCollapsed !== undefined) return
    setUncontrolledCollapsed(readCollapsedPreference())
  }, [controlledCollapsed])

  const collapsed = controlledCollapsed ?? uncontrolledCollapsed

  const setCollapsed = useCallback(
    (next: boolean) => {
      if (controlledCollapsed === undefined) {
        setUncontrolledCollapsed(next)
      }
      persistCollapsedPreference(next)
      onCollapsedChange?.(next)
    },
    [controlledCollapsed, onCollapsedChange],
  )

  const toggleCollapsed = useCallback(() => {
    setCollapsed(!collapsed)
  }, [collapsed, setCollapsed])

  const setItemRef = useCallback((href: string, node: HTMLElement | null) => {
    if (node) itemRefs.current.set(href, node)
    else itemRefs.current.delete(href)
  }, [])

  const updateIndicator = useCallback(() => {
    const nav = navRef.current
    if (!nav) return

    const activeHref =
      NAV_SECTIONS.flatMap((section) => section.items).find((item) =>
        isItemActive(pathname, item.href),
      )?.href

    if (!activeHref) {
      setIndicator(null)
      setIndicatorReady(false)
      return
    }

    const el = itemRefs.current.get(activeHref)
    if (!el) return

    const navRect = nav.getBoundingClientRect()
    const itemRect = el.getBoundingClientRect()

    // Pin 1:1 to the active link box — no inventing a second shadow layer
    // or offset hacks. The pill is just the original active surface, moved.
    setIndicator({
      top: itemRect.top - navRect.top + nav.scrollTop,
      left: itemRect.left - navRect.left + nav.scrollLeft,
      width: itemRect.width,
      height: itemRect.height,
    })
  }, [pathname])

  // Measure after paint so width collapse / route change both re-sync the pill.
  useLayoutEffect(() => {
    updateIndicator()
    const frame = window.requestAnimationFrame(() => {
      setIndicatorReady(true)
    })
    return () => window.cancelAnimationFrame(frame)
  }, [updateIndicator, collapsed, layoutSyncing])

  useEffect(() => {
    const nav = navRef.current
    if (!nav) return

    const onScroll = () => updateIndicator()
    nav.addEventListener('scroll', onScroll, { passive: true })

    const ro =
      typeof ResizeObserver !== 'undefined'
        ? new ResizeObserver(() => updateIndicator())
        : null
    ro?.observe(nav)

    window.addEventListener('resize', updateIndicator)

    return () => {
      nav.removeEventListener('scroll', onScroll)
      ro?.disconnect()
      window.removeEventListener('resize', updateIndicator)
    }
  }, [updateIndicator])

  // While the sidebar width is animating, pin the pill to the active item
  // without its own transition so it feels attached to the rail.
  useEffect(() => {
    if (prevCollapsedRef.current === null) {
      prevCollapsedRef.current = collapsed
      return
    }
    if (prevCollapsedRef.current === collapsed) return
    prevCollapsedRef.current = collapsed

    setLayoutSyncing(true)
    let raf = 0
    let frames = 0
    const tick = () => {
      updateIndicator()
      frames += 1
      // Cover the 300ms width transition at ~60fps with a little headroom.
      if (frames < 28) {
        raf = window.requestAnimationFrame(tick)
      }
    }
    raf = window.requestAnimationFrame(tick)

    const timer = window.setTimeout(() => {
      setLayoutSyncing(false)
      updateIndicator()
    }, SIDEBAR_WIDTH_TRANSITION_MS + 40)

    return () => {
      window.cancelAnimationFrame(raf)
      window.clearTimeout(timer)
    }
  }, [collapsed, updateIndicator])

  const collapseLabel = collapsed
    ? t('nav.expandSidebar')
    : t('nav.collapseSidebar')

  const collapseButton = (
    <button
      type="button"
      onClick={toggleCollapsed}
      className={cn(
        'inline-flex shrink-0 items-center justify-center rounded-md',
        // Keep the same 36×36 hit target in both states so hover bg matches.
        'h-9 w-9',
        'text-muted-foreground/80 transition-colors duration-150',
        'hover:bg-muted hover:text-foreground',
        'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/20',
      )}
      aria-label={collapseLabel}
      aria-expanded={!collapsed}
      aria-controls="app-sidebar-nav"
    >
      {collapsed ? (
        <PanelLeftOpen className="h-3.5 w-3.5" aria-hidden="true" />
      ) : (
        <PanelLeftClose className="h-3.5 w-3.5" aria-hidden="true" />
      )}
    </button>
  )

  const canAnimateIndicator = indicatorReady && !layoutSyncing

  return (
    <aside
      data-collapsed={collapsed || undefined}
      style={{
        width: collapsed ? SIDEBAR_COLLAPSED_WIDTH : SIDEBAR_EXPANDED_WIDTH,
      }}
      className={cn(
        'group/sidebar flex shrink-0 flex-col overflow-hidden rounded-xl bg-neutral-50 shadow-sm ring-1 ring-border',
        // Width-only transition; keep content clipped to avoid layout flash.
        'transition-[width] duration-300 ease-out-expo',
      )}
    >
      {/* Brand */}
      <div className="flex h-16 w-full items-center px-3">
        <div
          className={cn(
            'flex min-w-0 items-center',
            collapsed ? 'w-full justify-center' : 'gap-2.5',
          )}
        >
          <div className="relative flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-primary/10 ring-1 ring-primary/15">
            <Shield className="h-4 w-4 text-primary" aria-hidden="true" />
            <span
              className="absolute -right-0.5 -top-0.5 h-1.5 w-1.5 animate-pulse rounded-full bg-emerald-500 ring-1 ring-background"
              aria-hidden="true"
            />
          </div>
          <div
            className={cn(
              'min-w-0 overflow-hidden whitespace-nowrap leading-tight',
              'transition-[opacity,max-width] duration-200 ease-out',
              collapsed
                ? 'pointer-events-none max-w-0 opacity-0'
                : 'max-w-[160px] opacity-100 delay-75',
            )}
            aria-hidden={collapsed || undefined}
          >
            <span className="block bg-gradient-to-r from-primary to-sky-400 bg-clip-text font-mono text-sm font-extrabold uppercase tracking-wider text-transparent">
              {APP_NAME}
            </span>
          </div>
        </div>
      </div>

      {/* Nav */}
      <nav
        id="app-sidebar-nav"
        ref={navRef}
        className="relative flex-1 overflow-y-auto overflow-x-hidden px-3 pb-4 pt-1"
      >
        {/*
          Shared sliding active surface — same look as the original active item
          (bg-muted + design-token shadow-xs + left accent). One node moves
          between items; we do NOT invent a heavier/custom shadow.
        */}
        {indicator && (
          <div
            aria-hidden="true"
            className={cn(
              'pointer-events-none absolute left-0 top-0 z-0 rounded-lg bg-card shadow-xs',
              canAnimateIndicator
                ? 'transition-[transform,width,height] duration-300 ease-out-expo'
                : 'transition-none',
            )}
            style={{
              width: indicator.width,
              height: indicator.height,
              transform: `translate3d(${indicator.left}px, ${indicator.top}px, 0)`,
            }}
          >
            {/* Left accent bar rides with the original active surface. */}
            <span className="absolute left-0 top-1/2 h-[18px] w-0.5 -translate-y-1/2 rounded-full bg-primary" />
          </div>
        )}

        <div className="relative z-[1] flex flex-col gap-5">
          {NAV_SECTIONS.map((section) => (
            <div key={section.labelKey} className="space-y-0.5">
              <p
                className={cn(
                  'label-spec overflow-hidden whitespace-nowrap pb-1.5',
                  'transition-[opacity,max-height,padding,margin] duration-200 ease-out',
                  collapsed
                    ? 'pointer-events-none m-0 max-h-0 p-0 opacity-0'
                    : 'max-h-6 px-3 opacity-100 delay-75',
                )}
                aria-hidden={collapsed || undefined}
              >
                {t(section.labelKey)}
              </p>
              {section.items.map((item) => {
                const isActive = isItemActive(pathname, item.href)
                const label = t(item.nameKey)

                const link = (
                  <Link
                    ref={(node) => setItemRef(item.href, node as HTMLElement | null)}
                    to={item.href}
                    title={collapsed ? label : undefined}
                    className={cn(
                      'group relative flex items-center rounded-lg text-sm font-medium',
                      'transition-[color,width,height,padding] duration-200 ease-out',
                      collapsed
                        ? // Square 36×36 hit target; hover bg/shadow reads as a square.
                          'mx-auto h-9 w-9 justify-center p-0'
                        : 'h-9 w-full gap-2.5 px-3',
                      isActive
                        ? 'text-foreground'
                        : 'text-muted-foreground hover:bg-black/[0.03] hover:text-foreground',
                    )}
                  >
                    <item.icon
                      className={cn(
                        'h-4 w-4 shrink-0 transition-colors duration-200',
                        isActive
                          ? 'text-primary'
                          : 'text-muted-foreground/70 group-hover:text-muted-foreground',
                      )}
                      aria-hidden="true"
                    />
                    <span
                      className={cn(
                        'truncate overflow-hidden whitespace-nowrap',
                        'transition-[opacity,max-width] duration-200 ease-out',
                        collapsed
                          ? 'pointer-events-none max-w-0 opacity-0'
                          : 'max-w-[160px] opacity-100 delay-75',
                      )}
                      aria-hidden={collapsed || undefined}
                    >
                      {label}
                    </span>
                  </Link>
                )

                // Always wrap so the link does not remount when toggling collapsed.
                // When expanded, delay is effectively infinite so tooltips stay off.
                return (
                  <Tooltip
                    key={item.nameKey}
                    content={label}
                    side="right"
                    delayDuration={collapsed ? 200 : 100_000}
                  >
                    {link}
                  </Tooltip>
                )
              })}
            </div>
          ))}
        </div>
      </nav>

      {/* Footer: platform meta left, collapse control right-aligned. */}
      <div
        className={cn(
          'flex flex-col px-3',
          // Collapsed: pin the block lower with less bottom air.
          collapsed ? 'pb-3 pt-3' : 'pb-6 pt-4',
        )}
      >
        {/* 分隔线：左右留白（mx-1 + 容器 px-3 ≈ 16px），不占满整条卡宽。 */}
        <div className="mx-1 border-t border-border" aria-hidden="true" />
        <div
          className={cn(
            'flex items-center',
            collapsed ? 'mt-3 justify-center' : 'mt-4 justify-between gap-3',
          )}
        >
          <div
            className={cn(
              'min-w-0 overflow-hidden whitespace-nowrap',
              'transition-[opacity,max-width,max-height] duration-200 ease-out',
              collapsed
                ? // Fully collapse so it does not reserve vertical space and lift the control.
                  'pointer-events-none max-h-0 max-w-0 opacity-0'
                : // Meta stays left; collapse button is pushed to the right edge.
                  'max-h-12 max-w-[180px] opacity-100 delay-75',
            )}
            aria-hidden={collapsed || undefined}
          >
            <p className="text-[11px] font-medium uppercase tracking-wide text-muted-foreground/70">
              {t('nav.platformMeta')}
            </p>
            <p
              className="mt-0.5 font-mono text-xs tabular-nums text-muted-foreground/60"
              title={appVersion}
            >
              {appVersion}
            </p>
          </div>

          <Tooltip content={collapseLabel} side="right" delayDuration={200}>
            {collapseButton}
          </Tooltip>
        </div>
      </div>
    </aside>
  )
}
