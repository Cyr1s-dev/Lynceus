import { useEffect, useMemo, useState } from 'react';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { BookOpen, Copy, Check, Loader2, Plus, RefreshCw, Search, SearchX } from 'lucide-react';
import InfiniteScroll from 'react-infinite-scroll-component';
import { api, getApiErrorMessage } from '@/lib/api';
import type { KnowledgeCard, KnowledgeCardKind } from '@/lib/types';
import { Button } from '@/ui/untitled';
import { Input } from '@/ui/untitled';
import { Label } from '@/components/ui/label';
import { Textarea } from '@/ui/untitled';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/ui/untitled';
import { useToast } from '@/hooks/use-toast';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/ui/untitled';
import {
  PageContainer,
  PageHeader,
  Badge,
  Card,
  DataToolbar,
  EmptyState,
  ErrorState,
  LoadingState,
} from '@/ui/untitled';
import { cn } from '@/lib/utils';

// 类型筛选按知识库实际覆盖动态生成（库里没有的 kind 不再展示空结果）。
const ALL_KINDS: KnowledgeCardKind[] = [
  'tool_usage',
  'vulnerability_pattern',
  'case_reference',
  'payload_strategy',
  'false_positive_pattern',
  'remediation_pattern',
  'cloud_attack_path',
  'binary_pattern',
];

/** Cards rendered per lazy-load batch（客户端渐进渲染，避免全量大列表卡顿）. */
const PAGE_SIZE = 60;

interface KnowledgeCardWithScore extends KnowledgeCard {
  _score?: number;
  _matched_terms?: string[];
}

function isKnowledgeCardKind(value: string): value is KnowledgeCardKind {
  return ALL_KINDS.includes(value as KnowledgeCardKind);
}


/** 来源第二段 = 导入源的大块（工具Wiki/安全Wiki/反弹Shell/内网Payload）。 */
// 文字用同色系深色阶：粉/蓝底框保持用户要的浅色身份，10px 小字也要过
// WCAG AA（深粉 5.2:1、深蓝 6.6:1，浅色原字只有 1.9:1 / 1.7:1）。
const SOURCE_BLOCKS: Record<string, { label: string; badge: string }> = {
  toolCommands: { label: '工具Wiki', badge: 'text-[#C2185B] bg-[#FF99CC]/20 border-[#FF99CC]/60' },
  webPayloads: { label: '安全Wiki', badge: 'text-[#01579B] bg-[#66CCFF]/20 border-[#66CCFF]/60' },
  reverseShell: { label: '反弹Shell', badge: 'text-rose-700 bg-rose-50 border-rose-200' },
  intranetPayloads: { label: '内网Payload', badge: 'text-emerald-700 bg-emerald-50 border-emerald-200' },
};

function sourceBlock(card: KnowledgeCardWithScore): { slug: string; label: string; badge: string } | null {
  const parts = (card.source ?? '').split(':');
  const slug = parts.length >= 2 ? parts[1] : '';
  const def = SOURCE_BLOCKS[slug];
  return def ? { slug, ...def } : null;
}

/** body "分类:" 行的细分（如有）。 */
function cardSubcategory(card: KnowledgeCardWithScore): string | null {
  for (const line of (card.body ?? '').split('\n')) {
    if (line.startsWith('分类:')) {
      const value = line.replace('分类:', '').trim();
      // 取中文段（"反弹Shell | reverse shell" → "反弹Shell"）
      return value.split('|')[0].trim() || value;
    }
  }
  return null;
}

/**
 * 从知识卡真实字段提取版式区块。只使用卡片本身有的条目：
 * summary（描述）、content（载荷/命令块）、tool / platform（适用面）、
 * tags（分类标签）。不发明严重度等不存在的字段。
 */
