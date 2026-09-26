import {
  Crosshair,
  Boxes,
  Cable,
  ShieldAlert,
  FileSearch,
  TerminalSquare,
  GitBranch,
  FileText,
  BookOpen,
  Settings,
  Search,
  Bell,
  Shield,
  AlertTriangle,
  XCircle,
  CheckCircle,
  Info,
  type LucideIcon,
} from 'lucide-react';

/**
 * Icon Registry — a centralized mapping of semantic icon names to
 * lucide-react components. Use `getIcon(name)` to resolve an icon
 * by its semantic key, ensuring consistent iconography across the app.
 */

const REGISTRY: Record<string, LucideIcon> = {
  mission: Crosshair,
  asset: Boxes,
  engine: Cable,
  finding: ShieldAlert,
  evidence: FileSearch,
  tool: TerminalSquare,
  decision: GitBranch,
  branch: GitBranch,
  report: FileText,
  knowledge: BookOpen,
  settings: Settings,
  search: Search,
  bell: Bell,
  shield: Shield,
  warning: AlertTriangle,
  error: XCircle,
  success: CheckCircle,
  info: Info,
};

export function getIcon(name: string): LucideIcon {
  return REGISTRY[name] ?? Shield;
}

export function hasIcon(name: string): boolean {
  return name in REGISTRY;
}

export const ICON_NAMES = Object.keys(REGISTRY);
