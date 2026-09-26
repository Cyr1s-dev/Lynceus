import * as React from 'react';
import { CodeBlock } from './CodeBlock';

/**
 * Untitled UI JsonViewer — a JSON display component. Renders JSON data
 * with indentation in a CodeBlock.
 */
export interface JsonViewerProps {
  data: unknown;
  maxHeight?: string;
  showHeader?: boolean;
  className?: string;
}

export function JsonViewer({
  data,
  maxHeight = '400px',
  showHeader = true,
  className,
}: JsonViewerProps) {
  const json = React.useMemo(() => {
    try {
      return JSON.stringify(data, null, 2);
    } catch {
      return String(data);
    }
  }, [data]);

  return (
    <CodeBlock
      language="json"
      maxHeight={maxHeight}
      showHeader={showHeader}
      className={className}
    >
      {json}
    </CodeBlock>
  );
}
