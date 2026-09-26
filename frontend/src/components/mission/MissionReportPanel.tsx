/**
 * Mission Report Panel — 报告页。
 *
 * 报告页向后端 `GET /reports/{task_id}` 要一份 Markdown 渲染出来，
 * 只提供「复制」。Lynceus 现在也对齐了：`GET /reports/{report_id}` 由后端
 * 从已落库实体汇总生成（`crates/api/report.rs`），本面板只是它的展示壳。
 *
 * 和以前的区别：报告不再是前端拿 `MissionCanvas` 现算的 Markdown，而是后端
 * 的单一事实源——画布、覆盖图、报告从此共用同一套口径。
 *
 * - `available_formats` 只声明后端**真的实现了**的表示形式（当前 json /
 *   markdown）；SARIF / HTML 没有生成器，面板不会假装能选；
 * - `status = not_ready` 时（report_id 解析不出实体）展示原因而不是报错——
 *   契约要求这个端点永不 404；
 * - 渲染用极简 Markdown 子集（标题 / 列表 / 表格 / 代码 / 粗体），不引
 *   Markdown 依赖——本仓没有，也不想为一个只读面板加一个。
 */
import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useQuery } from '@tanstack/react-query';
import { Check, Copy, Download, FileText, RefreshCw } from 'lucide-react';

