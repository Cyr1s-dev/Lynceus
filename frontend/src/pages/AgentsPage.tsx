import { useMemo, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { Eye, Loader2, Plus, RotateCw, Sparkles } from 'lucide-react';
import { api, getApiErrorMessage } from '@/lib/api';
import type { AgentPreset } from '@/lib/types';
import { useQuery as useSkillsQuery } from '@tanstack/react-query';
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
  EmptyState,
  EntityCard,
  ErrorState,
  Input,
  Label,
  LoadingState,
  PageContainer,
  PageHeader,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
  Textarea,
} from '@/ui/untitled';
import { useToast } from '@/hooks/use-toast';

/**
 * /agents —— Agent 预设（模型分工提示词）管理页。
 *
 * 五个分工角色（任务执行 / 任务顾问 / Intake 分析 / 策略板维护 /
 * 元认知发散）的提示词以文件默认 + DB 可编辑副本的方式管理；worker 指令
 * 预设可在任务 config 或 Worker Runtime Profile 上绑定。
 */

/** 常见变量的预览示例值（其余变量用 <name> 占位）。 */
const VARIABLE_SAMPLES: Record<string, string> = {
  solver_name: 'web_recon',
  runtime_display_name: 'Claude Code',
  intent_title: 'Enumerate login surface',
  intent_description: 'Focus on auth bypass candidates',
  targets: 'url=https://t.example',
  mission_goal: 'Root the target',
  budget_steps: '40',
  context_hint: '任务目标：…\n目标：…\n任务状态：running',
  question: '当前分支进展如何？',
  history: '',
};

function sampleVariables(variables: string[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const name of variables) {
    out[name] = VARIABLE_SAMPLES[name] ?? `<${name}>`;
  }
  return out;
}

export function AgentsPage() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { toast } = useToast();
  const [editing, setEditing] = useState<AgentPreset | 'new' | null>(null);

  const presetsQuery = useQuery({
    queryKey: ['agent-presets'],
    queryFn: api.listAgentPresets,
  });

  const presets = useMemo(
    () => [...(presetsQuery.data ?? [])].sort((a, b) => Number(b.builtin) - Number(a.builtin) || a.key.localeCompare(b.key)),
    [presetsQuery.data],
  );

  return (
    <PageContainer>
      <PageHeader
        icon={<Sparkles className="h-5 w-5" />}
        title={t('agents.title')}
        description={t('agents.description')}
        count={presets.length}
        actions={
          <div className="flex shrink-0 gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => presetsQuery.refetch()}
              disabled={presetsQuery.isFetching}
            >
              <RotateCw className={cn('h-4 w-4', presetsQuery.isFetching && 'animate-spin')} />
              {t('common.refresh')}
            </Button>
            <Button size="sm" onClick={() => setEditing('new')}>
              <Plus className="h-4 w-4" />
              {t('agents.createButton')}
            </Button>
          </div>
        }
      />

      {presetsQuery.isError ? (
        <ErrorState
          title={t('agents.failedToLoad')}
          description={getApiErrorMessage(presetsQuery.error)}
          onRetry={() => presetsQuery.refetch()}
          retryLabel={t('common.retry')}
        />
      ) : presetsQuery.isLoading ? (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
          {Array.from({ length: 6 }).map((_, index) => (
            <LoadingState key={index} card showHeader lines={2} />
          ))}
        </div>
      ) : presets.length === 0 ? (
        <EmptyState
          icon={<Sparkles className="h-6 w-6" />}
          title={t('agents.emptyTitle')}
          description={t('agents.emptyDescription')}
        />
      ) : (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
          {presets.map((preset) => (
            <EntityCard
              key={preset.key}
              interactive
              role="button"
              tabIndex={0}
              onClick={() => setEditing(preset)}
              onKeyDown={(event) => {
                if (event.key === 'Enter' || event.key === ' ') {
                  event.preventDefault();
                  setEditing(preset);
                }
              }}
              tone={preset.enabled ? 'info' : 'neutral'}
              icon={<Sparkles className="h-4 w-4" />}
              title={preset.name}
              status={
                <span className="flex items-center gap-1.5">
                  {preset.builtin ? (
                    <Badge tone="info" variant="soft" size="sm">
                      {t('agents.builtinBadge')}
                    </Badge>
                  ) : (
                    <Badge tone="neutral" variant="soft" size="sm">
                      {t('agents.customBadge')}
                    </Badge>
                  )}
                  {!preset.enabled && (
                    <Badge tone="warning" variant="soft" size="sm">
                      {t('agents.disabledBadge')}
                    </Badge>
                  )}
                </span>
              }
              meta={
                <span className="font-mono text-[11px] text-muted-foreground">{preset.key}</span>
              }
              description={preset.description || t('agents.noDescription')}
              descriptionClassName="min-h-10"
            />
          ))}
        </div>
      )}

      {editing !== null && (
        <PresetEditorDialog
          preset={editing === 'new' ? null : editing}
          onClose={() => setEditing(null)}
          onSaved={(saved) => {
            setEditing(null);
            queryClient.invalidateQueries({ queryKey: ['agent-presets'] });
            toast({ title: t('agents.saved'), description: saved.name });
          }}
        />
      )}
    </PageContainer>
  );
}

