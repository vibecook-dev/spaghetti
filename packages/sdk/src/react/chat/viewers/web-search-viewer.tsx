/**
 * WebSearchViewer Component
 *
 * Displays web search operations with query and domain filters.
 */

import React, { memo } from 'react';
import { Search } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { chatCardVariants, chatCardHeaderVariants, badgeVariants } from '../variants';

interface WebSearchViewerProps {
  input: Record<string, unknown>;
}

const searchColor = '#8b5cf6';

export const WebSearchViewer = memo(function WebSearchViewer({ input }: WebSearchViewerProps) {
  const query = String(input.query || '');
  const allowedDomains = (input.allowed_domains as string[]) || [];
  const blockedDomains = (input.blocked_domains as string[]) || [];

  return (
    <div className={cn(chatCardVariants())}>
      {/* Header */}
      <div className={cn(chatCardHeaderVariants())}>
        <Search size={14} style={{ color: searchColor }} />
        <span className="text-[11px] font-bold text-foreground">Web Search</span>
      </div>

      {/* Query display */}
      <div className="px-3 py-2">
        <div
          className="font-mono text-[11px] px-2 py-1.5 rounded bg-card border border-border"
          style={{ color: searchColor }}
        >
          &ldquo;{query}&rdquo;
        </div>
      </div>

      {/* Domain filters */}
      {(allowedDomains.length > 0 || blockedDomains.length > 0) && (
        <div className="px-3 pb-2 flex flex-wrap gap-1">
          {allowedDomains.map((domain, i) => (
            <span key={`allow-${i}`} className={cn(badgeVariants({ variant: 'success' }))}>
              +{domain}
            </span>
          ))}
          {blockedDomains.map((domain, i) => (
            <span key={`block-${i}`} className={cn(badgeVariants({ variant: 'error' }))}>
              -{domain}
            </span>
          ))}
        </div>
      )}
    </div>
  );
});
