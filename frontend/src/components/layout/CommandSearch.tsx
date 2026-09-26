import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react';
import { useNavigate } from '@tanstack/react-router';
import { useQuery } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import {
  ArrowRight,
  Box,
  Inbox,
  Layers,
  PlayCircle,
  Radar,
  Search,
  Settings,
  Wrench,
  type LucideIcon,
} from 'lucide-react';
import { api } from '@/lib/api';
import type { Mission } from '@/lib/types';
import { cn, missionDisplayTitle } from '@/lib/utils';

type SearchKind = 'page' | 'mission' | 'action';

interface SearchItem {
  id: string;
  kind: SearchKind;
  title: string;
  subtitle?: string;
  keywords?: string;
  href: string;
  icon: LucideIcon;
}

function normalize(value: string): string {
  return value.trim().toLowerCase();
}

function scoreItem(item: SearchItem, query: string): number {
  if (!query) return 1;

  const haystack = normalize(
    [item.title, item.subtitle, item.keywords, item.kind].filter(Boolean).join(' '),
  );
  if (!haystack) return 0;

  const tokens = query.split(/\s+/).filter(Boolean);
  if (tokens.length === 0) return 1;

  let score = 0;
  for (const token of tokens) {
    if (!haystack.includes(token)) return 0;
    score += 10;
    if (normalize(item.title).startsWith(token)) score += 20;
    if (normalize(item.title).includes(token)) score += 8;
  }
  return score;
}

function kindOrder(kind: SearchKind): number {
  switch (kind) {
    case 'action':
      return 0;
    case 'mission':
      return 1;
    case 'page':
      return 3;
    default:
      return 9;
  }
}

function missionTargetText(mission: Mission): string {
  return Object.values(mission.target || {})
    .filter((value) => typeof value === 'string' && value.trim())
    .join(' ');
}

/**
 * Inline topbar search: type in the input, results drop down under it.
 * No modal / command palette dialog.
 */
