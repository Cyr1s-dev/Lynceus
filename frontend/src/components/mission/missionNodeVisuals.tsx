/**
 * Shared visual identity for Mission graph node types, so the Tree view, DAG
 * view, status queues, and detail panel render the same icon and accent color
 * for a given node type.
 */
import type { ReactNode } from 'react';
import {
  Target,
  GitBranch,
  ListTree,
  TerminalSquare,
  Database,
  ShieldAlert,
  MessageSquarePlus,
  Zap,
  LayoutGrid,
  AlertTriangle,
  CircleHelp,
  ArrowRightCircle,
  Layers,
  FileSearch,
  FileBox,
} from 'lucide-react';
import type { MissionGraphNodeType } from '@/lib/missionGraphTypes';

interface NodeVisual {
  icon: ReactNode;
  /** Tailwind classes for a subtle accent border + background. */
  accent: string;
  /** Hex color used by the DAG view (cannot rely on Tailwind there). */
  color: string;
}

const NODE_VISUALS: Record<MissionGraphNodeType, NodeVisual> = {
  mission: { icon: <Target className="w-4 h-4 text-purple-500" />, accent: 'border-purple-200 bg-purple-50/40', color: '#a855f7' },
  phase: { icon: <Layers className="w-4 h-4 text-sky-500" />, accent: 'border-sky-200 bg-sky-50/40', color: '#0ea5e9' },
  branch: { icon: <GitBranch className="w-4 h-4 text-blue-500" />, accent: 'border-blue-200 bg-blue-50/40', color: '#3b82f6' },
  exploration_task: { icon: <ListTree className="w-4 h-4 text-indigo-500" />, accent: 'border-indigo-200 bg-indigo-50/40', color: '#6366f1' },
  tool_invocation: { icon: <TerminalSquare className="w-4 h-4 text-emerald-500" />, accent: 'border-emerald-200 bg-emerald-50/40', color: '#10b981' },
  evidence: { icon: <Database className="w-4 h-4 text-slate-500" />, accent: 'border-slate-200 bg-slate-50', color: '#64748b' },
  finding: { icon: <ShieldAlert className="w-4 h-4 text-rose-500" />, accent: 'border-rose-200 bg-rose-50/40', color: '#f43f5e' },
  directive: { icon: <MessageSquarePlus className="w-4 h-4 text-pink-500" />, accent: 'border-pink-200 bg-pink-50/40', color: '#ec4899' },
  decision_gate: { icon: <Zap className="w-4 h-4 text-amber-500" />, accent: 'border-amber-200 bg-amber-50/40', color: '#f59e0b' },
  strategy_board: { icon: <LayoutGrid className="w-4 h-4 text-teal-500" />, accent: 'border-teal-200 bg-teal-50/40', color: '#14b8a6' },
  risk: { icon: <AlertTriangle className="w-4 h-4 text-orange-500" />, accent: 'border-orange-200 bg-orange-50/40', color: '#f97316' },
  gap: { icon: <FileSearch className="w-4 h-4 text-amber-500" />, accent: 'border-amber-200 bg-amber-50/40', color: '#f59e0b' },
  question: { icon: <CircleHelp className="w-4 h-4 text-violet-500" />, accent: 'border-violet-200 bg-violet-50/40', color: '#8b5cf6' },
  follow_up: { icon: <ArrowRightCircle className="w-4 h-4 text-cyan-500" />, accent: 'border-cyan-200 bg-cyan-50/40', color: '#06b6d4' },
  asset: { icon: <FileBox className="w-4 h-4 text-indigo-500" />, accent: 'border-indigo-200 bg-indigo-50/40', color: '#6366f1' },
  aggregate: { icon: <Layers className="w-4 h-4 text-slate-400" />, accent: 'border-slate-300 bg-slate-50', color: '#94a3b8' },
};

const FALLBACK: NodeVisual = {
  icon: <FileSearch className="w-4 h-4 text-slate-500" />,
  accent: 'border-slate-200 bg-white',
  color: '#64748b',
};

export function nodeVisual(type: MissionGraphNodeType): NodeVisual {
  return NODE_VISUALS[type] ?? FALLBACK;
}

export function nodeIcon(type: MissionGraphNodeType): ReactNode {
  return nodeVisual(type).icon;
}

export function nodeAccentClass(type: MissionGraphNodeType): string {
  return nodeVisual(type).accent;
}

export function nodeColor(type: MissionGraphNodeType): string {
  return nodeVisual(type).color;
}