/** 编辑器对话框：提示词（chips + 预览 + 版本）/ 配置 / Skill 可见性占位。 */
function PresetEditorDialog({
  preset,
  onClose,
  onSaved,
}: {
  preset: AgentPreset | null;
  onClose: () => void;
  onSaved: (preset: AgentPreset) => void;
}) {
  const { t } = useTranslation();
  const { toast } = useToast();
  const isNew = preset === null;
  const [tab, setTab] = useState('prompt');
  const [visibleSkills, setVisibleSkills] = useState<string[]>(preset?.skills ?? []);
  const [authorizedTools, setAuthorizedTools] = useState<string[]>(preset?.tools ?? []);
  const [key, setKey] = useState(preset?.key ?? '');
  const [name, setName] = useState(preset?.name ?? '');
  const [description, setDescription] = useState(preset?.description ?? '');
  const [template, setTemplate] = useState(preset?.instruction_template ?? '');
  const [modelAlias, setModelAlias] = useState(preset?.model_alias ?? '');
  const [maxTurns, setMaxTurns] = useState<string>(
    preset?.max_turns != null ? String(preset.max_turns) : '',
  );
  const [enabled, setEnabled] = useState(preset?.enabled ?? true);
  const [previewOpen, setPreviewOpen] = useState(false);
  const [previewText, setPreviewText] = useState('');

  const variables = useMemo(() => {
    const found: string[] = [];
    const re = /\{\{\s*([A-Za-z0-9_]+)\s*\}\}/g;
    for (const match of template.matchAll(re)) {
      if (!found.includes(match[1])) found.push(match[1]);
    }
    return found;
  }, [template]);

  const insertVariable = (name: string) => {
    setTemplate((current) => `${current}{{${name}}}`);
  };

  const saveMutation = useMutation({
    mutationFn: async () => {
      if (isNew) {
        return api.createAgentPreset({
          key: key.trim(),
          name: name.trim(),
          description: description.trim() || null,
          instruction_template: template,
          model_alias: modelAlias.trim() || null,
          max_turns: maxTurns ? Number(maxTurns) : null,
        });
      }
      return api.updateAgentPreset(preset!.key, {
        name: name.trim(),
        description: description.trim() || null,
        enabled,
        model_alias: modelAlias.trim() || null,
        max_turns: maxTurns ? Number(maxTurns) : null,
        skills: visibleSkills,
        tools: authorizedTools,
        instruction_template:
          template !== preset!.instruction_template ? template : null,
      });
    },
    onSuccess: onSaved,
    onError: (error) => {
      toast({
        title: t('agents.saveFailed'),
        description: getApiErrorMessage(error),
        variant: 'destructive',
      });
    },
  });

  const previewMutation = useMutation({
    mutationFn: async () => {
      const presetKey = isNew ? key.trim() : preset!.key;
      if (isNew) {
        // 新建预设尚未落库：前端本地渲染（与后端渲染器同语义的子集）。
        let rendered = template;
        for (const [name, value] of Object.entries(sampleVariables(variables))) {
          rendered = rendered.split(`{{${name}}}`).join(value);
          rendered = rendered.split(`{{ ${name} }}`).join(value);
        }
        return rendered;
      }
      const result = await api.previewAgentPreset(presetKey, sampleVariables(variables));
      return result.rendered;
    },
    onSuccess: (rendered) => {
      setPreviewText(rendered);
      setPreviewOpen(true);
    },
    onError: (error) => {
      toast({
        title: t('agents.previewFailed'),
        description: getApiErrorMessage(error),
        variant: 'destructive',
      });
    },
  });

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-3xl" closeLabel={t('common.close')}>
        <DialogHeader>
          <DialogTitle>{isNew ? t('agents.createTitle') : t('agents.editTitle', { name: preset!.name })}</DialogTitle>
          <DialogDescription>
            {isNew ? t('agents.createDescription') : t('agents.editDescription', { key: preset!.key })}
          </DialogDescription>
        </DialogHeader>

        <Tabs value={tab} onValueChange={setTab} className="w-full">
          <TabsList className="h-9 bg-muted/60 p-1">
            <TabsTrigger value="prompt">{t('agents.tabPrompt')}</TabsTrigger>
            <TabsTrigger value="config">{t('agents.tabConfig')}</TabsTrigger>
            <TabsTrigger value="skills">
              {t('agents.tabSkills')}
            </TabsTrigger>
            <TabsTrigger value="tools">{t('agents.tabTools')}</TabsTrigger>
          </TabsList>

          <TabsContent value="prompt" className="section-stack pt-3 outline-none">
            {isNew && (
              <div className="grid gap-2 sm:grid-cols-[1fr_1fr]">
                <div className="space-y-1.5">
                  <Label className="label-spec">{t('agents.keyLabel')}</Label>
                  <Input value={key} onChange={(event) => setKey(event.target.value)} placeholder="team_worker" className="h-9 font-mono" />
                </div>
                <div className="space-y-1.5">
                  <Label className="label-spec">{t('agents.nameLabel')}</Label>
                  <Input value={name} onChange={(event) => setName(event.target.value)} className="h-9" />
                </div>
              </div>
            )}
            {!isNew && (
              <div className="space-y-1.5">
                <Label className="label-spec">{t('agents.nameLabel')}</Label>
                <Input value={name} onChange={(event) => setName(event.target.value)} className="h-9" />
              </div>
            )}

            <div className="grid gap-2">
              <Label className="text-xs text-muted-foreground">
                {t('agents.variablesHint')}
              </Label>
              <div className="flex flex-wrap gap-2">
                {variables.map((name) => (
                  <button
                    key={name}
                    type="button"
                    onClick={() => insertVariable(name)}
                    title={t('agents.variableInsertHint')}
                    className="inline-flex items-center gap-1 rounded-md border bg-muted/40 px-2 py-1 font-mono text-xs transition-colors hover:bg-muted"
                  >
                    {`{{${name}}}`}
                  </button>
                ))}
                {variables.length === 0 && (
                  <span className="text-xs text-muted-foreground">{t('agents.noVariables')}</span>
                )}
              </div>
            </div>

            <div className="space-y-1.5">
              <Label className="label-spec">{t('agents.descriptionLabel')}</Label>
              <Input
                value={description}
                onChange={(event) => setDescription(event.target.value)}
                className="h-9"
              />
            </div>

            <Textarea
              value={template}
              onChange={(event) => setTemplate(event.target.value)}
              className="min-h-[280px] resize-y font-mono text-xs"
              placeholder={t('agents.templatePlaceholder')}
            />

            <div className="flex flex-wrap items-center gap-2">
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={previewMutation.isPending || !template.trim()}
                onClick={() => previewMutation.mutate()}
              >
                {previewMutation.isPending ? (
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                ) : (
                  <Eye className="h-3.5 w-3.5" />
                )}
                {t('agents.previewButton')}
              </Button>
            </div>
          </TabsContent>

          <TabsContent value="config" className="section-stack pt-3 outline-none">
            <div className="space-y-1.5">
              <Label className="label-spec">{t('agents.modelAliasLabel')}</Label>
              <Input
                value={modelAlias}
                onChange={(event) => setModelAlias(event.target.value)}
                placeholder={t('agents.modelAliasPlaceholder')}
                className="h-9 font-mono"
              />
              <p className="text-xs text-muted-foreground">{t('agents.modelAliasHint')}</p>
            </div>
            <div className="space-y-1.5">
              <Label className="label-spec">{t('agents.maxTurnsLabel')}</Label>
              <Input
                type="number"
                min={1}
                value={maxTurns}
                onChange={(event) => setMaxTurns(event.target.value)}
                placeholder={t('agents.maxTurnsPlaceholder')}
                className="h-9"
              />
              <p className="text-xs text-muted-foreground">{t('agents.maxTurnsHint')}</p>
            </div>
            <label className="flex cursor-pointer select-none items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={enabled}
                onChange={(event) => setEnabled(event.target.checked)}
                className="h-4 w-4 rounded border-border text-primary focus:ring-primary"
                disabled={isNew}
              />
              {t('agents.enabledLabel')}
            </label>
            {isNew && (
              <p className="text-xs text-muted-foreground">{t('agents.enabledNewHint')}</p>
            )}
          </TabsContent>

          <TabsContent value="tools" className="section-stack pt-3 outline-none">
            <ToolAuthorizationPicker
              selected={authorizedTools}
              onChange={setAuthorizedTools}
            />
            <p className="text-xs text-muted-foreground">{t('agents.toolsHint')}</p>
          </TabsContent>

          <TabsContent value="skills" className="section-stack pt-3 outline-none">
            <SkillVisibilityPicker
              selected={visibleSkills}
              onChange={setVisibleSkills}
            />
            <p className="text-xs text-muted-foreground">{t('agents.skillsVisibilityHint')}</p>
          </TabsContent>
        </Tabs>

        <DialogFooter className="gap-2 sm:gap-0">
          <Button variant="ghost" onClick={onClose}>
            {t('common.cancel')}
          </Button>
          <Button
            disabled={
              saveMutation.isPending ||
              !name.trim() ||
              !template.trim() ||
              (isNew && !key.trim())
            }
            onClick={() => saveMutation.mutate()}
          >
            {saveMutation.isPending && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
            {t('common.save')}
          </Button>
        </DialogFooter>
      </DialogContent>

      <Dialog open={previewOpen} onOpenChange={setPreviewOpen}>
        <DialogContent className="sm:max-w-2xl" closeLabel={t('common.close')}>
          <DialogHeader>
            <DialogTitle>{t('agents.previewTitle')}</DialogTitle>
            <DialogDescription>{t('agents.previewDescription')}</DialogDescription>
          </DialogHeader>
          <pre className="max-h-[60vh] overflow-auto whitespace-pre-wrap rounded-md border bg-muted/30 p-3 font-mono text-xs leading-relaxed">
            {previewText}
          </pre>
        </DialogContent>
      </Dialog>
    </Dialog>
  );
}

