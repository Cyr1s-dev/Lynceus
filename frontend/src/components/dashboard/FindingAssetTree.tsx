/**
 * 跨任务资产树。
 *
 * 数据来自后端 `GET /projects/{id}/findings/tree`：节点已按 (类别, 归一化值)
 * 跨 mission 合并、带 critical/high/total 计数；这里只负责按 `parent` 组装树、
 * 渲染、关键词过滤与选中。层级推导在后端，前端不算。
 */
import { useMemo, useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Building,
  CircleDashed,
  Globe,
  LayoutTemplate,
  Link2,
  type LucideIcon,
  Network,
  RefreshCw,
  Search,
  Smartphone,
  ChevronRight,
} from 'lucide-react';
import type { FindingAssetNode } from '@/lib/types';
import { cn } from '@/lib/utils';
import { Button, Input } from '@/ui/untitled';

const KIND_ICON: Record<FindingAssetNode['kind'], LucideIcon> = {
  mission: Building,
  root_domain: Globe,
  subdomain: Globe,
  ip: Network,
  app: Smartphone,
  service: LayoutTemplate,
  endpoint: Link2,
  other: CircleDashed,
};

const KIND_LABEL: Record<FindingAssetNode['kind'], string> = {
  mission: '任务',
  root_domain: '根域名',
  subdomain: '子域名',
  ip: 'IP',
  app: '应用',
  service: '服务',
  endpoint: '接口',
  other: '其他',
};

interface TreeNode extends FindingAssetNode {
  children: TreeNode[];
  depth: number;
  display: string;
}

function shortLabel(node: FindingAssetNode, parent?: FindingAssetNode): string {
  if (!parent) return node.label;
  if (node.kind === 'subdomain' && node.label.endsWith(`.${parent.label}`)) {
    return node.label.slice(0, -(parent.label.length + 1)) || node.label;
  }
  if (node.kind !== 'service' && node.kind !== 'endpoint') return node.label;
  if (node.label.startsWith(parent.label)) {
    return node.label.slice(parent.label.length) || node.label;
  }
  if (node.kind === 'endpoint') {
    try {
      const url = new URL(node.label);
      return `${url.pathname}${url.search}` || '/';
    } catch {
      return node.label;
    }
  }
  return node.label;
}

function buildAssetTree(nodes: FindingAssetNode[]): TreeNode[] {
  const byKey = new Map<string, TreeNode>();
  for (const node of nodes) {
    byKey.set(node.key, { ...node, children: [], depth: 0, display: node.label });
  }
  const roots: TreeNode[] = [];
  for (const node of nodes) {
    const current = byKey.get(node.key);
    if (!current) continue;
    const parent = node.parent ? byKey.get(node.parent) : undefined;
    // 父节点缺失（被截断）时上提为顶层，不让子树整个消失。
    if (parent) {
      parent.children.push(current);
      current.display = shortLabel(node, parent);
    } else {
      roots.push(current);
    }
  }
  const setDepth = (node: TreeNode, depth: number) => {
    node.depth = depth;
    for (const child of node.children) setDepth(child, depth + 1);
  };
  for (const root of roots) setDepth(root, 0);
  return roots;
}

function filterTree(nodes: TreeNode[], keyword: string): TreeNode[] {
  const kw = keyword.trim().toLowerCase();
  if (!kw) return nodes;
  const walk = (node: TreeNode): TreeNode | null => {
    if (node.label.toLowerCase().includes(kw)) return node;
    const children = node.children.map(walk).filter((c): c is TreeNode => c !== null);
    if (children.length === 0) return null;
    return { ...node, children };
  };
  return nodes.map(walk).filter((n): n is TreeNode => n !== null);
}

function collectKeys(nodes: TreeNode[], out: Set<string> = new Set()): Set<string> {
  for (const node of nodes) {
    out.add(node.key);
    collectKeys(node.children, out);
  }
  return out;
}