function parseCardSections(card: KnowledgeCardWithScore): {
  summary: string;
  payloadBlock: string;
  scope: string;
} {
  const summary = (card.summary ?? '').trim();
  const lines = (card.content ?? '')
    .split('\n')
    .map((line) => line.replace(/^>\s*/, '').trim())
    .filter((line) => line.length > 0 && line !== summary);
  const payloadBlock = lines.slice(0, 4).join('\n').slice(0, 200);
  const scopeParts = [...(card.platform ?? []), ...(card.tool ?? [])];
  return {
    summary,
    payloadBlock,
    scope: scopeParts.join(' · '),
  };
}

export function KnowledgeBasePage() {
  const { t } = useTranslation();
  const { toast } = useToast();
  const queryClient = useQueryClient();
  const [searchQuery, setSearchQuery] = useState('');
  const [selectedKind, setSelectedKind] = useState<KnowledgeCardKind | 'all'>('all');
  const [selectedBlock, setSelectedBlock] = useState<string>('all');
  const [detailCard, setDetailCard] = useState<KnowledgeCardWithScore | null>(null);
  const [visibleCount, setVisibleCount] = useState(PAGE_SIZE);
  const [copiedId, setCopiedId] = useState<string | null>(null);

  const handleCopyRule = (rule: string, id: string) => {
    navigator.clipboard.writeText(rule);
    setCopiedId(id);
    toast({ title: t('knowledgeBase.ruleCopied') });
    setTimeout(() => setCopiedId(null), 2000);
  };

  // 筛选条件变化时回到第一批。
  useEffect(() => {
    setVisibleCount(PAGE_SIZE);
  }, [searchQuery, selectedKind, selectedBlock]);

  const [isAddOpen, setIsAddOpen] = useState(false);
  const [newTitle, setNewTitle] = useState('');
  const [newKind, setNewKind] = useState<KnowledgeCardKind>('tool_usage');
  const [newContent, setNewContent] = useState('');
  const [newTags, setNewTags] = useState('');
  const [newPriority, setNewPriority] = useState('50');

  const { data: cards = [], isLoading, isError, error, refetch } = useQuery<KnowledgeCardWithScore[]>({
    queryKey: ['knowledge-cards', searchQuery, selectedKind],
    queryFn: async () => {
      if (searchQuery.trim() || selectedKind !== 'all') {
        const results = await api.searchKnowledgeCards({
          text: searchQuery.trim(),
          kinds: selectedKind !== 'all' ? [selectedKind] : undefined,
          limit: 20,
        });
        return results.map((r) => ({ ...r.card, _score: r.score, _matched_terms: r.matched_terms }));
      }
      return api.listKnowledgeCards();
    },
  });

  // 实际覆盖的 kind 及数量（独立全量查询：筛选后下拉仍显示全部类型）。
  const allCardsQuery = useQuery({
    queryKey: ['knowledge-cards-all'],
    queryFn: () => api.listKnowledgeCards(),
    staleTime: 60_000,
  });
  const kindCounts = useMemo(() => {
    const counts = new Map<KnowledgeCardKind, number>();
    for (const card of allCardsQuery.data ?? []) {
      counts.set(card.kind, (counts.get(card.kind) ?? 0) + 1);
    }
    return counts;
  }, [allCardsQuery.data]);

  const hasActiveFilters = searchQuery.trim() !== '' || selectedKind !== 'all' || selectedBlock !== 'all';
  const blockOf = (card: KnowledgeCardWithScore): string =>
    (card.source ?? '').split(':')[1] ?? '';
  const clearFilters = () => {
    setSearchQuery('');
    setSelectedKind('all');
    setSelectedBlock('all');
  };

  // Retrieval Substrate：语料/FTS 索引状态与手动重建。
  const indexStatusQuery = useQuery({
    queryKey: ['knowledge-index-status'],
    queryFn: () => api.knowledgeIndexStatus(),
  });
  const indexSyncMutation = useMutation({
    mutationFn: () => api.knowledgeIndexSync(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['knowledge-index-status'] });
      toast({ title: t('knowledgeBase.indexSynced') });
    },
    onError: (err: unknown) => {
      toast({ title: t('common.error'), description: getApiErrorMessage(err), variant: 'destructive' });
    },
  });

  const addMutation = useMutation({
    mutationFn: (input: Omit<KnowledgeCard, 'id' | 'created_at' | 'updated_at'>) => api.addKnowledgeCard(input),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['knowledge-cards'] });
      setIsAddOpen(false);
      setNewTitle('');
      setNewContent('');
      setNewTags('');
      setNewPriority('50');
      toast({ title: t('common.succeeded'), description: t('knowledgeBase.createCard') });
    },
    onError: (err: unknown) => {
      toast({ title: t('common.error'), description: getApiErrorMessage(err), variant: 'destructive' });
    },
  });

  const handleAdd = () => {
    if (!newTitle.trim() || !newContent.trim()) {
      toast({ title: t('common.error'), description: t('common.noInputProvided'), variant: 'destructive' });
      return;
    }
    const parsedPriority = parseInt(newPriority, 10);
    const priority = isNaN(parsedPriority) ? 50 : parsedPriority;
    const tags = newTags.split(',').map((s) => s.trim()).filter(Boolean);

    addMutation.mutate({
      title: newTitle.trim(),
      kind: newKind,
      content: newContent.trim(),
      tags,
      priority,
    });
  };

  return (
    <PageContainer>
      <PageHeader
        icon={<BookOpen className="h-5 w-5" />}
        title={t('knowledgeBase.title')}
        description={t('knowledgeBase.description')}
        count={cards.length}
      />

      {indexStatusQuery.data && (
        <div className="flex flex-wrap items-center gap-2 text-sm">
          <Badge
            variant={
              indexStatusQuery.data.state === 'ready'
                ? 'solid'
                : indexStatusQuery.data.state === 'stale'
                  ? 'soft'
                  : 'outline'
            }
          >
            {t(`knowledgeBase.indexState.${indexStatusQuery.data.state}`)}
          </Badge>
          <span className="text-muted-foreground">
            {t('knowledgeBase.indexCounts', {
              cards: indexStatusQuery.data.card_count,
              indexed: indexStatusQuery.data.indexed_count,
            })}
          </span>
          <Button
            variant="outline"
            size="sm"
            disabled={indexSyncMutation.isPending}
            onClick={() => indexSyncMutation.mutate()}
          >
            {t('knowledgeBase.indexSyncBtn')}
          </Button>
        </div>
      )}

      <DataToolbar
        count={selectedBlock === 'all' ? cards.length : cards.filter((c) => blockOf(c) === selectedBlock).length}
        countLabel={t('common.resultsCount', {
          count: selectedBlock === 'all' ? cards.length : cards.filter((c) => blockOf(c) === selectedBlock).length,
        })}
        filters={
          <>
            <div className="relative w-full sm:w-72">
              <Search className="pointer-events-none absolute left-2.5 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                placeholder={t('knowledgeBase.searchPlaceholder')}
                className="h-8 pl-9"
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
              />
            </div>
            <Select
              value={selectedBlock}
              onValueChange={setSelectedBlock}
            >
              <SelectTrigger className="h-8 w-[150px]">
                <SelectValue placeholder={t('common.all')} />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">{t('common.all')}</SelectItem>
                {Object.entries(SOURCE_BLOCKS).map(([slug, def]) => (
                  <SelectItem key={slug} value={slug}>
                    {def.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Select
              value={selectedKind}
              onValueChange={(val: string) =>
                setSelectedKind(val === 'all' || isKnowledgeCardKind(val) ? val : 'all')
              }
            >
              <SelectTrigger className="h-8 w-[200px]">
                <SelectValue placeholder={t('common.all')} />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">{t('common.all')}</SelectItem>
                {ALL_KINDS.filter((k) => (kindCounts.get(k) ?? 0) > 0).map((k) => (
                  <SelectItem key={k} value={k}>
                    {t(`knowledgeBase.kinds.${k}`)} ({kindCounts.get(k)})
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </>
        }
        actions={
          <Dialog open={isAddOpen} onOpenChange={setIsAddOpen}>
            <DialogTrigger asChild>
              <Button>
                <Plus className="mr-2 h-4 w-4" />
                {t('knowledgeBase.addCard')}
              </Button>
            </DialogTrigger>
            <DialogContent className="max-w-2xl">
              <DialogHeader>
                <DialogTitle>{t('knowledgeBase.createCard')}</DialogTitle>
                <DialogDescription>{t('knowledgeBase.description')}</DialogDescription>
              </DialogHeader>
              <div className="space-y-4 py-4">
                <div className="space-y-2">
                  <Label>{t('knowledgeBase.cardTitle')}</Label>
                  <Input value={newTitle} onChange={(e) => setNewTitle(e.target.value)} />
                </div>
                <div className="grid grid-cols-2 gap-4">
                  <div className="space-y-2">
                    <Label>{t('knowledgeBase.cardKind')}</Label>
                    <Select
                      value={newKind}
                      onValueChange={(val: string) => {
                        if (isKnowledgeCardKind(val)) setNewKind(val);
                      }}
                    >
                      <SelectTrigger>
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        {ALL_KINDS.map((k) => (
                          <SelectItem key={k} value={k}>
                            {t(`knowledgeBase.kinds.${k}`)}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                  <div className="space-y-2">
                    <Label>{t('knowledgeBase.cardPriority')}</Label>
                    <Input type="number" value={newPriority} onChange={(e) => setNewPriority(e.target.value)} />
                  </div>
                </div>
                <div className="space-y-2">
                  <Label>{t('knowledgeBase.cardTags')}</Label>
                  <Input
                    placeholder={t('knowledgeBase.cardTagsPlaceholder')}
                    value={newTags}
                    onChange={(e) => setNewTags(e.target.value)}
                  />
                </div>
                <div className="space-y-2">
                  <Label>{t('knowledgeBase.cardContent')}</Label>
                  <Textarea className="h-32" value={newContent} onChange={(e) => setNewContent(e.target.value)} />
                </div>
              </div>
              <DialogFooter>
                <Button variant="outline" onClick={() => setIsAddOpen(false)}>
                  {t('common.cancel')}
                </Button>
                <Button disabled={addMutation.isPending} onClick={handleAdd}>
                  {addMutation.isPending ? t('common.saving') : t('common.save')}
                </Button>
              </DialogFooter>
            </DialogContent>
          </Dialog>
        }
      />

      {isError ? (
        <ErrorState
          title={t('knowledgeBase.failedToLoad')}
          description={getApiErrorMessage(error)}
          onRetry={() => refetch()}
          retryLabel={t('common.retry')}
        />
      ) : isLoading ? (
        <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
          {Array.from({ length: 4 }).map((_, i) => (
            <LoadingState key={i} card showHeader lines={3} />
          ))}
        </div>
      ) : cards.length === 0 ? (
        hasActiveFilters ? (
          <EmptyState
            variant="card"
            icon={<SearchX className="h-5 w-5" />}
            title={t('common.noMatchingResults')}
            action={
              <Button variant="outline" size="sm" onClick={clearFilters}>
                {t('common.clearFilters')}
              </Button>
            }
          />
        ) : (
          <EmptyState
            variant="card"
            icon={<BookOpen className="h-6 w-6" />}
            title={t('knowledgeBase.emptyTitle')}
            description={t('knowledgeBase.emptyDescription')}
            action={
              <Button size="sm" onClick={() => setIsAddOpen(true)}>
                <Plus className="h-4 w-4" />
                {t('knowledgeBase.addCard')}
              </Button>
            }
            secondaryAction={
              <Button
                variant="outline"
                size="sm"
                disabled={indexSyncMutation.isPending}
                onClick={() => indexSyncMutation.mutate()}
              >
                <RefreshCw className="h-4 w-4" />
                {t('knowledgeBase.indexSyncBtn')}
              </Button>
            }
          />
        )
      ) : (
        <InfiniteScroll
          dataLength={Math.min(visibleCount, cards.length)}
          next={() => setVisibleCount((count) => count + PAGE_SIZE)}
          hasMore={visibleCount < cards.length}
          loader={
            <p className="flex items-center justify-center gap-2 py-3 text-xs text-muted-foreground">
              <Loader2 className="size-3.5 animate-spin" />
              {t('common.loading')}
            </p>
          }
          endMessage={
            <p className="py-3 text-center text-xs text-muted-foreground">
              {t('knowledgeBase.listEnd', { total: cards.length })}
            </p>
          }
        >
          <div className="grid grid-cols-1 gap-3 pt-1 md:grid-cols-2 xl:grid-cols-3">
            {cards
              .filter((card) => selectedBlock === 'all' || blockOf(card) === selectedBlock)
              .slice(0, visibleCount).map((card) => {
              const sections = parseCardSections(card);
              return (
              <Card
                key={card.id}
                role="button"
                tabIndex={0}
                onClick={() => setDetailCard(card)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault();
                    setDetailCard(card);
                  }
                }}
                className={cn(
                  'group min-w-0 cursor-pointer space-y-3 rounded-xl border border-slate-200 bg-white p-3.5 shadow-sm flex flex-col justify-between',
                  'transition-all hover:-translate-y-0.5 hover:border-[#FF99CC] hover:shadow-md',
                  'focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary',
                )}
              >
                <div className="space-y-2">
                  <div className="flex items-center justify-between">
                    <div className="flex min-w-0 items-center gap-1.5">
                      {(() => {
                        const block = sourceBlock(card);
                        return block ? (
                          <span className={cn('shrink-0 rounded border px-1.5 py-0.5 font-mono text-[10px] font-bold', block.badge)}>
                            {block.label}
                          </span>
                        ) : null;
                      })()}
                      <span className="truncate rounded border border-[#66CCFF]/60 bg-[#66CCFF]/20 px-2 py-0.5 font-mono text-[10px] font-bold text-[#01579B]">
                        {t(`knowledgeBase.kinds.${card.kind}`)}
                      </span>
                    </div>
                    {card.kind === 'tool_usage' && card.tool && card.tool.length > 0 && (
                      <span className="max-w-[30%] truncate rounded-full border border-[#FF99CC]/60 bg-[#FF99CC]/20 px-1.5 py-0.5 font-mono text-[9px] font-semibold text-[#C2185B]" title={card.tool.join(', ')}>
                        {card.tool.join(', ')}
                      </span>
                    )}
                  </div>

                  <h3 className="text-xs font-bold leading-snug text-slate-900 line-clamp-2">{card.title}</h3>
                  {cardSubcategory(card) && (
                    <p className="font-mono text-[9px] font-semibold text-[#C2185B]">
                      {cardSubcategory(card)}
                    </p>
                  )}

                  {sections.summary && (
                    <p className="font-sans text-[11px] leading-relaxed text-slate-600 line-clamp-3">{sections.summary}</p>
                  )}
                </div>

                <div className="space-y-2 border-t border-slate-100 pt-2.5 text-[11px]">
                  {sections.scope && (
                    <div>
                      <span className="block font-mono text-[9px] font-semibold uppercase text-slate-400">适用面:</span>
                      <p className="mt-0.5 text-[10px] leading-snug text-slate-800 line-clamp-2">{sections.scope}</p>
                    </div>
                  )}

                  {sections.payloadBlock && (
                    <div>
                      <div className="mb-0.5 flex items-center justify-between font-mono text-[9px] font-semibold uppercase text-slate-400">
                        <span>载荷 / 检测规则:</span>
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            handleCopyRule(sections.payloadBlock, card.id);
                          }}
                          className="flex items-center space-x-0.5 text-slate-400 hover:text-[#01579B] btn-press"
                        >
                          {copiedId === card.id ? <Check className="w-2.5 h-2.5 text-emerald-600" /> : <Copy className="w-2.5 h-2.5" />}
                          <span>复制</span>
                        </button>
                      </div>
                      <code className="block truncate rounded border border-[#66CCFF]/50 bg-[#66CCFF]/10 p-1.5 font-mono text-[10px] text-[#01579B]">
                        {sections.payloadBlock.split('\n')[0]}
                      </code>
                    </div>
                  )}

                  {card.tags && card.tags.length > 0 && (
                    <div className="flex flex-wrap gap-1">
                      {card.tags.slice(0, 4).map((tag) => (
                        <span key={tag} className="rounded bg-slate-100 px-1 py-0.5 font-mono text-[9px] text-slate-500">
                          #{tag}
                        </span>
                      ))}
                    </div>
                  )}

                  <div className="flex flex-wrap items-center gap-x-3 gap-y-1 font-mono text-[9px] text-slate-400">
                    {card._score !== undefined && <span>score {card._score.toFixed(2)}</span>}
                    <span>P{card.priority || 50}</span>
                    <span className="ml-auto text-[#C2185B] opacity-0 transition-opacity group-hover:opacity-100">
                      {t('knowledgeBase.viewDetailHint')}
                    </span>
                  </div>
                </div>
              </Card>
              );
            })}
          </div>
        </InfiniteScroll>
      )}

      {/* 知识卡详情：展示完整正文（命令 / 语法拆解 / OPSEC）与来源 */}
      <Dialog
        open={detailCard !== null}
        onOpenChange={(open) => {
          if (!open) setDetailCard(null);
        }}
      >
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle className="pr-6 text-left">{detailCard?.title}</DialogTitle>
            <DialogDescription className="text-left">
              {detailCard
                ? t(`knowledgeBase.kinds.${detailCard.kind}`) +
                  (detailCard._score !== undefined
                    ? ` · ${t('knowledgeBase.score')}: ${detailCard._score.toFixed(2)}`
                    : '')
                : ''}
            </DialogDescription>
          </DialogHeader>
          {detailCard && (
            <div className="space-y-4">
              <pre className="max-h-[50vh] overflow-y-auto whitespace-pre-wrap rounded-md border border-border bg-muted/40 p-3 font-mono text-xs leading-relaxed text-foreground [overflow-wrap:anywhere]">
                {detailCard.body?.trim() || detailCard.content}
              </pre>
              <dl className="grid grid-cols-[88px_minmax(0,1fr)] gap-x-3 gap-y-1.5 text-xs">
                <dt className="text-muted-foreground">{t('common.source')}</dt>
                <dd className="truncate font-mono text-foreground">
                  {detailCard.source_locator || detailCard.source || '—'}
                </dd>
                {detailCard.tags && detailCard.tags.length > 0 && (
                  <>
                    <dt className="text-muted-foreground">{t('knowledgeBase.cardTags')}</dt>
                    <dd className="flex flex-wrap gap-1">
                      {detailCard.tags.map((tag: string) => (
                        <Badge key={tag} tone="neutral">
                          #{tag}
                        </Badge>
                      ))}
                    </dd>
                  </>
                )}
                <dt className="text-muted-foreground">{t('knowledgeBase.cardPriority')}</dt>
                <dd className="tabular-nums text-foreground">{detailCard.priority || 50}</dd>
              </dl>
            </div>
          )}
          <DialogFooter>
            <Button variant="outline" onClick={() => setDetailCard(null)}>
              {t('common.close')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </PageContainer>
  );
}