/** Skill 可见性选择器：预设的 worker 只能 load 勾选的 skill（空 = 全部可见）。 */
function SkillVisibilityPicker({
  selected,
  onChange,
}: {
  selected: string[];
  onChange: (skills: string[]) => void;
}) {
  const { t } = useTranslation();
  const skillsQuery = useSkillsQuery({
    queryKey: ['skills'],
    queryFn: api.listSkills,
  });
  const skills = skillsQuery.data ?? [];

  const toggle = (name: string) => {
    onChange(
      selected.includes(name)
        ? selected.filter((item) => item !== name)
        : [...selected, name],
    );
  };

  if (skillsQuery.isLoading) {
    return (
      <div className="flex items-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="h-4 w-4 animate-spin" />
        {t('agents.skillsLoading')}
      </div>
    );
  }
  if (skills.length === 0) {
    return (
      <p className="rounded-lg border border-dashed border-border bg-muted/20 p-4 text-center text-sm text-muted-foreground">
        {t('agents.skillsEmptyCatalog')}
      </p>
    );
  }
  return (
    <div className="grid gap-2 sm:grid-cols-2">
      {skills.map((skill) => (
        <label
          key={skill.name}
          className="flex cursor-pointer select-none items-start gap-2 rounded-lg border border-border bg-card p-3 text-sm transition-colors hover:bg-muted/40"
        >
          <input
            type="checkbox"
            checked={selected.includes(skill.name)}
            onChange={() => toggle(skill.name)}
            className="mt-0.5 h-4 w-4 rounded border-border text-primary focus:ring-primary"
          />
          <span className="min-w-0">
            <span className="block truncate font-mono text-xs font-medium text-foreground">
              {skill.name}
            </span>
            <span className="block truncate text-xs text-muted-foreground">
              {skill.description}
            </span>
          </span>
        </label>
      ))}
    </div>
  );
}

