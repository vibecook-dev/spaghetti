/**
 * CodeBlock Component
 *
 * Displays code with line numbers and copy functionality.
 */

import React, { useState, useCallback, useMemo, memo } from 'react';
import { FileText, Copy, Check } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { typography } from '../theme';

interface CodeBlockProps {
  filename?: string;
  code: string;
}

export const CodeBlock = memo(function CodeBlock({ filename, code }: CodeBlockProps) {
  const [copied, setCopied] = useState(false);

  const handleCopy = useCallback(async () => {
    await navigator.clipboard.writeText(code);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  }, [code]);

  const lines = useMemo(() => code.split('\n'), [code]);

  return (
    <div
      className={cn('my-1 border border-border shadow-sm rounded bg-background')}
      style={{ fontFamily: typography.mono }}
    >
      {/* Header */}
      <div className="flex items-center justify-between px-2 py-1 border-b border-border bg-card">
        <span className="font-medium flex items-center gap-1.5 text-[10px] text-foreground">
          <FileText size={10} className="text-muted-foreground" />
          {filename || 'output'}
        </span>
        <button
          onClick={handleCopy}
          className="p-0.5 opacity-50 hover:opacity-100 transition-opacity"
          title={copied ? 'Copied!' : 'Copy'}
        >
          {copied ? <Check size={10} /> : <Copy size={10} />}
        </button>
      </div>

      {/* Code content */}
      <div className="p-2 overflow-x-auto max-h-48 overflow-y-auto">
        <pre className="leading-tight text-[10px] text-foreground">
          {lines.map((line, i) => (
            <div key={i} className="table-row">
              <span className="table-cell select-none text-right pr-2 opacity-30 w-6 text-[9px]">{i + 1}</span>
              <span className="table-cell whitespace-pre">{line}</span>
            </div>
          ))}
        </pre>
      </div>
    </div>
  );
});
