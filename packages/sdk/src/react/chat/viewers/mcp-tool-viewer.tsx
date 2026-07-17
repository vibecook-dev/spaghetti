/**
 * MCPToolViewer Component
 *
 * Displays MCP (Model Context Protocol) tool operations.
 * Handles dynamic mcp__<server>__<tool> format.
 */

import React, { memo } from 'react';
import { Plug } from 'lucide-react';
import { cn } from '../../lib/utils.js';
import { chatCardVariants, chatCardHeaderVariants, badgeVariants } from '../variants';

interface MCPToolViewerProps {
  toolName: string;
  input: Record<string, unknown>;
}

const mcpColor = '#14b8a6';

export const MCPToolViewer = memo(function MCPToolViewer({ toolName, input }: MCPToolViewerProps) {
  // Parse mcp__server__tool format
  const parts = toolName.split('__');
  const server = parts[1] || 'unknown';
  const tool = parts.slice(2).join('__') || 'unknown';

  const inputKeys = Object.keys(input);
  const hasInput = inputKeys.length > 0;

  return (
    <div className={cn(chatCardVariants())}>
      {/* Header */}
      <div className={cn(chatCardHeaderVariants())}>
        <Plug size={14} style={{ color: mcpColor }} />
        <span className="text-[11px] font-bold text-foreground">MCP</span>
        <span
          className={cn(badgeVariants({ variant: 'colored' }))}
          style={{
            backgroundColor: `${mcpColor}15`,
            color: mcpColor,
          }}
        >
          {server}
        </span>
        <span className="text-[10px] font-mono text-muted-foreground">&rarr;</span>
        <span className="text-[10px] font-mono font-bold text-foreground">{tool}</span>
      </div>

      {/* Input parameters preview */}
      {hasInput && (
        <div className="px-3 py-2">
          <div className="font-mono text-[10px] px-2 py-1.5 rounded space-y-1 bg-card border border-border">
            {inputKeys.slice(0, 4).map((key) => {
              const value = input[key];
              const displayValue =
                typeof value === 'string'
                  ? value.length > 40
                    ? value.slice(0, 40) + '...'
                    : value
                  : JSON.stringify(value);
              return (
                <div key={key} className="flex gap-2">
                  <span style={{ color: mcpColor }}>{key}:</span>
                  <span className="truncate text-muted-foreground">{displayValue}</span>
                </div>
              );
            })}
            {inputKeys.length > 4 && (
              <div className="text-muted-foreground opacity-60">+{inputKeys.length - 4} more parameters</div>
            )}
          </div>
        </div>
      )}
    </div>
  );
});