/** 工具授权选择器：预设的 MCP 会话只允许勾选的目录条目（空 = 不限制）。 */
function ToolAuthorizationPicker({
  selected,
  onChange,
}: {
  selected: string[];
  onChange: (tools: string[]) => void;
}) {
  const { t } = useTranslation();
  const catalogQuery = useSkillsQuery({
    queryKey: ['tool-catalog'],
    queryFn: api.getToolCatalog,
  });
  const tools = catalogQuery.data ?? [];

  const toggle = (id: string) => {
    onChange(
      selected.includes(id)
        ? selected.filter((item) => item !== id)
        : [...selected, id],
    );
  };

  if (catalogQuery.isLoading) {
    return (
      <div className="flex items-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="h-4 w-4 animate-spin" />
        {t('agents.toolsLoading')}
      </div>
    );
  }
  return (
    <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
      {tools.map((tool) => (
        <label
          key={tool.id}
          className="flex cursor-pointer select-none items-center gap-2 rounded-lg border border-border bg-card p-2.5 text-sm transition-colors hover:bg-muted/40"
        >
          <input
            type="checkbox"
            checked={selected.includes(tool.id)}
            onChange={() => toggle(tool.id)}
            className="h-4 w-4 rounded border-border text-primary focus:ring-primary"
          />
          <span className="min-w-0">
            <span className="block truncate font-mono text-xs font-medium text-foreground">
              {tool.id}
            </span>
            <span className="block truncate text-[11px] text-muted-foreground">
              {tool.enabled ? t('agents.toolsEnabled') : t('agents.toolsDisabled')}
              {' · '}
              {tool.name}
            </span>
          </span>
        </label>
      ))}
    </div>
  );
}