import { Badge, Button, Card, EmptyState, ErrorState, LoadingState, Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/ui/untitled';
import { api } from '@/lib/api';
import type { Mission } from '@/lib/types';

/* ───────────────────────── Markdown 渲染 ───────────────────────── */

function renderInline(text: string, keyPrefix: string): React.ReactNode[] {
  const nodes: React.ReactNode[] = [];
  const pattern = /(\*\*[^*]+\*\*|`[^`]+`)/g;
  let lastIndex = 0;
  let match: RegExpExecArray | null;
  let index = 0;
  while ((match = pattern.exec(text)) !== null) {
    if (match.index > lastIndex) nodes.push(text.slice(lastIndex, match.index));
    const token = match[0];
    if (token.startsWith('**')) {
      nodes.push(
        <strong key={`${keyPrefix}-b-${index++}`} className="font-semibold text-foreground">
          {token.slice(2, -2)}
        </strong>,
      );
    } else {
      nodes.push(
        <code
          key={`${keyPrefix}-c-${index++}`}
          className="rounded bg-muted px-1 py-0.5 font-mono text-[11px] text-foreground"
        >
          {token.slice(1, -1)}
        </code>,
      );
    }
    lastIndex = match.index + token.length;
  }
  if (lastIndex < text.length) nodes.push(text.slice(lastIndex));
  return nodes;
}

function splitRow(line: string): string[] {
  return line
    .replace(/^\s*\|/, '')
    .replace(/\|\s*$/, '')
    .split('|')
    .map((cell) => cell.trim());
}

function MarkdownView({ text }: { text: string }) {
  const blocks = useMemo(() => {
    const lines = text.split(/\r?\n/);
    const result: React.ReactNode[] = [];
    let index = 0;
    let key = 0;

    while (index < lines.length) {
      const line = lines[index];

      if (line.startsWith('### ')) {
        result.push(
          <h4 key={`h4-${key++}`} className="pt-2 text-xs font-semibold text-foreground">
            {renderInline(line.slice(4), `h4-${key}`)}
          </h4>,
        );
        index += 1;
        continue;
      }
      if (line.startsWith('## ')) {
        result.push(
          <h3 key={`h3-${key++}`} className="pt-3 text-sm font-semibold text-foreground">
            {renderInline(line.slice(3), `h3-${key}`)}
          </h3>,
        );
        index += 1;
        continue;
      }
      if (line.startsWith('# ')) {
        result.push(
          <h2 key={`h2-${key++}`} className="text-base font-semibold text-foreground">
            {renderInline(line.slice(2), `h2-${key}`)}
          </h2>,
        );
        index += 1;
        continue;
      }
      if (line.startsWith('|') && index + 1 < lines.length && /^\|[\s:|-]+\|?$/.test(lines[index + 1])) {
        const header = splitRow(line);
        index += 2;
        const rows: string[][] = [];
        while (index < lines.length && lines[index].startsWith('|')) {
          rows.push(splitRow(lines[index]));
          index += 1;
        }
        result.push(
          <div key={`t-${key++}`} className="overflow-x-auto">
            <table className="w-full border-collapse text-xs">
              <thead>
                <tr className="border-b border-border">
                  {header.map((cell, cellIndex) => (
                    <th
                      key={cellIndex}
                      className="px-2 py-1 text-left font-medium text-muted-foreground"
                    >
                      {renderInline(cell, `th-${key}-${cellIndex}`)}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {rows.map((row, rowIndex) => (
                  <tr key={rowIndex} className="border-b border-border/60 last:border-b-0">
                    {row.map((cell, cellIndex) => (
                      <td key={cellIndex} className="px-2 py-1 text-foreground">
                        {renderInline(cell, `td-${key}-${rowIndex}-${cellIndex}`)}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>,
        );
        continue;
      }
      if (line.startsWith('```')) {
        index += 1;
        const code: string[] = [];
        while (index < lines.length && !lines[index].startsWith('```')) {
          code.push(lines[index]);
          index += 1;
        }
        index += 1;
        result.push(
          <pre
            key={`pre-${key++}`}
            className="overflow-auto rounded-md border border-border bg-background p-3 font-mono text-[11px] text-foreground"
          >
            {code.join('\n')}
          </pre>,
        );
        continue;
      }
      if (line.startsWith('> ')) {
        result.push(
          <blockquote
            key={`q-${key++}`}
            className="border-l-2 border-border pl-3 text-xs leading-relaxed text-muted-foreground"
          >
            {renderInline(line.slice(2), `q-${key}`)}
          </blockquote>,
        );
        index += 1;
        continue;
      }
      if (line.startsWith('- ')) {
        const items: string[] = [];
        while (index < lines.length && lines[index].startsWith('- ')) {
          items.push(lines[index].slice(2));
          index += 1;
        }
        result.push(
          <ul key={`l-${key++}`} className="list-disc space-y-0.5 pl-5 text-xs text-foreground">
            {items.map((item, itemIndex) => (
              <li key={itemIndex}>{renderInline(item, `li-${key}-${itemIndex}`)}</li>
            ))}
          </ul>,
        );
        continue;
      }
      if (line.trim() === '---') {
        result.push(<hr key={`r-${key++}`} className="my-3 border-border" />);
        index += 1;
        continue;
      }
      if (line.trim() === '') {
        index += 1;
        continue;
      }
      result.push(
        <p key={`p-${key++}`} className="text-xs leading-relaxed text-foreground">
          {renderInline(line, `p-${key}`)}
        </p>,
      );
      index += 1;
    }
    return result;
  }, [text]);

  return <div className="space-y-1.5">{blocks}</div>;
}

/* ───────────────────────── Panel ───────────────────────── */

type ReportFormat = 'markdown' | 'json';
type StatusFilter = 'confirmed' | 'all';

