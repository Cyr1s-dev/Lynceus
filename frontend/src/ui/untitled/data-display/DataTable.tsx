import * as React from 'react';
import { cn } from '@/lib/utils';
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from '@/components/ui/table';

/**
 * Untitled UI DataTable — a unified table component with loading, empty,
 * and error states built in. Pages should use this instead of raw shadcn
 * Table components.
 */

export interface DataTableColumn<T> {
  key: string;
  header: React.ReactNode;
  render?: (row: T) => React.ReactNode;
  className?: string;
  width?: string;
}

export interface DataTableProps<T extends Record<string, unknown>> {
  columns: DataTableColumn<T>[];
  data: T[];
  rowKey?: string | ((row: T) => string);
  loading?: boolean;
  empty?: React.ReactNode;
  error?: React.ReactNode;
  onRowClick?: (row: T) => void;
  className?: string;
}

export function DataTable<T extends Record<string, unknown>>({
  columns,
  data,
  rowKey = 'id',
  loading = false,
  empty,
  error,
  onRowClick,
  className,
}: DataTableProps<T>) {
  const getKey = (row: T, index: number): string => {
    if (typeof rowKey === 'function') return rowKey(row);
    const val = row[rowKey];
    return val != null ? String(val) : String(index);
  };

  if (loading) {
    return (
      <div className={cn('space-y-2', className)}>
        {Array.from({ length: 5 }).map((_, i) => (
          <div key={i} className="h-10 animate-pulse rounded-md bg-muted/50" />
        ))}
      </div>
    );
  }

  if (error) {
    return (
      <div className={cn('p-6 text-center text-sm text-danger', className)}>
        {error}
      </div>
    );
  }

  if (data.length === 0) {
    return <div className={cn('p-6', className)}>{empty ?? 'No data'}</div>;
  }

  return (
    <div className={cn('w-full overflow-x-auto', className)}>
      <Table>
        <TableHeader>
          <TableRow>
            {columns.map((col) => (
              <TableHead
                key={col.key}
                className={col.className}
                style={col.width ? { width: col.width } : undefined}
              >
                {col.header}
              </TableHead>
            ))}
          </TableRow>
        </TableHeader>
        <TableBody>
          {data.map((row, index) => (
            <TableRow
              key={getKey(row, index)}
              className={onRowClick ? 'cursor-pointer' : undefined}
              onClick={onRowClick ? () => onRowClick(row) : undefined}
            >
              {columns.map((col) => (
                <TableCell key={col.key} className={col.className}>
                  {col.render
                    ? col.render(row)
                    : (row[col.key] as React.ReactNode) ?? '—'}
                </TableCell>
              ))}
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  );
}
