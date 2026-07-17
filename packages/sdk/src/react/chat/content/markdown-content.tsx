/**
 * MarkdownContent Component
 *
 * Renders markdown content with syntax highlighting.
 * Uses react-markdown with remark-gfm for GitHub-flavored markdown.
 * Auto-detects dark mode via useIsDark() hook instead of prop threading.
 */

import React, { useMemo, memo } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { Prism as SyntaxHighlighter } from 'react-syntax-highlighter';
import { oneDark, oneLight } from 'react-syntax-highlighter/dist/esm/styles/prism';
import { typography } from '../theme';
import { useIsDark } from '../utils/helpers';

// =============================================================================
// CODE BLOCK STYLES
// =============================================================================

const codeBlockStyle = {
  margin: 0,
  padding: '8px 10px',
  fontSize: '11px',
  lineHeight: '1.4',
  borderRadius: '4px',
  border: '1px solid var(--border)',
};

// =============================================================================
// HIGHLIGHTED CODE COMPONENT
// =============================================================================

const HighlightedCode = memo(function HighlightedCode({
  code,
  language,
  isDarkMode,
}: {
  code: string;
  language: string;
  isDarkMode: boolean;
}) {
  return (
    <SyntaxHighlighter
      style={isDarkMode ? oneDark : oneLight}
      language={language}
      PreTag="div"
      customStyle={codeBlockStyle}
      codeTagProps={{
        style: {
          fontFamily: typography.mono,
          fontSize: '11px',
          lineHeight: '1.4',
        },
      }}
    >
      {code}
    </SyntaxHighlighter>
  );
});

// =============================================================================
// MARKDOWN COMPONENTS FACTORY
// =============================================================================

const createMarkdownComponents = (isDarkMode: boolean) => ({
  code({ className, children, ...props }: { className?: string; children?: React.ReactNode }) {
    const match = /language-(\w+)/.exec(className || '');
    const codeString = String(children).replace(/\n$/, '');
    const isInline = !match && !codeString.includes('\n');

    if (isInline) {
      return (
        <code
          className="px-1 py-0.5 text-[11px] bg-foreground/10 rounded-[3px]"
          style={{ fontFamily: typography.mono }}
          {...props}
        >
          {children}
        </code>
      );
    }

    return (
      <div className="my-1">
        <HighlightedCode code={codeString} language={match?.[1] || 'text'} isDarkMode={isDarkMode} />
      </div>
    );
  },

  pre({ children }: { children?: React.ReactNode }) {
    return <>{children}</>;
  },

  p({ children }: { children?: React.ReactNode }) {
    return <p className="mb-1 last:mb-0 leading-tight text-[11px]">{children}</p>;
  },

  ul({ children }: { children?: React.ReactNode }) {
    return <ul className="list-disc pl-3 mb-1 space-y-0 text-[11px] leading-tight">{children}</ul>;
  },

  ol({ children }: { children?: React.ReactNode }) {
    return <ol className="list-decimal pl-3 mb-1 space-y-0 text-[11px] leading-tight">{children}</ol>;
  },

  li({ children }: { children?: React.ReactNode }) {
    return <li className="leading-tight">{children}</li>;
  },

  h1({ children }: { children?: React.ReactNode }) {
    return <h1 className="text-[13px] font-bold mb-1 mt-1.5 first:mt-0 leading-tight">{children}</h1>;
  },

  h2({ children }: { children?: React.ReactNode }) {
    return <h2 className="text-[12px] font-bold mb-0.5 mt-1 first:mt-0 leading-tight">{children}</h2>;
  },

  h3({ children }: { children?: React.ReactNode }) {
    return <h3 className="text-[11px] font-semibold mb-0.5 mt-1 first:mt-0 leading-tight">{children}</h3>;
  },

  blockquote({ children }: { children?: React.ReactNode }) {
    return (
      <blockquote className="border-l-2 border-border pl-2 my-1 text-[10px] opacity-80 leading-tight">
        {children}
      </blockquote>
    );
  },

  a({ href, children }: { href?: string; children?: React.ReactNode }) {
    return (
      <a
        href={href}
        className="text-[11px] underline text-accent opacity-80 hover:opacity-100"
        target="_blank"
        rel="noopener noreferrer"
      >
        {children}
      </a>
    );
  },

  strong({ children }: { children?: React.ReactNode }) {
    return <strong className="font-semibold">{children}</strong>;
  },

  em({ children }: { children?: React.ReactNode }) {
    return <em className="italic">{children}</em>;
  },

  hr() {
    return <hr className="my-1.5 border-t border-border" />;
  },

  table({ children }: { children?: React.ReactNode }) {
    return (
      <div className="overflow-x-auto my-1">
        <table className="text-[10px] border-collapse w-full leading-tight">{children}</table>
      </div>
    );
  },

  th({ children }: { children?: React.ReactNode }) {
    return <th className="border border-border px-1.5 py-0.5 text-left font-semibold bg-background">{children}</th>;
  },

  td({ children }: { children?: React.ReactNode }) {
    return <td className="border border-border px-1.5 py-0.5">{children}</td>;
  },
});

// =============================================================================
// COMPONENT CACHE
// =============================================================================

const markdownComponentsCache = new Map<boolean, ReturnType<typeof createMarkdownComponents>>();

function getMarkdownComponents(isDarkMode: boolean) {
  if (!markdownComponentsCache.has(isDarkMode)) {
    markdownComponentsCache.set(isDarkMode, createMarkdownComponents(isDarkMode));
  }
  return markdownComponentsCache.get(isDarkMode)!;
}

const remarkPlugins = [remarkGfm];

// =============================================================================
// MAIN COMPONENT
// =============================================================================

interface MarkdownContentProps {
  content: string;
}

export const MarkdownContent = memo(function MarkdownContent({ content }: MarkdownContentProps) {
  const isDarkMode = useIsDark();
  const components = useMemo(() => getMarkdownComponents(isDarkMode), [isDarkMode]);

  return (
    <ReactMarkdown remarkPlugins={remarkPlugins} components={components}>
      {content}
    </ReactMarkdown>
  );
});
