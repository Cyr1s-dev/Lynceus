import { useMemo, useRef, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  Radar,
  Search,
  ChevronDown,
  ChevronRight,
  ArrowUpToLine,
  Globe,
  Link2,
  Database,
  CheckCircle2,
  XCircle,
  Loader2,
} from 'lucide-react';
import { api, getApiErrorMessage } from '@/lib/api';
import type {
  IntelConfidence,
  IntelEntityRecord,
  IntelExpansionReport,
  IntelQueryType,
  IntelRelationRecord,
  IntelSourceInfo,
} from '@/lib/types';
import {
  Badge,
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  EmptyState,
  Input,
  Label,
  PageHeader,
  Section,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from '@/ui/untitled';
import { useToast } from '@/hooks/use-toast';
import { useTranslation } from 'react-i18next';

const QUERY_TYPES: IntelQueryType[] = ['domain', 'ip', 'url', 'certificate', 'organization', 'keyword'];

const CONFIDENCE_TONE: Record<IntelConfidence, 'success' | 'info' | 'warning' | 'neutral'> = {
  confirmed: 'success',
  high: 'info',
  medium: 'warning',
  weak: 'neutral',
};

export function IntelligenceHubPage() {
  const { t } = useTranslation();
  const { toast } = useToast();
  const queryClient = useQueryClient();
  const seedInputRef = useRef<HTMLInputElement>(null);

  const [seed, setSeed] = useState('');
  const [queryType, setQueryType] = useState<IntelQueryType>('domain');
  const [enabledSources, setEnabledSources] = useState<Set<string>>(new Set());
  const [report, setReport] = useState<IntelExpansionReport | null>(null);
  const [querying, setQuerying] = useState(false);
  const [expandedEntities, setExpandedEntities] = useState<Set<string>>(new Set());
  const [promoteTarget, setPromoteTarget] = useState<IntelEntityRecord | null>(null);

  const { data: sources = [] } = useQuery({
    queryKey: ['intel-sources'],
    queryFn: api.getIntelSources,
  });

  const applicableSources = useMemo(
    () => sources.filter((source) => source.capabilities.query_types.includes(queryType)),
    [sources, queryType],
  );

  const { data: missions = [] } = useQuery({
    queryKey: ['missions'],
    queryFn: () => api.getMissions(),
  });

  const { data: storedEntities = [] } = useQuery({
    queryKey: ['intel-entities'],
    queryFn: () => api.getIntelEntities(),
  });

  const { data: storedRelations = [] } = useQuery({
    queryKey: ['intel-relations'],
    queryFn: () => api.getIntelRelations(),
  });

  const queryMutation = useMutation({
    mutationFn: () => {
      const sourceIds = [...enabledSources].filter((id) =>
        applicableSources.some((source) => source.id === id),
      );
      return api.runIntelQuery({
        seed: seed.trim(),
        query_type: queryType,
        source_ids: sourceIds,
      });
    },
    onMutate: () => {
      setQuerying(true);
    },
    onSuccess: (nextReport) => {
      setReport(nextReport);
      setQuerying(false);
      if (nextReport.partial) {
        toast({
          title: t('intelligence.partialTitle'),
          description: t('intelligence.partialDescription', {
            failed: nextReport.failed_sources.join(', '),
          }),
          variant: 'destructive',
        });
      }
      void queryClient.invalidateQueries({ queryKey: ['intel-entities'] });
      void queryClient.invalidateQueries({ queryKey: ['intel-relations'] });
    },
    onError: (error: unknown) => {
      setQuerying(false);
      toast({
        title: t('intelligence.queryFailed'),
        description: getApiErrorMessage(error),
        variant: 'destructive',
      });
    },
  });

  const promoteMutation = useMutation({
    mutationFn: ({ entityId, missionId }: { entityId: string; missionId: string }) =>
      api.promoteIntelEntity(entityId, missionId),
    onSuccess: (outcome) => {
      setPromoteTarget(null);
      toast({
        title: t('intelligence.promoteSuccess'),
        description: outcome.asset.value,
      });
      setReport((current) =>
        current
          ? {
              ...current,
              entities: current.entities.map((entity) =>
                entity.id === outcome.entity.id ? { ...entity, ...outcome.entity } : entity,
              ),
            }
          : current,
      );
    },
    onError: (error: unknown) => {
      toast({
        title: t('intelligence.promoteFailed'),
        description: getApiErrorMessage(error),
        variant: 'destructive',
      });
    },
  });

  const handleSearch = () => {
    if (!seed.trim()) return;
    if (applicableSources.length === 0) {
      toast({
        title: t('intelligence.noApplicableSource'),
        description: t('intelligence.noApplicableSourceDescription'),
        variant: 'destructive',
      });
      return;
    }
    queryMutation.mutate();
  };

  const toggleSource = (id: string) => {
    setEnabledSources((current) => {
      const next = new Set(current);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  };

  const toggleEntityExpanded = (id: string) => {
    setExpandedEntities((current) => {
      const next = new Set(current);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  };

  const entities = report?.entities ?? storedEntities;
  const relations = report?.relations ?? storedRelations;

  const entityById = useMemo(() => {
    const map = new Map<string, IntelEntityRecord>();
    for (const entity of entities) map.set(entity.id, entity);
    return map;
  }, [entities]);

  const hasSearched = report !== null || querying;
  const hasResults = entities.length > 0 || relations.length > 0;

  return (
    <div className="page-stack">
      <PageHeader
        icon={<Radar className="h-5 w-5" />}
        title={t('intelligence.title')}
        description={t('intelligence.description')}
      />

      <Section title={t('intelligence.searchTitle')} flush>
        <div className="grid grid-cols-1 gap-3 p-4 lg:grid-cols-[1fr_180px_auto] lg:items-end">
          <div className="space-y-2">
            <Label>{t('intelligence.seed')}</Label>
            <Input
              ref={seedInputRef}
              value={seed}
              onChange={(event) => setSeed(event.target.value)}
              placeholder={t('intelligence.seedPlaceholder')}
              onKeyDown={(event) => {
                if (event.key === 'Enter') handleSearch();
              }}
            />
          </div>
          <div className="space-y-2">
            <Label>{t('intelligence.queryType')}</Label>
            <Select value={queryType} onValueChange={(value) => setQueryType(value as IntelQueryType)}>
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {QUERY_TYPES.map((type) => (
                  <SelectItem key={type} value={type}>
                    {t(`intelligence.queryTypes.${type}`)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <Button onClick={handleSearch} disabled={querying || !seed.trim()}>
            <Search className="mr-2 h-4 w-4" />
            {querying ? t('intelligence.searching') : t('intelligence.search')}
          </Button>
        </div>
        <div className="flex flex-wrap items-center gap-2 border-t border-border px-4 py-3">
          <span className="label-spec text-muted-foreground">{t('intelligence.sourcesLabel')}</span>
          {applicableSources.length === 0 ? (
            <span className="text-xs text-muted-foreground">{t('intelligence.noSourceForType')}</span>
          ) : (
            applicableSources.map((source) => (
              <SourceChip
                key={source.id}
                source={source}
                enabled={!enabledSources.size || enabledSources.has(source.id)}
                onToggle={() => toggleSource(source.id)}
              />
            ))
          )}
        </div>
        {hasSearched && (
          <div className="flex flex-wrap items-center gap-2 border-t border-border px-4 py-3">
            {querying && (
              <span className="flex items-center gap-2 text-xs text-muted-foreground">
                <Loader2 className="h-3 w-3 animate-spin" />
                {t('intelligence.queryingSources')}
              </span>
            )}
            {(report?.source_results ?? []).map((result) => {
              const failed = result.errors.length > 0;
              return (
                <span
                  key={result.source_id}
                  className="flex items-center gap-1.5 rounded-full border border-border bg-muted/40 px-2.5 py-1 text-xs"
                >
                  {failed ? (
                    <XCircle className="h-3 w-3 text-red-600" />
                  ) : (
                    <CheckCircle2 className="h-3 w-3 text-emerald-600" />
                  )}
                  {result.source_id}
                  <span className="font-mono text-muted-foreground">{result.record_count}</span>
                </span>
              );
            })}
            {report && (
              <span className="ml-auto flex items-center gap-2">
                <Badge tone="neutral">
                  {t('intelligence.entityCount', { count: report.entities.length })}
                </Badge>
                <Badge tone="neutral">
                  {t('intelligence.relationCount', { count: report.relations.length })}
                </Badge>
              </span>
            )}
          </div>
        )}
      </Section>

      {!hasSearched && !hasResults ? (
        <Section flush>
          <div className="p-8">
            <EmptyState
              variant="bare"
              icon={<Radar className="h-10 w-10" />}
              title={t('intelligence.emptyTitle')}
              description={t('intelligence.emptyDescription')}
              action={
                <Button onClick={() => seedInputRef.current?.focus()}>
                  {t('intelligence.search')}
                </Button>
              }
            />
          </div>
        </Section>
      ) : !querying ? (
        <Section flush>
          <Tabs defaultValue="entities">
            <div className="border-b border-border px-4 pt-3">
              <TabsList>
                <TabsTrigger value="entities">
                  {t('intelligence.tabEntities', { count: entities.length })}
                </TabsTrigger>
                <TabsTrigger value="relations">
                  {t('intelligence.tabRelations', { count: relations.length })}
                </TabsTrigger>
                <TabsTrigger value="sources">
                  {t('intelligence.tabSources', { count: report?.source_results.length ?? sources.length })}
                </TabsTrigger>
              </TabsList>
            </div>
            <TabsContent value="entities" className="section-stack p-4">
              {entities.length === 0 ? (
                <EmptyState
                  variant="bare"
                  title={t('intelligence.noEntities')}
                  description={t('intelligence.noEntitiesDescription')}
                />
              ) : (
                entities.map((entity) => (
                  <EntityCard
                    key={entity.id}
                    entity={entity}
                    expanded={expandedEntities.has(entity.id)}
                    onToggle={() => toggleEntityExpanded(entity.id)}
                    onPromote={() => setPromoteTarget(entity)}
                  />
                ))
              )}
            </TabsContent>
            <TabsContent value="relations" className="section-stack p-4">
              {relations.length === 0 ? (
                <EmptyState
                  variant="bare"
                  title={t('intelligence.noRelations')}
                  description={t('intelligence.noRelationsDescription')}
                />
              ) : (
                relations.map((relation) => (
                  <RelationCard key={relation.id} relation={relation} entityById={entityById} />
                ))
              )}
            </TabsContent>
            <TabsContent value="sources" className="section-stack p-4">
              {(report?.source_results ?? []).map((result) => (
                <div
                  key={result.source_id}
                  className="flex flex-wrap items-center gap-3 rounded-lg border border-border bg-card p-4"
                >
                  <Database className="h-4 w-4 text-primary" />
                  <span className="font-medium">{result.source_id}</span>
                  {result.errors.length > 0 ? (
                    <Badge tone="danger" dot>
                      {result.errors[0]}
                    </Badge>
                  ) : (
                    <Badge tone="success" dot>
                      {t('intelligence.sourceOk', { count: result.record_count })}
                    </Badge>
                  )}
                  <span className="ml-auto font-mono text-xs text-muted-foreground">
                    {new Date(result.fetched_at).toLocaleTimeString()}
                  </span>
                </div>
              ))}
              {!report && sources.map((source) => (
                <div
                  key={source.id}
                  className="flex flex-wrap items-center gap-3 rounded-lg border border-border bg-card p-4"
                >
                  <Database className="h-4 w-4 text-primary" />
                  <span className="font-medium">{source.display_name}</span>
                  <Badge tone="success" dot>{t('intelligence.sourceReady')}</Badge>
                  <span className="ml-auto font-mono text-xs text-muted-foreground">{source.id}</span>
                </div>
              ))}
              {(report?.warnings.length ?? 0) > 0 && (
                <div className="rounded-lg border border-border bg-muted/40 p-3 text-xs text-muted-foreground">
                  {report?.warnings.map((warning) => (
                    <p key={warning}>{warning}</p>
                  ))}
                </div>
              )}
            </TabsContent>
          </Tabs>
        </Section>
      ) : null}

      <Dialog open={promoteTarget !== null} onOpenChange={(open) => !open && setPromoteTarget(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('intelligence.promoteTitle')}</DialogTitle>
            <DialogDescription>
              {t('intelligence.promoteDescription', { value: promoteTarget?.value ?? '' })}
            </DialogDescription>
          </DialogHeader>
          {missions.length === 0 ? (
            <p className="text-sm text-muted-foreground">{t('intelligence.promoteNoMission')}</p>
          ) : (
            <div className="space-y-2">
              <Label>{t('intelligence.promoteMission')}</Label>
              {missions.map((mission) => (
                <Button
                  key={mission.id}
                  variant="outline"
                  className="w-full justify-between"
                  disabled={promoteMutation.isPending}
                  onClick={() =>
                    promoteMutation.mutate({
                      entityId: promoteTarget?.id ?? '',
                      missionId: mission.id,
                    })
                  }
                >
                  <span className="truncate">{mission.title || mission.id}</span>
                  <ArrowUpToLine className="h-4 w-4 shrink-0" />
                </Button>
              ))}
            </div>
          )}
          <DialogFooter>
            <Button variant="ghost" onClick={() => setPromoteTarget(null)}>
              {t('common.cancel')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

function SourceChip({
  source,
  enabled,
  onToggle,
}: {
  source: IntelSourceInfo;
  enabled: boolean;
  onToggle: () => void;
}) {
  const { t } = useTranslation();
  return (
    <button
      type="button"
      onClick={onToggle}
      className={`rounded-full border px-2.5 py-1 text-xs transition-colors ${
        enabled
          ? 'border-primary/40 bg-primary/10 text-foreground'
          : 'border-border bg-transparent text-muted-foreground'
      }`}
    >
      {source.display_name}
      <span className="ml-1.5 font-mono text-[10px] text-muted-foreground">
        {t('intelligence.sourceReady')}
      </span>
    </button>
  );
}

function EntityCard({
  entity,
  expanded,
  onToggle,
  onPromote,
}: {
  entity: IntelEntityRecord;
  expanded: boolean;
  onToggle: () => void;
  onPromote: () => void;
}) {
  const { t } = useTranslation();
  const promotable = ['domain', 'ip', 'url', 'service', 'repository'].includes(entity.kind);
  return (
    <div className="rounded-lg border border-border bg-card p-4">
      <div className="flex flex-wrap items-center gap-2">
        <button type="button" onClick={onToggle} className="text-muted-foreground hover:text-foreground">
          {expanded ? <ChevronDown className="h-4 w-4" /> : <ChevronRight className="h-4 w-4" />}
        </button>
        <Badge tone="neutral">{t(`intelligence.kinds.${entity.kind}`, entity.kind)}</Badge>
        <span className="font-mono text-sm font-medium">{entity.value}</span>
        <Badge tone={CONFIDENCE_TONE[entity.confidence]} dot>
          {t(`intelligence.confidence.${entity.confidence}`)}
        </Badge>
        {entity.status === 'promoted' && <Badge tone="success">{t('intelligence.promoted')}</Badge>}
        <Badge tone="info">{t('intelligence.sourceCount', { count: entity.source_count })}</Badge>
        <div className="ml-auto flex items-center gap-2">
          {promotable && entity.status !== 'promoted' && (
            <Button variant="outline" size="sm" onClick={onPromote}>
              <ArrowUpToLine className="mr-1.5 h-3.5 w-3.5" />
              {t('intelligence.promote')}
            </Button>
          )}
        </div>
      </div>
      <div className="mt-2 flex flex-wrap gap-4 pl-8 text-xs text-muted-foreground">
        <span>
          {t('intelligence.firstSeen')}: {new Date(entity.first_seen).toLocaleString()}
        </span>
        <span>
          {t('intelligence.lastSeen')}: {new Date(entity.last_seen).toLocaleString()}
        </span>
        <span>
          {t('intelligence.hitSources')}: {entity.hit_sources.join(', ') || '—'}
        </span>
      </div>
      {expanded && (
        <div className="mt-3 space-y-1.5 rounded-md border border-border bg-muted/40 p-3">
          <p className="label-spec text-muted-foreground">{t('intelligence.provenance')}</p>
          {entity.provenance.map((prov) => (
            <div key={prov.raw_record_id} className="flex flex-wrap items-center gap-2 text-xs">
              <Globe className="h-3 w-3 shrink-0 text-primary" />
              <Badge tone="neutral">{prov.source}</Badge>
              <span className="truncate font-mono text-muted-foreground">
                {prov.source_record_id || prov.raw_record_id}
              </span>
              <span className="ml-auto font-mono text-[10px] text-muted-foreground">
                {new Date(prov.fetched_at).toLocaleString()}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function RelationCard({
  relation,
  entityById,
}: {
  relation: IntelRelationRecord;
  entityById: Map<string, IntelEntityRecord>;
}) {
  const { t } = useTranslation();
  const from = entityById.get(relation.from_entity_id);
  const to = entityById.get(relation.to_entity_id);
  return (
    <details className="rounded-lg border border-border bg-card p-4">
      <summary className="flex cursor-pointer list-none flex-wrap items-center gap-2">
        <Badge tone="neutral">{t(`intelligence.kinds.${from?.kind ?? ''}`, from?.kind ?? '')}</Badge>
        <span className="font-mono text-sm">{from?.value ?? relation.from_entity_id}</span>
        <span className="flex items-center gap-1 text-xs text-primary">
          <Link2 className="h-3 w-3" />
          {t(`intelligence.relations.${relation.relation}`, relation.relation)}
        </span>
        <Badge tone="neutral">{t(`intelligence.kinds.${to?.kind ?? ''}`, to?.kind ?? '')}</Badge>
        <span className="font-mono text-sm">{to?.value ?? relation.to_entity_id}</span>
        <Badge tone={CONFIDENCE_TONE[relation.confidence]} dot>
          {t(`intelligence.confidence.${relation.confidence}`)}
        </Badge>
        <Badge tone="info">{t('intelligence.sourceCount', { count: relation.source_count })}</Badge>
        <span className="ml-auto text-xs text-muted-foreground">{relation.hit_sources.join(', ')}</span>
      </summary>
      <div className="mt-3 space-y-1.5 rounded-md border border-border bg-muted/40 p-3">
        <p className="label-spec text-muted-foreground">{t('intelligence.provenance')}</p>
        {relation.provenance.map((provenance) => (
          <div key={provenance.raw_record_id} className="flex flex-wrap items-center gap-2 text-xs">
            <Globe className="h-3 w-3 shrink-0 text-primary" />
            <Badge tone="neutral">{provenance.source}</Badge>
            <span className="truncate font-mono text-muted-foreground">
              {provenance.source_record_id || provenance.raw_record_id}
            </span>
            <span className="ml-auto font-mono text-[10px] text-muted-foreground">
              {new Date(provenance.fetched_at).toLocaleString()}
            </span>
          </div>
        ))}
      </div>
    </details>
  );
}
