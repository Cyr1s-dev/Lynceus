/**
 * Untitled UI-like Design Token System for Lynceus.
 *
 * This is the TypeScript-level token layer that complements the CSS custom
 * properties in `src/index.css` and the Tailwind config in `tailwind.config.js`.
 *
 * Pages and components import from here — never hand-write status colors,
 * severity colors, or spacing values.
 */

/* ───────────────────────── Status Tones ───────────────────────── */

export type StatusTone = 'success' | 'info' | 'warning' | 'danger' | 'neutral';

export interface StatusToken {
  /** Soft badge background + text + border classes */
  badge: string;
  /** Solid dot / icon color class */
  dot: string;
  /** Ring color for focus emphasis */
  ring: string;
  /** Progress bar / indicator fill */
  bar: string;
  /** Soft background tint for rows/cards */
  tint: string;
}

const TOKENS: Record<StatusTone, StatusToken> = {
  success: {
    badge: 'bg-success-soft text-success-soft-foreground border-success-border',
    dot: 'text-success',
    ring: 'ring-success/40',
    bar: 'bg-success',
    tint: 'bg-success-soft/50',
  },
  info: {
    badge: 'bg-info-soft text-info-soft-foreground border-info-border',
    dot: 'text-info',
    ring: 'ring-info/40',
    bar: 'bg-info',
    tint: 'bg-info-soft/50',
  },
  warning: {
    badge: 'bg-warning-soft text-warning-soft-foreground border-warning-border',
    dot: 'text-warning',
    ring: 'ring-warning/40',
    bar: 'bg-warning',
    tint: 'bg-warning-soft/50',
  },
  danger: {
    badge: 'bg-danger-soft text-danger-soft-foreground border-danger-border',
    dot: 'text-danger',
    ring: 'ring-danger/40',
    bar: 'bg-danger',
    tint: 'bg-danger-soft/50',
  },
  neutral: {
    badge: 'bg-neutral-soft text-neutral-soft-foreground border-neutral-border',
    dot: 'text-neutral',
    ring: 'ring-neutral/40',
    bar: 'bg-neutral',
    tint: 'bg-neutral-soft/50',
  },
};

export function getStatusToken(tone: StatusTone): StatusToken {
  return TOKENS[tone];
}

/* ───────────────────────── Tone Mappers ───────────────────────── */

/** Map a mission / run / branch status string to a semantic tone. */
export function missionStatusTone(status?: string | null): StatusTone {
  switch (status) {
    case 'running':
    case 'active':
      return 'info';
    case 'completed':
    case 'succeeded':
    case 'applied':
      return 'success';
    case 'paused':
    case 'waiting_for_decision':
    case 'blocked':
    case 'needs_review':
    case 'reviewing':
    case 'reporting':
      return 'warning';
    case 'failed':
    case 'cancelled':
    case 'error':
    case 'denied':
    case 'timeout':
      return 'danger';
    default:
      return 'neutral';
  }
}

/** Map a branch status string to a semantic tone. */
export function branchStatusTone(status?: string | null): StatusTone {
  switch (status) {
    case 'active':
    case 'running':
    case 'exploring':
      return 'info';
    case 'completed':
    case 'succeeded':
    case 'merged':
      return 'success';
    case 'paused':
    case 'waiting':
    case 'waiting_for_decision':
      return 'warning';
    case 'failed':
    case 'aborted':
    case 'cancelled':
    case 'dead_end':
      return 'danger';
    default:
      return 'neutral';
  }
}

/** Map a finding severity string to a semantic tone. */
export function severityTone(severity?: string | null): StatusTone {
  switch ((severity || '').toLowerCase()) {
    case 'critical':
    case 'high':
      return 'danger';
    case 'medium':
      return 'warning';
    case 'low':
    case 'info':
      return 'info';
    default:
      return 'neutral';
  }
}

/** Map a finding status string to a semantic tone. */
export function findingStatusTone(status?: string | null): StatusTone {
  switch ((status || '').toLowerCase()) {
    case 'confirmed':
    case 'fixed':
    case 'resolved':
    case 'closed':
      return 'success';
    case 'open':
    case 'new':
    case 'triaging':
      return 'warning';
    case 'false_positive':
    case 'ignored':
    case 'wont_fix':
      return 'neutral';
    case 'exploitable':
    case 'reopened':
      return 'danger';
    default:
      return 'neutral';
  }
}

/** Map a tool invocation status string to a semantic tone. */
export function toolStatusTone(status?: string | null): StatusTone {
  switch ((status || '').toLowerCase()) {
    case 'ok':
    case 'success':
    case 'succeeded':
      return 'success';
    case 'error':
    case 'failed':
    case 'timeout':
    case 'denied':
      return 'danger';
    case 'running':
    case 'pending':
      return 'info';
    default:
      return 'neutral';
  }
}

/** Map an engine / module status to a semantic tone. */
export function engineStatusTone(status?: string | null): StatusTone {
  switch ((status || '').toLowerCase()) {
    case 'installed':
    case 'ready':
    case 'healthy':
    case 'connected':
      return 'success';
    case 'configured':
    case 'detected':
    case 'available':
      return 'info';
    case 'needs_config':
    case 'missing_path':
    case 'missing':
    case 'planned':
      return 'warning';
    case 'disabled':
      return 'neutral';
    case 'error':
    case 'failed':
    case 'unreachable':
    case 'not_found':
      return 'danger';
    default:
      return 'neutral';
  }
}