function AssetTreeRow({
  node,
  open,
  selected,
  onToggle,
  onSelect,
}: {
  node: TreeNode;
  open: boolean;
  selected: boolean;
  onToggle: () => void;
  onSelect: () => void;
}) {
  const { t } = useTranslation();
  const Icon = KIND_ICON[node.kind] ?? Globe;
  const hasChildren = node.children.length > 0;
  return (
    <div
      className={cn(
        'group flex items-center gap-1 rounded-md pr-1 text-sm',
        selected ? 'bg-accent' : 'hover:bg-accent/50',
      )}
      style={{ paddingLeft: `${node.depth * 10}px` }}
    >
      {hasChildren ? (
        <button
          type="button"
          onClick={onToggle}
          className="flex size-5 shrink-0 items-center justify-center rounded text-muted-foreground hover:text-foreground"
          aria-label={open ? t('common.collapse') : t('common.expand')}
          aria-expanded={open}
        >
          <ChevronRight className={cn('size-3.5 transition-transform', open && 'rotate-90')} />
        </button>
      ) : (
        <span className="size-5 shrink-0" />
      )}
      <button
        type="button"
        onClick={onSelect}
        className="flex min-w-0 flex-1 items-center gap-1.5 py-1 text-left"
        title={`${KIND_LABEL[node.kind]} · ${node.label}`}
      >
        <Icon className="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
        <span className={cn('min-w-0 truncate', selected && 'font-medium')}>{node.display}</span>
      </button>
      <span className="flex shrink-0 items-center gap-1 text-xs tabular-nums">
        {node.critical > 0 && (
          <span className="text-rose-600" title={`严重 ${node.critical}`}>
            {node.critical}
          </span>
        )}
        {node.high > 0 && (
          <span className="text-red-500" title={`高危 ${node.high}`}>
            {node.high}
          </span>
        )}
        <span className="text-muted-foreground" title={`共 ${node.total} 条发现`}>
          {node.total}
        </span>
      </span>
    </div>
  );
}

export function FindingAssetTree({
  nodes,
  selected,
  onSelect,
  loading,
  findingTotal,
  onRefresh,
}: {
  nodes: FindingAssetNode[];
  selected: string | null;
  onSelect: (key: string | null) => void;
  loading?: boolean;
  findingTotal: number;
  onRefresh?: () => void;
}) {
  const { t } = useTranslation();
  const [keyword, setKeyword] = useState('');
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set());

  const roots = useMemo(() => buildAssetTree(nodes), [nodes]);
  const visible = useMemo(() => filterTree(roots, keyword), [roots, keyword]);
  const searching = keyword.trim() !== '';
  const searchKeys = useMemo(() => (searching ? collectKeys(visible) : null), [searching, visible]);

  const isExpanded = (node: TreeNode) => {
    if (searchKeys) return searchKeys.has(node.key);
    if (expanded.has(node.key)) return true;
    return node.depth === 0 && !collapsed.has(node.key);
  };
  const toggle = (node: TreeNode) => {
    const open = isExpanded(node);
    setExpanded((prev) => {
      const next = new Set(prev);
      if (open) next.delete(node.key);
      else next.add(node.key);
      return next;
    });
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (open) next.add(node.key);
      else next.delete(node.key);
      return next;
    });
  };

  let emptyHint = t('riskBoard.assetEmpty');
  if (loading) emptyHint = t('common.loading');
  else if (searching) emptyHint = t('riskBoard.assetNoMatch');

  const rows: ReactNode[] = [];
  const pushRows = (list: TreeNode[]) => {
    for (const node of list) {
      const open = isExpanded(node);
      rows.push(
        <AssetTreeRow
          key={node.key}
          node={node}
          open={open}
          selected={selected === node.key}
          onToggle={() => toggle(node)}
          onSelect={() => onSelect(selected === node.key ? null : node.key)}
        />,
      );
      if (open && node.children.length > 0) pushRows(node.children);
    }
  };
  pushRows(visible);

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2">
      <div className="flex items-center gap-1">
        <div className="relative flex-1">
          <Search className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={keyword}
            onChange={(event) => setKeyword(event.target.value)}
            placeholder={t('riskBoard.filterAsset')}
            aria-label={t('riskBoard.filterAsset')}
            className="h-9 pl-9 text-sm"
          />
        </div>
        {onRefresh && (
          <Button
            size="icon"
            variant="ghost"
            className="size-9 shrink-0 text-muted-foreground"
            onClick={onRefresh}
            disabled={loading}
            aria-label={t('riskBoard.refreshAsset')}
            title={t('riskBoard.refreshAsset')}
          >
            <RefreshCw className={cn('size-4', loading && 'animate-spin')} />
          </Button>
        )}
      </div>

      <button
        type="button"
        onClick={() => onSelect(null)}
        className={cn(
          'flex items-center justify-between gap-2 rounded-md px-2 py-1.5 text-left text-sm',
          selected === null ? 'bg-accent font-medium' : 'hover:bg-accent/50',
        )}
      >
        <span>{t('riskBoard.allAssets')}</span>
        <span className="text-xs tabular-nums text-muted-foreground">{findingTotal}</span>
      </button>

      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="flex flex-col pr-1">
          {rows}
          {rows.length === 0 && (
            <p className="px-2 py-8 text-center text-xs text-muted-foreground">{emptyHint}</p>
          )}
        </div>
      </div>
    </div>
  );
}