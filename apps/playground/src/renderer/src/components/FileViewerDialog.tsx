import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import * as Dialog from '@radix-ui/react-dialog';
import { ExternalLink, Maximize2, Minimize2, Moon, Sun, X } from 'lucide-react';
import { MarkdownContent } from '@vibecook/spaghetti-sdk/react';
import { Spinner } from './ui.js';

type BrowserTarget = 'default' | 'safari' | 'chrome' | 'firefox';

export interface FileViewerDialogProps {
  title: string;
  content: string | null;
  onClose: () => void;
  absolutePath?: string;
  loading?: boolean;
  error?: string | null;
  collection?: string;
  description?: string;
}

/** Archive catalogue viewer for project files and indexed session artifacts. */
export function FileViewerDialog({
  title,
  content,
  onClose,
  absolutePath,
  loading = false,
  error = null,
  collection = 'Project Files / Working Tree',
  description = 'A read-only file from the active local project workspace.',
}: FileViewerDialogProps): ReactNode {
  const [browserError, setBrowserError] = useState<string | null>(null);
  const [contentFullscreen, setContentFullscreen] = useState(false);
  const [fullscreenDark, setFullscreenDark] = useState(false);
  const originalThemeRef = useRef<boolean | null>(null);
  const extension = fileExtension(title);
  const isMarkdown = ['md', 'mdx', 'markdown'].includes(extension);
  const isHtml = ['html', 'htm'].includes(extension);
  const lineCount = content ? content.split('\n').length : 0;
  const reference = `A-${
    title
      .replace(/[^a-z0-9]/gi, '')
      .slice(0, 6)
      .toUpperCase() || '000000'
  }`;

  useEffect(() => setBrowserError(null), [title, absolutePath]);

  useEffect(
    () => () => {
      if (originalThemeRef.current !== null) {
        document.documentElement.classList.toggle('dark', originalThemeRef.current);
      }
    },
    [],
  );

  const renderedSource = useMemo(() => {
    if (content == null) return null;
    if (isMarkdown) {
      return (
        <div className="archive-md archive-md--assistant px-6 py-5 text-ink">
          <MarkdownContent content={content || '(empty)'} />
        </div>
      );
    }
    return (
      <div className="archive-md archive-md--code px-4 py-3 text-ink">
        <MarkdownContent content={fenced(languageForExtension(extension), content || '(empty)')} />
      </div>
    );
  }, [content, extension, isMarkdown]);

  const openInBrowser = async (browser: BrowserTarget) => {
    if (!absolutePath || !window.fileViewer) return;
    setBrowserError(null);
    try {
      await window.fileViewer.openHtmlInBrowser(absolutePath, browser);
    } catch (e: unknown) {
      setBrowserError(e instanceof Error ? e.message : String(e));
    }
  };

  const enterContentFullscreen = () => {
    const isDark = document.documentElement.classList.contains('dark');
    originalThemeRef.current = isDark;
    setFullscreenDark(isDark);
    setContentFullscreen(true);
  };

  const exitContentFullscreen = () => {
    if (originalThemeRef.current !== null) {
      document.documentElement.classList.toggle('dark', originalThemeRef.current);
    }
    originalThemeRef.current = null;
    setContentFullscreen(false);
  };

  const toggleFullscreenTheme = () => {
    const next = !fullscreenDark;
    document.documentElement.classList.toggle('dark', next);
    setFullscreenDark(next);
  };

  return (
    <Dialog.Root open onOpenChange={(open) => !open && onClose()}>
      <Dialog.Portal>
        <Dialog.Overlay className="fixed inset-0 z-50 bg-[#11100f]/72 backdrop-blur-[3px]" />
        <Dialog.Content
          className={
            contentFullscreen
              ? 'fixed inset-0 z-[60] flex h-screen w-screen flex-col bg-[#f8f6f0] text-[#2b2623] outline-none dark:bg-[#171615] dark:text-[#d4cbbd]'
              : 'fixed left-1/2 top-1/2 z-50 flex h-[min(48rem,88vh)] w-[min(62rem,calc(100vw-2rem))] -translate-x-1/2 -translate-y-1/2 flex-col border border-[#2b2623]/60 bg-[#f8f6f0] text-[#2b2623] shadow-2xl outline-none dark:border-[#d4cbbd]/45 dark:bg-[#171615] dark:text-[#d4cbbd]'
          }
          onEscapeKeyDown={(event) => {
            if (!contentFullscreen) return;
            event.preventDefault();
            exitContentFullscreen();
          }}
          onKeyDown={(event) => {
            if (event.key !== 'Escape' || !contentFullscreen) return;
            event.preventDefault();
            event.stopPropagation();
            exitContentFullscreen();
          }}
        >
          <div
            className={`items-center justify-between border-b border-[#2b2623]/20 px-6 py-3 dark:border-[#d4cbbd]/20 ${
              contentFullscreen ? 'hidden' : 'flex'
            }`}
          >
            <span className="font-mono text-[9px] tracking-[0.18em] opacity-55">SPAGHETTI ARCHIVE · FILE VIEWER</span>
            <Dialog.Close asChild>
              <button
                type="button"
                className="flex cursor-pointer items-center gap-2 border-0 bg-transparent font-mono text-[9px] tracking-[0.14em] text-inherit opacity-60 transition-opacity hover:opacity-100"
                aria-label="Close file viewer"
              >
                <span>CLOSE</span>
                <X size={14} />
              </button>
            </Dialog.Close>
          </div>

          <div className={`shrink-0 gap-6 px-6 py-6 md:grid-cols-[10rem_1fr] ${contentFullscreen ? 'hidden' : 'grid'}`}>
            <div className="border-r border-[#2b2623]/20 pr-5 font-mono text-[9px] tracking-[0.12em] dark:border-[#d4cbbd]/20">
              <p className="mb-5 opacity-50">CATALOGUE ENTRY</p>
              <dl className="space-y-4 leading-relaxed">
                <div>
                  <dt className="opacity-45">COLLECTION</dt>
                  <dd className="mt-0.5 text-[10px] tracking-[0.06em]">{collection}</dd>
                </div>
                <div>
                  <dt className="opacity-45">REFERENCE</dt>
                  <dd className="mt-0.5 text-[10px] tracking-[0.06em]">{reference}</dd>
                </div>
                <div>
                  <dt className="opacity-45">FORMAT</dt>
                  <dd className="mt-0.5 text-[10px] uppercase tracking-[0.06em]">{extension || 'text'} · UTF-8</dd>
                </div>
              </dl>
            </div>

            <div className="min-w-0">
              <Dialog.Title className="break-words font-serif text-[clamp(2rem,5vw,3.35rem)] font-medium leading-[0.92] tracking-[-0.02em]">
                {title}
              </Dialog.Title>
              <Dialog.Description className="mt-4 max-w-xl font-serif text-[15px] leading-relaxed opacity-70">
                {description}
              </Dialog.Description>
              <div className="mt-5 flex flex-wrap items-center gap-3 border-t border-[#2b2623]/20 pt-3 font-mono text-[9px] tracking-[0.12em] opacity-55 dark:border-[#d4cbbd]/20">
                <span>READ-ONLY</span>
                <span>·</span>
                <span>{loading ? 'READING' : `${lineCount} LINES`}</span>
                <span>·</span>
                <span>{isMarkdown ? 'MARKDOWN RENDER' : 'SYNTAX VIEW'}</span>
              </div>

              {isHtml && absolutePath ? (
                <div className="mt-4 flex flex-wrap items-center gap-2">
                  <span className="mr-1 font-mono text-[9px] uppercase tracking-[0.12em] opacity-45">Open in</span>
                  {(
                    [
                      ['default', 'Default'],
                      ['safari', 'Safari'],
                      ['chrome', 'Chrome'],
                      ['firefox', 'Firefox'],
                    ] as const
                  ).map(([browser, label]) => (
                    <button
                      key={browser}
                      type="button"
                      onClick={() => void openInBrowser(browser)}
                      className="inline-flex cursor-pointer items-center gap-1.5 border border-[#2b2623]/25 bg-transparent px-2.5 py-1 font-mono text-[9px] uppercase tracking-[0.1em] text-inherit transition-colors hover:bg-[#2b2623] hover:text-[#f8f6f0] dark:border-[#d4cbbd]/25 dark:hover:bg-[#d4cbbd] dark:hover:text-[#11100f]"
                    >
                      <ExternalLink size={10} />
                      {label}
                    </button>
                  ))}
                </div>
              ) : null}

              {browserError ? <p className="mt-2 font-mono text-[9px] text-sanguine">{browserError}</p> : null}
            </div>
          </div>

          <div
            className={
              contentFullscreen
                ? 'relative min-h-0 flex-1 bg-[#eee9de] dark:bg-[#0f0e0d]'
                : 'relative mx-6 min-h-0 flex-1 border border-[#2b2623]/20 bg-[#eee9de] dark:border-[#d4cbbd]/20 dark:bg-[#0f0e0d]'
            }
          >
            <div className="absolute right-3 top-3 z-10 flex items-center gap-2">
              {contentFullscreen ? (
                <button
                  type="button"
                  onClick={toggleFullscreenTheme}
                  className="inline-flex size-7 cursor-pointer items-center justify-center border border-[#2b2623]/25 bg-[#f8f6f0]/95 text-[#2b2623] shadow-sm backdrop-blur-sm transition-colors hover:bg-[#2b2623] hover:text-[#f8f6f0] dark:border-[#d4cbbd]/30 dark:bg-[#171615]/95 dark:text-[#d4cbbd] dark:hover:bg-[#d4cbbd] dark:hover:text-[#11100f]"
                  aria-label={fullscreenDark ? 'Use light file viewer theme' : 'Use dark file viewer theme'}
                  title={fullscreenDark ? 'Use light theme' : 'Use dark theme'}
                >
                  {fullscreenDark ? <Sun size={13} /> : <Moon size={13} />}
                </button>
              ) : null}

              <button
                type="button"
                onClick={contentFullscreen ? exitContentFullscreen : enterContentFullscreen}
                className="inline-flex size-7 cursor-pointer items-center justify-center border border-[#2b2623]/25 bg-[#f8f6f0]/95 text-[#2b2623] shadow-sm backdrop-blur-sm transition-colors hover:bg-[#2b2623] hover:text-[#f8f6f0] dark:border-[#d4cbbd]/30 dark:bg-[#171615]/95 dark:text-[#d4cbbd] dark:hover:bg-[#d4cbbd] dark:hover:text-[#11100f]"
                aria-label={contentFullscreen ? 'Exit fullscreen file content' : 'View file content fullscreen'}
                title={contentFullscreen ? 'Exit fullscreen (Esc)' : 'View content fullscreen'}
              >
                {contentFullscreen ? <Minimize2 size={13} /> : <Maximize2 size={13} />}
              </button>
            </div>

            <div className="h-full overflow-auto scrollbar-hide">
              {loading ? (
                <div className="flex h-full items-center justify-center gap-3 font-mono text-[10px] uppercase tracking-widest opacity-50">
                  <Spinner /> Reading file…
                </div>
              ) : error ? (
                <div className="p-5 pr-14 font-mono text-[10px] leading-relaxed text-sanguine">{error}</div>
              ) : (
                renderedSource
              )}
            </div>
          </div>

          <div
            className={`mx-6 mt-4 items-center justify-between border-t border-[#2b2623]/20 py-3 font-mono text-[9px] tracking-[0.12em] opacity-50 dark:border-[#d4cbbd]/20 ${
              contentFullscreen ? 'hidden' : 'flex'
            }`}
          >
            <span className="max-w-[70%] truncate" title={absolutePath}>
              {absolutePath || 'LOCAL SESSION ARCHIVE'}
            </span>
            <span>ESC TO RETURN</span>
          </div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

function fileExtension(name: string): string {
  const dot = name.lastIndexOf('.');
  return dot >= 0 ? name.slice(dot + 1).toLowerCase() : '';
}

function languageForExtension(extension: string): string {
  const aliases: Record<string, string> = {
    cjs: 'javascript',
    css: 'css',
    go: 'go',
    htm: 'html',
    html: 'html',
    java: 'java',
    js: 'javascript',
    json: 'json',
    jsonl: 'json',
    jsx: 'jsx',
    mdx: 'markdown',
    mjs: 'javascript',
    py: 'python',
    rb: 'ruby',
    rs: 'rust',
    sh: 'bash',
    sql: 'sql',
    svg: 'xml',
    toml: 'toml',
    ts: 'typescript',
    tsx: 'tsx',
    xml: 'xml',
    yaml: 'yaml',
    yml: 'yaml',
    zsh: 'bash',
  };
  return aliases[extension] || extension || 'text';
}

function fenced(language: string, content: string): string {
  const fence = '```';
  return `${fence}${language}\n${content.replace(/```/g, '``\u200b`')}\n${fence}`;
}
