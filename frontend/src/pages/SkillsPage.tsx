import { useMemo, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import {
  AlertTriangle,
  FileText,
  Loader2,
  Plus,
  Puzzle,
  RotateCw,
  SearchX,
  Trash2,
  Upload,
} from 'lucide-react';
import { formatDistanceToNow } from 'date-fns';
import { api, getApiErrorMessage } from '@/lib/api';
import type { SkillUsageRow } from '@/lib/types';
import { cn } from '@/lib/utils';
import {
  Badge,
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Drawer,
  EmptyState,
  EntityCard,
  ErrorState,
  Input,
  Label,
  LoadingState,
  PageContainer,
  PageHeader,
  Textarea,
} from '@/ui/untitled';
import { useToast } from '@/hooks/use-toast';

/**
 * /skills —— Skill 板块（WP6）。
 *
 * 顶部缺口清单（模型点名但不存在的 skill，按被点名次数排序）+ skill 目录
 * 列表 + 详情编辑器（手册编辑 / 附属文件树 / 调用台账 / 关联模块）。
 * worker 侧经 MCP `skill_list` / `skill_load` 使用；load 只解锁 frontmatter
 * `modules:` 声明的目录条目（fail-closed）。
 */

export function SkillsPage() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { toast } = useToast();
  const [detailName, setDetailName] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);

  const skillsQuery = useQuery({ queryKey: ['skills'], queryFn: api.listSkills });
  const missingQuery = useQuery({
    queryKey: ['skills-missing'],
    queryFn: api.getSkillMissing,
    refetchInterval: 30_000,
  });

  const deleteMutation = useMutation({
    mutationFn: (name: string) => api.deleteSkill(name),
    onSuccess: (_result, name) => {
      queryClient.invalidateQueries({ queryKey: ['skills'] });
      queryClient.invalidateQueries({ queryKey: ['skills-missing'] });
      toast({ title: t('skills.deleted'), description: name });
    },
    onError: (error) => {
      toast({
        title: t('skills.deleteFailed'),
        description: getApiErrorMessage(error),
        variant: 'destructive',
      });
    },
  });

  const skills = skillsQuery.data ?? [];
  const missing = missingQuery.data ?? [];

  return (
    <PageContainer>
      <PageHeader
        icon={<Puzzle className="h-5 w-5" />}
        title={t('skills.title')}
        description={t('skills.description')}
        count={skills.length}
        actions={
          <div className="flex shrink-0 gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                void skillsQuery.refetch();
                void missingQuery.refetch();
              }}
              disabled={skillsQuery.isFetching}
            >
              <RotateCw className={cn('h-4 w-4', skillsQuery.isFetching && 'animate-spin')} />
              {t('common.refresh')}
            </Button>
            <Button size="sm" onClick={() => setCreating(true)}>
              <Plus className="h-4 w-4" />
              {t('skills.createButton')}
            </Button>
          </div>
        }
      />

      {/* 缺口清单 */}
      {missingQuery.isError ? null : missingQuery.isLoading ? null : missing.length > 0 ? (
        <section className="rounded-xl border border-warning-border bg-warning-soft/60 p-4">
          <h2 className="flex items-center gap-1.5 text-sm font-bold text-warning-foreground">
            <AlertTriangle className="h-4 w-4" />
            {t('skills.missingTitle', { count: missing.length })}
          </h2>
          <p className="mt-1 text-xs text-warning-foreground/80">{t('skills.missingHint')}</p>
          <div className="mt-3 flex flex-wrap gap-2">
            {missing.slice(0, 8).map((entry) => (
              <span
                key={entry.skill}
                className="inline-flex items-center gap-1.5 rounded-md border border-warning-border bg-card px-2 py-1 font-mono text-xs text-foreground"
              >
                {entry.skill}
                <Badge tone="warning" size="sm">
                  ×{entry.misses}
                </Badge>
              </span>
            ))}
          </div>
        </section>
      ) : null}

      {skillsQuery.isError ? (
        <ErrorState
          title={t('skills.failedToLoad')}
          description={getApiErrorMessage(skillsQuery.error)}
          onRetry={() => skillsQuery.refetch()}
          retryLabel={t('common.retry')}
        />
      ) : skillsQuery.isLoading ? (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
          {Array.from({ length: 3 }).map((_, index) => (
            <LoadingState key={index} card showHeader lines={2} />
          ))}
        </div>
      ) : skills.length === 0 ? (
        <EmptyState
          icon={<SearchX className="h-6 w-6" />}
          title={t('skills.emptyTitle')}
          description={t('skills.emptyDescription')}
          action={
            <Button size="sm" onClick={() => setCreating(true)}>
              <Plus className="h-4 w-4" />
              {t('skills.createButton')}
            </Button>
          }
        />
      ) : (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
          {skills.map((skill) => (
            <div key={skill.name} className="relative">
              <EntityCard
                interactive
                role="button"
                tabIndex={0}
                onClick={() => setDetailName(skill.name)}
                onKeyDown={(event) => {
                  if (event.key === 'Enter' || event.key === ' ') {
                    event.preventDefault();
                    setDetailName(skill.name);
                  }
                }}
                tone="info"
                icon={<FileText className="h-4 w-4" />}
                title={skill.name}
                description={skill.description}
                descriptionClassName="min-h-10"
                meta={
                  <span className="flex flex-wrap items-center gap-1.5">
                    {skill.modules.length > 0 ? (
                      skill.modules.map((moduleId) => (
                        <Badge key={moduleId} tone="neutral" variant="outline">
                          {moduleId}
                        </Badge>
                      ))
                    ) : (
                      <Badge tone="neutral" variant="outline">
                        {t('skills.noModules')}
                      </Badge>
                    )}
                  </span>
                }
                actions={
                  <Button
                    type="button"
                    variant="outline"
                    size="icon"
                    className="h-8 w-8 rounded-lg border-border bg-background text-muted-foreground shadow-xs transition-colors duration-150 hover:border-danger/40 hover:bg-danger-soft hover:text-danger"
                    onClick={(event) => {
                      event.stopPropagation();
                      deleteMutation.mutate(skill.name);
                    }}
                    onPointerDown={(event) => event.stopPropagation()}
                    aria-label={t('skills.deleteButton')}
                    title={t('skills.deleteButton')}
                  >
                    <Trash2 className="h-4 w-4" />
                  </Button>
                }
              />
            </div>
          ))}
        </div>
      )}

      {creating && (
        <CreateSkillDialog
          onClose={() => setCreating(false)}
          onCreated={(name) => {
            setCreating(false);
            queryClient.invalidateQueries({ queryKey: ['skills'] });
            toast({ title: t('skills.created'), description: name });
          }}
        />
      )}

      {detailName !== null && (
        <SkillDetailDrawer name={detailName} onClose={() => setDetailName(null)} />
      )}
    </PageContainer>
  );
}

