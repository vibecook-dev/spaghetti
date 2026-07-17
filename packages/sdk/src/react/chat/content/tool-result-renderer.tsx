/**
 * ToolResultRenderer Component
 *
 * Renders tool results with appropriate formatting based on content type.
 * - Text items: rendered as markdown
 * - Images: rendered inline as base64
 * - Errors: styled with destructive color
 */

import React, { memo, useMemo } from 'react';
import { MarkdownContent } from './markdown-content';
import { typography } from '../theme';

// =============================================================================
// TYPES
// =============================================================================

interface ToolResultRendererProps {
  toolName: string;
  content: string;
  isError?: boolean;
  rawJson?: unknown;
}

interface ContentItem {
  type: 'text' | 'image';
  text?: string;
  source?: {
    type: 'base64';
    media_type: string;
    data: string;
  };
}

interface RawToolResult {
  content?: string | ContentItem[];
}

interface RawUserMessage {
  message?: {
    content?: Array<{
      type: 'tool_result';
      content?: string | ContentItem[];
    }>;
  };
}

// =============================================================================
// CONTENT EXTRACTION
// =============================================================================

function extractContentItems(rawJson: unknown): ContentItem[] {
  if (!rawJson || typeof rawJson !== 'object') return [];

  const raw = rawJson as RawToolResult & RawUserMessage;

  if (raw.content) {
    if (Array.isArray(raw.content)) {
      return raw.content.filter(
        (item): item is ContentItem => typeof item === 'object' && item !== null && 'type' in item,
      );
    }
    if (typeof raw.content === 'string') {
      return [{ type: 'text', text: raw.content }];
    }
  }

  if (raw.message?.content && Array.isArray(raw.message.content)) {
    for (const item of raw.message.content) {
      if (item.type === 'tool_result' && item.content) {
        if (Array.isArray(item.content)) {
          return item.content.filter((c): c is ContentItem => typeof c === 'object' && c !== null && 'type' in c);
        }
        if (typeof item.content === 'string') {
          return [{ type: 'text', text: item.content }];
        }
      }
    }
  }

  return [];
}

// =============================================================================
// IMAGE RENDERER
// =============================================================================

const ImageRenderer = memo(function ImageRenderer({
  source,
}: {
  source: { type: string; media_type: string; data: string };
}) {
  const src = `data:${source.media_type};base64,${source.data}`;
  return (
    <div className="my-1">
      <img src={src} alt="Tool result" className="max-w-full h-auto rounded border border-border" />
    </div>
  );
});

// =============================================================================
// TEXT RENDERER
// =============================================================================

const TextRenderer = memo(function TextRenderer({ text }: { text: string }) {
  return (
    <div className="text-[10px]">
      <MarkdownContent content={text} />
    </div>
  );
});

// =============================================================================
// FALLBACK RENDERER
// =============================================================================

const FallbackRenderer = memo(function FallbackRenderer({ content }: { content: string }) {
  return (
    <pre
      className="text-[10px] font-mono leading-relaxed whitespace-pre-wrap break-all text-muted-foreground"
      style={{ fontFamily: typography.mono }}
    >
      {content || 'No output'}
    </pre>
  );
});

// =============================================================================
// MAIN COMPONENT
// =============================================================================

export const ToolResultRenderer = memo(function ToolResultRenderer({
  content,
  isError = false,
  rawJson,
}: ToolResultRendererProps) {
  const contentItems = useMemo(() => extractContentItems(rawJson), [rawJson]);

  if (isError) {
    return (
      <div
        className="text-[10px] font-mono leading-relaxed whitespace-pre-wrap break-all text-destructive"
        style={{ fontFamily: typography.mono }}
      >
        {content || 'Error (no details)'}
      </div>
    );
  }

  if (contentItems.length > 0) {
    return (
      <div className="space-y-1">
        {contentItems.map((item, idx) => {
          if (item.type === 'image' && item.source) {
            return <ImageRenderer key={idx} source={item.source} />;
          }
          if (item.type === 'text' && item.text) {
            return <TextRenderer key={idx} text={item.text} />;
          }
          return null;
        })}
      </div>
    );
  }

  return <FallbackRenderer content={content} />;
});
