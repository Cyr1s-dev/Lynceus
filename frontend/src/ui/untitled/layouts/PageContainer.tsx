import * as React from 'react';
import { cn } from '@/lib/utils';

/**
 * Untitled UI PageContainer — standard page wrapper providing consistent
 * max-width and vertical spacing. Replaces ad-hoc page wrapper divs.
 *
 * Lynceus 是宽幅安全控制台：默认档直接吃到 1920。旧的 1440 阅读宽度在 2232px
 * 宽的屏幕上两侧各空 200+ px，画布、表格、看板类页面尤其浪费。只有明确需要
 * 窄阅读宽度（长表单/设置项）时才用 `narrow`，画布工作区用 `full`。
 */
export interface PageContainerProps extends React.HTMLAttributes<HTMLDivElement> {
  /** Content max width. */
  maxWidth?: 'default' | 'narrow' | 'full';
  /** Vertical spacing between sections. */
  gap?: 'sm' | 'md' | 'lg';
}

const MAX_WIDTH_MAP = {
  default: 'max-w-[1920px]',
  narrow: 'max-w-[1200px]',
  full: 'max-w-full',
};

const GAP_MAP = {
  sm: 'gap-4',
  md: 'gap-6',
  lg: 'gap-8',
};

export function PageContainer({
  maxWidth = 'default',
  gap = 'md',
  className,
  children,
  ...props
}: PageContainerProps) {
  return (
    <div
      className={cn(
        'mx-auto h-full',
        MAX_WIDTH_MAP[maxWidth],
        GAP_MAP[gap],
        'flex flex-col',
        className,
      )}
      {...props}
    >
      {children}
    </div>
  );
}
