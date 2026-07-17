/**
 * WebFetchViewer Component
 *
 * Displays web fetch operations with URL and prompt.
 */

import React, { memo } from 'react';
import { Globe } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { chatCardVariants, chatCardHeaderVariants, badgeVariants } from '../variants';

interface WebFetchViewerProps {
  input: Record<string, unknown>;
}

const webColor = '#06b6d4';

function getDomain(urlString: string): string {
  try {
    const urlObj = new URL(urlString);
    return urlObj.hostname;
  } catch {
    return urlString;
  }
}

export const WebFetchViewer = memo(function WebFetchViewer({ input }: WebFetchViewerProps) {
  const url = String(input.url || '');
  const prompt = String(input.prompt || '');
  const domain = getDomain(url);

  return (
    <div className={cn(chatCardVariants())}>
      {/* Header */}
      <div className={cn(chatCardHeaderVariants())}>
        <Globe size={14} style={{ color: webColor }} />
        <span className="text-[11px] font-bold text-foreground">Web Fetch</span>
        <span
          className={cn(badgeVariants({ variant: 'colored' }))}
          style={{
            backgroundColor: `${webColor}15`,
            color: webColor,
          }}
        >
          {domain}
        </span>
      </div>

      {/* URL display */}
      <div className="px-3 py-2">
        <div
          className="font-mono text-[10px] px-2 py-1.5 rounded truncate bg-card border border-border"
          style={{ color: webColor }}
        >
          {url}
        </div>
      </div>

      {/* Prompt */}
      {prompt && (
        <div className="px-3 pb-2">
          <div className="text-[10px] px-2 py-1.5 rounded bg-card border border-border text-muted-foreground">
            <span className="font-bold text-foreground">Prompt: </span>
            {prompt.length > 100 ? prompt.slice(0, 100) + '...' : prompt}
          </div>
        </div>
      )}
    </div>
  );
});