export function MissionReportPanel({ mission }: { mission: Mission }) {
  const { t } = useTranslation();
  const [format, setFormat] = useState<ReportFormat>('markdown');
  const [statusFilter, setStatusFilter] = useState<StatusFilter>('confirmed');
  const [copied, setCopied] = useState(false);

  const reportQuery = useQuery({
    queryKey: ['mission-report', mission.id, format, statusFilter],
    queryFn: () =>
      api.getReport(mission.id, {
        format,
        include_statuses: statusFilter,
      }),
  });

  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 2000);
    return () => clearTimeout(timer);
  }, [copied]);

  const envelope = reportQuery.data;
  const markdown = envelope?.markdown ?? '';
  const hasBody = format === 'markdown' ? markdown.trim().length > 0 : envelope?.payload != null;

  const copy = async () => {
    if (!markdown) return;
    try {
      await navigator.clipboard.writeText(markdown);
      setCopied(true);
    } catch {
      // 剪贴板在非安全上下文不可用——忽略，用户仍可下载。
    }
  };

  const download = () => {
    const body =
      format === 'markdown' ? markdown : JSON.stringify(envelope?.payload ?? {}, null, 2);
    const blob = new Blob([body], {
      type: format === 'markdown' ? 'text/markdown;charset=utf-8' : 'application/json',
    });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = `lynceus-report-${mission.id}.${format === 'markdown' ? 'md' : 'json'}`;
    anchor.click();
    URL.revokeObjectURL(url);
  };

  if (reportQuery.isPending) {
    return <LoadingState card lines={8} />;
  }

  if (reportQuery.isError) {
    return (
      <ErrorState
        title={t('missions.report.errorTitle')}
        description={
          reportQuery.error instanceof Error
            ? reportQuery.error.message
            : t('missions.report.errorDescription')
        }
        retryLabel={t('common.retry')}
        onRetry={() => void reportQuery.refetch()}
      />
    );
  }

  if (envelope?.status === 'not_ready') {
    return (
      <ErrorState
        title={t('missions.report.notReadyTitle')}
        description={envelope.reason ?? t('missions.report.notReadyDescription')}
        retryLabel={t('common.retry')}
        onRetry={() => void reportQuery.refetch()}
      />
    );
  }

  if (!hasBody) {
    return (
      <EmptyState
        variant="card"
        icon={<FileText className="h-6 w-6" />}
        title={t('missions.report.emptyTitle')}
        description={t('missions.report.emptyDescription')}
      />
    );
  }

  return (
    <Card className="shadow-xs">
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-4 py-3">
        <div className="flex items-center gap-2">
          <FileText className="h-4 w-4 text-muted-foreground" />
          <h2 className="text-sm font-medium text-foreground">{t('missions.report.title')}</h2>
          <Badge tone="neutral" variant="soft">
            {envelope?.payload?.title || mission.id}
          </Badge>
          {/* 只显示后端真实支持的形式——不支持的不出现在下拉里。 */}
          <Badge tone="info" variant="soft">
            {(envelope?.available_formats ?? []).join(' / ')}
          </Badge>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Select value={statusFilter} onValueChange={(value) => setStatusFilter(value as StatusFilter)}>
            <SelectTrigger className="w-36">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="confirmed">{t('missions.report.filterConfirmed')}</SelectItem>
              <SelectItem value="all">{t('missions.report.filterAll')}</SelectItem>
            </SelectContent>
          </Select>
          <Select value={format} onValueChange={(value) => setFormat(value as ReportFormat)}>
            <SelectTrigger className="w-28">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="markdown">Markdown</SelectItem>
              <SelectItem value="json">JSON</SelectItem>
            </SelectContent>
          </Select>
          <Button
            variant="ghost"
            size="icon"
            className="h-8 w-8"
            onClick={() => void reportQuery.refetch()}
            title={t('common.refresh')}
          >
            <RefreshCw className="h-3.5 w-3.5" />
          </Button>
          <Button variant="outline" size="sm" onClick={copy} disabled={format !== 'markdown'}>
            {copied ? <Check className="h-3.5 w-3.5" /> : <Copy className="h-3.5 w-3.5" />}
            {copied ? t('missions.report.copied') : t('missions.report.copy')}
          </Button>
          <Button variant="outline" size="sm" onClick={download}>
            <Download className="h-3.5 w-3.5" />
            {format === 'markdown' ? t('missions.report.download') : t('missions.report.downloadJson')}
          </Button>
        </div>
      </div>
      <div className="max-h-[68vh] overflow-auto p-4">
        {format === 'markdown' ? (
          <MarkdownView text={markdown} />
        ) : (
          <pre className="overflow-auto whitespace-pre-wrap break-words rounded-md border border-border bg-background p-3 font-mono text-[11px] text-foreground">
            {JSON.stringify(envelope?.payload ?? {}, null, 2)}
          </pre>
        )}
      </div>
    </Card>
  );
}
