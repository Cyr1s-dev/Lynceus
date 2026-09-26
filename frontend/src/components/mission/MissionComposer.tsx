import { useState, useRef, useEffect, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import {
  Paperclip,
  ArrowUp,
  Loader2,
  X,
  FileText,
  CheckCircle2,
  AlertCircle,
  RotateCw,
} from 'lucide-react';
import { cn } from '@/lib/utils';
import type { ApprovalMode } from '@/lib/types';
import { Button } from '@/ui/untitled';
import { ApprovalModeSelector } from '@/components/mission/ApprovalModeSelector';
import { ModelSelector } from '@/components/mission/ModelSelector';

/* ───────────────────────── Types ───────────────────────── */

/**
 * A file queued for upload as part of a mission intake.
 *
 * The Composer is a pure UI component: it only renders state and emits
 * callbacks. The actual upload lifecycle is owned by the parent page.
 */
export interface PendingFile {
  file: File;
  status: 'pending' | 'uploading' | 'uploaded' | 'failed';
  artifactId?: string;
  detectedType?: string;
  error?: string;
}

export interface MissionComposerProps {
  /** Current natural-language task text. */
  value: string;
  onChange: (value: string) => void;
  /** Files queued for upload, owned by the parent. */
  pendingFiles: PendingFile[];
  /** Called when files are added (click, drag-drop, or paste). */
  onFilesSelected: (files: FileList | File[]) => void;
  /** Remove a queued file by index. */
  onRemoveFile: (index: number) => void;
  /** Retry an upload for a failed file by index. */
  onRetryFile?: (index: number) => void;
  /** Submit the mission (parent decides provider gating / upload / creation). */
  onSubmit: () => void;
  /** True while the mission is being created. */
  isSubmitting?: boolean;
  /** Fully disable the composer. */
  disabled?: boolean;
  /** Placeholder for the text area. */
  placeholder?: string;
  approvalMode?: ApprovalMode;
  onApprovalModeChange?: (mode: ApprovalMode) => void;
}

/* ───────────────────────── Helpers ───────────────────────── */

function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

const MAX_TEXTAREA_HEIGHT = 160; // px — keeps the capsule under ~180px

/* ───────────────────────── Component ───────────────────────── */

/**
 * MissionComposer — a modern, conversational AI input for creating missions.
 *
 * Visual reference: ChatGPT / Claude / Linear / Untitled UI capsule input.
 *
 * Capabilities:
 *  - Natural-language task input with auto-expanding height.
 *  - Click-to-attach, drag-and-drop, and clipboard-paste file upload.
 *  - Pending-file chips with upload status, retry, and remove.
 *  - Enter to submit, Shift+Enter for newline, IME-composition safe.
 *
 * The component is pure UI: it never calls APIs directly. All side effects
 * are delegated to the parent via callbacks.
 */
export function MissionComposer({
  value,
  onChange,
  pendingFiles,
  onFilesSelected,
  onRemoveFile,
  onRetryFile,
  onSubmit,
  isSubmitting = false,
  disabled = false,
  placeholder,
  approvalMode,
  onApprovalModeChange,
}: MissionComposerProps) {
  const { t } = useTranslation();

  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const isComposingRef = useRef(false);

  const [isDragOver, setIsDragOver] = useState(false);

  /* ── Auto-resize textarea ── */
  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, MAX_TEXTAREA_HEIGHT)}px`;
  }, [value]);

  /* ── Derived submit state ── */
  const hasUploading = pendingFiles.some((f) => f.status === 'uploading');
  const canSubmit =
    (value.trim().length > 0 || pendingFiles.length > 0) &&
    !isSubmitting &&
    !hasUploading &&
    !disabled;

  /* ── File input ── */
  const openFilePicker = useCallback(() => {
    fileInputRef.current?.click();
  }, []);

  const handleFileInputChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    if (e.target.files) {
      onFilesSelected(e.target.files);
    }
    // Reset so selecting the same file again re-triggers change.
    e.target.value = '';
  };

  /* ── Drag & drop ── */
  const handleDragOver = (e: React.DragEvent) => {
    if (disabled) return;
    e.preventDefault();
    e.stopPropagation();
    if (!isDragOver) setIsDragOver(true);
  };

  const handleDragLeave = (e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    // Only clear when leaving the container itself, not a child element.
    if (e.currentTarget === e.target) {
      setIsDragOver(false);
    }
  };

  const handleDrop = (e: React.DragEvent) => {
    if (disabled) return;
    e.preventDefault();
    e.stopPropagation();
    setIsDragOver(false);
    if (e.dataTransfer.files && e.dataTransfer.files.length > 0) {
      onFilesSelected(e.dataTransfer.files);
    }
  };

  /* ── Paste ── */
  const handlePaste = (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
    const files = e.clipboardData?.files;
    if (files && files.length > 0) {
      onFilesSelected(files);
      // Don't prevent default — let text paste through normally.
    }
  };

  /* ── Keyboard ── */
  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      // Respect IME composition (Chinese/Japanese/Korean input).
      const composing =
        isComposingRef.current || (e.nativeEvent as KeyboardEvent).isComposing;
      if (composing) return;
      e.preventDefault();
      if (canSubmit) onSubmit();
    }
  };

  const handleCompositionStart = () => {
    isComposingRef.current = true;
  };

  const handleCompositionEnd = () => {
    isComposingRef.current = false;
  };

  /* ── Chip status label ── */
  const statusLabel = (status: PendingFile['status']) => {
    switch (status) {
      case 'pending':
        return t('missionComposer.fileQueued');
      case 'uploading':
        return t('missionComposer.fileUploading');
      case 'uploaded':
        return t('missionComposer.fileUploaded');
      case 'failed':
        return t('missionComposer.fileFailed');
    }
  };

  return (
    <div
      onDragOver={handleDragOver}
      onDragLeave={handleDragLeave}
      onDrop={handleDrop}
      className={cn(
        'group flex w-full flex-col rounded-2xl border bg-card shadow-card transition-all duration-150',
        'focus-within:border-primary/30 focus-within:shadow-elev focus-within:ring-2 focus-within:ring-primary/10',
        isDragOver
          ? 'border-primary ring-2 ring-primary/20'
          : 'border-border hover:border-border-strong',
        disabled && 'pointer-events-none opacity-60',
      )}
    >
      {/* Pending file chips (inside capsule, top) */}
      {pendingFiles.length > 0 && (
        <div className="flex max-h-[120px] flex-wrap gap-1.5 overflow-y-auto px-4 pt-3.5">
          {pendingFiles.map((pf, index) => (
            <div
              key={`${pf.file.name}-${index}`}
              className="flex items-center gap-1.5 rounded-lg border border-border bg-muted/40 px-2 py-1 text-xs"
            >
              <FileText className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
              <div className="flex min-w-0 flex-col">
                <span className="max-w-[220px] truncate font-medium text-foreground">
                  {pf.file.name}
                </span>
                <span className="text-[10px] text-muted-foreground">
                  {formatFileSize(pf.file.size)}
                  {pf.detectedType
                    ? ` · ${t('uploads.detectedType.' + pf.detectedType, {
                        defaultValue: pf.detectedType,
                      })}`
                    : ''}
                  {' · '}
                  {statusLabel(pf.status)}
                </span>
              </div>

              {pf.status === 'uploading' && (
                <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin text-primary" />
              )}
              {pf.status === 'uploaded' && (
                <CheckCircle2 className="h-3.5 w-3.5 shrink-0 text-success" />
              )}
              {pf.status === 'failed' && onRetryFile && (
                <button
                  type="button"
                  onClick={() => onRetryFile(index)}
                  className="shrink-0 text-destructive transition-colors hover:text-destructive/80"
                  aria-label={t('missionComposer.retryFile')}
                  title={t('missionComposer.retryFile')}
                >
                  <RotateCw className="h-3.5 w-3.5" />
                </button>
              )}
              {pf.status === 'failed' && !onRetryFile && (
                <AlertCircle className="h-3.5 w-3.5 shrink-0 text-destructive" />
              )}

              <button
                type="button"
                onClick={() => onRemoveFile(index)}
                className="shrink-0 text-muted-foreground transition-colors hover:text-foreground"
                aria-label={t('missionComposer.removeFile')}
                title={t('missionComposer.removeFile')}
              >
                <X className="h-3.5 w-3.5" />
              </button>
            </div>
          ))}
        </div>
      )}

      {/* Input row */}
      <div className="flex min-h-[56px] items-end gap-2.5 px-3.5 py-3">
        {/* Attach button */}
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className="h-10 w-10 shrink-0 rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"
          onClick={openFilePicker}
          disabled={disabled}
          aria-label={t('missionComposer.attach')}
          title={t('missionComposer.attach')}
        >
          <Paperclip />
        </Button>

        {/* Textarea */}
        <textarea
          ref={textareaRef}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onPaste={handlePaste}
          onKeyDown={handleKeyDown}
          onCompositionStart={handleCompositionStart}
          onCompositionEnd={handleCompositionEnd}
          rows={1}
          placeholder={placeholder ?? t('missionComposer.placeholder')}
          disabled={disabled}
          className={cn(
            'flex-1 resize-none border-0 bg-transparent px-1 py-2 text-sm leading-6 text-foreground',
            'max-h-[160px] min-h-[40px] outline-none ring-0',
            'placeholder:text-muted-foreground focus:outline-none focus:ring-0',
          )}
        />

        {/* Send button */}
        <Button
          type="button"
          tone="primary"
          size="icon"
          className="h-10 w-10 shrink-0 rounded-full"
          onClick={onSubmit}
          disabled={!canSubmit}
          aria-label={t('missionComposer.send')}
          title={t('missionComposer.send')}
        >
          {isSubmitting ? (
            <Loader2 className="animate-spin" />
          ) : (
            <ArrowUp />
          )}
        </Button>

        {/* Hidden file input — multiple, NO accept attribute */}
        <input
          ref={fileInputRef}
          type="file"
          multiple
          className="hidden"
          onChange={handleFileInputChange}
        />
      </div>

      {approvalMode && onApprovalModeChange && (
        <div className="flex items-center justify-between gap-2 border-t border-border/70 px-4 py-2.5">
          <ApprovalModeSelector
            value={approvalMode}
            onChange={onApprovalModeChange}
            disabled={disabled}
          />
          <ModelSelector />
        </div>
      )}
    </div>
  );
}
