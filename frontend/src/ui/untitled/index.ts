/**
 * Untitled UI Design System — Lynceus.
 *
 * This is the canonical UI layer for the Lynceus frontend. Pages should
 * import components from `@/ui/untitled` instead of directly from
 * `@/components/ui/*` (shadcn) or `@/components/design/*` (legacy).
 *
 * The directory structure:
 *   tokens.ts        — Design token system (colors, spacing, typography)
 *   primitives/      — Base components (Button, Input, Badge, etc.)
 *   layouts/         — AppShell, Sidebar, Topbar, PageContainer, etc.
 *   data-display/    — DataTable, EntityCard, MetricCard, Timeline, etc.
 *   feedback/        — EmptyState, LoadingState, ErrorState
 *   security/        — MissionStatusBadge, SeverityBadge, etc.
 *   icons/           — Centralized icon registry
 */

/* ── Tokens ── */
export {
  type StatusTone,
  type StatusToken,
  type Severity,
  type EvidenceQuality,
  getStatusToken,
  missionStatusTone,
  branchStatusTone,
  severityTone,
  findingStatusTone,
  toolStatusTone,
  engineStatusTone,
  providerReadinessTone,
  decisionGateTone,
  evidenceQualityTone,
  riskScoreTone,
  severityLabel,
  SEVERITY_ORDER,
  typography,
  typeScale,
  type TypeScaleRole,
  spacing,
  padding,
  radius,
  shadows,
  focusRing,
} from './tokens';

/* ── Primitives ── */
export {
  Button,
  type ButtonProps,
  type ButtonTone,
  buttonVariants,
  IconButton,
  type IconButtonProps,
  type IconButtonSize,
  Input,
  type InputProps,
  Textarea,
  type TextareaProps,
  Badge,
  type BadgeProps,
  type BadgeVariant,
  type BadgeSize,
  Tooltip,
  type TooltipProps,
  Tabs,
  TabsList,
  TabsTrigger,
  TabsContent,
  Dialog,
  DialogTrigger,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
  DialogClose,
  Drawer,
  type DrawerProps,
  type DrawerWidth,
  DropdownMenu,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuLabel,
  type DropdownMenuProps,
  type DropdownMenuItemProps,
  Checkbox,
  type CheckboxProps,
  Switch,
  type SwitchProps,
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
  Breadcrumbs,
  type BreadcrumbsProps,
  type BreadcrumbItem,
} from './primitives';

/* ── Layouts ── */
export {
  AppShell,
  type AppShellProps,
  Sidebar,
  Topbar,
  PageContainer,
  type PageContainerProps,
  PageHeader,
  type PageHeaderProps,
  type PageHeaderBreadcrumb,
  Section,
  type SectionProps,
  SectionHeader,
  type SectionHeaderProps,
  SplitPanel,
  type SplitPanelProps,
  DetailPanel,
  type DetailPanelProps,
  usePageWidth,
  usePageWidthMode,
  type PageWidth,
} from './layouts';

/* ── Data Display ── */
export {
  DataTable,
  type DataTableColumn,
  type DataTableProps,
  DataToolbar,
  type DataToolbarProps,
  FilterTabs,
  type FilterTabsProps,
  type FilterTabItem,
  EntityCard,
  type EntityCardProps,
  type EntityCardMetric,
  EntityList,
  type EntityListProps,
  MetricCard,
  type MetricCardProps,
  type MetricCardTrend,
  Timeline,
  type TimelineProps,
  type TimelineEvent,
  KeyValueList,
  type KeyValueListProps,
  type KeyValueItem,
  CodeBlock,
  type CodeBlockProps,
  JsonViewer,
  type JsonViewerProps,
  // Re-exported from @/components/design — thin wrapper around shadcn Card
  TableShell,
  type TableShellProps,
} from './data-display';

/* ── Feedback ── */
export {
  EmptyState,
  type EmptyStateProps,
  EmptyStateAction,
  LoadingState,
  type LoadingStateProps,
  RowSkeleton,
  type RowSkeletonProps,
  ErrorState,
  type ErrorStateProps,
} from './feedback';

/* ── Security ── */
export {
  MissionStatusBadge,
  type MissionStatusBadgeProps,
  BranchStatusBadge,
  type BranchStatusBadgeProps,
  SeverityBadge,
  type SeverityBadgeProps,
  FindingStatusBadge,
  type FindingStatusBadgeProps,
  EngineStatusBadge,
  type EngineStatusBadgeProps,
  ProviderReadinessBadge,
  type ProviderReadinessBadgeProps,
  DecisionGateStatusBadge,
  type DecisionGateStatusBadgeProps,
  ToolInvocationStatusBadge,
  type ToolInvocationStatusBadgeProps,
  EvidenceQualityIndicator,
  type EvidenceQualityIndicatorProps,
  RiskScoreIndicator,
  type RiskScoreIndicatorProps,
} from './security';

/* ── Icons ── */
export { getIcon, hasIcon, ICON_NAMES } from './icons';

/* ── Re-exports from shadcn (thin wrappers) ── */
/* Page layers should import from @/ui/untitled, not @/components/ui directly. */
export {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  CardDescription,
  CardFooter,
} from '@/components/ui/card';
export { Label } from '@/components/ui/label';
export { Progress } from '@/components/ui/progress';
export { ScrollArea } from '@/components/ui/scroll-area';
export {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  TableCaption,
} from '@/components/ui/table';