/** 新建 skill 对话框。 */
function CreateSkillDialog({
  onClose,
  onCreated,
}: {
  onClose: () => void;
  onCreated: (name: string) => void;
}) {
  const { t } = useTranslation();
  const { toast } = useToast();
  const [name, setName] = useState('');
  const [description, setDescription] = useState('');
  const [modules, setModules] = useState('');
  const [body, setBody] = useState('');

  const createMutation = useMutation({
    mutationFn: () =>
      api.createSkill({
        name: name.trim(),
        description: description.trim(),
        modules: modules
          .split(',')
          .map((item) => item.trim())
          .filter(Boolean),
        body,
      }),
    onSuccess: (result) => onCreated(result.created),
    onError: (error) => {
      toast({
        title: t('skills.createFailed'),
        description: getApiErrorMessage(error),
        variant: 'destructive',
      });
    },
  });

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-2xl" closeLabel={t('common.close')}>
        <DialogHeader>
          <DialogTitle>{t('skills.createTitle')}</DialogTitle>
          <DialogDescription>{t('skills.createDescription')}</DialogDescription>
        </DialogHeader>
        <div className="section-stack pt-1">
          <div className="grid gap-3 sm:grid-cols-2">
            <div className="space-y-1.5">
              <Label className="label-spec">{t('skills.nameLabel')}</Label>
              <Input
                value={name}
                onChange={(event) => setName(event.target.value)}
                placeholder="api-recon"
                className="h-9 font-mono"
              />
            </div>
            <div className="space-y-1.5">
              <Label className="label-spec">{t('skills.modulesLabel')}</Label>
              <Input
                value={modules}
                onChange={(event) => setModules(event.target.value)}
                placeholder="nuclei, httpx"
                className="h-9 font-mono"
              />
              <p className="text-xs text-muted-foreground">{t('skills.modulesHint')}</p>
            </div>
          </div>
          <div className="space-y-1.5">
            <Label className="label-spec">{t('skills.descriptionLabel')}</Label>
            <Input
              value={description}
              onChange={(event) => setDescription(event.target.value)}
              className="h-9"
            />
          </div>
          <div className="space-y-1.5">
            <Label className="label-spec">{t('skills.bodyLabel')}</Label>
            <Textarea
              value={body}
              onChange={(event) => setBody(event.target.value)}
              className="min-h-[160px] resize-y font-mono text-xs"
              placeholder={t('skills.bodyPlaceholder')}
            />
          </div>
        </div>
        <DialogFooter className="gap-2 sm:gap-0">
          <Button variant="ghost" onClick={onClose}>
            {t('common.cancel')}
          </Button>
          <Button
            disabled={
              createMutation.isPending || !name.trim() || !description.trim()
            }
            onClick={() => createMutation.mutate()}
          >
            {createMutation.isPending && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
            {t('common.save')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** 详情抽屉：手册编辑 + 附属文件 + 调用台账 + zip 上传。 */
function SkillDetailDrawer({
  name,
  onClose,
}: {
  name: string;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { toast } = useToast();
  const [activeFile, setActiveFile] = useState('SKILL.md');
  const [draft, setDraft] = useState<string | null>(null);
  const [dirtyPath, setDirtyPath] = useState<string | null>(null);

  const detailQuery = useQuery({
    queryKey: ['skill', name],
    queryFn: () => api.getSkill(name),
  });
  const filesQuery = useQuery({
    queryKey: ['skill-files', name],
    queryFn: () => api.listSkillFiles(name),
  });
  const usageQuery = useQuery({
    queryKey: ['skill-usage', name],
    queryFn: () => api.getSkillUsage(name),
  });

  const fileContentQuery = useQuery({
    queryKey: ['skill-file', name, activeFile],
    queryFn: () => api.readSkillFile(name, activeFile),
  });

  const openFile = (path: string) => {
    setActiveFile(path);
    setDraft(null);
    setDirtyPath(null);
  };

  const writeMutation = useMutation({
    mutationFn: async (content: string) => api.writeSkillFile(name, activeFile, content),
    onSuccess: () => {
      setDraft(null);
      setDirtyPath(null);
      queryClient.invalidateQueries({ queryKey: ['skill-file', name, activeFile] });
      queryClient.invalidateQueries({ queryKey: ['skill', name] });
      toast({ title: t('skills.fileSaved'), description: activeFile });
    },
    onError: (error) => {
      toast({
        title: t('skills.fileSaveFailed'),
        description: getApiErrorMessage(error),
        variant: 'destructive',
      });
    },
  });

  const uploadMutation = useMutation({
    mutationFn: (file: File) => api.uploadSkillZip(file),
    onError: (error) => {
      toast({
        title: t('skills.uploadFailed'),
        description: getApiErrorMessage(error),
        variant: 'destructive',
      });
    },
  });

  const manual = draft ?? fileContentQuery.data?.content ?? '';
  const usageRows: SkillUsageRow[] = usageQuery.data ?? [];
  const recentHits = useMemo(
    () => usageRows.filter((row) => row.found).slice(0, 8),
    [usageRows],
  );

  return (
    <Drawer
      open
      onOpenChange={(open) => !open && onClose()}
      icon={<FileText className="h-4 w-4" />}
      title={`skills/${name}`}
      description={t('skills.detailDescription')}
      width="lg"
    >
      <div className="section-stack">
        <div className="flex flex-wrap items-center gap-2">
          <label className="inline-flex cursor-pointer items-center gap-1.5 rounded-md border border-border bg-card px-2.5 py-1.5 text-xs font-medium text-foreground shadow-xs transition-colors hover:bg-muted/50">
            {uploadMutation.isPending ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
            ) : (
              <Upload className="h-3.5 w-3.5" />
            )}
            {t('skills.uploadButton')}
            <input
              type="file"
              accept=".zip"
              className="hidden"
              onChange={(event) => {
                const file = event.target.files?.[0];
                if (file) {
                  uploadMutation.mutate(file);
                  event.target.value = '';
                }
              }}
            />
          </label>
          {detailQuery.data?.meta.modules.map((moduleId) => (
            <Badge key={moduleId} tone="info" variant="soft" size="sm">
              {moduleId}
            </Badge>
          ))}
        </div>

        {/* 文件树 */}
        <div className="flex flex-wrap gap-1.5">
          {(filesQuery.data?.files ?? ['SKILL.md']).map((file) => (
            <button
              key={file}
              type="button"
              onClick={() => openFile(file)}
              className={cn(
                'rounded-md border px-2 py-1 font-mono text-[11px] transition-colors',
                activeFile === file
                  ? 'border-primary/40 bg-primary/10 text-foreground'
                  : 'border-border bg-card text-muted-foreground hover:text-foreground',
              )}
            >
              {file}
            </button>
          ))}
        </div>

        {/* 文件编辑器 */}
        <div className="space-y-2">
          <div className="flex items-center justify-between gap-2">
            <span className="font-mono text-xs text-muted-foreground">{activeFile}</span>
            <div className="flex items-center gap-2">
              {dirtyPath === activeFile && (
                <Badge tone="warning" size="sm">
                  {t('skills.unsavedChanges')}
                </Badge>
              )}
              <Button
                size="sm"
                variant="outline"
                disabled={draft === null || writeMutation.isPending}
                onClick={() => draft !== null && writeMutation.mutate(draft)}
              >
                {writeMutation.isPending ? (
                  <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" />
                ) : null}
                {t('common.save')}
              </Button>
            </div>
          </div>
          {fileContentQuery.isLoading ? (
            <LoadingState lines={8} />
          ) : fileContentQuery.isError ? (
            <ErrorState
              title={t('skills.fileLoadFailed')}
              description={getApiErrorMessage(fileContentQuery.error)}
              onRetry={() => fileContentQuery.refetch()}
              retryLabel={t('common.retry')}
            />
          ) : (
            <Textarea
              value={manual}
              onChange={(event) => {
                setDraft(event.target.value);
                setDirtyPath(activeFile);
              }}
              className="min-h-[320px] resize-y font-mono text-xs leading-relaxed"
              spellCheck={false}
            />
          )}
        </div>

        {/* 调用台账 */}
        <div className="overflow-hidden rounded-lg border border-border">
          <div className="border-b border-border bg-muted/30 px-3 py-2 text-xs font-semibold text-foreground">
            {t('skills.usageTitle')}
          </div>
          {recentHits.length === 0 ? (
            <p className="px-3 py-4 text-xs text-muted-foreground">{t('skills.usageEmpty')}</p>
          ) : (
            <div className="divide-y divide-border">
              {recentHits.map((row) => (
                <div key={`${row.ts}-${row.skill}`} className="flex items-center justify-between gap-3 px-3 py-2 text-xs">
                  <span className="text-muted-foreground">
                    {row.mission_id ? `mission ${row.mission_id.slice(0, 12)}` : t('skills.usageNoMission')}
                  </span>
                  <span className="font-mono text-[11px] text-muted-foreground">
                    {formatDistanceToNow(new Date(row.ts), { addSuffix: true })}
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </Drawer>
  );
}