export function CommandSearch() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const rootRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [activeIndex, setActiveIndex] = useState(0);

  const isMac =
    typeof navigator !== 'undefined' &&
    /Mac|iPhone|iPad|iPod/i.test(navigator.platform || navigator.userAgent);

  const { data: missions = [] } = useQuery({
    queryKey: ['missions', 'command-search'],
    queryFn: () => api.getMissions(),
    enabled: open,
    staleTime: 30_000,
  });

  const pageItems = useMemo<SearchItem[]>(
    () => [
      {
        id: 'page-dashboard',
        kind: 'page',
        title: t('nav.missionControl'),
        subtitle: t('nav.section.workspace'),
        keywords: 'dashboard home mission control 任务中心',
        href: '/',
        icon: Layers,
      },
      {
        id: 'page-missions',
        kind: 'page',
        title: t('nav.missionArchive'),
        subtitle: t('nav.section.missions'),
        keywords: 'missions archive 任务 归档',
        href: '/missions',
        icon: PlayCircle,
      },
      {
        id: 'page-inbox',
        kind: 'page',
        title: t('nav.findingsInbox'),
        subtitle: t('nav.section.execution'),
        keywords: 'findings inbox risk 风险 研判',
        href: '/inbox',
        icon: Inbox,
      },
      {
        id: 'page-runs',
        kind: 'page',
        title: t('nav.auditRuns'),
        subtitle: t('nav.section.execution'),
        keywords: 'runs audit executions 审计执行',
        href: '/runs',
        icon: PlayCircle,
      },
      {
        id: 'page-tools',
        kind: 'page',
        title: t('nav.toolAudit'),
        subtitle: t('nav.section.audit'),
        keywords: 'tools invocations audit 工具',
        href: '/tools',
        icon: Wrench,
      },
      {
        id: 'page-modules',
        kind: 'page',
        title: t('nav.engineManager'),
        subtitle: t('nav.section.platform'),
        keywords: 'engines modules 引擎',
        href: '/modules',
        icon: Box,
      },
      {
        id: 'page-intelligence-hub',
        kind: 'page',
        title: t('nav.intelligenceHub'),
        subtitle: t('nav.section.intelligence'),
        keywords: 'intelligence hub osint intel 情报中台',
        href: '/intelligence',
        icon: Radar,
      },
      {
        id: 'page-knowledge',
        kind: 'page',
        title: t('nav.knowledgeBase'),
        subtitle: t('nav.section.knowledge'),
        keywords: 'knowledge base 知识库',
        href: '/knowledge',
        icon: Layers,
      },
      {
        id: 'page-settings',
        kind: 'page',
        title: t('nav.settings'),
        subtitle: t('nav.section.platform'),
        keywords: 'settings providers 设置',
        href: '/settings',
        icon: Settings,
      },
    ],
    [t],
  );

  const catalog = useMemo<SearchItem[]>(() => {
    const missionItems: SearchItem[] = missions.map((mission) => ({
      id: `mission-${mission.id}`,
      kind: 'mission',
      title: missionDisplayTitle(mission) || mission.id,
      subtitle: [mission.status, mission.category, missionTargetText(mission)]
        .filter(Boolean)
        .join(' · '),
      keywords: [
        mission.id,
        mission.project_id,
        mission.status,
        mission.category,
        ...(mission.tags || []),
        missionTargetText(mission),
      ]
        .filter(Boolean)
        .join(' '),
      href: `/missions/${mission.id}`,
      icon: PlayCircle,
    }));

    return [...missionItems, ...pageItems];
  }, [missions, pageItems]);

  const results = useMemo(() => {
    const q = normalize(query);
    const ranked = catalog
      .map((item) => ({ item, score: scoreItem(item, q) }))
      .filter((entry) => entry.score > 0)
      .sort((a, b) => {
        if (b.score !== a.score) return b.score - a.score;
        return kindOrder(a.item.kind) - kindOrder(b.item.kind);
      })
      .map((entry) => entry.item);

    // Empty query: show pages first, then a few recent entities.
    if (!q) {
      const pages = ranked.filter((item) => item.kind === 'page');
      const entities = ranked.filter((item) => item.kind !== 'page').slice(0, 8);
      return [...pages, ...entities].slice(0, 12);
    }

    return ranked.slice(0, 12);
  }, [catalog, query]);

  useEffect(() => {
    setActiveIndex(0);
  }, [query, open, results.length]);

  const close = useCallback(() => {
    setOpen(false);
    setActiveIndex(0);
  }, []);

  const clearAndClose = useCallback(() => {
    setOpen(false);
    setQuery('');
    setActiveIndex(0);
  }, []);

  const goTo = useCallback(
    (item: SearchItem) => {
      clearAndClose();
      inputRef.current?.blur();
      // Dynamic entity routes are built as strings; cast for the router.
      void navigate({ to: item.href as '/' });
    },
    [clearAndClose, navigate],
  );

  // Click outside closes the dropdown (keeps typed query).
  useEffect(() => {
    if (!open) return;

    const onPointerDown = (event: MouseEvent) => {
      const target = event.target as Node | null;
      if (!target) return;
      if (rootRef.current?.contains(target)) return;
      close();
    };

    document.addEventListener('mousedown', onPointerDown);
    return () => document.removeEventListener('mousedown', onPointerDown);
  }, [close, open]);

  // Global shortcut: focus the top search input (no modal).
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      const tag = target?.tagName?.toLowerCase();
      const isTyping =
        tag === 'input' ||
        tag === 'textarea' ||
        target?.isContentEditable ||
        Boolean(target?.closest('[role="textbox"]'));

      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') {
        event.preventDefault();
        inputRef.current?.focus();
        setOpen(true);
        return;
      }

      if (event.key === '/' && !isTyping) {
        event.preventDefault();
        inputRef.current?.focus();
        setOpen(true);
      }
    };

    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, []);

  const kindLabel = (kind: SearchKind) => {
    switch (kind) {
      case 'mission':
        return t('commandSearch.kinds.mission');
      case 'action':
        return t('commandSearch.kinds.action');
      default:
        return t('commandSearch.kinds.page');
    }
  };

  return (
    <div ref={rootRef} className="relative w-full max-w-md">
      <div
        className={cn(
          'flex h-10 w-full items-center gap-2.5 rounded-xl border px-3',
          // Stable colors — no bg flash on focus
          'border-border/80 bg-muted/40 text-foreground',
          'transition-[border-color,box-shadow] duration-150',
          'hover:border-border',
          open
            ? 'border-primary/35 shadow-[0_0_0_3px_hsl(var(--primary)/0.12)]'
            : 'focus-within:border-primary/35 focus-within:shadow-[0_0_0_3px_hsl(var(--primary)/0.12)]',
        )}
      >
        <Search className="h-4 w-4 shrink-0 text-muted-foreground" />
        <input
          id="global-command-search"
          name="global-command-search"
          ref={inputRef}
          value={query}
          onChange={(event) => {
            setQuery(event.target.value);
            if (!open) setOpen(true);
          }}
          onFocus={() => setOpen(true)}
          placeholder={t('topbar.searchPlaceholder')}
          className={cn(
            'h-full min-w-0 flex-1 bg-transparent text-sm text-foreground',
            'placeholder:text-muted-foreground',
            // Kill browser default focus fill / outline
            'border-0 outline-none ring-0',
            'focus:border-0 focus:outline-none focus:ring-0',
            'appearance-none',
          )}
          autoComplete="off"
          spellCheck={false}
          role="combobox"
          aria-expanded={open}
          aria-controls="topbar-search-results"
          aria-autocomplete="list"
          aria-activedescendant={
            open && results[activeIndex] ? `search-item-${results[activeIndex].id}` : undefined
          }
          onKeyDown={(event) => {
            if (event.key === 'ArrowDown') {
              event.preventDefault();
              if (!open) setOpen(true);
              setActiveIndex((index) =>
                results.length === 0 ? 0 : (index + 1) % results.length,
              );
            } else if (event.key === 'ArrowUp') {
              event.preventDefault();
              if (!open) setOpen(true);
              setActiveIndex((index) =>
                results.length === 0
                  ? 0
                  : (index - 1 + results.length) % results.length,
              );
            } else if (event.key === 'Enter') {
              event.preventDefault();
              const item = results[activeIndex];
              if (item) goTo(item);
            } else if (event.key === 'Escape') {
              event.preventDefault();
              if (query) {
                setQuery('');
                setActiveIndex(0);
              } else {
                close();
                inputRef.current?.blur();
              }
            }
          }}
        />
        <kbd className="pointer-events-none hidden h-5 shrink-0 items-center rounded-md border border-border bg-card/80 px-1.5 font-mono text-[10px] font-medium text-muted-foreground sm:inline-flex">
          {isMac ? '⌘K' : 'Ctrl K'}
        </kbd>
      </div>

      {open && (
        <div
          id="topbar-search-results"
          className={cn(
            'absolute left-0 right-0 top-[calc(100%+6px)] z-50 overflow-hidden',
            'rounded-xl border border-border bg-card shadow-float',
          )}
        >
          <div className="max-h-[min(420px,60vh)] overflow-auto p-1.5">
            {results.length === 0 ? (
              <div className="px-3 py-8 text-center text-sm text-muted-foreground">
                {t('commandSearch.empty')}
              </div>
            ) : (
              <ul className="flex flex-col gap-0.5" role="listbox">
                {results.map((item, index) => {
                  const Icon = item.icon;
                  const active = index === activeIndex;
                  return (
                    <li key={item.id}>
                      <button
                        type="button"
                        id={`search-item-${item.id}`}
                        role="option"
                        aria-selected={active}
                        className={cn(
                          'flex w-full items-center gap-3 rounded-lg px-2.5 py-2 text-left transition-colors',
                          active
                            ? 'bg-primary/10 text-foreground'
                            : 'text-foreground hover:bg-muted/70',
                        )}
                        onMouseEnter={() => setActiveIndex(index)}
                        onMouseDown={(event) => {
                          // Prevent input blur before click navigates.
                          event.preventDefault();
                        }}
                        onClick={() => goTo(item)}
                      >
                        <span
                          className={cn(
                            'flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border',
                            active
                              ? 'border-primary/20 bg-primary/10 text-primary'
                              : 'border-border bg-muted/50 text-muted-foreground',
                          )}
                        >
                          <Icon className="h-4 w-4" />
                        </span>
                        <span className="min-w-0 flex-1">
                          <span className="block truncate text-sm font-medium">
                            {item.title}
                          </span>
                          {item.subtitle && (
                            <span className="mt-0.5 block truncate text-xs text-muted-foreground">
                              {item.subtitle}
                            </span>
                          )}
                        </span>
                        <span className="flex shrink-0 items-center gap-2">
                          <span className="rounded-md bg-muted px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
                            {kindLabel(item.kind)}
                          </span>
                          <ArrowRight
                            className={cn(
                              'h-3.5 w-3.5 text-muted-foreground',
                              active ? 'opacity-100' : 'opacity-0',
                            )}
                          />
                        </span>
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
          </div>

          <div className="flex items-center justify-between border-t border-border px-3 py-1.5 text-[11px] text-muted-foreground">
            <span>{t('commandSearch.footerHint')}</span>
            <span className="hidden sm:inline">{t('commandSearch.footerKeys')}</span>
          </div>
        </div>
      )}
    </div>
  );
}
