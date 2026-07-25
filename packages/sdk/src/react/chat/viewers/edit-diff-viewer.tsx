/**
 * EditDiffViewer Component
 *
 * Displays unified diff for Edit tool operations.
 */

import React, { useMemo, memo } from 'react';
import { Edit3 } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { chatCardVariants, chatCardHeaderVariants } from '../variants';
import { syntaxColors } from '../theme';
import { useIsDark } from '../utils/helpers';

interface DiffLine {
  type: 'context' | 'removed' | 'added';
  content: string;
  oldLineNum?: number;
  newLineNum?: number;
}

interface EditDiffViewerProps {
  input: Record<string, unknown>;
}

function computeUnifiedDiff(oldStr: string, newStr: string): { lines: DiffLine[]; added: number; removed: number } {
  const oldLines = oldStr.split('\n');
  const newLines = newStr.split('\n');
  const m = oldLines.length;
  const n = newLines.length;

  const C: number[][] = Array(m + 1)
    .fill(0)
    .map(() => Array(n + 1).fill(0));

  for (let i = 1; i <= m; i++) {
    for (let j = 1; j <= n; j++) {
      if (oldLines[i - 1] === newLines[j - 1]) {
        C[i][j] = C[i - 1][j - 1] + 1;
      } else {
        C[i][j] = Math.max(C[i][j - 1], C[i - 1][j]);
      }
    }
  }

  const lines: DiffLine[] = [];
  let i = m;
  let j = n;
  let added = 0;
  let removed = 0;

  while (i > 0 || j > 0) {
    if (i > 0 && j > 0 && oldLines[i - 1] === newLines[j - 1]) {
      lines.unshift({ type: 'context', content: oldLines[i - 1], oldLineNum: i, newLineNum: j });
      i--;
      j--;
    } else if (j > 0 && (i === 0 || C[i][j - 1] >= C[i - 1][j])) {
      lines.unshift({ type: 'added', content: newLines[j - 1], newLineNum: j });
      added++;
      j--;
    } else {
      lines.unshift({ type: 'removed', content: oldLines[i - 1], oldLineNum: i });
      removed++;
      i--;
    }
  }

  return { lines, added, removed };
}

export const EditDiffViewer = memo(function EditDiffViewer({ input }: EditDiffViewerProps) {
  const isDarkMode = useIsDark();
  const filePath = String(input.file_path || '');
  const oldString = String(input.old_string || '');
  const newString = String(input.new_string || '');
  const replaceAll = Boolean(input.replace_all);

  const diff = useMemo(() => computeUnifiedDiff(oldString, newString), [oldString, newString]);
  const fileName = filePath.split('/').pop() || filePath;

  return (
    <div
      className={cn(chatCardVariants())}
      style={{ fontFamily: 'ui-monospace, "SF Mono", "Monaco", "Inconsolata", "Fira Mono", monospace' }}
    >
      <div className={cn(chatCardHeaderVariants(), 'text-[10px] py-1.5')}>
        <Edit3 size={10} className="text-accent" />
        <span className="font-bold text-foreground">Update</span>
        <span className="opacity-70 text-muted-foreground">({fileName})</span>
        {replaceAll && <span className="px-1 py-0.5 rounded text-[8px] font-bold bg-accent text-white">ALL</span>}
        <div className="ml-auto flex items-center gap-2 text-[9px]">
          {diff.added > 0 && <span style={{ color: syntaxColors.added }}>+{diff.added}</span>}
          {diff.removed > 0 && <span style={{ color: syntaxColors.removed }}>-{diff.removed}</span>}
        </div>
      </div>

      <div className="overflow-x-auto max-h-64 overflow-y-auto">
        {diff.lines.map((line, i) => {
          const isRemoved = line.type === 'removed';
          const isAdded = line.type === 'added';
          const isContext = line.type === 'context';
          const lineNum = isRemoved ? line.oldLineNum : line.newLineNum || line.oldLineNum;

          let bgColor = 'transparent';
          if (isRemoved) bgColor = isDarkMode ? 'rgba(239, 68, 68, 0.12)' : 'rgba(239, 68, 68, 0.08)';
          else if (isAdded) bgColor = isDarkMode ? 'rgba(34, 197, 94, 0.12)' : 'rgba(34, 197, 94, 0.08)';

          let textColor = 'var(--foreground)';
          if (isRemoved) textColor = syntaxColors.removed;
          if (isAdded) textColor = syntaxColors.added;
          if (isContext) textColor = 'var(--muted-foreground)';

          return (
            <div key={i} className="flex text-[10px] leading-relaxed" style={{ backgroundColor: bgColor }}>
              <span className="select-none px-1.5 shrink-0 text-right w-8 border-r border-border text-muted-foreground opacity-50">
                {lineNum}
              </span>
              <span
                className="select-none w-4 text-center shrink-0 font-bold"
                style={{ color: isRemoved ? syntaxColors.removed : isAdded ? syntaxColors.added : 'transparent' }}
              >
                {isRemoved ? '-' : isAdded ? '+' : ' '}
              </span>
              <span className="whitespace-pre pr-2" style={{ color: textColor }}>
                {line.content}
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
});