/** Map a provider readiness level to a semantic tone. */
export function providerReadinessTone(
  usableForTextGeneration: boolean,
  configComplete: boolean,
  tested?: boolean | null,
): StatusTone {
  if (usableForTextGeneration && tested !== false) return 'success';
  if (configComplete && tested === false) return 'warning';
  if (configComplete) return 'info';
  return 'neutral';
}

/** Map a decision gate status to a semantic tone. */
export function decisionGateTone(status?: string | null): StatusTone {
  switch (status) {
    case 'approved':
    case 'allowed':
    case 'proceed':
      return 'success';
    case 'pending':
    case 'waiting':
      return 'warning';
    case 'denied':
    case 'rejected':
    case 'blocked':
      return 'danger';
    case 'expired':
    case 'cancelled':
      return 'neutral';
    default:
      return 'warning';
  }
}

/* ───────────────────────── Typography ───────────────────────── */

export const typography = {
  fontSize: {
    xs: 'text-[11px]',
    sm: 'text-xs',
    base: 'text-sm',
    lg: 'text-base',
    xl: 'text-lg',
    '2xl': 'text-xl',
    '3xl': 'text-2xl',
  },
  fontWeight: {
    normal: 'font-normal',
    medium: 'font-medium',
    semibold: 'font-semibold',
    bold: 'font-bold',
  },
  lineHeight: {
    tight: 'leading-tight',
    normal: 'leading-normal',
    relaxed: 'leading-relaxed',
    none: 'leading-none',
  },
  tracking: {
    tight: 'tracking-tight',
    normal: 'tracking-normal',
    wide: 'tracking-wide',
    wider: 'tracking-wider',
  },
} as const;

/**
 * Semantic typography scale — the ONLY text sizes pages should use.
 * Arbitrary px font sizes (text-[10px] / text-[13px] / …) are banned in pages;
 * compose from these roles instead.
 */
export const typeScale = {
  /** Page title: 20px / 28px semibold */
  pageTitle: 'text-xl leading-7 font-semibold tracking-tight',
  /** Dense page title (sub-pages / detail headers): 18px / 28px semibold */
  pageTitleDense: 'text-lg leading-7 font-semibold tracking-tight',
  /** Section title: 15px / 22px semibold */
  sectionTitle: 'text-[15px] leading-[22px] font-semibold tracking-tight',
  /** Body copy: 14px / 20px */
  body: 'text-sm leading-5',
  /** List item / table primary text: 13px / 20px */
  listItem: 'text-[13px] leading-5',
  /** Metadata / timestamps / secondary info: 12px / 16px */
  metadata: 'text-xs leading-4',
  /** Code / IDs / hashes: 12px monospace */
  code: 'font-mono text-xs leading-5',
} as const;

export type TypeScaleRole = keyof typeof typeScale;

/* ───────────────────────── Spacing & Layout ───────────────────────── */

export const spacing = {
  0: '0',
  1: 'gap-1',
  2: 'gap-2',
  3: 'gap-3',
  4: 'gap-4',
  5: 'gap-5',
  6: 'gap-6',
  8: 'gap-8',
  10: 'gap-10',
  12: 'gap-12',
} as const;

export const padding = {
  0: 'p-0',
  1: 'p-1',
  2: 'p-2',
  3: 'p-3',
  4: 'p-4',
  5: 'p-5',
  6: 'p-6',
  8: 'p-8',
} as const;

/* ───────────────────────── Radius ───────────────────────── */

export const radius = {
  none: 'rounded-none',
  sm: 'rounded-sm',
  md: 'rounded-md',
  lg: 'rounded-lg',
  xl: 'rounded-xl',
  full: 'rounded-full',
} as const;

/* ───────────────────────── Shadow ───────────────────────── */

export const shadows = {
  none: 'shadow-none',
  xs: 'shadow-xs',
  card: 'shadow-card',
  elev: 'shadow-elev',
  float: 'shadow-float',
} as const;

/* ───────────────────────── Focus Ring ───────────────────────── */

export const focusRing = {
  default:
    'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/15 focus-visible:ring-offset-2 focus-visible:ring-offset-background',
  inset:
    'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/15 focus-visible:border-primary/40',
} as const;

/* ───────────────────────── Severity Scale ───────────────────────── */

export type Severity = 'critical' | 'high' | 'medium' | 'low' | 'info';

export const SEVERITY_ORDER: Severity[] = ['critical', 'high', 'medium', 'low', 'info'];

export function severityLabel(severity: Severity): string {
  return severity.charAt(0).toUpperCase() + severity.slice(1);
}

/* ───────────────────────── Evidence Quality ───────────────────────── */

export type EvidenceQuality = 'verified' | 'strong' | 'moderate' | 'weak' | 'unverified';

export function evidenceQualityTone(quality?: string | null): StatusTone {
  switch ((quality || '').toLowerCase()) {
    case 'verified':
    case 'confirmed':
      return 'success';
    case 'strong':
      return 'info';
    case 'moderate':
      return 'warning';
    case 'weak':
    case 'unverified':
    case 'tentative':
      return 'neutral';
    default:
      return 'neutral';
  }
}

/* ───────────────────────── Risk Score ───────────────────────── */

export function riskScoreTone(score?: number | null): StatusTone {
  if (score == null) return 'neutral';
  if (score >= 8) return 'danger';
  if (score >= 5) return 'warning';
  if (score >= 1) return 'info';
  return 'neutral';
}
