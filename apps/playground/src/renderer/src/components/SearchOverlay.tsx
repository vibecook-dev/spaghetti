/**
 * Full-text search overlay (Cmd/Ctrl+K).
 * Hits navigate into project → session when possible.
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import type { SearchResult, SearchResultSet } from '@vibecook/spaghetti-sdk';
import { SourceBadge } from './SourceBadge.js';
import { Btn, Chip, EmptyState, Kbd, Spinner } from './ui.js';
import { sourceLabel } from '../lib/source-progress.js';
import { flattenPrompt } from '../lib/format.js';

export interface SearchNavigateTarget {
  projectSlug: string;
  sourceId: string;
  sessionId?: string;
}

export interface SearchOverlayProps {
  open: boolean;
  onClose: () => void;
  /** Known sources for filter chips (from project list). */
  sourceIds: string[];
  /** Optional scope: limit search to selected project. */
  scopeProject?: { slug: string; sourceId: string; folderName?: string } | null;
  onNavigate: (target: SearchNavigateTarget) => void;
  isDark?: boolean;
}

const LIMIT = 40;

export function SearchOverlay({
  open,
  onClose,
  sourceIds,
  scopeProject,
  onNavigate,
  isDark = true,
}: SearchOverlayProps) {
  const inputRef = useRef<HTMLInputElement>(null);
  const [text, setText] = useState('');
  const [sourceFilter, setSourceFilter] = useState<string | null>(null);
  const [scopeToProject, setScopeToProject] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [results, setResults] = useState<SearchResultSet | null>(null);
  const [activeIndex, setActiveIndex] = useState(0);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const seqRef = useRef(0);

  // Reset + focus when opened
  useEffect(() => {
    if (!open) return;
    setText('');
    setResults(null);
    setError(null);
    setActiveIndex(0);
    setSourceFilter(null);
    setScopeToProject(false);
    const t = window.setTimeout(() => inputRef.current?.focus(), 30);
    return () => window.clearTimeout(t);
  }, [open]);

  // Escape to close
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [open, onClose]);

  const runSearch = useCallback(
    async (query: string, sourceId: string | null, projectOnly: boolean) => {
      const q = query.trim();
      if (!q) {
        setResults(null);
        setLoading(false);
        setError(null);
        return;
      }
      const seq = ++seqRef.current;
      setLoading(true);
      setError(null);
      try {
        const res = await window.spaghetti.search({
          text: q,
          limit: LIMIT,
          ...(sourceId ? { sourceId } : {}),
          ...(projectOnly && scopeProject ? { projectSlug: scopeProject.slug, sourceId: scopeProject.sourceId } : {}),
        });
        if (seq !== seqRef.current) return;
        setResults(res);
        setActiveIndex(0);
      } catch (e: unknown) {
        if (seq !== seqRef.current) return;
        setError(String(e));
        setResults(null);
      } finally {
        if (seq === seqRef.current) setLoading(false);
      }
    },
    [scopeProject],
  );

  // Debounced search as you type
  useEffect(() => {
    if (!open) return;
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => {
      void runSearch(text, sourceFilter, scopeToProject);
    }, 220);
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, [text, sourceFilter, scopeToProject, open, runSearch]);

  const hits = results?.results ?? [];

  const navigateTo = useCallback(
    (r: SearchResult) => {
      if (!r.projectSlug) return;
      onNavigate({
        projectSlug: r.projectSlug,
        sourceId: r.sourceId ?? 'claude-code',
        sessionId: r.sessionId,
      });
      onClose();
    },
    [onNavigate, onClose],
  );

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      setActiveIndex((i) => Math.min(i + 1, Math.max(hits.length - 1, 0)));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setActiveIndex((i) => Math.max(i - 1, 0));
    } else if (e.key === 'Enter' && hits[activeIndex]) {
      e.preventDefault();
      navigateTo(hits[activeIndex]);
    }
  };

  if (!open) return null;

  return (
    <div
      className={`fixed inset-0 z-50 flex items-start justify-center pt-[12vh] px-4 backdrop-blur-[3px] ${
        isDark ? 'bg-[#11100f]/72' : 'bg-[#2b2623]/40'
      }`}
      role="dialog"
      aria-modal="true"
      aria-label="Search agent history"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className={`w-full max-w-xl border shadow-2xl overflow-hidden flex flex-col max-h-[70vh] ${
          isDark ? 'border-[#d4cbbd]/45 bg-[#171615] text-ink' : 'border-[#2b2623]/50 bg-[#f8f6f0] text-ink'
        }`}
      >
        <div className="flex items-center gap-2 px-4 py-3 border-b border-[color:var(--archive-ink-line)]">
          <span className="font-mono text-[9px] tracking-[0.18em] opacity-55">SPAGHETTI ARCHIVE · SEARCH</span>
          <span className="flex-1" />
          <Kbd>esc</Kbd>
        </div>

        <div className="flex items-center gap-2 px-4 py-2.5 border-b border-[color:var(--archive-ink-line-mid)]">
          <span className="opacity-40 text-sm select-none" aria-hidden>
            ⌕
          </span>
          <input
            ref={inputRef}
            type="search"
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={onKeyDown}
            placeholder="Search messages, plans, todos…"
            className="flex-1 bg-transparent font-serif text-[15px] text-ink outline-none placeholder:opacity-35 min-w-0"
            autoComplete="off"
            spellCheck={false}
          />
          {loading ? <Spinner /> : null}
        </div>

        <div className="flex items-center gap-1.5 px-3 py-2 border-b border-[color:var(--archive-ink-line-soft)] flex-wrap">
          <Chip active={sourceFilter === null} onClick={() => setSourceFilter(null)}>
            All agents
          </Chip>
          {sourceIds.map((id) => (
            <Chip key={id} active={sourceFilter === id} onClick={() => setSourceFilter(id)} title={id}>
              {sourceLabel(id)}
            </Chip>
          ))}
          {scopeProject ? (
            <Chip
              active={scopeToProject}
              onClick={() => setScopeToProject((v) => !v)}
              className="ml-auto"
              title={`${scopeProject.slug} · ${scopeProject.sourceId}`}
            >
              In {scopeProject.folderName || 'project'}
            </Chip>
          ) : null}
        </div>

        <div className="flex-1 overflow-y-auto min-h-0 scrollbar-hide">
          {error ? (
            <div className="px-4 py-3 font-mono text-[11px] text-sanguine">{error}</div>
          ) : !text.trim() ? (
            <EmptyState
              title="Search local agent history"
              detail="Full-text over indexed messages and artifacts. Use ↑↓ to move, Enter to open."
            />
          ) : loading && hits.length === 0 ? (
            <div className="flex items-center justify-center gap-2 py-10 font-mono text-[10px] tracking-widest uppercase opacity-50">
              <Spinner />
              Searching…
            </div>
          ) : hits.length === 0 ? (
            <EmptyState
              title="No matches"
              detail={`Nothing for “${text.trim()}”. Try another phrase or agent filter.`}
            />
          ) : (
            <ul className="py-1" role="listbox">
              {hits.map((r, i) => {
                const active = i === activeIndex;
                return (
                  <li key={`${r.key}-${i}`} role="option" aria-selected={active}>
                    <button
                      type="button"
                      onMouseEnter={() => setActiveIndex(i)}
                      onClick={() => navigateTo(r)}
                      className={`w-full text-left px-4 py-2.5 border-none cursor-pointer transition-colors ${
                        active ? 'bg-ink/10' : 'bg-transparent hover:bg-ink/5'
                      }`}
                    >
                      <div className="flex items-center gap-2 mb-1">
                        <span className="text-[9px] font-mono text-sanguine/80 uppercase tracking-widest shrink-0">
                          {r.type}
                        </span>
                        {r.sourceId ? <SourceBadge sourceId={r.sourceId} isDark={isDark} /> : null}
                        {r.projectSlug ? (
                          <span className="text-[10px] font-mono tracking-tight text-ink/50 truncate min-w-0">
                            {shortSlug(r.projectSlug)}
                          </span>
                        ) : null}
                        {r.sessionId ? (
                          <span className="text-[9px] font-mono tracking-[0.08em] text-ink/30 ml-auto shrink-0">
                            {r.sessionId.slice(0, 8)}
                          </span>
                        ) : null}
                      </div>
                      {/* Design prose snippets: serif ~13–14px relaxed */}
                      <div className="font-serif text-[13px] italic text-ink/75 leading-relaxed line-clamp-2">
                        {flattenPrompt(r.snippet, 180) || '(empty snippet)'}
                      </div>
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>

        <div className="flex items-center gap-3 px-4 py-2 border-t border-[color:var(--archive-ink-line-mid)] font-mono text-[9px] tracking-widest uppercase opacity-50">
          <span>
            {results ? (
              <>
                {results.total.toLocaleString()} result{results.total === 1 ? '' : 's'}
                {results.hasMore ? '+' : ''}
              </>
            ) : (
              'Ready'
            )}
          </span>
          <span className="flex-1" />
          <Btn variant="ghost" className="!py-0.5 !px-2" onClick={onClose}>
            Close
          </Btn>
        </div>
      </div>
    </div>
  );
}

function shortSlug(slug: string): string {
  // Claude slugs are often path-encoded (-Users-…-Projects-foo)
  const parts = slug.split('-').filter(Boolean);
  if (parts.length <= 3) return slug;
  return parts.slice(-2).join('/') || slug;
}
